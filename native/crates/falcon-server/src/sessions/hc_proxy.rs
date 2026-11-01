//! Remote SSH session proxy for Herdr (`hc` / `herdr-app`).
//!
//! When `falcon-server` attaches to a durable session on a remote SSH host, it mirrors that
//! session into the local `<data_dir>/sessiond/` directory as a local Unix domain socket proxy
//! (`<sid>.json`, `<sid>.labels.json`, and `<sid>.sock`).
//!
//! When `falcon-sessiond` (`herdr-core`) connects to `<sid>.sock`, `falcon-server` opens an SSH
//! channel running `falcon-sessiond bridge --sid <sid>` on the remote host, waits for the
//! `FALCONBR\n` preamble, and bidirectionally forwards raw bytes between the local Unix socket
//! and the remote SSH channel.
//!
//! In addition, `falcon-server` listens on `<data_dir>/sessiond/server-hc.sock` so that
//! `terminal.run` requests targeting a remote SSH host or project can be delegated to
//! `SessionManager::create_session`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use falcon_proto::OscColorHint;
use falcon_proto::sessiond::{
    BRIDGE_PREAMBLE, ExitRecord, ExitStatus, SESSIOND_DIR, SessionMeta, Welcome, sid_of, socket_path,
};
use serde::{Deserialize, Serialize};
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;

use super::machine::owner_row;
use super::manager::SessionManager;
use super::ssh::SshLink;
use crate::auth::now_ms;
use crate::db::{Db, ProjectRow, SessionRow};
use crate::sessiond::command::{bridge_argv, bridge_command};
use crate::sessiond::deploy::Deployed;

/// Handle to an active local Unix socket proxy for a remote SSH session.
pub struct HcProxyHandle {
    cancel: CancellationToken,
    meta_path: PathBuf,
    endpoint: PathBuf,
    exit_path: PathBuf,
}

impl HcProxyHandle {
    /// Stops the proxy accept loop, removes `<sid>.sock` and `<sid>.json`, and optionally writes
    /// `<sid>.exit` when the remote session exited normally.
    pub fn stop(&self, write_exit: bool) {
        self.cancel.cancel();
        let _ = std::fs::remove_file(&self.endpoint);
        let _ = std::fs::remove_file(&self.meta_path);
        if write_exit {
            let rec = ExitRecord {
                at: now_ms(),
                status: ExitStatus { code: Some(0), signal: None },
            };
            let _ = write_atomic_json(&self.exit_path, &rec);
        }
    }
}

impl Drop for HcProxyHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        let _ = std::fs::remove_file(&self.endpoint);
        let _ = std::fs::remove_file(&self.meta_path);
    }
}

/// Synchronizes `<data_dir>/sessiond/sessions/<sid>.labels.json` for a session (local or remote).
/// Preserves existing non-`falcon.*` labels (such as `hc.name`).
pub fn sync_session_labels(data_dir: &Path, db: &Db, project: &ProjectRow, row: &SessionRow) {
    let sid = sid_of(&row.id);
    let sessions_dir = data_dir.join(SESSIOND_DIR).join("sessions");
    if ensure_dir_0700(&sessions_dir).is_err() {
        return;
    }
    let labels_path = sessions_dir.join(format!("{sid}.labels.json"));
    let mut labels: BTreeMap<String, String> = std::fs::read(&labels_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();

    labels.insert("falcon.project".into(), project.id.clone());
    labels.insert("falcon.project_name".into(), project.name.clone());
    labels.insert("falcon.session".into(), row.name.clone());
    labels.insert("falcon.session_id".into(), row.id.clone());
    if falcon_proto::is_standalone_project_id(&project.id) {
        labels.insert("falcon.standalone".into(), "true".into());
    } else {
        labels.remove("falcon.standalone");
    }

    if project.project_type != "local" {
        labels.insert("falcon.remote".into(), "true".into());
        let host_id = project
            .host_id
            .as_deref()
            .or_else(|| falcon_proto::standalone_host_id(&project.id));
        let host_key = host_id
            .map(str::to_string)
            .or_else(|| project.ssh_host.clone())
            .unwrap_or_default();
        let host_name = host_id
            .and_then(|id| db.get_host(id))
            .map(|h| h.name)
            .or_else(|| project.ssh_host.clone())
            .unwrap_or_default();
        if !host_key.is_empty() {
            labels.insert("falcon.host".into(), host_key);
        }
        if !host_name.is_empty() {
            labels.insert("falcon.host_name".into(), host_name);
        }
    }

    let _ = write_atomic_json(&labels_path, &labels);
}

/// Merges additional user/herdr labels into `<sid>.labels.json` without clobbering `falcon.*`.
pub fn merge_extra_labels(data_dir: &Path, session_id: &str, extra: &BTreeMap<String, String>) {
    if extra.is_empty() {
        return;
    }
    let sid = sid_of(session_id);
    let labels_path = data_dir
        .join(SESSIOND_DIR)
        .join("sessions")
        .join(format!("{sid}.labels.json"));
    let mut labels: BTreeMap<String, String> = std::fs::read(&labels_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    for (k, v) in extra {
        if !k.starts_with("falcon.") {
            labels.insert(k.clone(), v.clone());
        }
    }
    let _ = write_atomic_json(&labels_path, &labels);
}

/// Starts a local Unix domain socket proxy for an attached remote SSH session.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
pub fn start_remote_proxy(
    data_dir: &Path,
    db: &Db,
    project: &ProjectRow,
    row: &SessionRow,
    welcome: &Welcome,
    cwd: Option<&str>,
    deployed: Deployed,
    link: Rc<SshLink>,
) -> Option<HcProxyHandle> {
    let sid = sid_of(&row.id);
    let sessiond_dir = data_dir.join(SESSIOND_DIR);
    let sessions_dir = sessiond_dir.join("sessions");
    let run_dir = sessiond_dir.join("run");
    ensure_dir_0700(&sessiond_dir).ok()?;
    ensure_dir_0700(&sessions_dir).ok()?;
    ensure_dir_0700(&run_dir).ok()?;

    let uid = unsafe { libc::getuid() };
    let endpoint = socket_path(&data_dir.to_string_lossy(), &sid, uid);
    let endpoint_path = PathBuf::from(&endpoint);
    if let Some(parent) = endpoint_path.parent() {
        ensure_dir_0700(parent).ok()?;
    }

    let meta_path = sessions_dir.join(format!("{sid}.json"));
    let exit_path = sessions_dir.join(format!("{sid}.exit"));
    let _ = std::fs::remove_file(&endpoint_path);
    let _ = std::fs::remove_file(&exit_path);

    let std_listener = std::os::unix::net::UnixListener::bind(&endpoint_path).ok()?;
    std_listener.set_nonblocking(true).ok()?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&endpoint_path, std::fs::Permissions::from_mode(0o600));
    }
    let listener = tokio::net::UnixListener::from_std(std_listener).ok()?;

    sync_session_labels(data_dir, db, project, row);

    let meta = SessionMeta {
        sid: sid.clone(),
        session_id: row.id.clone(),
        pid: std::process::id(),
        shell_pid: 0,
        version: welcome.version.clone(),
        owner: db.instance_id(),
        endpoint: endpoint.clone(),
        created_at: row.created_at,
        cwd: cwd
            .or(project.working_dir.as_deref())
            .map(str::to_string),
        argv: Vec::new(),
    };
    write_atomic_json(&meta_path, &meta).ok()?;

    let cancel = CancellationToken::new();
    let loop_cancel = cancel.clone();
    tokio::task::spawn_local(async move {
        loop {
            let accepted = tokio::select! {
                _ = loop_cancel.cancelled() => break,
                res = listener.accept() => res,
            };
            let Ok((stream, _)) = accepted else { break };
            let link = link.clone();
            let deployed = deployed.clone();
            let sid = sid.clone();
            let conn_cancel = loop_cancel.clone();
            tokio::task::spawn_local(async move {
                proxy_one_connection(stream, link, deployed, sid, conn_cancel).await;
            });
        }
    });

    Some(HcProxyHandle {
        cancel,
        meta_path,
        endpoint: endpoint_path,
        exit_path,
    })
}

#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
pub fn start_remote_proxy(
    _data_dir: &Path,
    _db: &Db,
    _project: &ProjectRow,
    _row: &SessionRow,
    _welcome: &Welcome,
    _cwd: Option<&str>,
    _deployed: Deployed,
    _link: Rc<SshLink>,
) -> Option<HcProxyHandle> {
    None
}

#[cfg(unix)]
async fn proxy_one_connection(
    stream: tokio::net::UnixStream,
    link: Rc<SshLink>,
    deployed: Deployed,
    sid: String,
    cancel: CancellationToken,
) {
    use russh::ChannelMsg;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let argv = bridge_argv(&deployed.bin, &deployed.root, &sid, None);
    let cmd = bridge_command(deployed.kind, &argv, link.probed_shell().as_deref(), None);
    let Ok(mut channel) = link.exec_stream(&cmd, false).await else {
        return;
    };

    let (mut u_read, mut u_write) = stream.into_split();
    let mut read_buf = vec![0u8; 16 * 1024];
    let mut pre_buf: Vec<u8> = Vec::with_capacity(64);
    let mut preamble_done = false;
    let mut u_eof = false;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = channel.close().await;
                break;
            }
            res = u_read.read(&mut read_buf), if preamble_done && !u_eof => {
                match res {
                    Ok(0) | Err(_) => {
                        u_eof = true;
                        let _ = channel.eof().await;
                    }
                    Ok(n) => {
                        if channel.data(&read_buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
            msg = channel.wait() => {
                let Some(msg) = msg else { break };
                match msg {
                    ChannelMsg::Data { data } => {
                        if preamble_done {
                            if u_write.write_all(&data).await.is_err() {
                                let _ = channel.close().await;
                                break;
                            }
                        } else {
                            pre_buf.extend_from_slice(&data);
                            if let Some(pos) = pre_buf.windows(BRIDGE_PREAMBLE.len()).position(|w| w == BRIDGE_PREAMBLE) {
                                preamble_done = true;
                                let rest = &pre_buf[pos + BRIDGE_PREAMBLE.len()..];
                                if !rest.is_empty() && u_write.write_all(rest).await.is_err() {
                                    let _ = channel.close().await;
                                    break;
                                }
                                pre_buf.clear();
                            } else if pre_buf.len() > 64 * 1024 {
                                let _ = channel.close().await;
                                break;
                            }
                        }
                    }
                    ChannelMsg::Eof | ChannelMsg::Close => {
                        let _ = u_write.shutdown().await;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct ServerHcRequest {
    action: String,
    #[serde(default)]
    #[allow(dead_code)]
    argv: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ServerHcResponse {
    handled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Starts the `<data_dir>/sessiond/server-hc.sock` listener so `falcon-sessiond` can delegate
/// remote `terminal.run` requests to `falcon-server`.
#[cfg(unix)]
pub fn start_server_listener(mgr: Weak<SessionManager>, data_dir: PathBuf) -> Option<AbortHandle> {
    let sessiond_dir = data_dir.join(SESSIOND_DIR);
    ensure_dir_0700(&sessiond_dir).ok()?;
    let sock_path = sessiond_dir.join("server-hc.sock");
    let _ = std::fs::remove_file(&sock_path);

    let std_listener = std::os::unix::net::UnixListener::bind(&sock_path).ok()?;
    std_listener.set_nonblocking(true).ok()?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600));
    }
    let listener = tokio::net::UnixListener::from_std(std_listener).ok()?;

    let task = tokio::task::spawn_local(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let mgr = mgr.clone();
            let data_dir = data_dir.clone();
            tokio::task::spawn_local(async move {
                handle_server_hc_stream(mgr, data_dir, stream).await;
            });
        }
    });
    Some(task.abort_handle())
}

#[cfg(not(unix))]
pub fn start_server_listener(_mgr: Weak<SessionManager>, _data_dir: PathBuf) -> Option<AbortHandle> {
    None
}

#[cfg(unix)]
async fn handle_server_hc_stream(
    mgr: Weak<SessionManager>,
    data_dir: PathBuf,
    stream: tokio::net::UnixStream,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() || line.trim().is_empty() {
        return;
    }
    let Ok(req) = serde_json::from_str::<ServerHcRequest>(line.trim()) else {
        return;
    };
    let resp = if req.action == "spawn" {
        handle_spawn_request(mgr, &data_dir, req).await
    } else {
        ServerHcResponse {
            handled: false,
            ok: None,
            sid: None,
            error: None,
        }
    };
    if let Ok(mut bytes) = serde_json::to_vec(&resp) {
        bytes.push(b'\n');
        let _ = write_half.write_all(&bytes).await;
        let _ = write_half.shutdown().await;
    }
}

#[cfg(unix)]
async fn handle_spawn_request(
    mgr: Weak<SessionManager>,
    data_dir: &Path,
    req: ServerHcRequest,
) -> ServerHcResponse {
    let Some(me) = mgr.upgrade() else {
        return ServerHcResponse {
            handled: false,
            ok: None,
            sid: None,
            error: None,
        };
    };

    let Some(mut project) = resolve_target_project(&me.db(), &req.labels) else {
        return ServerHcResponse {
            handled: false,
            ok: None,
            sid: None,
            error: None,
        };
    };

    if project.project_type != "ssh" {
        return ServerHcResponse {
            handled: false,
            ok: None,
            sid: None,
            error: None,
        };
    }

    if let Some(cwd) = req.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        project.working_dir = Some(cwd.to_string());
    }

    let session_name = req
        .name
        .as_deref()
        .or_else(|| req.labels.get("hc.name").map(String::as_str))
        .or_else(|| req.labels.get("falcon.session").map(String::as_str))
        .map(str::trim)
        .unwrap_or_default()
        .to_string();

    match me
        .create_session(&project, session_name, OscColorHint::default(), None)
        .await
    {
        Ok(session) => {
            merge_extra_labels(data_dir, &session.id, &req.labels);
            let sid = sid_of(&session.id);
            ServerHcResponse {
                handled: true,
                ok: Some(true),
                sid: Some(sid),
                error: None,
            }
        }
        Err(err) => ServerHcResponse {
            handled: true,
            ok: Some(false),
            sid: None,
            error: Some(err.to_string()),
        },
    }
}

#[cfg(unix)]
fn resolve_target_project(db: &Db, labels: &BTreeMap<String, String>) -> Option<ProjectRow> {
    let proj_ref = labels
        .get("falcon.project")
        .map(String::as_str)
        .or_else(|| labels.get("herdr.space").and_then(|s| s.strip_prefix("falcon.")))
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(pref) = proj_ref {
        if let Some(p) = owner_row(db, pref) {
            return Some(p);
        }
        if let Some(p) = db
            .list_projects()
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(pref))
        {
            return Some(p);
        }
    }

    let host_ref = labels
        .get("falcon.host")
        .or_else(|| labels.get("falcon.host_name"))
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(href) = host_ref {
        if let Some(host) = db.list_hosts().into_iter().find(|h| {
            h.id == href
                || h.name.eq_ignore_ascii_case(href)
                || h.host.eq_ignore_ascii_case(href)
        }) {
            let standalone_id = falcon_proto::standalone_for_host(&host.id);
            if let Some(p) = owner_row(db, &standalone_id) {
                return Some(p);
            }
        }
        if let Some(p) = db.list_projects().into_iter().find(|p| {
            p.project_type == "ssh"
                && (p.ssh_host.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(href))
                    || p.name.eq_ignore_ascii_case(href))
        }) {
            return Some(p);
        }
    }

    if labels.get("falcon.remote").is_some_and(|v| v == "true") {
        if let Some(host) = db.list_hosts().into_iter().next() {
            let standalone_id = falcon_proto::standalone_for_host(&host.id);
            if let Some(p) = owner_row(db, &standalone_id) {
                return Some(p);
            }
        }
    }

    None
}

fn ensure_dir_0700(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn write_atomic_json<T: Serialize>(path: &Path, val: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(val)?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SshHostRow;

    #[test]
    fn sync_session_labels_writes_remote_and_host_labels_and_preserves_custom() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let host = SshHostRow {
            id: "host-1".into(),
            name: "Prod EU".into(),
            host: "10.0.0.5".into(),
            port: 22,
            username: "ubuntu".into(),
            auth_method: "agent".into(),
            key_path: None,
            secret_enc: None,
            created_at: 1000,
        };
        db.insert_host(&host);
        let project = ProjectRow {
            id: "proj-ssh-1".into(),
            name: "backend-api".into(),
            project_type: "ssh".into(),
            working_dir: Some("/srv/api".into()),
            host_id: Some("host-1".into()),
            ssh_host: Some("10.0.0.5".into()),
            ssh_port: Some(22),
            ssh_username: Some("ubuntu".into()),
            created_at: 1000,
            ..Default::default()
        };
        db.insert_project(&project);
        let row = SessionRow {
            id: "11111111-2222-3333-4444-555555555555".into(),
            project_id: project.id.clone(),
            name: "logs".into(),
            state: "active".into(),
            durable: 1,
            dead_reason: None,
            non_durable_reason: None,
            created_at: 2000,
            last_active_at: 2000,
            cols: None,
            rows: None,
            agent: None,
        };

        let mut extra = BTreeMap::new();
        extra.insert("hc.name".into(), "custom-pane".into());
        sync_session_labels(dir.path(), &db, &project, &row);
        merge_extra_labels(dir.path(), &row.id, &extra);

        let mut renamed_row = row.clone();
        renamed_row.name = "tail-logs".into();
        sync_session_labels(dir.path(), &db, &project, &renamed_row);

        let sid = sid_of(&row.id);
        let labels_path = dir
            .path()
            .join(SESSIOND_DIR)
            .join("sessions")
            .join(format!("{sid}.labels.json"));
        let labels: BTreeMap<String, String> =
            serde_json::from_slice(&std::fs::read(labels_path).unwrap()).unwrap();

        assert_eq!(labels.get("falcon.project").map(String::as_str), Some("proj-ssh-1"));
        assert_eq!(labels.get("falcon.project_name").map(String::as_str), Some("backend-api"));
        assert_eq!(labels.get("falcon.session").map(String::as_str), Some("tail-logs"));
        assert_eq!(labels.get("falcon.remote").map(String::as_str), Some("true"));
        assert_eq!(labels.get("falcon.host").map(String::as_str), Some("host-1"));
        assert_eq!(labels.get("falcon.host_name").map(String::as_str), Some("Prod EU"));
        assert_eq!(labels.get("hc.name").map(String::as_str), Some("custom-pane"));

        #[cfg(unix)]
        {
            let mut by_host_name = BTreeMap::new();
            by_host_name.insert("falcon.host_name".into(), "prod eu".into());
            let resolved = resolve_target_project(&db, &by_host_name).unwrap();
            assert_eq!(resolved.id, "standalone:host:host-1");
            assert_eq!(resolved.project_type, "ssh");

            let mut by_space = BTreeMap::new();
            by_space.insert("herdr.space".into(), "falcon.proj-ssh-1".into());
            let resolved2 = resolve_target_project(&db, &by_space).unwrap();
            assert_eq!(resolved2.id, "proj-ssh-1");
        }
    }
}

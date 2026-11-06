use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use falcon_proto::sessiond::{CURRENT_BINARY_FILE, SESSIOND_DIR, SessionMeta, sid_of, valid_sid};

fn command(root: &Path, id: &str) -> Result<Command> {
    let sid = sid_of(id);
    ensure!(valid_sid(&sid), "invalid local session ID");
    let metadata = root.join(SESSIOND_DIR).join("sessions").join(format!("{sid}.json"));
    let metadata: SessionMeta = serde_json::from_slice(
        &std::fs::read(&metadata).context("cannot read local session metadata; check sessiondRoot")?,
    ).context("invalid local session metadata")?;
    // A root may belong to a different Falcon instance. Never attach merely because
    // the shortened filename matches: validate the complete ID from the backend.
    ensure!(metadata.sid == sid && metadata.session_id == id, "sessiondRoot belongs to a different session");
    let published = root.join(SESSIOND_DIR).join(CURRENT_BINARY_FILE);
    let binary: std::path::PathBuf = serde_json::from_slice(
        &std::fs::read(published).context("current daemon deployment is missing; update and reconnect Falcon")?,
    ).context("invalid daemon deployment metadata")?;
    let binary = binary.canonicalize().context("deployed falcon-sessiond is missing")?;
    ensure!(binary.parent() == Some(root.join("bin").canonicalize()?.as_path()), "deployed daemon is outside sessiondRoot/bin");
    let mut command = Command::new(binary);
    command.arg("attach").arg(sid).arg("--root").arg(root).arg("--force");
    Ok(command)
}

pub(super) fn attach(root: &Path, id: &str) -> Result<()> {
    let mut command = command(root, id)?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Replace only the viewer, preserving its PTY and the daemon's existing
        // detach, replay and resize handling. The session process stays alive.
        Err(command.exec()).context("cannot start local falcon-sessiond attachment")
    }
    #[cfg(not(unix))]
    {
        let status = command.status().context("cannot start local falcon-sessiond attachment")?;
        ensure!(status.success(), "local falcon-sessiond attachment ended with {status}");
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_instance_metadata_cannot_attach_a_matching_short_id() {
        let root = std::env::temp_dir().join(format!("falcon-tern-root-{}", std::process::id()));
        let id = "12345678-1234-1234-1234-123456789abc";
        let sid = sid_of(id);
        let sessions = root.join(SESSIOND_DIR).join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let meta = SessionMeta {
            sid: sid.clone(),
            session_id: "12345678-1234-1234-ffff-123456789abc".into(),
            ..Default::default()
        };
        std::fs::write(sessions.join(format!("{sid}.json")), serde_json::to_vec(&meta).unwrap()).unwrap();
        let result = command(&root, id);
        std::fs::remove_dir_all(root).unwrap();
        assert!(result.unwrap_err().to_string().contains("different session"));
    }
}

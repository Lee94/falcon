//! Ask the OMP extension to restart its display lifecycle through private local IPC.
//! No terminal keys or process signals are injected: a delayed startup cannot leave
//! the agent suspended or accidentally submit its draft.

use anyhow::Result;
use crate::layout::Layout;

#[cfg(not(unix))]
pub fn run(_layout: &Layout, _sid: &str) -> Result<bool> {
    anyhow::bail!("OMP 原生显示切换目前需要 Unix 本机套接字")
}

#[cfg(unix)]
pub fn run(layout: &Layout, sid: &str) -> Result<bool> {
    unix::run(layout, sid)
}

#[cfg(unix)]
mod unix {
    use std::io::{self, Read, Write};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail, ensure};
    use falcon_proto::sessiond::{ClientMsg, Hello, HolderMsg, PROTO_MIN, PROTO_MAX, Role, read_frame};
    use serde::Deserialize;

    use crate::layout::Layout;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Endpoint {
        version: u32,
        pid: u32,
        session_id: String,
        sessiond_id: String,
        endpoint: PathBuf,
        token: String,
    }

    struct Connection {
        stream: UnixStream,
        deadline: Instant,
    }

    impl Connection {
        fn connect(path: &Path, deadline: Instant) -> Result<Self> {
            Ok(Self { stream: UnixStream::connect(path).context("连接本机会话显示控制端失败")?, deadline })
        }

        fn remaining(&self) -> io::Result<Duration> {
            self.deadline.checked_duration_since(Instant::now()).filter(|value| !value.is_zero())
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "OMP 显示控制请求超时"))
        }
    }

    impl Read for Connection {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.stream.set_read_timeout(Some(self.remaining()?))?;
            self.stream.read(bytes)
        }
    }

    impl Write for Connection {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.stream.set_write_timeout(Some(self.remaining()?))?;
            self.stream.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.stream.flush()
        }
    }

    fn private(metadata: &std::fs::Metadata) -> Result<()> {
        ensure!(metadata.uid() == unsafe { libc::geteuid() }, "显示控制文件不属于当前用户");
        ensure!(metadata.mode() & 0o077 == 0, "显示控制文件必须只允许当前用户访问");
        ensure!(!metadata.file_type().is_symlink(), "显示控制文件不能是符号链接");
        Ok(())
    }

    fn foreground(endpoint: &str, deadline: Instant) -> Result<(Option<String>, Option<u32>)> {
        let mut connection = Connection::connect(Path::new(endpoint), deadline)?;
        let hello = Hello { proto_min: PROTO_MIN, proto_max: PROTO_MAX, role: Role::Attach,
            client: format!("falcon-sessiond/{}", falcon_proto::sessiond::FALCON_VERSION) };
        connection.write_all(&ClientMsg::Hello(hello).encode())?;
        connection.write_all(&ClientMsg::Foreground { req: 1 }.encode())?;
        loop {
            let (kind, bytes) = read_frame(&mut connection)?.context("会话显示控制连接已关闭")?;
            match HolderMsg::decode(kind, bytes)? {
                HolderMsg::ForegroundIs { req: 1, name, pid, .. } => return Ok((name, pid)),
                HolderMsg::Error { message } => bail!("{message}"),
                HolderMsg::Exit(_) => bail!("会话已经结束"),
                _ => {}
            }
        }
    }

    fn terminal_group(shell_pid: u32, deadline: Instant) -> Result<u32> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = tx.send(std::process::Command::new("ps")
                .args(["-p", &shell_pid.to_string(), "-o", "tpgid="]).env("LC_ALL", "C").output());
        });
        let wait = deadline.checked_duration_since(Instant::now()).context("读取终端前台组超时")?;
        let output = rx.recv_timeout(wait).context("读取终端前台组超时")??;
        ensure!(output.status.success(), "无法读取原终端前台组：{}", String::from_utf8_lossy(&output.stderr));
        let pgid: u32 = std::str::from_utf8(&output.stdout)?.trim().parse().context("无法核实终端前台组")?;
        ensure!(pgid > 1, "终端前台组无效");
        Ok(pgid)
    }

    fn request(endpoint: &Endpoint, deadline: Instant) -> Result<()> {
        let mut connection = Connection::connect(&endpoint.endpoint, deadline)?;
        let mut command = serde_json::to_vec(&serde_json::json!({"v":1,"token":endpoint.token,"op":"reprobe"}))?;
        command.push(b'\n');
        connection.write_all(&command)?;
        let mut response = Vec::with_capacity(128);
        let mut buf = [0; 512];
        loop {
            let size = connection.read(&mut buf)?;
            ensure!(size != 0, "OMP 扩展关闭了显示控制连接");
            response.extend_from_slice(&buf[..size]);
            ensure!(response.len() <= 4096, "OMP 显示控制响应过大");
            if let Some(end) = response.iter().position(|byte| *byte == b'\n') {
                let value: serde_json::Value = serde_json::from_slice(&response[..end])?;
                ensure!(value["v"] == 1, "OMP 显示控制协议版本不兼容");
                if value["ok"] != true {
                    bail!("OMP 显示刷新失败：{}", value["error"].as_str().unwrap_or("未知扩展错误"));
                }
                return Ok(());
            }
        }
    }

    pub(super) fn run(layout: &Layout, sid: &str) -> Result<bool> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let meta = layout.read_meta(sid).context("无法读取会话元数据")?;
        let (name, pgid) = foreground(&meta.endpoint, deadline)?;
        let pgid = match pgid {
            Some(pid) => pid,
            None => terminal_group(meta.shell_pid, deadline)?,
        };
        let argv = crate::fg::process_argv(pgid);
        let is_omp = argv.as_deref().is_some_and(crate::omp_extension::recognizes)
            || name.as_deref().and_then(|name| Path::new(name).file_name()).is_some_and(|name| name == "omp");
        if is_omp {
            let argv = argv.as_deref().context("无法读取目标 OMP 参数；不能确认扩展配置目录")?;
            crate::omp_extension::install_for(Some(argv))?;
        } else {
            crate::omp_extension::install_for(None)?;
        }
        let registry = PathBuf::from(std::env::var_os("HOME").context("HOME 未设置")?)
            .join(".omp/run/falcon-tern");
        let entries = match std::fs::symlink_metadata(&registry) {
            Ok(metadata) => {
                private(&metadata)?;
                ensure!(metadata.is_dir(), "显示控制注册路径不是目录");
                Some(std::fs::read_dir(&registry)?)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut found = None;
        if let Some(entries) = entries {
            for entry in entries {
                let path = entry?.path();
                if path.extension().is_none_or(|extension| extension != "json") { continue; }
                let mut file = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&path) {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                let metadata = file.metadata()?;
                private(&metadata)?;
                ensure!(metadata.is_file() && metadata.len() <= 4096, "显示控制元数据无效");
                let mut body = Vec::with_capacity(metadata.len() as usize);
                (&mut file).take(4097).read_to_end(&mut body)?;
                ensure!(body.len() <= 4096, "显示控制元数据过大");
                let endpoint: Endpoint = serde_json::from_slice(&body).context("OMP 显示控制元数据无效")?;
                if endpoint.version != 1 || endpoint.session_id != meta.session_id || endpoint.sessiond_id != sid { continue; }
                if endpoint.pid == 0 || unsafe { libc::getpgid(endpoint.pid as libc::pid_t) } != pgid as libc::pid_t { continue; }
                ensure!(endpoint.endpoint.parent() == Some(registry.as_path()), "显示控制套接字不在私有注册目录");
                private(&std::fs::symlink_metadata(&endpoint.endpoint)?)?;
                ensure!(endpoint.token.len() == 64 && endpoint.token.bytes().all(|byte| byte.is_ascii_hexdigit()), "显示控制凭据无效");
                ensure!(found.is_none(), "当前前台组存在多个 OMP 显示控制端，无法安全选择");
                found = Some(endpoint);
            }
        }
        if let Some(endpoint) = found {
            request(&endpoint, deadline)?;
            return Ok(true);
        }
        if is_omp {
            bail!("当前 OMP 尚未加载 Falcon Tern 扩展；已安装扩展，请重启 OMP 并恢复会话后重新接入");
        }
        Ok(false)
    }
}

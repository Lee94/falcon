//! 开发用：拿 SshLink 连一台真 sshd，跑一遍传输层（TOFU 指纹、exec、带 stdin 的 exec、direct-tcpip）。
//! `cargo run -p falcon-server --example ssh_check -- <host> <port> <user>`，认证走 ssh-agent。
//! 数据目录用临时目录，不碰真实的库。**不跑 probe**：探测会顺手把远端 ~/.mojito 迁到 ~/.falcon，
//! 对着自己这台机器的账户跑会把正在用的数据目录挪走。
use std::rc::Rc;
use std::sync::Arc;

use falcon_server::crypto::SecretBox;
use falcon_server::db::{Db, ProjectRow};
use falcon_server::exec::Exec as _;
use falcon_server::sessions::ssh::SshLink;
use tokio::io::AsyncReadExt as _;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (host, port, user) = (args[0].clone(), args[1].parse::<i64>()?, args[2].clone());
    // 可选：key <私钥路径> [passphrase]，缺省走 agent
    let auth = args.get(3).cloned().unwrap_or_else(|| "agent".into());
    let dir = tempfile::tempdir()?;
    let db = Arc::new(Db::open(dir.path())?);
    let secrets = Arc::new(SecretBox::open(dir.path())?);
    let secret_enc = args.get(5).map(|p| secrets.encrypt(p));
    let project = ProjectRow {
        id: "p".into(),
        name: "p".into(),
        project_type: "ssh".into(),
        ssh_host: Some(host.clone()),
        ssh_port: Some(port),
        ssh_username: Some(user),
        ssh_auth_method: Some(auth),
        ssh_key_path: args.get(4).cloned(),
        ssh_secret_enc: secret_enc,
        created_at: 1,
        ..Default::default()
    };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let link: Rc<SshLink> = SshLink::new(project, db.clone(), secrets);
        link.on_up(|| println!("[event] up"));
        link.on_down(|| println!("[event] down"));
        let r = link.exec("echo hi; printf '中文' >&2; exit 3", None).await?;
        println!("exec: code={:?} stdout={:?} stderr={:?}", r.code, r.stdout, r.stderr);
        println!("fingerprint(db)={:?}", db.get_known_host(&host, port as u16));
        let r = link.exec_with_input("cat; echo; echo done", "stdin-数据".as_bytes()).await?;
        println!("exec_with_input: code={:?} stdout={:?}", r.code, r.stdout);
        let mut ch = link.forward_out("127.0.0.1", port as u16).await?.into_stream();
        let mut banner = [0u8; 64];
        let n = ch.read(&mut banner).await?;
        println!("direct-tcpip banner: {:?}", String::from_utf8_lossy(&banner[..n]).lines().next());
        // 反向转发：远端 127.0.0.1:41999 进来的连接交给本端，写一句话回去
        let mut incoming = link.add_remote_forward("127.0.0.1", 41999).await?;
        tokio::task::spawn_local(async move {
            if let Some(ch) = incoming.recv().await {
                let _ = ch.data_bytes(&b"pong-from-falcon\n"[..]).await;
                let _ = ch.eof().await;
            }
        });
        let r = link.exec("nc -w 3 127.0.0.1 41999 </dev/null", None).await?;
        println!("remote forward: code={:?} stdout={:?}", r.code, r.stdout);
        link.remove_remote_forward("127.0.0.1", 41999).await;
        anyhow::Ok(())
    })
}

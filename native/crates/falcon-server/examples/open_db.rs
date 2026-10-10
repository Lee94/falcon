//! 开发用：用 Rust 版打开一个现成的数据目录（Node 版建的库），列出项目与会话。
//! `cargo run -p falcon-server --example open_db -- <数据目录>`。会跑迁移，别对着正在用的库跑。
fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).expect("用法：open_db <数据目录>");
    let dir = std::path::Path::new(&dir);
    let db = falcon_server::db::Db::open(dir)?;
    let _secrets = falcon_server::crypto::SecretBox::open(dir)?;
    for p in db.list_projects() {
        println!("project {}", serde_json::to_string(&falcon_server::db::Db::to_project(&p))?);
    }
    for s in db.list_sessions() {
        println!("session {}", serde_json::to_string(&falcon_server::db::Db::to_session(&s))?);
    }
    println!("password_set={}", db.get_setting("password_hash").is_some());
    Ok(())
}

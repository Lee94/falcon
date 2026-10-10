//! 启动参数与数据目录。移植自 `packages/server/src/config.ts`。

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub data_dir: PathBuf,
}

pub fn is_loopback(host: &str) -> bool {
    host == "127.0.0.1" || host == "::1" || host == "localhost"
}

/// 默认数据目录。产品从 Mojito 改名为 Falcon 后根目录是 `~/.falcon`；
/// 若新目录还不存在而旧的 `~/.mojito` 在，继续用旧的，避免用户丢库。
/// 本机不能 `mv`：SQLite 开在这个目录里，进程还活着就抽走会把库弄丢。
/// 远端由 SshLink 的探测在之后尝试把 `~/.mojito` 改名为 `~/.falcon`，
/// 迁不了则沿用旧路径（见 zellij::host::migrate_remote_root_command）。
///
/// 打包后的启动引导（S7，替换 scripts/build-binary.mjs 的 SEA bootstrap）必须与这里同一套回退。
pub fn default_data_dir(home: &Path) -> PathBuf {
    let next = home.join(".falcon");
    let prev = home.join(".mojito");
    if !next.exists() && prev.exists() {
        return prev;
    }
    next
}

/// 读环境变量与命令行。`env` 注入以便测试；正式启动传 `|k| std::env::var(k).ok()`。
/// 顺带建好数据目录。
pub fn parse_args(argv: &[String], env: impl Fn(&str) -> Option<String>) -> anyhow::Result<ServerConfig> {
    let host = env("FALCON_HOST").or_else(|| env("MOJITO_HOST")).unwrap_or_else(|| "127.0.0.1".into());
    let port_raw = env("FALCON_PORT").or_else(|| env("MOJITO_PORT")).unwrap_or_else(|| "4923".into());
    let data_dir = env("FALCON_DATA_DIR")
        .or_else(|| env("MOJITO_DATA_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| default_data_dir(&home_dir()));
    let mut config = ServerConfig { host, port: parse_port(&port_raw)?, data_dir };
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--host" => config.host = it.next().context("--host 后面缺值")?.clone(),
            "--port" => config.port = parse_port(it.next().context("--port 后面缺值")?)?,
            "--data-dir" => config.data_dir = PathBuf::from(it.next().context("--data-dir 后面缺值")?),
            _ => {}
        }
    }
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("建数据目录 {} 失败", config.data_dir.display()))?;
    // 与 Node 版的出入：相对路径在这里就解析成绝对路径。数据目录会拼进 Zellij 二进制、
    // 启动脚本、askpass 包装的路径里；node-pty 的 execvp 对带斜杠的相对路径按 cwd 解析，
    // portable-pty 却只在 PATH 里找，相对的 `target/fd/bin/zellij` 会 spawn 失败
    config.data_dir = std::path::absolute(&config.data_dir)
        .with_context(|| format!("解析数据目录 {} 失败", config.data_dir.display()))?;
    Ok(config)
}

/// Node 版是 `Number(x)`，非数字得到 NaN、到 listen 才炸；这里在入口就拒绝
fn parse_port(raw: &str) -> anyhow::Result<u16> {
    match raw.trim().parse::<u16>() {
        Ok(p) => Ok(p),
        Err(_) => bail!("端口不合法：{raw}"),
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_falcon_when_nothing_exists() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(default_data_dir(home.path()), home.path().join(".falcon"));
    }

    #[test]
    fn falls_back_to_mojito_when_only_it_exists() {
        let home = tempfile::tempdir().unwrap();
        let prev = home.path().join(".mojito");
        std::fs::create_dir(&prev).unwrap();
        assert_eq!(default_data_dir(home.path()), prev);
    }

    #[test]
    fn prefers_falcon_once_it_exists() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".mojito")).unwrap();
        let next = home.path().join(".falcon");
        std::fs::create_dir(&next).unwrap();
        assert_eq!(default_data_dir(home.path()), next);
    }

    #[test]
    fn args_override_env() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("d");
        let env = |k: &str| match k {
            "MOJITO_PORT" => Some("5000".to_string()),
            "FALCON_DATA_DIR" => Some(data.to_string_lossy().into_owned()),
            _ => None,
        };
        let c = parse_args(&[], env).unwrap();
        assert_eq!((c.host.as_str(), c.port), ("127.0.0.1", 5000));
        assert!(data.is_dir());
        let argv: Vec<String> = ["--port", "4961", "--host", "0.0.0.0"].iter().map(|s| s.to_string()).collect();
        let c = parse_args(&argv, env).unwrap();
        assert_eq!((c.host.as_str(), c.port), ("0.0.0.0", 4961));
        assert!(parse_args(&["--port".into(), "x".into()], env).is_err());
    }
}

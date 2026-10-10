//! 把 sudo 包装和 askpass helper 写到宿主机 `<falcon 根>/bin`。移植自 `packages/server/src/askpass/install.ts`。
//! 本地直接写文件；远端 POSIX 走一条 printf + chmod（virtualdir 同款）。

use std::path::{Path, PathBuf};

use super::hub::AskpassHub;
use super::scripts::{ASKPASS_CONF_NAME, ASKPASS_NAME, SUDO_SHIM_NAME, render_askpass_conf, render_askpass_helper, render_sudo_shim};
use crate::git::path::join_path;
use crate::zellij::host::{HostKind, quote_posix};

pub fn askpass_bin_dir(kind: HostKind, root: &str) -> String {
    join_path(kind, &[root, "bin"])
}

pub fn local_askpass_bin_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("bin")
}

pub fn write_local_askpass(data_dir: &Path, hub: &AskpassHub) -> std::io::Result<PathBuf> {
    let dir = local_askpass_bin_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(SUDO_SHIM_NAME), render_sudo_shim())?;
    std::fs::write(dir.join(ASKPASS_NAME), render_askpass_helper())?;
    std::fs::write(dir.join(ASKPASS_CONF_NAME), render_askpass_conf(&hub.helper_url(), &hub.token))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir.join(SUDO_SHIM_NAME), std::fs::Permissions::from_mode(0o755))?;
        std::fs::set_permissions(dir.join(ASKPASS_NAME), std::fs::Permissions::from_mode(0o755))?;
        std::fs::set_permissions(dir.join(ASKPASS_CONF_NAME), std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(dir)
}

/// 远端 POSIX：建 bin、写三个文件、chmod。退出码 = 链上第一个失败者。
pub fn posix_write_askpass_command(dir: &str, hub: &AskpassHub, helper_url: &str) -> String {
    let files = [
        (SUDO_SHIM_NAME, render_sudo_shim(), "755"),
        (ASKPASS_NAME, render_askpass_helper(), "755"),
        (ASKPASS_CONF_NAME, render_askpass_conf(helper_url, &hub.token), "600"),
    ];
    let writes: Vec<String> = files
        .iter()
        .map(|(name, body, mode)| format!("printf %s {} > \"$d\"/{name} && chmod {mode} \"$d\"/{name}", quote_posix(body)))
        .collect();
    format!("d={}; mkdir -p \"$d\" && {}", quote_posix(dir), writes.join(" && "))
}

/// 把 `dir` 插到 PATH 最前面（有序 env 里原位替换，没有就追加在末尾——同 JS 对象展开覆盖同名键）
pub fn prepend_path(env: &mut Vec<(String, String)>, dir: &str) {
    match env.iter_mut().find(|(k, _)| k == "PATH") {
        Some((_, v)) if !v.is_empty() => *v = format!("{dir}:{v}"),
        Some((_, v)) => *v = dir.to_string(),
        None => env.push(("PATH".into(), dir.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepend_keeps_position() {
        let mut env = vec![("A".to_string(), "1".to_string()), ("PATH".to_string(), "/usr/bin".to_string())];
        prepend_path(&mut env, "/x/bin");
        assert_eq!(env[1], ("PATH".to_string(), "/x/bin:/usr/bin".to_string()));
        let mut empty = vec![];
        prepend_path(&mut empty, "/x/bin");
        assert_eq!(empty, [("PATH".to_string(), "/x/bin".to_string())]);
    }

    #[test]
    fn remote_command_writes_three_files() {
        let hub = AskpassHub::new();
        let cmd = posix_write_askpass_command("/home/u/.falcon/bin", &hub, "http://127.0.0.1:41234/api/askpass");
        assert!(cmd.starts_with("d='/home/u/.falcon/bin'; mkdir -p \"$d\" && printf %s "));
        assert!(cmd.contains("chmod 755 \"$d\"/sudo"));
        assert!(cmd.contains("chmod 600 \"$d\"/askpass.conf"));
        assert!(cmd.contains(&hub.token));
    }

    #[cfg(unix)]
    #[test]
    fn local_write_sets_modes() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let hub = AskpassHub::new();
        let bin = write_local_askpass(dir.path(), &hub).unwrap();
        let mode = |n: &str| std::fs::metadata(bin.join(n)).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode("sudo"), mode("falcon-askpass"), mode("askpass.conf")), (0o755, 0o755, 0o600));
    }
}

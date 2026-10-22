//! docker CLI 的 argv、远端的定位脚本、失败的分类、按钮动作 → 命令。纯函数，零 I/O。
//!
//! 一律走 `docker` 命令行，不直连 Docker 的 API socket：远端的 socket 在 SSH 那头，要转发；
//! 本机的 socket 位置因实现而异（Docker Desktop / OrbStack / colima / rootless 各不相同），
//! 而 CLI 自己认 `docker context`，用户在终端里看到的是哪个守护进程，面板上就是哪个。

use std::time::Duration;

/// 远端（POSIX）找 docker 的脚本，经 `sh -c` 跑，后面跟 docker 的参数（`$@`）。
///
/// 非交互的 SSH exec 拿不到登录 shell 的 PATH：macOS 上 sshd 给的只有 `/usr/bin:/bin:/usr/sbin:/sbin`，
/// Docker Desktop（`/usr/local/bin`）、Homebrew、OrbStack（`~/.orbstack/bin`）全不在里面。所以先问
/// `command -v`，再挨个试常见的安装位置；找到了把它所在的目录放到 PATH 前头（凭据助手
/// `docker-credential-*` 与它装在一起）再 exec。都没有就在 stderr 上留 [`NOT_FOUND`]、退 127。
pub const POSIX_RUN: &str = r#"for d in "$(command -v docker 2>/dev/null)" /usr/local/bin/docker /opt/homebrew/bin/docker "$HOME/.orbstack/bin/docker" "$HOME/.docker/bin/docker" "$HOME/.rd/bin/docker" /Applications/Docker.app/Contents/Resources/bin/docker /usr/bin/docker /snap/bin/docker; do
  if [ -n "$d" ] && [ -x "$d" ]; then PATH="${d%/*}:$PATH"; export PATH; exec "$d" "$@"; fi
done
echo "falcon: docker: command not found" >&2
exit 127"#;

/// [`POSIX_RUN`] 找不到 docker 时 stderr 里的记号
pub const NOT_FOUND: &str = "falcon: docker: command not found";

/// 本机找 docker 的位置：PATH（登录 shell 的那份）之后再试这些。launchd 拉起的后端连登录环境都可能
/// 没解析出来，Docker Desktop 与 OrbStack 又都不往 `/usr/bin` 里放东西
pub fn local_candidates(home: Option<&str>, windows: bool) -> Vec<String> {
    if windows {
        return vec![r"C:\Program Files\Docker\Docker\resources\bin\docker.exe".into()];
    }
    let mut v: Vec<String> = vec!["/usr/local/bin/docker".into(), "/opt/homebrew/bin/docker".into()];
    if let Some(h) = home.filter(|h| !h.is_empty()) {
        v.extend([".orbstack/bin/docker", ".docker/bin/docker", ".rd/bin/docker"].map(|p| format!("{h}/{p}")));
    }
    v.extend(["/Applications/Docker.app/Contents/Resources/bin/docker", "/usr/bin/docker", "/snap/bin/docker"].map(String::from));
    v
}

/// `docker container inspect` 的模板：只要面板用得上的那几段，形状照 inspect 自己的 JSON（解析与
/// `{{json .}}` 共用一个结构）。整份 inspect 一个容器 4 KB 上下，大半是 GraphDriver / HostConfig，
/// 这样一个是 1 KB 左右。
///
/// `State` 整段要：新版 CLI（29.x 实测）在 map 上跑模板，`.State.Health` 不存在时直接报
/// "map has no entry for key"；老版在结构体上跑，`index` 又不认结构体——两边都认的只有整段取。
pub const INSPECT_TEMPLATE: &str = concat!(
    r#"{"Id":{{json .Id}},"Name":{{json .Name}},"Created":{{json .Created}},"RestartCount":{{json .RestartCount}},"#,
    r#""State":{{json .State}},"#,
    r#""Config":{"Image":{{json .Config.Image}},"Labels":{{json .Config.Labels}},"Cmd":{{json .Config.Cmd}},"#,
    r#""Entrypoint":{{json .Config.Entrypoint}},"WorkingDir":{{json .Config.WorkingDir}}},"#,
    r#""HostConfig":{"RestartPolicy":{{json .HostConfig.RestartPolicy}}},"#,
    r#""NetworkSettings":{"Ports":{{json .NetworkSettings.Ports}},"Networks":{{json .NetworkSettings.Networks}}},"#,
    r#""Mounts":{{json .Mounts}}}"#
);

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// 全部容器的 id（含停了的）
pub fn ps_args() -> Vec<String> {
    args(&["ps", "-a", "-q", "--no-trunc"])
}

/// 查几个容器的详情，一行一个 JSON。`windows` = 远端是 Windows：命令要经 PowerShell 5.1 交给
/// docker.exe，它不转义参数里的双引号（7.3 才修），模板一进去就碎了——那边退回整份 `{{json .}}`
pub fn inspect_args(ids: &[String], windows: bool) -> Vec<String> {
    let mut v = args(&["container", "inspect", "--format", if windows { "{{json .}}" } else { INSPECT_TEMPLATE }]);
    v.extend(ids.iter().cloned());
    v
}

/// 正在跑的容器的占用。`--no-stream` 要等守护进程攒两次采样，实测 1–2 秒——所以它不在画面板的
/// 路上跑（见 mod.rs 的 `kick_stats`）
pub fn stats_args() -> Vec<String> {
    args(&["stats", "--no-stream", "--no-trunc", "--format", "{{json .}}"])
}

pub fn info_args() -> Vec<String> {
    args(&["info", "--format", "{{json .}}"])
}

pub fn images_args() -> Vec<String> {
    args(&["image", "ls", "--format", "{{json .}}"])
}

pub fn df_args() -> Vec<String> {
    args(&["system", "df", "--format", "{{json .}}"])
}

/// 最近的日志。`-t` 给每行带上时间戳：容器的 stdout 与 stderr 是分开的两条流（经 SSH 也是），
/// 按时间戳才能合回原来的先后（parse.rs 的 `merge_logs`），合完再去掉
pub fn logs_args(id: &str, tail: usize) -> Vec<String> {
    vec!["logs".into(), "--tail".into(), tail.to_string(), "-t".into(), id.into()]
}

/// 容器 id：`docker ps --no-trunc` 给的 64 位十六进制，或它的前缀（至少 12 位）。
/// 它会成为 docker 的一个参数，所以只认十六进制——`-f` 之类的东西进不了 argv
pub fn valid_container_id(s: &str) -> bool {
    (12..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// 镜像引用：`repo:tag`、`registry:5000/a/b:tag`、`sha256:<hex>` 或十六进制 id。不许以 `-` 开头
pub fn valid_image_ref(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 512
        && !s.starts_with('-')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/:@".contains(&b))
}

/// 一个按钮动作要跑的命令
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionSpec {
    pub args: Vec<String>,
    pub timeout: Duration,
    pub kind: ActionKind,
    /// 动作针对的容器 / 镜像（报结果时找名字用）
    pub target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Remove,
    PruneContainers,
    RemoveImage,
    PruneImages,
    PruneBuildCache,
}

impl ActionKind {
    /// 做成了的说法（"已停止"），toast 的标题前半句
    pub fn done(self) -> &'static str {
        match self {
            ActionKind::Start => "已启动",
            ActionKind::Stop => "已停止",
            ActionKind::Restart => "已重启",
            ActionKind::Pause => "已暂停",
            ActionKind::Unpause => "已恢复",
            ActionKind::Remove => "已删除容器",
            ActionKind::PruneContainers => "已清理停止的容器",
            ActionKind::RemoveImage => "已删除镜像",
            ActionKind::PruneImages => "已清理悬空镜像",
            ActionKind::PruneBuildCache => "已清理构建缓存",
        }
    }

    /// 没做成时的说法（"停止"），toast 的标题是"停止 web 失败"
    pub fn verb(self) -> &'static str {
        match self {
            ActionKind::Start => "启动",
            ActionKind::Stop => "停止",
            ActionKind::Restart => "重启",
            ActionKind::Pause => "暂停",
            ActionKind::Unpause => "恢复",
            ActionKind::Remove => "删除容器",
            ActionKind::PruneContainers => "清理停止的容器",
            ActionKind::RemoveImage => "删除镜像",
            ActionKind::PruneImages => "清理悬空镜像",
            ActionKind::PruneBuildCache => "清理构建缓存",
        }
    }
}

/// 按钮的动作 id + value → 命令。不认识的 id、不合法的 value 是 `Err`（400：只有面板上的按钮会发这些，
/// 对不上就是请求被改过）
pub fn action_spec(action: &str, value: Option<&str>) -> Result<ActionSpec, String> {
    let container = |kind: ActionKind, argv: &[&str], timeout: u64| -> Result<ActionSpec, String> {
        let id = value.filter(|v| valid_container_id(v)).ok_or_else(|| format!("{action} 要一个容器 id"))?;
        let mut a = args(argv);
        a.push(id.to_string());
        Ok(ActionSpec { args: a, timeout: Duration::from_secs(timeout), kind, target: Some(id.to_string()) })
    };
    let plain = |kind: ActionKind, argv: &[&str], timeout: u64| ActionSpec {
        args: args(argv),
        timeout: Duration::from_secs(timeout),
        kind,
        target: None,
    };
    match action {
        "container.start" => container(ActionKind::Start, &["container", "start"], 30),
        // stop 先给 10 秒 SIGTERM（docker 的缺省，写明了好对上时限），再 SIGKILL
        "container.stop" => container(ActionKind::Stop, &["container", "stop", "-t", "10"], 40),
        "container.restart" => container(ActionKind::Restart, &["container", "restart", "-t", "10"], 50),
        "container.pause" => container(ActionKind::Pause, &["container", "pause"], 15),
        "container.unpause" => container(ActionKind::Unpause, &["container", "unpause"], 15),
        // 不带 -f：在跑的容器删不掉，要先停（面板上也只给停了的容器"删除"）
        "container.remove" => container(ActionKind::Remove, &["container", "rm"], 30),
        "containers.prune" => Ok(plain(ActionKind::PruneContainers, &["container", "prune", "-f"], 60)),
        "image.remove" => {
            let r = value.filter(|v| valid_image_ref(v)).ok_or_else(|| "image.remove 要一个镜像引用".to_string())?;
            Ok(ActionSpec {
                args: vec!["image".into(), "rm".into(), r.to_string()],
                timeout: Duration::from_secs(60),
                kind: ActionKind::RemoveImage,
                target: Some(r.to_string()),
            })
        }
        // 只清悬空的（没有标签、没人引用）：`-a` 会把所有没在用的镜像都删掉，下次 up 又要重新拉
        "images.prune" => Ok(plain(ActionKind::PruneImages, &["image", "prune", "-f"], 120)),
        "builder.prune" => Ok(plain(ActionKind::PruneBuildCache, &["builder", "prune", "-f"], 120)),
        _ => Err(format!("Docker 没有动作 {action}")),
    }
}

/// prune 的输出里"腾出了多少"：`Total reclaimed space: 1.2GB`（container / image prune）、
/// `Total:\t9.5GB`（builder prune）
pub fn reclaimed(stdout: &str) -> Option<String> {
    stdout.lines().rev().find_map(|l| {
        let l = l.trim();
        l.strip_prefix("Total reclaimed space:").or_else(|| l.strip_prefix("Total:")).map(|v| v.trim().to_string())
    })
}

/// docker 命令跑不成的几种，面板上各有各的说法
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// 这台机器上没有 docker 命令
    NotInstalled,
    /// 有命令，守护进程没在跑（Docker Desktop / OrbStack 没开）
    DaemonDown(String),
    /// 连守护进程的 socket 没权限（Linux 上用户不在 docker 组里）
    Permission(String),
    Other(String),
}

/// 按退出码与 stderr 分类。只在非零退出时调
pub fn classify(code: Option<i32>, stderr: &str) -> Failure {
    let s = stderr.trim();
    let lower = s.to_ascii_lowercase();
    if s.contains(NOT_FOUND)
        || code == Some(127) && (lower.contains("not found") || lower.contains("not recognized") || lower.contains("commandnotfound"))
    {
        return Failure::NotInstalled;
    }
    if lower.contains("permission denied") && lower.contains("docker") {
        return Failure::Permission(s.to_string());
    }
    if lower.contains("cannot connect to the docker daemon")
        || lower.contains("is the docker daemon running")
        || lower.contains("error during connect")
        || lower.contains("docker_engine: the system cannot find the file")
    {
        return Failure::DaemonDown(s.to_string());
    }
    Failure::Other(if s.is_empty() { format!("docker 退出码 {}", code.map_or("?".into(), |c| c.to_string())) } else { s.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_refs() {
        assert!(valid_container_id("b7f1499cdf57"));
        assert!(valid_container_id(&"a".repeat(64)));
        for bad in ["", "b7f1499cdf5", "-f", "--all", "B7F1499CDF57", "b7f1499cdf57 x", &"a".repeat(65)] {
            assert!(!valid_container_id(bad), "{bad}");
        }
        assert!(valid_image_ref("nginx:1.27"));
        assert!(valid_image_ref("registry.cn-hangzhou.aliyuncs.com/a/b:pw1.64.1"));
        assert!(valid_image_ref("sha256:5aabe105d210"));
        assert!(valid_image_ref("localhost:5000/x@sha256:abc"));
        for bad in ["", "-f", "--force", "a b", "a;rm", "$(x)"] {
            assert!(!valid_image_ref(bad), "{bad}");
        }
    }

    #[test]
    fn actions_map_to_argv() {
        let id = "b7f1499cdf573c4bc0d4168a8a19daf2fed81141e0c8f5bee12cac1d2cd56f62";
        let s = action_spec("container.stop", Some(id)).unwrap();
        assert_eq!(s.args, vec!["container", "stop", "-t", "10", id]);
        assert_eq!(s.kind, ActionKind::Stop);
        assert!(s.timeout > Duration::from_secs(10), "时限要比 docker 自己等 SIGTERM 的 10 秒长");
        assert_eq!(action_spec("container.remove", Some(id)).unwrap().args, vec!["container", "rm", id]);
        assert_eq!(action_spec("image.remove", Some("nginx:1.27")).unwrap().args, vec!["image", "rm", "nginx:1.27"]);
        assert_eq!(action_spec("images.prune", None).unwrap().args, vec!["image", "prune", "-f"]);
        assert!(action_spec("container.stop", Some("--all")).is_err());
        assert!(action_spec("container.stop", None).is_err());
        assert!(action_spec("image.remove", Some("-f")).is_err());
        assert!(action_spec("system.prune", None).is_err());
    }

    #[test]
    fn argv_shapes() {
        let ids = vec!["abc".to_string(), "def".to_string()];
        let v = inspect_args(&ids, false);
        assert_eq!(&v[..3], &["container", "inspect", "--format"]);
        assert_eq!(&v[4..], &ids[..]);
        assert!(v[3].contains("\"State\":{{json .State}}"));
        assert!(!v[3].contains("Env"), "环境变量里常有密码，不往面板上拿");
        assert_eq!(inspect_args(&ids, true)[3], "{{json .}}");
        assert_eq!(logs_args("abc", 200), vec!["logs", "--tail", "200", "-t", "abc"]);
    }

    #[test]
    fn reclaimed_space() {
        assert_eq!(reclaimed("Deleted Containers:\nabc\n\nTotal reclaimed space: 1.2GB\n").as_deref(), Some("1.2GB"));
        assert_eq!(reclaimed("ID\tRECLAIMABLE\nx\t1MB\nTotal:\t9.536GB\n").as_deref(), Some("9.536GB"));
        assert_eq!(reclaimed(""), None);
    }

    #[test]
    fn failures() {
        assert_eq!(classify(Some(127), NOT_FOUND), Failure::NotInstalled);
        assert_eq!(classify(Some(127), "sh: docker: not found"), Failure::NotInstalled);
        assert_eq!(
            classify(Some(127), "& : The term 'docker' is not recognized as the name of a cmdlet"),
            Failure::NotInstalled
        );
        assert!(matches!(
            classify(Some(1), "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?"),
            Failure::DaemonDown(_)
        ));
        assert!(matches!(
            classify(
                Some(1),
                "permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock: Get \"http://%2Fvar%2Frun%2Fdocker.sock/v1.24/containers/json\": dial unix /var/run/docker.sock: connect: permission denied"
            ),
            Failure::Permission(_)
        ));
        assert_eq!(classify(Some(1), "  "), Failure::Other("docker 退出码 1".into()));
    }

    #[test]
    fn local_candidates_cover_the_usual_installs() {
        let v = local_candidates(Some("/Users/me"), false);
        assert!(v.contains(&"/Users/me/.orbstack/bin/docker".to_string()));
        assert!(v.contains(&"/usr/local/bin/docker".to_string()));
        assert!(!local_candidates(None, false).iter().any(|p| p.contains(".orbstack")));
        assert!(local_candidates(None, true)[0].ends_with("docker.exe"));
    }
}

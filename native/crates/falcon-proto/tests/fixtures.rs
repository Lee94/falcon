//! 协议契约测试（设计文档决定五）：真服务端产出的响应逐个反序列化成 falcon-proto 的类型。
//!
//! fixture 由 `cargo xtask fixtures` 生成（何时重跑见 `native/xtask/src/fixtures.rs` 顶部注释）。服务端
//! 改字段名、删字段、加了新的字面量，重生 fixture 之后这里就红。
//!
//! 两道检查：
//! 1. **能解出来**：每个 fixture 必须能反序列化成对应类型——解不出来就是 Rust 镜像与
//!    服务端对不上，客户端会在那个接口上整个失败。
//! 2. **解了不丢东西**：解出来再序列化回去，与原 JSON 比。服务端多给了我们不认识的字段、
//!    把 `null` 写成了我们以为会缺省的字段、给了一个落进 `Unknown` 兜底的值，都会在这里
//!    显形。已知且接受的差异写在 [`ACCEPTED`] 里，每条都说明为什么可以接受；清单之外
//!    多出一条、或清单里有一条不再出现，测试都会红——前者是新的不一致，后者说明清单
//!    该删了。

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use falcon_proto::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// 已知的"解了会丢东西"的差异：`fixture: JSON 路径: 说明`。
///
/// 这些都不影响客户端使用（字段本来就不读，或者 null 与缺省对客户端是一回事），但它们
/// 是 TS 类型与服务端实际输出之间的出入，列在这里让人看得见。
const ACCEPTED: &[&str] = &[
    // 飞书项目 409 的错误体比 shared 的 ApiError 多带 reason / code（meegle/routes.ts 的
    // fail()）。客户端经 falcon-client 的 ApiError::meegle_reason() 从原始 body 里读，
    // 不经这个类型。只有本机 meegle CLI 没装 / 没登录时才会生成这个 fixture。
    "error-meegle-unavailable: .code: 字段被丢掉",
    "error-meegle-unavailable: .reason: 字段被丢掉",
];

/// 本机环境决定生不生成的 fixture（见 xtask 的 fixtures.rs）。
const OPTIONAL: &[&str] = &["error-meegle-unavailable"];

type Check = fn(&str, &str, &mut Vec<String>) -> Result<(), String>;

macro_rules! fixtures {
    ($($name:literal => $ty:ty,)+) => {
        vec![$(($name, check::<$ty> as Check),)+]
    };
}

fn table() -> Vec<(&'static str, Check)> {
    fixtures! {
        // 认证 / 系统
        "auth-status" => AuthStatus,
        "auth-status-locked" => AuthStatus,
        "auth-status-authenticated" => AuthStatus,
        "auth-password" => OkResponse,
        "auth-login" => OkResponse,
        "auth-logout" => OkResponse,
        "askpass-pending" => Vec<AskpassPrompt>,
        "system" => SystemInfo,
        "system-after-session" => SystemInfo,
        "fs-validate-ok" => FsValidateResult,
        "fs-validate-missing" => FsValidateResult,
        "fs-list" => FsListing,
        "shells" => ShellsInfo,
        // 远端主机
        "host-create" => SshHost,
        "host-update" => SshHost,
        "hosts" => Vec<SshHost>,
        "host-test" => SshProbeResult,
        "host-test-draft" => SshProbeResult,
        // 项目 / 附属项目
        "project-create-local" => Project,
        "project-create-ssh" => Project,
        "project-create-multi" => Project,
        "project-update" => Project,
        "projects" => Vec<Project>,
        "repo-info" => RepoInfo,
        "repos-multi" => MultiRepoProbe,
        "worktree-create" => Project,
        "worktree-status" => WorktreeStatus,
        "project-archive" => Project,
        "project-restore" => Project,
        "project-delete" => DeleteProjectResult,
        "host-zellij-status" => HostZellijStatus,
        "host-zellij-status-denied" => HostZellijStatus,
        "host-authorization" => OkResponse,
        // Git
        "git-snapshot" => GitSnapshot,
        "git-snapshot-multi" => GitSnapshot,
        "git-snapshot-unavailable" => GitSnapshot,
        "git-changes" => GitChangeCounts,
        "git-changes-batch" => HashMap<String, GitChangeCounts>,
        "git-diff" => GitFileDiff,
        "git-diff-untracked" => GitFileDiff,
        "git-diff-unavailable" => GitFileDiff,
        "git-working" => GitWorkingChanges,
        "git-working-unavailable" => GitWorkingChanges,
        "git-log" => GitLogPage,
        "git-log-unavailable" => GitLogPage,
        "git-refs" => GitRefsInfo,
        "git-refs-unavailable" => GitRefsInfo,
        "git-commit-detail" => GitCommitDetail,
        "git-commit-diff" => GitFileDiff,
        "git-op-fetch" => GitSyncResult,
        "git-push" => GitSyncResult,
        // 文件
        "files" => WorkspaceListing,
        "files-subdir" => WorkspaceListing,
        "files-index" => WorkspaceIndex,
        "file-text" => WorkspaceFile,
        "file-image" => WorkspaceFile,
        "file-binary" => WorkspaceFile,
        "mkdir" => FileOpResult,
        "upload" => UploadResult,
        "rename" => FileOpResult,
        "remove" => FileRemoveResult,
        // 中转：转发 / 发布（挂机器，ADR 0016）
        "forward-create" => PortForward,
        "forward-update" => PortForward,
        "forward-delete" => OkResponse,
        "share-create" => PublicShare,
        "share-create-host" => PublicShare,
        "share-update" => PublicShare,
        "share-delete" => OkResponse,
        "relays" => RelayList,
        // 会话
        "session-create" => Session,
        "sessions" => Vec<SessionWithProject>,
        "sessions-after-terminate" => Vec<SessionWithProject>,
        "sessions-with-dead" => Vec<SessionWithProject>,
        "session-foreground" => SessionForeground,
        "session-foreground-busy" => SessionForeground,
        "session-rename" => OkResponse,
        "session-reattach" => Session,
        "session-reattach-dead" => Session,
        "session-terminate" => OkResponse,
        "session-clear" => OkResponse,
        "paste-image" => PasteImageResult,
        // WebSocket 控制消息（文本帧，按到达顺序）
        "ws-session-messages" => Vec<ServerMessage>,
        "ws-session-exit" => Vec<ServerMessage>,
        "ws-session-dead" => Vec<ServerMessage>,
        "ws-session-missing" => Vec<ServerMessage>,
        "ws-install-local" => Vec<InstallServerMessage>,
        "ws-install-missing" => Vec<InstallServerMessage>,
        // 飞书项目
        "meegle-status" => MeegleStatus,
        "meegle-pin-create" => MeeglePin,
        "meegle-pin-rename" => MeeglePin,
        "meegle-pins" => Vec<MeeglePin>,
        "meegle-pin-delete" => OkResponse,
        // 错误体
        "error-bad-request" => ApiError,
        "error-conflict" => ApiError,
        "error-login" => ApiError,
        "error-not-found" => ApiError,
        "error-unauthorized" => ApiError,
        "error-session-clear" => ApiError,
        "error-meegle-unavailable" => ApiError,
    }
}

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(dir().join(format!("{name}.json")))
        .unwrap_or_else(|e| panic!("读不了 fixture {name}：{e}（先在 native/ 下跑 cargo xtask fixtures）"))
}

fn check<T: DeserializeOwned + Serialize>(name: &str, raw: &str, lossy: &mut Vec<String>) -> Result<(), String> {
    let original: Value = serde_json::from_str(raw).map_err(|e| format!("{name}: 不是合法 JSON：{e}"))?;
    // 走 from_str：客户端真实的解码路径就是字符串
    let value: T = serde_json::from_str(raw)
        .map_err(|e| format!("{name}: 解不成 {}：{e}", std::any::type_name::<T>()))?;
    let back = serde_json::to_value(&value).map_err(|e| format!("{name}: 写不回去：{e}"))?;
    diff(name, "", &original, &back, lossy);
    Ok(())
}

/// 原 JSON 与"解了再写回"的 JSON 之间的差异。数字按数值比（JS 没有整数 / 浮点之分）。
fn diff(name: &str, at: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in x {
                let p = format!("{at}.{k}");
                match y.get(k) {
                    Some(w) => diff(name, &p, v, w, out),
                    None if v.is_null() => out.push(format!("{name}: {p}: 服务端给的 null 写回时被省略")),
                    None => out.push(format!("{name}: {p}: 字段被丢掉")),
                }
            }
            for k in y.keys().filter(|k| !x.contains_key(*k)) {
                out.push(format!("{name}: {at}.{k}: 写回时凭空多出来"));
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (v, w)) in x.iter().zip(y).enumerate() {
                diff(name, &format!("{at}[{i}]"), v, w, out);
            }
        }
        (Value::Number(x), Value::Number(y)) if x.as_f64() == y.as_f64() => {}
        _ if a == b => {}
        _ => out.push(format!("{name}: {at}: {a} 写回成了 {b}")),
    }
}

#[test]
fn every_fixture_file_is_covered() {
    let listed: BTreeSet<&str> = table().iter().map(|(n, _)| *n).collect();
    assert_eq!(listed.len(), table().len(), "表里有重复的名字");
    let on_disk: BTreeSet<String> = std::fs::read_dir(dir())
        .expect("fixtures 目录不存在：先在 native/ 下跑 cargo xtask fixtures")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".json").map(str::to_owned))
        .collect();
    let unlisted: Vec<_> = on_disk.iter().filter(|n| !listed.contains(n.as_str())).collect();
    assert!(unlisted.is_empty(), "这些 fixture 没有对应的类型，在 table() 里补上：{unlisted:?}");
    let missing: Vec<_> = listed
        .iter()
        .filter(|n| !on_disk.contains(**n) && !OPTIONAL.contains(n))
        .collect();
    assert!(missing.is_empty(), "这些 fixture 没生成出来（脚本改了？）：{missing:?}");
}

#[test]
fn every_fixture_deserializes_without_loss() {
    let mut failures = Vec::new();
    let mut lossy = Vec::new();
    for (name, check) in table() {
        if !dir().join(format!("{name}.json")).exists() && OPTIONAL.contains(&name) {
            continue;
        }
        if let Err(e) = check(name, &read(name), &mut lossy) {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "解不出来（Rust 镜像与服务端对不上）：\n{}", failures.join("\n"));

    let accepted: BTreeSet<&str> = ACCEPTED.iter().copied().collect();
    let found: BTreeSet<&str> = lossy.iter().map(String::as_str).collect();
    let new: Vec<_> = found.difference(&accepted).collect();
    assert!(new.is_empty(), "解了会丢东西（新的不一致，要么修 proto，要么说明理由加进 ACCEPTED）：\n{new:#?}");
    // 可选 fixture 没生成时，它名下的条目自然不会出现，不算过期
    let stale: Vec<_> = accepted
        .difference(&found)
        .filter(|a| !OPTIONAL.iter().any(|o| a.starts_with(&format!("{o}:")) && !dir().join(format!("{o}.json")).exists()))
        .collect();
    assert!(stale.is_empty(), "ACCEPTED 里这些差异已经不再出现，删掉：\n{stale:#?}");
}

/// 几处在语义上值得钉住的地方（不只是"能解出来"）。
#[test]
fn fixture_semantics() {
    let de = |name: &str| read(name);

    // 没有 upstream 的仓库：ahead / behind 的三态里取 null（不是缺省）
    let snap: GitSnapshot = serde_json::from_str(&de("git-snapshot")).unwrap();
    assert!(snap.available);
    assert_eq!(snap.ahead_count(), None);
    let unavailable: GitSnapshot = serde_json::from_str(&de("git-snapshot-unavailable")).unwrap();
    assert!(!unavailable.available);
    assert_eq!(unavailable.reason, Some(GitUnavailableReason::NotARepo));

    // 探测连不上是 200 + ok:false，不是错误
    let probe: SshProbeResult = serde_json::from_str(&de("host-test")).unwrap();
    assert!(matches!(probe, SshProbeResult::Failed { .. }));

    // 三种预览各自落在对的分支上
    let kinds: Vec<FilePreview> = ["file-text", "file-image", "file-binary"]
        .iter()
        .map(|n| serde_json::from_str::<WorkspaceFile>(&de(n)).unwrap().preview)
        .collect();
    assert!(matches!(kinds[0], FilePreview::Text { truncated: false, .. }));
    assert!(matches!(&kinds[1], FilePreview::Image { mime, .. } if mime == "image/png"));
    assert!(matches!(kinds[2], FilePreview::Binary { .. }));

    // 会话：新建即 active 且持久；shell 退出后是 dead + exited
    let s: Session = serde_json::from_str(&de("session-create")).unwrap();
    assert_eq!(s.state, SessionState::Active);
    assert!(s.durable);
    let exit: Vec<ServerMessage> = serde_json::from_str(&de("ws-session-exit")).unwrap();
    assert!(exit.contains(&ServerMessage::State {
        state: SessionState::Dead,
        dead_reason: Some(DeadReason::Exited)
    }));
    let msgs: Vec<ServerMessage> = serde_json::from_str(&de("ws-session-messages")).unwrap();
    assert_eq!(msgs[0], ServerMessage::State { state: SessionState::Active, dead_reason: None });
    assert!(!msgs.contains(&ServerMessage::Unknown), "服务端发了本版本不认识的控制消息：{msgs:?}");
    let install: Vec<InstallServerMessage> = serde_json::from_str(&de("ws-install-local")).unwrap();
    assert_eq!(install.last(), Some(&InstallServerMessage::Done));

    // 派生出的附属项目带 worktree，且是 falcon 建的目录
    let wt: Project = serde_json::from_str(&de("worktree-create")).unwrap();
    assert!(wt.worktree.as_ref().is_some_and(|w| w.created_by_falcon && w.branch == "feat/fixture"));
    let multi: Project = serde_json::from_str(&de("project-create-multi")).unwrap();
    assert_eq!(multi.multi.map(|m| m.repos.len()), Some(1));
}

/// 丢失检测本身得管用，不然上面那条"不丢东西"是空话。
#[test]
fn loss_detector_is_not_vacuous() {
    let mut lossy = Vec::new();
    check::<OkResponse>("x", r#"{"ok":true,"extra":1}"#, &mut lossy).unwrap();
    check::<Session>(
        "y",
        r#"{"id":"s","projectId":"p","name":"","title":null,"state":"hibernating","durable":true,
            "createdAt":1,"lastActiveAt":2}"#,
        &mut lossy,
    )
    .unwrap();
    // 报告顺序跟着 serde_json 的 Map 遍历顺序走：单独跑是按键名排序，整个 workspace 一起编时
    // 别的 crate 打开了 serde_json 的 preserve_order（feature 合并），就变成插入顺序。顺序没有
    // 语义，排好再比
    lossy.sort();
    assert_eq!(
        lossy,
        [
            "x: .extra: 字段被丢掉",
            "y: .state: \"hibernating\" 写回成了 \"unknown\"",
            "y: .title: 服务端给的 null 写回时被省略",
        ]
    );
    assert!(check::<Session>("z", r#"{"id":"s"}"#, &mut lossy).is_err());
}

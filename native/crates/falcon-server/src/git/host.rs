//! 取得一个可以在宿主机上跑 git 的执行环境。移植自 `packages/server/src/git/host.ts`。
//!
//! 本地与 SSH 归一到同一个 GitHost：一套命令构造、一套错误分类、一套护栏。
//! 这与 zellij 那边"本地复用与远端完全相同的安装编排"是同一个取舍。
//!
//! TS 版的 `gitHostFor` 吃 SessionManager 只为了调一次 `manager.getLink(row)`；这里改成
//! 吃一个取链路的闭包，git 层就不必依赖会话核心。

use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::sync::{LazyLock, Mutex};

use falcon_proto::WorktreeFailure;

use super::command::js_trim;
use super::error::{WorktreeError, worktree_failure_text};
use crate::db::ProjectRow;
use crate::exec::{Exec, LocalExec};
use crate::sessions::local::{home_dir, local_kind};
use crate::sessions::ssh::SshLink;
use crate::zellij::host::{HostKind, encode_powershell};

/// 一台能跑 git 的宿主机。clone 只是多一份 `Rc`
#[derive(Clone)]
pub struct GitHost {
    pub exec: Rc<dyn Exec>,
    pub kind: HostKind,
    /// git 的绝对路径（探测所得，见 [`resolve_git`]）
    pub git: String,
    /// 宿主机家目录。删除护栏用它挡住"删到 home 头上"
    pub home: String,
    /// 宿主机标识：缓存与并发锁的键
    pub key: String,
}

impl fmt::Debug for GitHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHost")
            .field("kind", &self.kind)
            .field("git", &self.git)
            .field("home", &self.home)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// 宿主机标识。注意 SSH 按 user@host:port，不按 projectId——同一台机器共用一份探测结果。
///
/// 照 TS 模板字符串的语义：缺的列拼成 `null`（`${null}`）。它只是进程内缓存与锁的键，
/// 这样写只是为了与 TS 版逐字一致，不影响任何判据。
pub fn host_key_of(row: &ProjectRow) -> String {
    if row.project_type == "local" {
        return "local".into();
    }
    format!(
        "ssh:{}@{}:{}",
        row.ssh_username.as_deref().unwrap_or("null"),
        row.ssh_host.as_deref().unwrap_or("null"),
        row.ssh_port.unwrap_or(22)
    )
}

/// git 绝对路径的缓存，按宿主机标识。
///
/// 探测一次就够，与 SshLink 的 probe() 的做法同构。缓存的是绝对路径而不是"能不能用"：
/// 之后所有调用都按绝对路径走，不再套登录 shell（见 [`resolve_git`]）。
static GIT_PATH_CACHE: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Mutex::default);

fn git_path_cache() -> std::sync::MutexGuard<'static, HashMap<String, String>> {
    GIT_PATH_CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// 用户改了主机配置时清掉缓存（目前只有测试与手动重试会用到）
pub fn reset_git_probe(key: Option<&str>) {
    match key {
        Some(k) => {
            git_path_cache().remove(k);
        }
        None => git_path_cache().clear(),
    }
}

fn link_failed(detail: String) -> WorktreeError {
    WorktreeError::new(WorktreeFailure::LinkFailed, worktree_failure_text(WorktreeFailure::LinkFailed), Some(detail))
}

/// 探测 git 的绝对路径。
///
/// POSIX 侧必须套一层**登录 shell**：SSH exec 拿到的是非登录、非交互 shell，
/// `/etc/profile`、`~/.profile`、`~/.bash_profile` 里的 PATH 一概不读——而
/// Homebrew(/opt/homebrew/bin)、asdf、nix、~/.local/bin 恰恰大多写在那里。
/// 不套这一层，用户会发现"终端里 git 用得好好的，功能却说没装 git"，
/// 与 zellij/host.rs 里 build_pty_command_line 注释描述的是同一类症状。
///
/// 但只在**探测**时套：拿到绝对路径之后一律直接调用，这样 stdout 干净（profile 里的
/// echo 不会混进 git 输出）、快（不必每条命令都重跑一遍 profile）。
///
/// 探测不带超时（TS 版同样没有）：本地 exec 不会卡，SSH 的连接有握手超时兜着。
pub async fn resolve_git(key: &str, exec: &dyn Exec, kind: HostKind) -> Result<String, WorktreeError> {
    if let Some(cached) = git_path_cache().get(key).cloned() {
        return Ok(cached);
    }

    let probes: Vec<String> = if kind == HostKind::Windows {
        vec![encode_powershell("$c = Get-Command git.exe -EA SilentlyContinue; if ($c) { $c.Source }")]
    } else {
        // 先按登录 shell 找；万一宿主机没有可用的登录 shell，退回裸探测
        vec!["sh -l -c 'command -v git' 2>/dev/null".into(), "command -v git".into()]
    };

    let mut detail: Option<String> = None;
    for probe in &probes {
        let res = exec.exec(probe, None).await.map_err(|e| link_failed(format!("{e:#}")))?;
        // 登录 shell 里的 profile 可能往 stdout 打东西，取最后一行非空输出
        let found = res.stdout.split('\n').map(js_trim).rfind(|l| !l.is_empty());
        if res.code == Some(0)
            && let Some(found) = found
        {
            git_path_cache().insert(key.to_string(), found.to_string());
            return Ok(found.to_string());
        }
        if detail.is_none() {
            detail = Some(js_trim(&res.stderr)).filter(|s| !s.is_empty()).map(str::to_string);
        }
    }

    Err(WorktreeError::new(WorktreeFailure::GitMissing, worktree_failure_text(WorktreeFailure::GitMissing), detail))
}

/// 为一个项目取得 GitHost。
///
/// SSH 侧复用会话核心已有的链路（`ssh_link` 闭包，TS 里是 `manager.getLink(row)`，
/// 按 projectId 缓存），不另开连接；宿主机类型与 home 来自 SshLink 的探测缓存，
/// 重复调用不产生额外往返。闭包只在 SSH 项目上调用。
pub async fn git_host_for(row: &ProjectRow, ssh_link: impl FnOnce() -> Rc<SshLink>) -> Result<GitHost, WorktreeError> {
    let key = host_key_of(row);
    if row.project_type == "local" {
        let kind = local_kind();
        let exec: Rc<dyn Exec> = Rc::new(LocalExec);
        let git = resolve_git(&key, exec.as_ref(), kind).await?;
        return Ok(GitHost { exec, kind, git, home: home_dir(), key });
    }
    let link = ssh_link();
    // TS 取的是 `err.message`（InstallError 的那句说明），detail 不往上带——照搬
    let facts = link.host_facts().await.map_err(|e| link_failed(e.message))?;
    let exec: Rc<dyn Exec> = link;
    let git = resolve_git(&key, exec.as_ref(), facts.kind).await?;
    Ok(GitHost { exec, kind: facts.kind, git, home: facts.home, key })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::ExecResult;
    use crate::git::repo::tests::Script;
    use std::cell::RefCell;

    fn res(code: Option<i32>, stdout: &str, stderr: &str) -> anyhow::Result<ExecResult> {
        Ok(ExecResult { code, stdout: stdout.into(), stderr: stderr.into() })
    }

    #[test]
    fn host_key_is_per_machine_not_per_project() {
        let local = ProjectRow { project_type: "local".into(), ..Default::default() };
        assert_eq!(host_key_of(&local), "local");
        let ssh = ProjectRow {
            id: "p1".into(),
            project_type: "ssh".into(),
            ssh_host: Some("box".into()),
            ssh_username: Some("fay".into()),
            ..Default::default()
        };
        assert_eq!(host_key_of(&ssh), "ssh:fay@box:22");
        let other = ProjectRow { id: "p2".into(), ssh_port: Some(2222), ..ssh.clone() };
        assert_eq!(host_key_of(&other), "ssh:fay@box:2222");
    }

    #[tokio::test]
    async fn posix_probe_takes_last_line_of_login_shell_and_caches_it() {
        let ex = Script {
            calls: RefCell::default(),
            respond: Box::new(|_| res(Some(0), "welcome from .profile\n/opt/homebrew/bin/git\n\n", "")),
        };
        let key = "test:posix-login";
        reset_git_probe(Some(key));
        assert_eq!(resolve_git(key, &ex, HostKind::Posix).await.unwrap(), "/opt/homebrew/bin/git");
        assert_eq!(resolve_git(key, &ex, HostKind::Posix).await.unwrap(), "/opt/homebrew/bin/git");
        // 第二次走缓存
        assert_eq!(*ex.calls.borrow(), ["sh -l -c 'command -v git' 2>/dev/null"]);
        reset_git_probe(Some(key));
    }

    #[tokio::test]
    async fn posix_probe_falls_back_to_bare_lookup_then_reports_git_missing() {
        let ex = Script {
            calls: RefCell::default(),
            respond: Box::new(|cmd| {
                if cmd.starts_with("sh -l") {
                    res(Some(127), "", "sh: bad profile\n")
                } else {
                    res(Some(0), "/usr/bin/git\n", "")
                }
            }),
        };
        let key = "test:posix-fallback";
        reset_git_probe(Some(key));
        assert_eq!(resolve_git(key, &ex, HostKind::Posix).await.unwrap(), "/usr/bin/git");
        reset_git_probe(Some(key));

        let none = Script { calls: RefCell::default(), respond: Box::new(|_| res(Some(1), "", "")) };
        let key = "test:posix-missing";
        reset_git_probe(Some(key));
        let err = resolve_git(key, &none, HostKind::Posix).await.unwrap_err();
        assert_eq!(err.reason, WorktreeFailure::GitMissing);
        assert_eq!(err.detail, None);
        assert_eq!(none.calls.borrow().len(), 2);
    }

    #[tokio::test]
    async fn windows_probe_is_one_encoded_command_and_link_errors_are_link_failed() {
        let ex =
            Script { calls: RefCell::default(), respond: Box::new(|_| Err(anyhow::anyhow!("channel open failed"))) };
        let key = "test:windows-link";
        reset_git_probe(Some(key));
        let err = resolve_git(key, &ex, HostKind::Windows).await.unwrap_err();
        assert_eq!(err.reason, WorktreeFailure::LinkFailed);
        assert_eq!(err.detail.as_deref(), Some("channel open failed"));
        let calls = ex.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("-EncodedCommand"));
    }
}

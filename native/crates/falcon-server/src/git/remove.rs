//! 删除附属项目的 worktree 目录。移植自 `packages/server/src/git/remove.ts`，
//! 规矩与护栏的来龙去脉在 `docs/adr/0002-worktree-derived-projects.md`（多仓库版在 0003）。
//!
//! 这是删除**附属项目 worktree 目录**的唯一入口。工作目录内部的文件删除在
//! files.rs（用户从文件面板点的、resolve_inside 挡在工作目录里、确认框过了才动手），
//! 两件事不要合成一条路径：这边的护栏是"别把仓库根 / 家目录 rm -rf 掉"，那边的
//! 护栏是"别跑出项目工作目录"。
//!
//! 所以这个模块刻意做成单一入口：不导出任何裸的"删目录"函数，静态断言是一个纯
//! 函数（可以被单独盯着读、单独用手改的脏数据去打），动态取证与删除各自只有一条
//! 路径。
//!
//! 总的取舍：**DB 行无条件删，文件系统清理 best-effort**。留一条删不掉的项目行，
//! 用户唯一的出路是去改 SQLite；残留目录他自己删得掉——前提是我们把路径原样告诉他。
//! 所以这里的返回值是一串 warning，而不是错误。

use std::path::Path;
use std::time::Duration;

use falcon_proto::{MULTI_REPO_MAX, MultiRepoMember};

use super::command::{self as gc, js_trim};
use super::error::git_error_line;
use super::host::GitHost;
use super::lock::{repo_lock_key, with_repo_lock};
use super::path::{canon_key, is_absolute, is_ancestor, is_unc, join_path, normalize_sep, path_depth, same_path};
use super::repo::{RunOpts, TIMEOUT_REMOVE, code_text, exec_raw, list_worktrees, path_exists, probe_git};
use crate::db::{Db, ProjectRow};
use crate::exec::ExecResult;
use crate::virtualdir::{CLAUDE_MD_BODY, FALCON_GENERATED_MARK, central_manifest_sweep_command};
use crate::zellij::host::{HostKind, encode_powershell, quote_posix, quote_powershell};

fn remove_opts() -> RunOpts {
    RunOpts::timeout(TIMEOUT_REMOVE)
}

/// TS 里 `.catch((err: Error) => ({ code: null, stdout: "", stderr: err.message }))`。
/// 注意取的是 WorktreeError 的 message（那句"命令没能在宿主机上跑起来"），不是 detail
fn caught(res: Result<ExecResult, super::error::WorktreeError>) -> ExecResult {
    res.unwrap_or_else(|err| ExecResult { code: None, stdout: String::new(), stderr: err.message })
}

/// 删除前的静态否决。返回 `Some` 即"不许删"，字符串直接作为 warning 给用户。
///
/// 纯函数、只吃行数据：不需要一台宿主机就能验证它。这是本功能里唯一一段
/// "写错了会删掉用户东西"的代码，它的可审查性比复用度重要。
///
/// other_dirs 是**其他**项目的工作目录，用来挡住"删掉别人正在用的目录"。
pub fn veto_removal<S: AsRef<str>>(kind: HostKind, row: &ProjectRow, home: &str, other_dirs: &[S]) -> Option<String> {
    let dir = row.working_dir.as_deref();
    let repo = row.worktree_repo_dir.as_deref();
    // TS 的 `dir ?? "(空)"`：只有缺省才换，空串照原样
    let shown = dir.unwrap_or("(空)");

    // ① 必须是附属项目
    if row.source_project_id.is_none() {
        return Some(format!("「{}」不是附属项目，不清理任何目录", row.name));
    }

    // ② 必须是 falcon 建的目录。用户手工建的 worktree、以及将来"接管已有 worktree"的
    //    场景一律不删——对应 ADR 0001 的"绝不接管用户自有的 Zellij 会话"
    if row.worktree_created_by_mojito != Some(1) {
        return Some(format!("{shown} 不是 Falcon 创建的，未删除"));
    }

    // ③ 路径必须非空且绝对。相对路径意味着这行是脏数据，宁可不删
    let Some(dir) = dir.filter(|d| !d.is_empty() && is_absolute(kind, d)) else {
        return Some(format!("工作目录不是绝对路径，未删除：{shown}"));
    };
    let Some(repo) = repo.filter(|r| !r.is_empty() && is_absolute(kind, r)) else {
        return Some(format!("仓库根记录缺失，未删除：{dir}"));
    };

    // ④ 深度门槛。真实的 worktree 都是"某个仓库目录的同级"，而仓库不会直接躺在
    //    盘符根上。门槛设在 2 能挡掉绝大多数脏数据造成的灾难（C:\ 、/ 、D:\x），
    //    代价只是拒绝一种没人真会用的布局
    if path_depth(kind, dir) < 2 {
        return Some(format!("路径过浅，拒绝删除：{dir}"));
    }

    // ⑤ 绝不删仓库根本身，也绝不删包住仓库的任何一层
    if same_path(kind, dir, repo) {
        return Some(format!("目标就是仓库根，拒绝删除：{dir}"));
    }
    if is_ancestor(kind, dir, repo) {
        return Some(format!("目标包含仓库根，拒绝删除：{dir}"));
    }

    // ⑥ 绝不删家目录本身或它的上层。注意 worktree **可以**在 home 里面
    //    （~/code/foo-feat-x 是最常见的布局），所以这里只挡"是 home"和"是 home 的祖先"
    if !home.is_empty() && (same_path(kind, dir, home) || is_ancestor(kind, dir, home)) {
        return Some(format!("目标是家目录或其上层，拒绝删除：{dir}"));
    }

    // ⑦ 别踩到其他项目的工作目录上
    if let Some(clash) =
        other_dirs.iter().map(AsRef::as_ref).find(|o| same_path(kind, dir, o) || is_ancestor(kind, dir, o))
    {
        return Some(format!("目标包含另一个项目的工作目录（{clash}），拒绝删除：{dir}"));
    }

    None
}

/// 动态取证用的是哪条证据
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProofKind {
    Registered,
    GitFile,
}

/// 动态取证的结论。locked 只在 registered 时可知
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Proof {
    kind: ProofKind,
    locked: bool,
}

/// 证明 dir 此刻仍是 repo 的一棵 worktree。两条独立证据，任一成立即可。
///
/// A) `git -C <repo> worktree list --porcelain` 里仍有这条路径。最强的一条：同时
///    证明了目录是该仓库的 worktree、仓库还在、用户没在别处 worktree move 走。
///
/// B) 目录里有一个 .git **文件**（不是目录）且以 "gitdir: " 开头。这是 linked
///    worktree 独有的特征——普通仓库那里是目录，普通目录根本没有。留这条降级路径是
///    因为最常见的破损场景是"用户把整个源仓库删了"：此时 A 永远失败，但目录本身仍然
///    应该被清理掉。没有这条豁免，用户会被卡死——目录删不掉，也没法自己收拾。
async fn prove_worktree(host: &GitHost, repo: &str, dir: &str) -> Option<Proof> {
    // 仓库没了 / 链路抖了都落到证据 B
    if let Ok(entries) = list_worktrees(host, repo, &remove_opts()).await
        && let Some(hit) = entries.iter().find(|e| canon_key(host.kind, &e.path) == canon_key(host.kind, dir))
    {
        return Some(Proof { kind: ProofKind::Registered, locked: hit.locked });
    }
    // 读不到就是没证据
    if let Ok(res) = exec_raw(host, &gc::git_file_head_command(host.kind, dir), &remove_opts()).await
        && res.stdout.trim_start_matches(gc::is_js_whitespace).starts_with("gitdir:")
    {
        return Some(Proof { kind: ProofKind::GitFile, locked: false });
    }
    None
}

/// 被 lock 时给用户的说明。永远不自动解锁、永远不给第二个 --force
fn locked_warning(dir: &str) -> String {
    format!("该 worktree 被 git worktree lock 锁定，目录未删除：{dir}（先 git worktree unlock 再重试）")
}

/// POSIX 远端：把校验与删除压成**一条**命令。
///
/// 真正的风险不是注入（quote_posix 的单引号包裹让 $ / 反引号 / ; 全部惰性），
/// 而是"验证"与"删除"之间目标被换成符号链接——两次 SSH 往返之间的 TOCTOU。
/// 压成一条就没有那个窗口了。
///
/// `--` 挡住以 - 开头的路径被 rm 当成选项；不带尾斜杠才不跟随最后一段的符号链接；
/// `cd -P && pwd -P` 确认它就是物理路径（我们记的是 git 报的路径，git 用 getcwd，
/// 拿到的本来就是物理路径）。
fn posix_remove_command(dir: &str) -> String {
    let p = quote_posix(dir);
    [
        format!("p={p}"),
        r#"if [ ! -e "$p" ]; then printf gone; exit 0; fi"#.into(),
        r#"if [ -L "$p" ]; then printf symlink; exit 0; fi"#.into(),
        r#"if [ ! -d "$p" ]; then printf notdir; exit 0; fi"#.into(),
        r#"if [ "$(cd -P -- "$p" && pwd -P)" != "$p" ]; then printf notphysical; exit 0; fi"#.into(),
        r#"rm -rf -- "$p" && printf ok || printf failed"#.into(),
    ]
    .join("; ")
}

/// Windows 远端：同样一条命令里完成校验与删除。
///
/// 用 .NET 的 Directory.Delete 而不是 Remove-Item：
/// - Remove-Item **-Path** 会先把路径当通配符去匹配。git refname 禁止 [ ] * ?，
///   所以分支名安全，但**仓库目录名不受限**——D:\code\my[old]repo 派生出的路径含 [，
///   -Path 匹配不到任何东西，实测退出码 0 而目录纹丝不动：静默报成功，我们据此删掉
///   DB 行，目录就永远成了孤儿。（-LiteralPath 能解决这一条。）
/// - 但 PowerShell 5.1 的 Remove-Item -Recurse 对目录 junction **会递归进目标**，
///   junction 指向 worktree 之外就会删到别处。encode_powershell 调的正是 5.1。
///
/// Directory.Delete 两个问题都没有：吃字面量字符串、且递归时不穿越 reparse point。
/// 代价是遇到只读文件会抛——那种情况报 warning 让用户自己收拾，比冒险强。
///
/// 顶层是 reparse point 一票否决。注意**不能**一刀切"任何后代含 reparse point 就拒绝"：
/// Windows 上 pnpm 的 node_modules 遍地是 junction，但都指向内部的 node_modules/.pnpm。
fn windows_remove_command(dir: &str) -> String {
    let p = quote_powershell(dir);
    encode_powershell(
        &[
            format!("$p = {p}"),
            "if (-not (Test-Path -LiteralPath $p)) { 'gone'; exit 0 }".into(),
            "$i = Get-Item -LiteralPath $p -Force".into(),
            "if (-not ($i -is [System.IO.DirectoryInfo])) { 'notdir'; exit 0 }".into(),
            "if ($i.Attributes -band [System.IO.FileAttributes]::ReparsePoint) { 'symlink'; exit 0 }".into(),
            "try { [System.IO.Directory]::Delete($p, $true); 'ok' } catch { 'failed: ' + $_.Exception.Message }".into(),
        ]
        .join("; "),
    )
}

/// Node `fs.rm` 的 maxRetries / retryDelay：杀毒软件 / 索引服务偶尔会短暂持有句柄
const LOCAL_RM_RETRIES: u32 = 3;
const LOCAL_RM_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Node 只对这几类错误重试（EBUSY / EMFILE / ENFILE / ENOTEMPTY / EPERM），其余立刻失败
fn retryable(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    if matches!(e.kind(), ResourceBusy | DirectoryNotEmpty | PermissionDenied) {
        return true;
    }
    #[cfg(unix)]
    if matches!(e.raw_os_error(), Some(libc::EMFILE | libc::ENFILE)) {
        return true;
    }
    false
}

/// 本地删除**不走 shell**。
///
/// std 的 remove_dir_all 不跟随符号链接与 junction（顶层与递归中都只删链接本身）、
/// 没有通配展开、没有退出码传播问题，长路径也比 rd 好。只有 SSH 才需要命令行那一套——
/// 这一条把 Windows 本地的整类风险直接归零。
///
/// 必须走 tokio::fs（阻塞线程池）：worktree 里常有 node_modules（十万级文件），在会话
/// 核心的 LocalSet 上同步递归删除会把它冻住几秒到几十秒，期间所有终端 WS 与 HTTP 全部
/// 停摆——而且每小时的存档清扫会在用户无感知时触发这条路径。
///
/// 重试是给 Windows 的：杀毒软件 / 索引服务偶尔会短暂持有句柄（同 Node 的线性退避）。
async fn remove_local_dir(dir: &str) -> Option<String> {
    match tokio::fs::symlink_metadata(dir).await {
        // Windows 上 std 把目录 junction 也算作 symlink（name surrogate 类 reparse point），
        // 与 Node lstat 的 isSymbolicLink 一致
        Ok(st) if st.file_type().is_symlink() => return Some(format!("目标是符号链接，未删除：{dir}")),
        Ok(st) if !st.is_dir() => return Some(format!("目标不是文件夹，未删除：{dir}")),
        Ok(_) => {}
        Err(_) => return None, // 已经不在了
    }
    let mut attempt = 0;
    loop {
        match tokio::fs::remove_dir_all(dir).await {
            Ok(()) => break,
            // force: true —— 删的过程中被别人先删掉也算成功
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) if attempt < LOCAL_RM_RETRIES && retryable(&e) => {
                attempt += 1;
                tokio::time::sleep(LOCAL_RM_RETRY_DELAY * attempt).await;
            }
            Err(e) => return Some(format!("删除目录失败：{dir}（{e}）")),
        }
    }
    if tokio::fs::try_exists(dir).await.unwrap_or(false) {
        return Some(format!("目录仍然存在，请手动清理：{dir}"));
    }
    None
}

/// 远端删除命令打出来的结论 → 给用户的说明（ok / gone 不在表里：那是成功）
fn remote_result_text(out: &str, dir: &str) -> Option<String> {
    match out {
        "symlink" => Some(format!("目标是符号链接 / junction，出于安全没有删除：{dir}")),
        "notdir" => Some(format!("目标不是文件夹，未删除：{dir}")),
        "notphysical" => Some(format!("目标路径经过符号链接，出于安全没有删除：{dir}")),
        _ => None,
    }
}

/// 远端删除：校验与删除在同一条命令里完成，消掉 TOCTOU 窗口
async fn remove_remote_dir(host: &GitHost, dir: &str) -> Option<String> {
    let cmd = if host.kind == HostKind::Windows { windows_remove_command(dir) } else { posix_remove_command(dir) };
    let res = caught(exec_raw(host, &cmd, &remove_opts()).await);
    let out = js_trim(&res.stdout);
    if out == "ok" || out == "gone" {
        return None;
    }
    if let Some(known) = remote_result_text(out, dir) {
        return Some(known);
    }
    let line = git_error_line(if out.is_empty() { &res.stderr } else { out });
    let why = if line.is_empty() { format!("退出码 {}", code_text(res.code)) } else { line.to_string() };
    Some(format!("删除目录失败：{dir}（{why}）"))
}

/// 清理一个附属项目的 worktree。返回给用户看的 warning 列表（空 = 全部干净）。
///
/// 顺序恒为 **取证 → worktree remove → 兜底删目录 → prune**：
///
/// - 取证必须在 remove 之前。remove 成功后目录就没了，证据 A 自然不再成立，
///   所以 prove_worktree 在整个函数里只求一次。
/// - 只删目录而不 worktree remove，会在 $GIT_DIR/worktrees/<id>/ 留一条 stale 管理
///   记录；下次用同名路径再派生，git 会报 "is a missing but locked working tree"
///   之类，而用户完全看不懂这跟上次删除有什么关系。
/// - prune **只在走了兜底删除时才跑**：它是仓库级的，会清掉 falcon 没创建的 stale
///   条目（比如用户放在未挂载盘上的 worktree）。remove 成功时会自己清理管理项。
pub async fn cleanup_worktree<S: AsRef<str>>(row: &ProjectRow, host: &GitHost, other_dirs: &[S]) -> Vec<String> {
    // 多仓库派生行走成员清单逐棵清理；调用点（routes 的 DELETE 与 archive 清扫）不感知
    if row.multi_repos.is_some() {
        return cleanup_multi_worktree(row, host, other_dirs).await;
    }

    if let Some(veto) = veto_removal(host.kind, row, &host.home, other_dirs) {
        return vec![veto];
    }

    // veto_removal 已经保证这两个非空
    let (Some(working_dir), Some(repo)) = (row.working_dir.as_deref(), row.worktree_repo_dir.as_deref()) else {
        unreachable!("veto_removal 的 ③ 已挡住空值")
    };
    let dir = normalize_sep(host.kind, working_dir);

    let Some(proof) = prove_worktree(host, repo, &dir).await else {
        return vec![format!("无法确认 {dir} 仍是 {repo} 的 worktree，出于安全没有删除，请手动清理")];
    };

    // lock 是用户明说的"别碰"，兜底删除绝不能越过它——绝不自动解锁、
    // 绝不给第二个 --force，也绝不"remove 失败就直接 rm"。这条必须在动手之前拦住
    if proof.locked {
        return vec![locked_warning(&dir)];
    }

    clear_one_worktree(host, repo, &dir, proof).await.0
}

/// 取证之后对单棵 worktree 的实际清理：worktree remove → 兜底删目录 → 兜底时 prune。
/// 单仓库与多仓库共用；调用方必须已经做完 veto、取证与 locked 拦截。
/// 第二项 gone = 目录确认已不在磁盘上（多仓库版据此决定要不要删集中目录）。
async fn clear_one_worktree(host: &GitHost, repo: &str, dir: &str, proof: Proof) -> (Vec<String>, bool) {
    let mut warn = Vec::new();

    if proof.kind == ProofKind::Registered {
        let res =
            caught(probe_git(host, &gc::worktree_remove_args(&host.git, repo, dir), gc::GIT_ENV, &remove_opts()).await);
        if res.code != Some(0) {
            let line = git_error_line(if res.stderr.is_empty() { &res.stdout } else { &res.stderr });
            // list 与 remove 之间被 lock 上了：同样立刻收手，不落到兜底删除
            if line.to_ascii_lowercase().contains("locked working tree") {
                return (vec![locked_warning(dir)], false);
            }
            let why = if line.is_empty() { format!("退出码 {}", code_text(res.code)) } else { line.to_string() };
            warn.push(format!("git worktree remove 失败：{why}"));
        }
    }

    // remove 成功时目录已经没了，这一步是幂等兜底
    let mut used_fallback = false;
    let mut gone = true;
    let still_there = path_exists(host, dir, &remove_opts()).await.unwrap_or(true);
    if still_there {
        used_fallback = true;
        let problem =
            if host.key == "local" { remove_local_dir(dir).await } else { remove_remote_dir(host, dir).await };
        if let Some(problem) = problem {
            warn.push(problem);
            gone = false;
        }
    }

    if used_fallback {
        let _ = probe_git(host, &gc::worktree_prune_args(&host.git, repo), gc::GIT_ENV, &remove_opts()).await;
    }

    (warn, gone)
}

// ---------------- 多仓库派生行 ----------------

/// 多仓库派生行的静态否决。与 veto_removal 同一地位：纯函数、只吃行数据，
/// 写错了会删掉用户东西的那一段，可审查性优先于复用度——所以不去改 veto_removal，
/// 而是给多仓库形态一份自己的完整断言清单。
///
/// 多版最关键的新断言是**包围盒**：集中目录（working_dir）是唯一允许动手的范围，
/// 每个成员 worktree 必须严格在它内部——成员出圈即脏数据，整行拒删。
pub fn veto_multi_removal<S: AsRef<str>>(
    kind: HostKind,
    row: &ProjectRow,
    members: Option<&[MultiRepoMember]>,
    home: &str,
    other_dirs: &[S],
) -> Option<String> {
    let central = row.working_dir.as_deref();
    let shown = central.unwrap_or("(空)");
    let clash_of = |p: &str| {
        other_dirs
            .iter()
            .map(AsRef::as_ref)
            .find(|o| same_path(kind, p, o) || is_ancestor(kind, p, o))
            .map(str::to_string)
    };

    // ① 必须是派生行。容器的成员是用户的真仓库，绝不进删除路径
    if row.source_project_id.is_none() {
        return Some(format!("「{}」不是附属项目，不清理任何目录", row.name));
    }

    // ② 必须是 falcon 建的目录
    if row.worktree_created_by_mojito != Some(1) {
        return Some(format!("{shown} 不是 Falcon 创建的，未删除"));
    }

    // ③ 成员清单必须完好。JSON 损坏 / 空清单 / 超上限都视为脏数据——
    //    清单就是删除目标，读不出清单等于不知道该删什么
    let Some(members) = members.filter(|m| !m.is_empty() && m.len() <= MULTI_REPO_MAX) else {
        return Some(format!("成员记录损坏或异常，未删除任何目录：{shown}"));
    };

    // ④ 集中目录非空、绝对、非 UNC、深度门槛（与单版 ③④ 同理）
    let Some(central) = central.filter(|c| !c.is_empty() && is_absolute(kind, c)) else {
        return Some(format!("集中目录不是绝对路径，未删除：{shown}"));
    };
    if is_unc(kind, central) {
        return Some(format!("集中目录是 UNC 路径，拒绝删除：{central}"));
    }
    if path_depth(kind, central) < 2 {
        return Some(format!("路径过浅，拒绝删除：{central}"));
    }

    // ⑤ 绝不删家目录本身或它的上层（可以在 home 里面，同单版 ⑥）
    if !home.is_empty() && (same_path(kind, central, home) || is_ancestor(kind, central, home)) {
        return Some(format!("集中目录是家目录或其上层，拒绝删除：{central}"));
    }

    for m in members {
        // ⑥ 包围盒：每个成员必须严格在集中目录内部
        if m.dir.is_empty() || !is_absolute(kind, &m.dir) {
            let shown = if m.dir.is_empty() { "(空)" } else { &m.dir };
            return Some(format!("成员路径不是绝对路径，未删除任何目录：{shown}"));
        }
        if !is_ancestor(kind, central, &m.dir) {
            return Some(format!("成员 {} 不在集中目录 {central} 内部，记录异常，未删除任何目录", m.dir));
        }

        // ⑦ 仓库根记录必须完好，且成员不得等于/包住任何成员的仓库根
        let Some(repo_dir) = m.repo_dir.as_deref().filter(|r| !r.is_empty() && is_absolute(kind, r)) else {
            return Some(format!("成员 {} 缺少仓库根记录，未删除任何目录", m.dir));
        };
        for other in members {
            let Some(other_repo) = other.repo_dir.as_deref().filter(|r| !r.is_empty()) else { continue };
            if same_path(kind, &m.dir, other_repo) || is_ancestor(kind, &m.dir, other_repo) {
                return Some(format!("成员 {} 覆盖仓库根 {other_repo}，拒绝删除", m.dir));
            }
        }

        // ⑧ 集中目录不得等于/包住任何仓库根——仓库先于集中目录存在，落进去只可能是脏数据
        if same_path(kind, central, repo_dir) || is_ancestor(kind, central, repo_dir) {
            return Some(format!("集中目录 {central} 包含仓库根 {repo_dir}，拒绝删除"));
        }

        // ⑨ 成员别踩到其他项目的目录上。⑥ 保证成员在集中目录内部，检查集中目录本可覆盖
        //    成员，但这里仍逐成员各查一遍——将来若有人放宽 ⑥，这条不至于跟着失守
        if let Some(clash) = clash_of(&m.dir) {
            return Some(format!("成员 {} 包含另一个项目的目录（{clash}），拒绝删除", m.dir));
        }
    }

    // ⑨ 集中目录别踩到其他项目的目录上
    if let Some(clash) = clash_of(central) {
        return Some(format!("集中目录包含另一个项目的目录（{clash}），拒绝删除：{central}"));
    }

    None
}

/// 多仓库派生行的清理：逐成员 取证 → 清理，最后收拾集中目录。
///
/// locked 的成员**成员级保留**、其余照删（与"DB 行无条件删、清理 best-effort"一致：
/// 整体收手会留下 N 个目录让用户手工收拾，更糟）。集中目录只走**空目录删除**——
/// 里面可能有用户放的别的东西，非空一律留下 + warning 带路径。
async fn cleanup_multi_worktree<S: AsRef<str>>(row: &ProjectRow, host: &GitHost, other_dirs: &[S]) -> Vec<String> {
    let members = Db::parse_multi_repos(row.multi_repos.as_deref());
    if let Some(veto) = veto_multi_removal(host.kind, row, members.as_deref(), &host.home, other_dirs) {
        return vec![veto];
    }
    let (Some(members), Some(working_dir)) = (members, row.working_dir.as_deref()) else {
        unreachable!("veto_multi_removal 的 ③④ 已挡住空值")
    };

    let mut warn = Vec::new();
    let mut all_gone = true;
    for m in &members {
        let dir = normalize_sep(host.kind, &m.dir);
        let Some(repo) = m.repo_dir.as_deref() else { unreachable!("veto ⑦ 已保证非空") };
        let Some(proof) = prove_worktree(host, repo, &dir).await else {
            warn.push(format!("无法确认 {dir} 仍是 {repo} 的 worktree，出于安全没有删除，请手动清理"));
            all_gone = false;
            continue;
        };
        if proof.locked {
            warn.push(locked_warning(&dir));
            all_gone = false;
            continue;
        }
        let (w, gone) = clear_one_worktree(host, repo, &dir, proof).await;
        warn.extend(w);
        all_gone &= gone;
    }

    let central = normalize_sep(host.kind, working_dir);
    if all_gone {
        // falcon 派生时写进集中目录的引导清单（AGENTS.md / CLAUDE.md，见 virtualdir）：
        // 不先删掉它们，下面的空目录删除永远非空、永远留 warning
        remove_central_manifests(host, &central).await;
        if let Some(problem) = remove_empty_dir(host, &central).await {
            warn.push(problem);
        }
    } else {
        warn.push(format!("集中目录未删除（内有残留）：{central}"));
    }
    warn
}

/// 守护式删除集中目录里 falcon 自己写的清单：AGENTS.md 首行要含生成标记、
/// CLAUDE.md 全文要精确等于 @AGENTS.md 才删——用户改写过的内容视为用户文件，
/// 保留 → 后面的 rmdir 因非空失败 → 既有 warning 带路径，方向安全
/// （与 veto_multi_removal 同一个态度：比对不过宁可不删）。
///
/// 只认两个固定 basename、非递归；best-effort，失败一律静默——它的失败必然
/// 让 remove_empty_dir 报出带路径的 warning，不需要第二条。远端脚本在
/// virtualdir 构造（有单测盯形态），执行只在这里——remove 继续是
/// 全仓库唯一删除用户可见路径的地方。
async fn remove_central_manifests(host: &GitHost, central: &str) {
    if host.key == "local" {
        let sweep = |name: &str, ours: &dyn Fn(&str) -> bool| {
            let p = join_path(host.kind, &[central, name]);
            // ENOENT = 本来就没有；读不动就留给 rmdir 去失败。按 UTF-8 宽松解码（同 Node 的 "utf8"）
            if let Ok(bytes) = std::fs::read(&p)
                && ours(&String::from_utf8_lossy(&bytes))
            {
                let _ = std::fs::remove_file(&p);
            }
        };
        sweep("AGENTS.md", &|t| t.split('\n').next().unwrap_or("").contains(FALCON_GENERATED_MARK));
        sweep("CLAUDE.md", &|t| js_trim(t) == js_trim(CLAUDE_MD_BODY));
        return;
    }
    let _ = exec_raw(host, &central_manifest_sweep_command(host.kind, central), &remove_opts()).await;
}

/// 只删**空**目录：posix rmdir / windows [IO.Directory]::Delete($p, $false) /
/// 本地 std::fs::remove_dir。非递归在类别上无法毁数据（最多删掉一个空壳），但它仍然
/// 住在这个文件里——remove 继续是全仓库唯一删除用户可见路径的地方。
/// 返回 None = 删掉了或本来就不在；Some 是给用户的 warning（自带路径）。
pub async fn remove_empty_dir(host: &GitHost, dir: &str) -> Option<String> {
    let fail_text = |detail: &str| format!("集中目录未删除（可能有残留文件）：{dir}（{detail}）");
    if host.key == "local" {
        return match std::fs::remove_dir(Path::new(dir)) {
            Ok(()) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => Some(fail_text(&e.to_string())),
        };
    }
    let cmd = if host.kind == HostKind::Windows {
        encode_powershell(
            &[
                format!("$p = {}", quote_powershell(dir)),
                "if (-not (Test-Path -LiteralPath $p)) { 'gone'; exit 0 }".into(),
                "try { [System.IO.Directory]::Delete($p, $false); 'ok' } catch { 'failed: ' + $_.Exception.Message }"
                    .into(),
            ]
            .join("; "),
        )
    } else {
        [
            format!("p={}", quote_posix(dir)),
            r#"if [ ! -e "$p" ]; then printf gone"#.into(),
            r#"elif rmdir -- "$p" 2>/dev/null; then printf ok"#.into(),
            "else printf failed; fi".into(),
        ]
        .join("; ")
    };
    let res = caught(exec_raw(host, &cmd, &remove_opts()).await);
    let out = js_trim(&res.stdout);
    if out == "ok" || out == "gone" {
        return None;
    }
    let line = git_error_line(if out.is_empty() { &res.stderr } else { out });
    let why = if line.is_empty() { format!("退出码 {}", code_text(res.code)) } else { line.to_string() };
    Some(fail_text(&why))
}

/// 本请求刚建出的一棵 worktree（[`rollback_created_worktrees`] 的入参）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedWorktree {
    /// git 报的 worktree 路径
    pub dir: String,
    /// 主 worktree 路径
    pub repo_dir: String,
}

/// 批量派生失败后的回滚。只对"本请求刚建出、claim_worktree 已确认"的成员动手：
/// 逆序逐个 `git worktree remove --force`（一个 --force，与删除链路同一条规矩），
/// 失败就记 leftover——刚建的树是干净的，remove 理应成功；万一失败（比如毫秒级内
/// 被 lock），把路径原样报给用户比再开一条裸删路径便宜且诚实。绝不落 rm 兜底。
///
/// 返回残留的绝对路径（空 = 回滚干净）。
pub async fn rollback_created_worktrees(
    host: &GitHost,
    created: &[CreatedWorktree],
    central_dir: &str,
    central_created_by_us: bool,
) -> Vec<String> {
    let mut leftover = Vec::new();
    for c in created.iter().rev() {
        let res = caught(
            with_repo_lock(
                repo_lock_key(host, &c.repo_dir),
                probe_git(host, &gc::worktree_remove_args(&host.git, &c.repo_dir, &c.dir), gc::GIT_ENV, &remove_opts()),
            )
            .await,
        );
        if res.code != Some(0) {
            leftover.push(c.dir.clone());
            // remove 失败会留 stale 管理项，prune 一下别让下次同名派生报莫名其妙的错
            let _ =
                probe_git(host, &gc::worktree_prune_args(&host.git, &c.repo_dir), gc::GIT_ENV, &remove_opts()).await;
        }
    }
    if central_created_by_us {
        if leftover.is_empty() {
            if remove_empty_dir(host, central_dir).await.is_some() {
                leftover.push(central_dir.to_string());
            }
        } else {
            // 成员还在里面，集中目录必然留着——一并列出来，用户照单收拾
            leftover.push(central_dir.to_string());
        }
    }
    leftover
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::tests::{have_git, init_repo, local_host, quote, real_tempdir, sh};
    use crate::git::repo::{AddWorktreeOptions, add_worktree};
    use falcon_proto::WorktreeMode;

    // ---------------- veto_multi_removal（remove.test.ts 原样）----------------

    /// 手工构造脏数据行。缺省是一条"应该允许删"的健康多仓库派生行
    fn row(f: impl FnOnce(&mut ProjectRow)) -> ProjectRow {
        let mut r = ProjectRow {
            id: "p1".into(),
            name: "组合-feat-x".into(),
            project_type: "local".into(),
            working_dir: Some("/home/u/code/app-feat-x".into()),
            source_project_id: Some("src1".into()),
            worktree_branch: Some("feat/x".into()),
            worktree_created_by_mojito: Some(1),
            ..Default::default()
        };
        f(&mut r);
        r
    }

    fn m(dir: &str, repo_dir: Option<&str>) -> MultiRepoMember {
        MultiRepoMember { dir: dir.into(), repo_dir: repo_dir.map(str::to_string) }
    }

    fn members() -> Vec<MultiRepoMember> {
        vec![
            m("/home/u/code/app-feat-x/web", Some("/home/u/code/web")),
            m("/home/u/code/app-feat-x/server", Some("/home/u/code/server")),
        ]
    }

    const HOME: &str = "/home/u";
    const NONE: &[&str] = &[];

    fn veto_with(
        f: impl FnOnce(&mut ProjectRow),
        members: Option<&[MultiRepoMember]>,
        other: &[&str],
    ) -> Option<String> {
        veto_multi_removal(HostKind::Posix, &row(f), members, HOME, other)
    }

    fn veto(f: impl FnOnce(&mut ProjectRow)) -> Option<String> {
        veto_with(f, Some(&members()), NONE)
    }

    fn veto_members(ms: &[MultiRepoMember]) -> Option<String> {
        veto_with(|_| {}, Some(ms), NONE)
    }

    #[track_caller]
    fn assert_has(got: Option<String>, needle: &str) {
        let got = got.unwrap_or_else(|| panic!("应当拒删（{needle}），却放行了"));
        assert!(got.contains(needle), "{got:?} 不含 {needle:?}");
    }

    #[test]
    fn multi_allows_a_healthy_derived_row() {
        assert_eq!(veto(|_| {}), None);
    }

    #[test]
    fn multi_1_rejects_a_non_derived_row() {
        // 容器绝不能走进删除路径
        assert_has(veto(|r| r.source_project_id = None), "不是附属项目");
    }

    #[test]
    fn multi_2_rejects_when_not_created_by_falcon() {
        assert_has(veto(|r| r.worktree_created_by_mojito = Some(0)), "不是 Falcon 创建");
        assert_has(veto(|r| r.worktree_created_by_mojito = None), "不是 Falcon 创建");
    }

    #[test]
    fn multi_3_rejects_broken_empty_or_oversized_member_list() {
        assert_has(veto_with(|_| {}, None, NONE), "成员记录损坏");
        assert_has(veto_members(&[]), "成员记录损坏");
        let many: Vec<_> =
            (0..17).map(|i| m(&format!("/home/u/code/app-feat-x/r{i}"), Some(&format!("/home/u/code/r{i}")))).collect();
        assert_has(veto_members(&many), "成员记录损坏");
    }

    #[test]
    fn multi_4_rejects_relative_unc_or_too_shallow_central_dir() {
        assert_has(veto(|r| r.working_dir = Some("code/app-feat-x".into())), "绝对路径");
        assert_has(veto(|r| r.working_dir = None), "绝对路径");
        assert_has(
            veto_multi_removal(
                HostKind::Windows,
                &row(|r| r.working_dir = Some("\\\\srv\\share\\x".into())),
                Some(&members()),
                HOME,
                NONE,
            ),
            "UNC",
        );
        assert_has(veto(|r| r.working_dir = Some("/app-feat-x".into())), "过浅");
    }

    #[test]
    fn multi_5_rejects_home_and_its_ancestors_but_allows_living_inside_home() {
        assert_has(veto(|r| r.working_dir = Some("/home/u".into())), "家目录");
        // central = /home 是 home 的祖先——同时也过浅，但先撞哪条都必须拒
        assert!(veto(|r| r.working_dir = Some("/home".into())).is_some());
        // 健康行本来就在 home 里面
        assert_eq!(veto(|_| {}), None);
    }

    #[test]
    fn multi_6_rejects_members_outside_the_central_dir() {
        let out = [m("/home/u/code/elsewhere/web", Some("/home/u/code/web")), members()[1].clone()];
        assert_has(veto_members(&out), "不在集中目录");
        // 成员等于集中目录本身也不行——is_ancestor 是严格的
        assert_has(veto_members(&[m("/home/u/code/app-feat-x", Some("/home/u/code/web"))]), "不在集中目录");
    }

    #[test]
    fn multi_6_rejects_relative_member_dirs() {
        assert_has(veto_members(&[m("web", Some("/home/u/code/web"))]), "绝对路径");
        assert_has(veto_members(&[m("", Some("/home/u/code/web"))]), "(空)");
    }

    #[test]
    fn multi_7_rejects_a_missing_repo_dir_record() {
        assert_has(veto_members(&[m("/home/u/code/app-feat-x/web", None)]), "缺少仓库根");
    }

    #[test]
    fn multi_7_rejects_a_member_that_equals_or_contains_any_repo_root() {
        assert_has(
            veto_members(&[m("/home/u/code/app-feat-x/web", Some("/home/u/code/app-feat-x/web"))]),
            "覆盖仓库根",
        );
    }

    #[test]
    fn multi_8_rejects_a_central_dir_that_contains_a_repo_root() {
        // 成员 dir 是 repoDir 的祖先 → ⑦ 先拦
        assert!(
            veto_members(&[m("/home/u/code/app-feat-x/web", Some("/home/u/code/app-feat-x/web/upstream"))]).is_some()
        );
        // 只触发 ⑧ 的形状
        let got = veto_members(&[m("/home/u/code/app-feat-x/other", Some("/home/u/code/app-feat-x/repo"))]).unwrap();
        assert!(got.starts_with("集中目录 ") && got.contains(" 包含仓库根"), "{got}");
    }

    #[test]
    fn multi_9_rejects_clashes_with_other_projects_member_and_central_level() {
        assert_has(veto_with(|_| {}, Some(&members()), &["/home/u/code/app-feat-x/web/sub"]), "另一个项目");
        assert_has(veto_with(|_| {}, Some(&members()), &["/home/u/code/app-feat-x"]), "另一个项目");
    }

    #[test]
    fn multi_windows_mixed_separators_and_case_still_match() {
        let win_row = row(|r| r.working_dir = Some("D:\\code\\App-feat-x".into()));
        let win_members = [m("D:/code/app-feat-x/Web", Some("D:\\repos\\Web"))];
        assert_eq!(veto_multi_removal(HostKind::Windows, &win_row, Some(&win_members), "C:\\Users\\u", NONE), None);
        // 集中目录大小写别名踩到别的项目目录上
        assert_has(
            veto_multi_removal(
                HostKind::Windows,
                &win_row,
                Some(&win_members),
                "C:\\Users\\u",
                &["d:\\code\\app-feat-x"],
            ),
            "另一个项目",
        );
    }

    #[test]
    fn multi_posix_backslash_is_a_filename_char_not_a_separator() {
        // 成员名里带反斜杠，不该被归一化成路径层级
        assert_eq!(veto_members(&[m("/home/u/code/app-feat-x/a\\b", Some("/home/u/code/ab"))]), None);
    }

    #[test]
    fn single_guard_still_rejects_multi_shaped_misuse() {
        // cleanup_worktree 按 multi_repos 分派，理论上到不了这里；万一到了，
        // 单版护栏的 ③（worktree_repo_dir 缺失）也要能兜住
        let r = row(|r| r.multi_repos = Some("[]".into()));
        assert_has(veto_removal(HostKind::Posix, &r, HOME, NONE), "仓库根记录缺失");
    }

    // ---------------- veto_removal（ADR 0002 的七条静态断言）----------------

    fn single(f: impl FnOnce(&mut ProjectRow)) -> ProjectRow {
        row(|r| {
            r.working_dir = Some("/home/u/code/app-feat-x".into());
            r.worktree_repo_dir = Some("/home/u/code/app".into());
            f(r);
        })
    }

    fn veto1(f: impl FnOnce(&mut ProjectRow)) -> Option<String> {
        veto_removal(HostKind::Posix, &single(f), HOME, NONE)
    }

    #[test]
    fn single_allows_a_sibling_worktree_inside_home() {
        assert_eq!(veto1(|_| {}), None);
    }

    #[test]
    fn single_1_2_rejects_non_derived_and_not_ours() {
        assert_has(veto1(|r| r.source_project_id = None), "不是附属项目");
        assert_has(veto1(|r| r.worktree_created_by_mojito = Some(0)), "不是 Falcon 创建");
        assert_has(veto1(|r| r.worktree_created_by_mojito = None), "不是 Falcon 创建");
    }

    #[test]
    fn single_3_rejects_empty_or_relative_paths() {
        assert_has(veto1(|r| r.working_dir = None), "工作目录不是绝对路径，未删除：(空)");
        // 空串不是缺省：TS 的 `?? "(空)"` 不替换它
        assert_eq!(veto1(|r| r.working_dir = Some(String::new())).unwrap(), "工作目录不是绝对路径，未删除：");
        assert_has(veto1(|r| r.working_dir = Some("code/app-feat-x".into())), "不是绝对路径");
        assert_has(veto1(|r| r.worktree_repo_dir = None), "仓库根记录缺失");
        assert_has(veto1(|r| r.worktree_repo_dir = Some("app".into())), "仓库根记录缺失");
    }

    #[test]
    fn single_4_rejects_drive_roots_and_one_level_paths() {
        assert_has(veto1(|r| r.working_dir = Some("/".into())), "过浅");
        assert_has(veto1(|r| r.working_dir = Some("/x".into())), "过浅");
        let win = |dir: &str| {
            veto_removal(
                HostKind::Windows,
                &single(|r| {
                    r.working_dir = Some(dir.into());
                    r.worktree_repo_dir = Some("D:\\code\\app".into());
                }),
                "C:\\Users\\u",
                NONE,
            )
        };
        assert_has(win("C:\\"), "过浅");
        assert_has(win("D:\\x"), "过浅");
        assert_eq!(win("D:\\code\\app-feat-x"), None);
    }

    #[test]
    fn single_5_rejects_repo_root_and_its_ancestors() {
        assert_has(veto1(|r| r.working_dir = Some("/home/u/code/app".into())), "目标就是仓库根");
        assert_has(veto1(|r| r.working_dir = Some("/home/u/code/app/".into())), "目标就是仓库根");
        assert_has(veto1(|r| r.working_dir = Some("/home/u/code".into())), "目标包含仓库根");
    }

    #[test]
    fn single_6_rejects_home_and_its_ancestors() {
        let r = single(|r| {
            r.working_dir = Some("/home/u".into());
            r.worktree_repo_dir = Some("/srv/app".into());
        });
        assert_has(veto_removal(HostKind::Posix, &r, HOME, NONE), "家目录");
        let r = single(|r| {
            r.working_dir = Some("/home/u/..".into());
            r.worktree_repo_dir = Some("/srv/app".into());
        });
        // 静态断言按字面比、不做 `..` 归一（TS 同样如此）：/home/u/.. 在这里放行，
        // 兜底的是动态取证（不是 worktree 就不删）与远端删除命令的物理路径校验。
        // 落库的路径取自 git 的输出，正常数据里不会出现 `..`
        assert_eq!(veto_removal(HostKind::Posix, &r, HOME, NONE), None);
        // Windows 大小写别名
        let r = single(|r| {
            r.working_dir = Some("c:\\users\\U".into());
            r.worktree_repo_dir = Some("D:\\code\\app".into());
        });
        assert_has(veto_removal(HostKind::Windows, &r, "C:\\Users\\u", NONE), "家目录");
    }

    #[test]
    fn single_7_rejects_stepping_on_other_projects() {
        assert_has(veto_removal(HostKind::Posix, &single(|_| {}), HOME, &["/home/u/code/app-feat-x"]), "另一个项目");
        assert_has(
            veto_removal(HostKind::Posix, &single(|_| {}), HOME, &["/home/u/code/app-feat-x/pkg"]),
            "另一个项目",
        );
        // 兄弟目录不算
        assert_eq!(veto_removal(HostKind::Posix, &single(|_| {}), HOME, &["/home/u/code/app-feat-y"]), None);
    }

    // ---------------- 命令形态 ----------------

    #[test]
    fn posix_remove_command_is_one_check_and_delete_line() {
        assert_eq!(
            posix_remove_command("/a/it's"),
            "p='/a/it'\\''s'; if [ ! -e \"$p\" ]; then printf gone; exit 0; fi; \
             if [ -L \"$p\" ]; then printf symlink; exit 0; fi; \
             if [ ! -d \"$p\" ]; then printf notdir; exit 0; fi; \
             if [ \"$(cd -P -- \"$p\" && pwd -P)\" != \"$p\" ]; then printf notphysical; exit 0; fi; \
             rm -rf -- \"$p\" && printf ok || printf failed"
        );
    }

    #[test]
    fn windows_remove_command_uses_literal_paths_and_directory_delete() {
        let inner = crate::zellij::host::tests::decode(&windows_remove_command("D:\\code\\my[old]repo-feat"));
        assert!(
            inner.contains(
                "$p = 'D:\\code\\my[old]repo-feat'; if (-not (Test-Path -LiteralPath $p)) { 'gone'; exit 0 }; "
            )
        );
        assert!(inner.contains("$i = Get-Item -LiteralPath $p -Force; "));
        assert!(inner.contains("[System.IO.FileAttributes]::ReparsePoint) { 'symlink'; exit 0 }; "));
        assert!(inner.contains(
            "try { [System.IO.Directory]::Delete($p, $true); 'ok' } catch { 'failed: ' + $_.Exception.Message }"
        ));
        assert!(!inner.contains("Remove-Item"));
    }

    // ---------------- 真文件系统（临时目录）----------------

    #[tokio::test]
    async fn local_removal_refuses_symlinks_and_files_and_is_idempotent() {
        let (_t, base) = real_tempdir();
        let target = base.join("victim");
        std::fs::create_dir_all(target.join("keep")).unwrap();
        std::fs::write(target.join("keep/f"), "x").unwrap();
        let link = base.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(unix)]
        assert_has(remove_local_dir(link.to_str().unwrap()).await, "符号链接");
        assert!(target.join("keep/f").exists());

        let file = base.join("file");
        std::fs::write(&file, "x").unwrap();
        assert_has(remove_local_dir(file.to_str().unwrap()).await, "不是文件夹");

        // 目录里的符号链接只删链接本身，不跟进去
        let wt = base.join("wt");
        std::fs::create_dir_all(wt.join("node_modules")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, wt.join("node_modules/escape")).unwrap();
        assert_eq!(remove_local_dir(wt.to_str().unwrap()).await, None);
        assert!(!wt.exists());
        assert!(target.join("keep/f").exists());
        assert_eq!(remove_local_dir(wt.to_str().unwrap()).await, None);
    }

    #[tokio::test]
    async fn remote_posix_removal_script_refuses_symlinks_and_non_physical_paths() {
        // 用本机 /bin/sh 跑 POSIX 远端那条命令：key 不是 "local" 就走 remove_remote_dir
        let (_t, base) = real_tempdir();
        let host = local_host("test:remote-posix");
        let target = base.join("victim");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("f"), "x").unwrap();
        let link = base.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(unix)]
        assert_has(remove_remote_dir(&host, link.to_str().unwrap()).await, "符号链接 / junction");
        // 经过符号链接的父目录：不是物理路径，拒删
        #[cfg(unix)]
        {
            std::fs::create_dir_all(target.join("inner")).unwrap();
            assert_has(remove_remote_dir(&host, link.join("inner").to_str().unwrap()).await, "经过符号链接");
            assert!(target.join("inner").exists());
        }
        assert_has(remove_remote_dir(&host, target.join("f").to_str().unwrap()).await, "不是文件夹");
        assert_eq!(remove_remote_dir(&host, target.to_str().unwrap()).await, None);
        assert!(!target.exists());
        assert_eq!(remove_remote_dir(&host, target.to_str().unwrap()).await, None, "gone 也算成功");
    }

    #[tokio::test]
    async fn remove_empty_dir_never_removes_contents() {
        let (_t, base) = real_tempdir();
        for key in ["local", "test:remote-empty"] {
            let host = local_host(key);
            let dir = base.join(format!("central-{}", key.len()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("user-file"), "x").unwrap();
            let p = dir.to_str().unwrap();
            assert_has(remove_empty_dir(&host, p).await, "集中目录未删除（可能有残留文件）");
            assert!(dir.join("user-file").exists());
            std::fs::remove_file(dir.join("user-file")).unwrap();
            assert_eq!(remove_empty_dir(&host, p).await, None);
            assert!(!dir.exists());
            assert_eq!(remove_empty_dir(&host, p).await, None);
        }
    }

    #[tokio::test]
    async fn cleanup_worktree_end_to_end_on_a_real_repo() {
        if !have_git() {
            return;
        }
        let (_t, base) = real_tempdir();
        let repo = base.join("app");
        init_repo(&repo);
        let repo_s = repo.to_str().unwrap().to_string();
        let host = local_host("local");
        let wt = format!("{}/app-feat-x", base.to_str().unwrap());
        let add = AddWorktreeOptions {
            mode: WorktreeMode::NewBranch,
            branch: "feat/x".into(),
            start_point: None,
            dir: wt.clone(),
        };
        assert_eq!(add_worktree(&host, &repo_s, &add).await.unwrap(), wt);
        sh(Path::new(&wt), "printf dirty > untracked.txt");

        let r = single(|r| {
            r.working_dir = Some(wt.clone());
            r.worktree_repo_dir = Some(repo_s.clone());
        });

        // 别的项目指着它：静态断言拦下，目录不动
        let warn = cleanup_worktree(&r, &host, std::slice::from_ref(&wt)).await;
        assert_eq!(warn.len(), 1);
        assert!(Path::new(&wt).exists());

        // 被 lock：立刻收手，不落兜底删除
        sh(&repo, &format!("git worktree lock {}", quote(&wt)));
        let warn = cleanup_worktree(&r, &host, NONE).await;
        assert_eq!(warn, [locked_warning(&wt)]);
        assert!(Path::new(&wt).join("untracked.txt").exists());
        sh(&repo, &format!("git worktree unlock {}", quote(&wt)));

        // 正常删除：目录没了、分支还在、源仓库纹丝不动
        let warn = cleanup_worktree(&r, &host, NONE).await;
        assert!(warn.is_empty(), "{warn:?}");
        assert!(!Path::new(&wt).exists());
        assert!(repo.join("a.txt").exists());
        sh(&repo, "git rev-parse --verify --quiet refs/heads/feat/x");
        // 幂等：再删一次没有证据，只给 warning
        let warn = cleanup_worktree(&r, &host, NONE).await;
        assert_has(warn.into_iter().next(), "无法确认");
    }

    #[tokio::test]
    async fn cleanup_falls_back_to_gitfile_proof_when_the_source_repo_is_gone() {
        if !have_git() {
            return;
        }
        let (_t, base) = real_tempdir();
        let repo = base.join("app");
        init_repo(&repo);
        let repo_s = repo.to_str().unwrap().to_string();
        let host = local_host("local");
        let wt = format!("{}/app-feat-y", base.to_str().unwrap());
        let add = AddWorktreeOptions {
            mode: WorktreeMode::NewBranch,
            branch: "feat/y".into(),
            start_point: None,
            dir: wt.clone(),
        };
        add_worktree(&host, &repo_s, &add).await.unwrap();
        // 用户把整个源仓库挪走了：证据 A 永远失败，靠 .git 文件（证据 B）清理
        std::fs::rename(&repo, base.join("app-moved")).unwrap();
        let r = single(|r| {
            r.working_dir = Some(wt.clone());
            r.worktree_repo_dir = Some(repo_s.clone());
        });
        let warn = cleanup_worktree(&r, &host, NONE).await;
        assert!(warn.is_empty(), "{warn:?}");
        assert!(!Path::new(&wt).exists());
        assert!(base.join("app-moved/a.txt").exists());

        // 一个普通目录（没有 .git 文件）：没有任何证据，绝不删
        let plain = format!("{}/app-feat-z", base.to_str().unwrap());
        std::fs::create_dir_all(&plain).unwrap();
        let r = single(|r| {
            r.working_dir = Some(plain.clone());
            r.worktree_repo_dir = Some(format!("{}/app-moved", base.to_str().unwrap()));
        });
        assert_has(cleanup_worktree(&r, &host, NONE).await.into_iter().next(), "无法确认");
        assert!(Path::new(&plain).exists());
    }
}

//! git 命令构造与输出解析。移植自 `packages/server/src/git/command.ts`。纯函数，零 I/O。
//!
//! 与 zellij/command.rs 同一条规矩：**只产出 argv 数组与 env 列表**，转义交给
//! [`build_git_command_line`] 一处完成——远端 POSIX 与远端 Windows 的转义规则不同，
//! 绝不在这里拼字符串。
//!
//! 所有命令一律带 `-C <dir>` 而不依赖执行层的 cwd：SSH exec 根本没有 cwd 概念
//! （要另外套一层 `cd X &&`），localExec 走 shell 也没传 cwd。-C 是两种执行环境下
//! 唯一都成立的写法。同理，后端进程**永远不 chdir 进 worktree**——cwd 一旦
//! 落在里面，Windows 上这个目录就永久删不掉。
//!
//! 留到 S5 的函数：无。command.ts 全是纯函数，这里整份移完；要执行器的部分
//! （`repo.ts` 的 batchGit / probeGit / describe*、`multi.ts`、`remove.ts`、`lock.ts`）在 S5。
//!
//! 移植约定（JS → Rust 的语义差异都收在本文件底部的私有辅助函数里）：
//! - TS 的默认参数写成 `Option`，`None` = 取 TS 的默认值；
//! - `.length` / `slice` 按 UTF-16 码元算；`trim()` / 正则 `\s` 用 JS 的空白集合；
//! - `Number(x)` 走 [`js_string_to_number`]，不用 `str::parse`（后者不认 `0x`、空串，反而认 `inf`）。

use std::collections::HashMap;
use std::sync::LazyLock;

use falcon_proto::{
    GitBranchRef, GitCommit, GitCommitFile, GitConflictKind, GitFileChange, GitOpInput, GitRefKind, GitRefLabel,
    GitRemote, GitResetMode, GitTakeSide,
};
use indexmap::IndexMap;
use regex::Regex;
use serde_json::Value;

use super::path::sep_for;
use crate::zellij::host::{HostKind, build_command_line, encode_powershell, quote_posix, quote_powershell};

/// 所有 git 调用共用的环境变量。有序：发到宿主机的命令串要与 TS 的 `Object.entries` 逐字节一致
pub const GIT_ENV: &[(&str, &str)] = &[
    // 错误分类靠解析 stderr，而 git 会按 locale 翻译错误消息——不锁死 C 的话，
    // 一台中文系统上 "is already used by worktree at" 这条判据直接失效，
    // 用户看到的会是笼统的"操作失败"而不是"这个分支已经在别处检出了"。
    ("LC_ALL", "C"),
    // 需要凭据时绝不弹交互提示：SSH exec 没有 tty，git 会挂在那里等到天荒地老，
    // 表现为请求永远不返回——最难查的一类故障。localExec 与 SshLink.exec 都没有超时，
    // HTTP 层（TS 版是 Fastify）也没配 request timeout，所以这条与下面的 askpass 是唯一的防线。
    ("GIT_TERMINAL_PROMPT", "0"),
];

/// 只读查询额外带上：避免 status 去写 index.lock（只读挂载 / 并发时会失败）。
/// 顺序同 TS 的 `{ ...GIT_ENV, GIT_OPTIONAL_LOCKS: "0" }`
pub const GIT_ENV_RO: &[(&str, &str)] = &[("LC_ALL", "C"), ("GIT_TERMINAL_PROMPT", "0"), ("GIT_OPTIONAL_LOCKS", "0")];

/// 必须**删掉**（而不是置空）的环境变量。
///
/// `localExec` 用 `spawn(shell:true)` 继承后端进程的环境。拉起后端的进程——IDE、
/// 进程管理器、或者干脆是某个 git hook——若设了这些，所有 git 调用会静默作用到
/// 别的仓库上。askpass 同理：继承来的 helper 会让"绝不弹交互提示"落空。
///
/// 曾经写成 `GIT_DIR: ""` 塞进 env 里，在 Windows 上一路绿灯——因为 PowerShell 的
/// `$env:X = ''` 恰好等价于删除。但 POSIX 侧 `env GIT_DIR='' git ...` 是**设成空串**，
/// 实测直接 `fatal: not a git repository: ''`；`GIT_INDEX_FILE=''` 更狠，git 会拿
/// `.lock` 当索引文件，`status` 报出一整片并不存在的删除。也就是说这个功能在
/// 每一台 POSIX 宿主上都是坏的，而只在 Windows 上测根本看不出来。
pub const GIT_UNSET: &[&str] = &["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_ASKPASS", "SSH_ASKPASS"];

/// core.quotepath=false：让 git 原样输出非 ASCII 路径，不转成 \346\226\207。
/// --no-pager：非 tty 下 git 本就不分页，但 core.pager 被显式配成 `less -F` 之类时
/// 仍会挂住，加一道保险。
const BASE: [&str; 3] = ["-c", "core.quotepath=false", "--no-pager"];

fn at(git: &str, dir: &str, args: &[&str]) -> Vec<String> {
    let mut v = Vec::with_capacity(BASE.len() + 3 + args.len());
    v.push(git.to_string());
    v.extend(BASE.iter().map(|s| s.to_string()));
    v.push("-C".to_string());
    v.push(dir.to_string());
    v.extend(args.iter().map(|s| s.to_string()));
    v
}

/// `[...fixed, ...paths]`
fn with_paths<'a, S: AsRef<str>>(fixed: &[&'a str], paths: &'a [S]) -> Vec<&'a str> {
    fixed.iter().copied().chain(paths.iter().map(AsRef::as_ref)).collect()
}

/// TS 里 `origPath ? [origPath, path] : [path]`：空串也算没有
fn path_pair<'a>(path: &'a str, orig_path: Option<&'a str>) -> Vec<&'a str> {
    match orig_path {
        Some(o) if !o.is_empty() => vec![o, path],
        _ => vec![path],
    }
}

/// 探测 git 本身是否可用。必须先跑它，理由见 repo.ts 的 repoRoot 注释。
pub fn version_args(git: &str) -> Vec<String> {
    vec![git.to_string(), "--version".to_string()]
}

/// 仓库根。
///
/// --path-format=absolute（git ≥ 2.31）挡住 MSYS2 / Git-Bash 环境下吐出
/// /d/code/xxx 这种不能直接喂给 Windows API 的路径。注意即便如此，Windows 上返回的
/// 仍是正斜杠（D:/code/falcon）——调用方必须过一遍 normalize_sep。
pub fn repo_root_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["rev-parse", "--path-format=absolute", "--show-toplevel"])
}

/// detached 时输出字面量 "HEAD"
pub fn head_branch_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["rev-parse", "--abbrev-ref", "HEAD"])
}

pub fn head_short_sha_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["rev-parse", "--short", "HEAD"])
}

pub fn remote_list_args(git: &str, repo: &str) -> Vec<String> {
    at(git, repo, &["remote"])
}

/// 分支列表。字段分隔用 %09（TAB）：git check-ref-format 禁止分支名含 ASCII
/// 控制字符，所以 TAB 是安全分隔符，而空格不是（分支名可以含空格）。
///
/// 末列 %(symref) 是用来认出 origin/HEAD 的。不能按名字认：
/// `refs/remotes/origin/HEAD` 的 `%(refname:short)` 是 **origin**（git 取的是
/// 最短无歧义名），不是 origin/HEAD——于是下拉里会冒出一条叫 "origin" 的
/// 假分支，检出它必然失败。symref 只有符号引用才非空，判据是准的。
pub fn branch_list_args(git: &str, repo: &str) -> Vec<String> {
    at(
        git,
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short)%09%(upstream:short)%09%(HEAD)%09%(symref)",
            "refs/heads",
            "refs/remotes",
        ],
    )
}

pub fn worktree_list_args(git: &str, repo: &str) -> Vec<String> {
    at(git, repo, &["worktree", "list", "--porcelain"])
}

/// 本地分支是否存在：退出码即答案（0 = 存在）。批量派生的 auto 模式与预检用。
/// --verify 拒绝前缀匹配之类的猜测，--quiet 让"不存在"保持静默、不往 stderr 写 fatal。
pub fn branch_exists_args(git: &str, repo: &str, branch: &str) -> Vec<String> {
    at(git, repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
}

/// 新建分支并检出到新 worktree。
///
/// start_point 显式传（缺省由调用方填 "HEAD"）而不是省略：行为相同，但 argv 长度固定，
/// 日志里不会出现"这条为什么少一个参数"的疑问。
///
/// 不传 --track：从 origin/x 起分支时 branch.autoSetupMerge 默认就会建跟踪关系；
/// 用户显式关掉了那是他的配置，我们不该覆盖。
///
/// core.longpaths=true 只影响 Windows，POSIX 上是个无害的未知配置（git 会忽略）。
pub fn worktree_add_new_args(git: &str, repo: &str, path: &str, branch: &str, start_point: &str) -> Vec<String> {
    at(git, repo, &["-c", "core.longpaths=true", "worktree", "add", "-b", branch, path, start_point])
}

/// 把一条已存在的本地分支检出到新 worktree
pub fn worktree_add_existing_args(git: &str, repo: &str, path: &str, branch: &str) -> Vec<String> {
    at(git, repo, &["-c", "core.longpaths=true", "worktree", "add", path, branch])
}

/// remove 只给**一个** --force。
///
/// 第二个 --force 才会无视 `git worktree lock`，而 lock 是用户明说的"别碰"——
/// 正是 ADR 0001 里"绝不接管用户自有的东西"那条原则。锁住了就报给用户，不自动解锁。
pub fn worktree_remove_args(git: &str, repo: &str, path: &str) -> Vec<String> {
    at(git, repo, &["worktree", "remove", "--force", path])
}

pub fn worktree_prune_args(git: &str, repo: &str) -> Vec<String> {
    at(git, repo, &["worktree", "prune"])
}

// ---------------- 脏状态 ----------------
//
// 决策一律看退出码，不解析文本：Windows 远端上原生命令的输出按控制台代码页解码
// （zh-CN 默认 936），而 SshLink.exec 按 UTF-8 解码，中文路径会变乱码。
// 解析只用于**展示**样例清单，读不到就退化成计数，不影响任何判断。

/// 工作区有未暂存改动则退出码 1
pub fn diff_dirty_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["diff", "--quiet"])
}

/// 暂存区有改动则退出码 1
pub fn diff_cached_dirty_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["diff", "--cached", "--quiet"])
}

/// 未跟踪文件清单（一行一个）。输出非空即有未跟踪文件
pub fn untracked_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["ls-files", "--others", "--exclude-standard"])
}

/// Quick Open 的文件清单：已跟踪 + 未跟踪，排除 gitignore。
/// 路径相对 `-C` 的目录（工作目录），正斜杠分隔。
pub fn ls_files_index_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["ls-files", "-co", "--exclude-standard"])
}

/// 一行一个相对路径。空行丢掉；Windows 偶发反斜杠收成 `/`
pub fn parse_ls_files(stdout: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in lines(stdout) {
        // TS 是 split(/\r?\n/) 之后再 replace(/\r$/, "")：`a\r\r\n` 要去掉两个 \r
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() {
            continue;
        }
        out.push(line.replace('\\', "/"));
    }
    out
}

/// 展示用：已跟踪改动 + 未跟踪文件
pub fn status_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["status", "--porcelain"])
}

/// 同上，但把未跟踪**目录**展开成一个个文件。
///
/// 默认的 porcelain 会把整个未跟踪目录折叠成一条 `?? sub/`：那既算不出行数，
/// 在目录树视图里也会变成一个名字带斜杠的假"文件"。
///
/// 侧栏那条轮询（describeGitChanges）**不用**这个：它每 8 秒问一遍所有项目，
/// 而展开一个巨大的未跟踪目录（漏进来的 node_modules 之类）要枚举几万条。
/// 代价是面板的文件数可能比侧栏徽标大，那是面板更准确，不是它算错了。
pub fn status_all_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["status", "--porcelain", "--untracked-files=all"])
}

/// 被 .gitignore 忽略的条目。必须单独取：status --porcelain 默认不含它们，
/// 但 .env、本地 sqlite、上传目录会跟着一起被删——.env 通常是全世界唯一一份。
pub fn status_ignored_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["status", "--porcelain", "--ignored=matching"])
}

/// 未推送提交数。没有 upstream 时退出码 128，调用方按 null 处理，不算错误。
pub fn ahead_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["rev-list", "--count", "@{upstream}..HEAD"])
}

/// 未拉取提交数。与 ahead 一样，没有 upstream 时退出码 128。
pub fn behind_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["rev-list", "--count", "HEAD..@{upstream}"])
}

/// 当前分支跟踪的远程。没有 upstream 时退出码 128。
pub fn upstream_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["rev-parse", "--abbrev-ref", "@{upstream}"])
}

pub fn remote_verbose_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["remote", "-v"])
}

/// git 内建的空树对象。仓库还没有任何提交（无 HEAD）时拿它当 diff 基准，
/// 语义不变：工作区对比"一无所有"。
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// 单个文件相对 base（HEAD 或空树）的 diff：暂存 + 未暂存合在一起看。
///
/// 重命名要把新旧两个路径都传进 pathspec。实测 worktree↔tree 的 diff 不做
/// rename 配对（那是 --cached 视图的事），出来的是删除 + 新增两段——两半都在，
/// 这就是如实的答案；只传新路径会丢掉删除那一半。
pub fn diff_file_args(git: &str, dir: &str, base: &str, path: &str, orig_path: Option<&str>) -> Vec<String> {
    let mut args = vec!["diff", base, "--"];
    args.extend(path_pair(path, orig_path));
    at(git, dir, &args)
}

/// 未跟踪文件的伪 diff：与空文件比对。
///
/// `/dev/null` 是 git 在 diff --no-index 里特判的字面量（当空输入），Windows 上
/// 同样成立，不必换成 NUL。有差异时退出码 1——这是答案不是错误，调用方用 probeGit；
/// 空的未跟踪文件两边相同，退出码 0、无输出。
pub fn diff_untracked_args(git: &str, dir: &str, path: &str) -> Vec<String> {
    at(git, dir, &["diff", "--no-index", "--", "/dev/null", path])
}

/// 最近提交。%at 是 unix 秒——相对时间在前端按界面语言格式化，
/// 不拿 git 的 %ar（那会跟 LC_ALL=C 一起变成英文）。`n` 缺省 12。
pub fn log_args(git: &str, dir: &str, n: Option<usize>) -> Vec<String> {
    let n = n.unwrap_or(12).to_string();
    at(git, dir, &["log", "-n", &n, "--format=%h%x09%an%x09%at%x09%s"])
}

// ---------------- History 面板 ----------------

/// 只看这三类 ref，**不用 `--all`**。
///
/// --all 会把 refs/ 下的一切都算进来，包括 IDE 写的私有 ref——JetBrains 的
/// Local History 就挂在 refs/jb/* 下，每按几下保存就是一条提交。实测本仓库
/// `git log --all` 的头几条全是 "Local History"，把真实历史挤到了看不见的地方。
/// refs/stash 同理。
const HISTORY_REFS: [&str; 3] = ["--branches", "--remotes", "--tags"];

/// History 每页条数与作者聚合的采样深度
pub const LOG_PAGE: usize = 60;
pub const AUTHOR_SAMPLE: usize = 400;

/// [`log_page_args`] 的查询。空串与 `None` 等价（TS 里按真假判）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogQuery {
    /// 只看某条分支（Branch 下拉）。缺省看 HISTORY_REFS
    pub rev: Option<String>,
    /// 提交信息搜索（字面量，不是正则）
    pub grep: Option<String>,
    /// 作者筛选（字面量）
    pub author: Option<String>,
    pub skip: Option<usize>,
    /// 缺省 [`LOG_PAGE`]
    pub limit: Option<usize>,
}

/// `Option<String>` 在 TS `if (q.x)` 里的真假
fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

/// History 列表。
///
/// `-F`（--fixed-strings）对 --grep 与 --author 同时生效，把用户在搜索框里
/// 敲的东西一律当字面量——不加它，一个 `(` 就够 git 报 "Unmatched ( or \("
/// 然后整页空白。`-i` 对固定字符串同样生效。
///
/// rev 走 `<rev> --` 的位置参数：分支名以 `-` 开头时（git 允许 `--` 之后的
/// refname 长这样）不加终结符会被当成选项解析。
pub fn log_page_args(git: &str, dir: &str, q: &LogQuery) -> Vec<String> {
    let limit = q.limit.unwrap_or(LOG_PAGE);
    let n = limit.saturating_add(1).to_string();
    let mut args: Vec<String> = vec![
        "log".into(),
        // %P 空 = 根提交；%D 空 = 这条上没有任何 ref。subject 放最后一列，
        // 它是唯一可能含 TAB 的字段（ref 名与作者名都禁止控制字符）
        "--format=%H%x09%h%x09%an%x09%ae%x09%at%x09%P%x09%D%x09%s".into(),
        // 多取一条探"还有没有下一页"，比 rev-list --count 便宜得多
        "-n".into(),
        n,
    ];
    if let Some(skip) = q.skip.filter(|&s| s != 0) {
        args.push(format!("--skip={skip}"));
    }
    let grep = non_empty(&q.grep);
    let author = non_empty(&q.author);
    if grep.is_some() || author.is_some() {
        args.push("--fixed-strings".into());
        args.push("--regexp-ignore-case".into());
    }
    if let Some(g) = grep {
        args.push(format!("--grep={g}"));
    }
    if let Some(a) = author {
        args.push(format!("--author={a}"));
    }
    // 指定了分支就只看它，否则看全部分支/远程/标签——两者都要 --date-order，
    // 不然多分支并行时提交会按拓扑挤成一坨，画出来的图与时间轴对不上
    args.push("--date-order".into());
    match non_empty(&q.rev) {
        Some(rev) => {
            args.push(rev.into());
            args.push("--".into());
        }
        None => args.extend(HISTORY_REFS.iter().map(|s| s.to_string())),
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    at(git, dir, &refs)
}

/// 作者下拉的候选：采样近若干条提交的 %an，去重与排序在解析侧做。`n` 缺省 [`AUTHOR_SAMPLE`]
pub fn log_authors_args(git: &str, dir: &str, n: Option<usize>) -> Vec<String> {
    let n = n.unwrap_or(AUTHOR_SAMPLE).to_string();
    let mut args = vec!["log", "-n", &n, "--format=%an"];
    args.extend(HISTORY_REFS);
    at(git, dir, &args)
}

/// 作者下拉里「我」是谁。没配 user.name 时退出码 1、无输出——那是正常状态
/// （这台机器上还没设过身份），不是错误。
pub fn config_user_name_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["config", "--get", "user.name"])
}

/// 合并提交的 diff 取 first-parent。
///
/// 不加这个的话 `git show <merge>` 一个文件都不输出（默认 --diff-merges=off），
/// 面板上看起来就像"这次合并什么都没改"。git ≥ 2.31，与 --path-format=absolute
/// 是同一代要求。
const FIRST_PARENT: &str = "--diff-merges=first-parent";

/// 提交元数据（一行）。字段顺序与 [`parse_commit_meta`] 一一对应
pub fn commit_meta_args(git: &str, dir: &str, sha: &str) -> Vec<String> {
    at(git, dir, &["show", "-s", "--format=%H%x09%h%x09%an%x09%ae%x09%at%x09%cn%x09%ct%x09%P%x09%D", sha, "--"])
}

/// 完整提交信息。单独一条命令：%B 含换行，塞不进上面那种一行多列的格式
pub fn commit_message_args(git: &str, dir: &str, sha: &str) -> Vec<String> {
    at(git, dir, &["show", "-s", "--format=%B", sha, "--"])
}

/// 改动文件：一条命令同时要 --raw 与 --numstat。
///
/// 两段都要是因为各缺一半：--raw 给状态字母与**未压缩**的新旧路径（重命名是
/// `R100\told\tnew` 两列），--numstat 给增删行数但重命名路径会被压成
/// `dir/{old => new}/f` 这种紧凑形式。git 对两段用的是同一个 diff queue，
/// 文件顺序一致，所以按下标配对——解析侧对不上时退化成"数字未知"，见 [`parse_commit_files`]。
pub fn commit_files_args(git: &str, dir: &str, sha: &str) -> Vec<String> {
    at(git, dir, &["show", "--format=", FIRST_PARENT, "--raw", "--numstat", sha, "--"])
}

/// 某条提交里单个文件的 diff
pub fn commit_file_diff_args(git: &str, dir: &str, sha: &str, path: &str, orig_path: Option<&str>) -> Vec<String> {
    let mut args = vec!["show", "--format=", FIRST_PARENT, sha, "--"];
    args.extend(path_pair(path, orig_path));
    at(git, dir, &args)
}

/// 工作区已跟踪文件的增删行数，基准 HEAD（暂存 + 未暂存一起看）。
///
/// 与 status_args 是同一个视角，两者的结果按路径配对——不能按下标配，
/// status 会列出未跟踪文件而 numstat 不会，两边条数本来就不一样。
pub fn working_numstat_args(git: &str, dir: &str, base: &str) -> Vec<String> {
    at(git, dir, &["diff", "--numstat", base])
}

/// 未跟踪文件的行数：与空文件比。
///
/// 只能一个文件一条命令（--no-index 恰好收两个路径），所以调用方要把它们
/// 批成一次 exec，并且限个数——见 [`UNTRACKED_NUMSTAT_CAP`]。
///
/// 不用 `git add -N` 那个更省事的办法：那会写 index，而这个面板是只读的，
/// 用户的暂存区不该因为看了一眼就被动过。
pub fn untracked_numstat_args(git: &str, dir: &str, path: &str) -> Vec<String> {
    at(git, dir, &["diff", "--numstat", "--no-index", "--", "/dev/null", path])
}

/// 最多为多少个未跟踪文件算行数。
///
/// 新建一个装满文件的目录就能有几百个未跟踪文件，每个都要一条命令，
/// 命令行会被撑爆（Windows 上尤其紧）。超出的显示成"未知"，不是 0。
pub const UNTRACKED_NUMSTAT_CAP: usize = 80;

/// 「修改」面板最多列多少个文件
pub const WORKING_FILE_CAP: usize = 500;

// ---------------- 提交 ----------------

/// 把选中的未跟踪文件放进 index。已跟踪的不用——pathspec commit 直接取工作区
pub fn add_paths_args<S: AsRef<str>>(git: &str, dir: &str, paths: &[S]) -> Vec<String> {
    at(git, dir, &with_paths(&["add", "--"], paths))
}

/// 提交全部改动前的那一步。-A 含未跟踪文件与删除
pub fn add_all_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["add", "-A"])
}

/// [`commit_args`] 的选项（TS 的 `opts?: { amend?, noEdit? }`）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitOpts {
    pub amend: bool,
    /// 只在 amend 时有意义：沿用原提交信息
    pub no_edit: bool,
}

/// 提交。`paths` 为空等同 TS 的不传（`paths && paths.length > 0`）。
///
/// 带 pathspec 时 git **忽略 index**，直接拿这些路径的工作区内容成提交——
/// 实测过：index 里别人暂存的东西不会被顺带提交走，未选中的文件也不动。
/// 这正是面板要的语义（它显示的就是暂存+未暂存的合并视图）。
///
/// 不加 --no-verify：pre-commit 钩子是用户自己配的，面板没有资格跳过它。
/// 钩子可能很慢，所以调用方要给足超时（TIMEOUT_SYNC）。
pub fn commit_args<S: AsRef<str>>(git: &str, dir: &str, message: &str, paths: &[S], opts: CommitOpts) -> Vec<String> {
    let mut args = vec!["commit"];
    if opts.amend {
        args.push("--amend");
    }
    if opts.amend && opts.no_edit {
        args.push("--no-edit");
    } else {
        args.push("-m");
        args.push(message);
    }
    if !paths.is_empty() {
        args.push("--");
        args.extend(paths.iter().map(AsRef::as_ref));
    }
    at(git, dir, &args)
}

/// pathspec 拼进命令行的字符预算。
///
/// 卡得这么死是因为 Windows 远端：命令走 `powershell -EncodedCommand`，载荷先
/// 转 UTF-16LE 再 base64（长度 ×8/3），而 cmd.exe 的命令行上限是 8191。
/// 留出 git 参数与 env 前缀后，路径部分能用的也就两千出头。
///
/// 超了不是截断——截断会**悄悄少提交几个文件**，那是最糟的一种失败。
/// 服务端直接拒绝，让用户改用"全选"（走 add -A，不拼路径）或者分两次提交。
pub const COMMIT_PATHSPEC_BUDGET: usize = 2000;

pub fn pathspec_too_long<S: AsRef<str>>(paths: &[S]) -> bool {
    // +3 给引号与分隔符留的余量，宁可保守。长度按 UTF-16 码元（= TS 的 .length）
    paths.iter().map(|p| utf16_len(p.as_ref()) + 3).sum::<usize>() > COMMIT_PATHSPEC_BUDGET
}

/// Pull。--ff-only 是刻意的：能快进就快进，不能就停下报错。
///
/// 面板上一个按钮不该在用户看不见的地方造出合并提交，更不该把工作区搅成冲突
/// 状态——那之后所有会话里的 shell 都在一个半挂的仓库里干活。真要合并/变基，
/// 用户在终端里做，那是他清楚自己在做什么的地方。
pub fn pull_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["pull", "--ff-only"])
}

/// Push 当前分支到它的 upstream。
///
/// 不传 refspec 也不加 --set-upstream：没有 upstream 时 git 会直接报错并把
/// 该敲的命令印在 stderr 里，那比我们替他猜一个远程分支名要好。
/// 绝不加 --force——按钮点下去要么是安全的，要么就失败。
pub fn push_args(git: &str, dir: &str, force_with_lease: bool) -> Vec<String> {
    let mut args = vec!["push"];
    // --force-with-lease 不是 --force：远端若多了你没 fetch 到的提交会拒绝，
    // 不会把别人刚推上去的历史盖掉。面板上的「强制推送」只走这条。
    if force_with_lease {
        args.push("--force-with-lease");
    }
    at(git, dir, &args)
}

// ---------------- History 面板写操作 ----------------

/// Fetch 全部远程并剪掉已删的跟踪分支。不改工作区、不动 HEAD——所以可以
/// 做成工具条上的 icon，不必再确认。
///
/// `--all` 而不是只 fetch upstream：面板上能看见的远程分支都该能刷新，
/// 用户点 origin/foo 的徽标时不想还停在上周的 sha。
pub fn fetch_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["fetch", "--all", "--prune"])
}

/// 检出一次提交 / 一个标签，HEAD 进入游离。
///
/// 用 `--detach` 而不是光传 sha：意图写在 argv 里，git 不会在 rev 恰好
/// 也是分支名时（少见，但 `git checkout v1` 会优先匹配分支）悄悄切走。
/// 不加 `-f`：脏工作区让 git 自己拒绝，面板按钮不该丢掉未提交的改动。
pub fn checkout_detach_args(git: &str, dir: &str, rev: &str) -> Vec<String> {
    at(git, dir, &["checkout", "--detach", rev])
}

/// 检出一条本地分支。
///
/// **不能**写成 `checkout -- <branch>`：checkout 的 `--` 后面是 pathspec，
/// 会去工作区里找同名文件，而不是切分支。分支名以 `-` 开头的在进这里
/// 之前就被 is_safe_ref_name 挡掉了。
pub fn checkout_branch_args(git: &str, dir: &str, branch: &str) -> Vec<String> {
    at(git, dir, &["checkout", branch])
}

/// 从远程分支建本地跟踪分支并检出。本地还没有对应分支时才走这条。
///
/// `--track` 显式写上：即使用户关了 autoSetupMerge，点远程徽标的预期
/// 也是"这条本地分支对着那条远程推"，跟 IDEA 点 origin/x 的行为一致。
pub fn checkout_track_args(git: &str, dir: &str, local: &str, remote: &str) -> Vec<String> {
    at(git, dir, &["checkout", "-b", local, "--track", remote])
}

pub fn cherry_pick_args(git: &str, dir: &str, sha: &str) -> Vec<String> {
    at(git, dir, &["cherry-pick", sha])
}

/// 用一次新提交撤销指定提交。`--no-edit`：面板上没有提交信息编辑器，
/// 沿用 git 默认的 `Revert "…"` 标题。撞冲突不 abort——Falcon 没有
/// merge tool，终端就在旁边，git 的原文比我们替他善后更有用。
pub fn revert_args(git: &str, dir: &str, sha: &str) -> Vec<String> {
    at(git, dir, &["revert", "--no-edit", sha])
}

/// 只建分支，不检出。`--` 挡住以 `-` 开头的名字被当成选项（调用方也会先拒）。
pub fn branch_create_args(git: &str, dir: &str, name: &str, start_point: &str) -> Vec<String> {
    at(git, dir, &["branch", "--", name, start_point])
}

/// 建分支并立刻检出。与 branch_create_args 成对，对应对话框里「创建后检出」。
pub fn checkout_new_branch_args(git: &str, dir: &str, name: &str, start_point: &str) -> Vec<String> {
    at(git, dir, &["checkout", "-b", name, start_point])
}

/// 重置当前分支到某次提交。
///
/// hard 会丢掉工作区——面板上单独确认。不加 `--force` 之外的花样，
/// mixed / soft 也把 mode 写进 argv，避免默认值让人猜。
pub fn reset_args(git: &str, dir: &str, rev: &str, mode: GitResetMode) -> Vec<String> {
    at(git, dir, &["reset", &format!("--{}", mode.as_str()), rev])
}

/// 把 rev 合并进当前分支。`--no-edit`：面板没有提交信息编辑器。
/// 不加 `--ff-only`：用户点 Merge 就是要这次合并，快进不了就该造合并提交。
/// 冲突不 abort。
pub fn merge_args(git: &str, dir: &str, rev: &str) -> Vec<String> {
    at(git, dir, &["merge", "--no-edit", rev])
}

/// 把当前分支变基到 rev 之上。不加 `--force` / `-i`：交互式变基没有终端 UI
/// 可接。冲突不 abort，git 的原文告诉用户该在终端里怎么继续。
pub fn rebase_args(git: &str, dir: &str, rev: &str) -> Vec<String> {
    at(git, dir, &["rebase", rev])
}

/// 从当前分支拿掉 sha 这一笔：把它后面的提交接到它的第一父上。
///
/// 不用 `rebase -i`：那要一个能改 todo 文件的编辑器，POSIX / Windows /
/// SSH 各写一套太脆。`--onto <sha>~1 <sha>` 对线性历史就是 drop；
/// 合并提交 git 会自己拒绝，原文回给面板。
pub fn drop_commit_args(git: &str, dir: &str, sha: &str) -> Vec<String> {
    at(git, dir, &["rebase", "--onto", &format!("{sha}~1"), sha])
}

/// 丢掉当前 HEAD。空 todo 的 rebase --onto 不会移动 HEAD，必须走 reset。
pub fn drop_head_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["reset", "--hard", "HEAD~1"])
}

/// 把 HEAD 压进上一条：软重置留下工作区，amend 合成一次提交。
pub fn squash_head_soft_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["reset", "--soft", "HEAD~1"])
}

pub fn rev_exists_args(git: &str, dir: &str, rev: &str) -> Vec<String> {
    at(git, dir, &["rev-parse", "-q", "--verify", rev])
}

pub fn head_sha_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["rev-parse", "HEAD"])
}

/// TS 的三元链：merge / rebase / cherry-pick 各自对上，其余（含 Rust 侧的 `Unknown`）落到 revert
fn conflict_verb(kind: GitConflictKind) -> &'static str {
    match kind {
        GitConflictKind::Merge => "merge",
        GitConflictKind::Rebase => "rebase",
        GitConflictKind::CherryPick => "cherry-pick",
        GitConflictKind::Revert | GitConflictKind::Unknown => "revert",
    }
}

/// 冲突解决之后继续。`core.editor=true` 让 git 别打开 vim 等提交说明——
/// SSH exec 没有 tty，一弹编辑器请求就挂死。
pub fn continue_conflict_args(git: &str, dir: &str, kind: GitConflictKind) -> Vec<String> {
    at(git, dir, &["-c", "core.editor=true", conflict_verb(kind), "--continue"])
}

pub fn abort_conflict_args(git: &str, dir: &str, kind: GitConflictKind) -> Vec<String> {
    at(git, dir, &[conflict_verb(kind), "--abort"])
}

pub fn checkout_conflict_side_args<S: AsRef<str>>(git: &str, dir: &str, side: GitTakeSide, paths: &[S]) -> Vec<String> {
    let flag = format!("--{}", side.as_str());
    at(git, dir, &with_paths(&["checkout", &flag, "--"], paths))
}

/// 把已跟踪文件恢复成 HEAD。`--staged --worktree` 两边一起丢掉，
/// 跟面板「暂存+未暂存合着看」的视角一致。
pub fn restore_args<S: AsRef<str>>(git: &str, dir: &str, paths: &[S]) -> Vec<String> {
    at(git, dir, &with_paths(&["restore", "--source=HEAD", "--staged", "--worktree", "--"], paths))
}

/// 丢掉未跟踪文件。`-f` 是 clean 的硬要求（否则 git 拒绝），不是 checkout -f。
/// 不加 `-d`：工作区列表已经把未跟踪目录展开成文件（status --untracked-files=all）。
pub fn clean_paths_args<S: AsRef<str>>(git: &str, dir: &str, paths: &[S]) -> Vec<String> {
    at(git, dir, &with_paths(&["clean", "-f", "--"], paths))
}

/// 标签短名，新的在前。History 左侧树 Tags 一组用
pub fn tag_list_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["for-each-ref", "--format=%(refname:short)", "--sort=-creatordate", "refs/tags"])
}

/// `cap` 缺省 200。注意 TS 是先 push 再判 `>= cap`，所以 cap 为 0 时仍会留一条——照搬。
pub fn parse_tag_list(stdout: &str, cap: Option<usize>) -> Vec<String> {
    let cap = cap.unwrap_or(200);
    let mut out = Vec::new();
    for line in lines(stdout) {
        let name = js_trim(line);
        if name.is_empty() {
            continue;
        }
        out.push(name.to_string());
        if out.len() >= cap {
            break;
        }
    }
    out
}

/// HEAD 完整提交说明。空仓库时 git log 退出码 128，调用方当没有。
pub fn head_message_args(git: &str, dir: &str) -> Vec<String> {
    at(git, dir, &["log", "-1", "--format=%B"])
}

/// 完整 sha 或 git 允许的短 sha（TS 的 `GIT_SHA_RE = /^[0-9a-f]{4,40}$/i`）。
/// 面板点进来的都是 40 位，短的留给手填/测试。
pub fn is_git_sha(s: &str) -> bool {
    (4..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 能安全塞进 argv 当 ref 的名字。不是 git check-ref-format 的完整复刻，
/// 只挡会让命令行变义或被当成选项的那种：空、前导 `-`、控制字符、
/// git 的修订语法（`..` `~` `^` `:` `@{}`）和通配。
pub fn is_safe_ref_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('-') || name.ends_with('.') || name.ends_with('/') {
        return false;
    }
    if name.ends_with(".lock") {
        return false;
    }
    if name.contains("..") || name.contains("@{") {
        return false;
    }
    // /[\x00-\x20\x7f~^:?*\\[]/
    !name.chars().any(|c| c <= '\x20' || matches!(c, '\x7f' | '~' | '^' | ':' | '?' | '*' | '\\' | '['))
}

/// 项目上「默认 worktree 基点」的归一化。空 / 字面量 HEAD 都表示用当前 HEAD（`Ok(None)`）。
/// 形状不对或会改 argv 语义的名字返回 `Err`，让路由 400，而不是静默丢掉用户填的值。
///
/// `raw` 是请求体里的原值，`None` = 字段缺省（TS 的 undefined）。
pub fn parse_default_worktree_branch(raw: Option<&Value>) -> Result<Option<String>, String> {
    let s = match raw {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) if s.is_empty() => return Ok(None),
        Some(Value::String(s)) => js_trim(s),
        Some(_) => return Err("默认 worktree 基点必须是字符串".into()),
    };
    if s.is_empty() || s == "HEAD" {
        return Ok(None);
    }
    if !is_safe_ref_name(s) {
        return Err("默认 worktree 基点不合法".into());
    }
    Ok(Some(s.to_string()))
}

/// `origin/main` + remotes `["origin"]` → `"main"`。多个远程时取最长前缀，
/// 避免 `origin` 把 `origin-backup/x` 切错。对不上或切完不合法就 None。
pub fn tracking_local_name<S: AsRef<str>>(remote_branch: &str, remotes: &[S]) -> Option<String> {
    let mut names: Vec<&str> = remotes.iter().map(AsRef::as_ref).collect();
    // 稳定排序，按 UTF-16 长度降序（同 TS 的 sort((a, b) => b.length - a.length)）
    names.sort_by_key(|r| std::cmp::Reverse(utf16_len(r)));
    for remote in names {
        let Some(local) = remote_branch.strip_prefix(remote).and_then(|r| r.strip_prefix('/')) else {
            continue;
        };
        if is_safe_ref_name(local) {
            return Some(local.to_string());
        }
    }
    None
}

fn as_trimmed(v: Option<&Value>) -> Option<&str> {
    match v {
        Some(Value::String(s)) => Some(js_trim(s)).filter(|t| !t.is_empty()),
        _ => None,
    }
}

fn as_sha(v: Option<&Value>) -> Option<String> {
    as_trimmed(v).filter(|s| is_git_sha(s)).map(str::to_string)
}

fn as_ref_name(v: Option<&Value>) -> Option<String> {
    as_trimmed(v).filter(|s| is_safe_ref_name(s)).map(str::to_string)
}

fn as_rev(v: Option<&Value>) -> Option<String> {
    as_sha(v).or_else(|| as_ref_name(v))
}

/// TS 的 `o.x === true`
fn is_true(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Bool(true)))
}

/// 缺省 / null → 空列表；不是数组、或任一项不是非空字符串 / 以 `-` 开头 / 含 TAB 以外的
/// 控制字符（`/[\x00-\x08\x0a-\x1f]/`）→ `None`
fn as_paths(v: Option<&Value>) -> Option<Vec<String>> {
    let items = match v {
        None | Some(Value::Null) => return Some(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return None,
    };
    let mut out = Vec::with_capacity(items.len());
    for p in items {
        let Value::String(p) = p else { return None };
        if p.is_empty() || p.starts_with('-') || p.chars().any(|c| matches!(c, '\x00'..='\x08' | '\x0a'..='\x1f')) {
            return None;
        }
        out.push(p.clone());
    }
    Some(out)
}

/// POST /git/op 的 body。形状不对返回 `Err(说明)`，让路由回 400；
/// git 自己会失败的情况（脏工作区、分支占用）不在这里拦，留给 runGitOp。
///
/// 可选的布尔字段一律填 `Some(..)`：TS 版总是带上 `detach: o.detach === true` 这类字段，
/// 序列化出来的形状要一致。
pub fn parse_git_op_input(body: &Value) -> Result<GitOpInput, String> {
    let o = match body {
        Value::Object(o) => o,
        // typeof [] === "object"：数组过了第一道检查，取不到 op，落到"未知操作"
        Value::Array(_) => return Err("未知操作".into()),
        _ => return Err("缺少操作".into()),
    };
    let g = |k: &str| o.get(k);
    let op = g("op").and_then(Value::as_str).unwrap_or("");
    match op {
        "fetch" => Ok(GitOpInput::Fetch),
        "checkout" => {
            let rev = as_rev(g("rev")).ok_or("rev 不合法")?;
            Ok(GitOpInput::Checkout { rev, detach: Some(is_true(g("detach"))) })
        }
        "checkout-branch" => {
            let branch = as_ref_name(g("branch")).ok_or("branch 不合法")?;
            Ok(GitOpInput::CheckoutBranch { branch, create_tracking: Some(is_true(g("createTracking"))) })
        }
        "cherry-pick" | "revert" => {
            let sha = as_sha(g("sha")).ok_or("sha 参数不合法")?;
            Ok(if op == "cherry-pick" { GitOpInput::CherryPick { sha } } else { GitOpInput::Revert { sha } })
        }
        "branch-create" => {
            let name = as_ref_name(g("name"));
            let start_point = as_rev(g("startPoint"));
            let name = name.ok_or("分支名不合法")?;
            let start_point = start_point.ok_or("起点不合法")?;
            Ok(GitOpInput::BranchCreate { name, start_point, checkout: Some(is_true(g("checkout"))) })
        }
        "reset" => {
            let rev = as_rev(g("rev")).ok_or("rev 不合法")?;
            let mode = match g("mode").and_then(Value::as_str) {
                Some("soft") => GitResetMode::Soft,
                Some("mixed") => GitResetMode::Mixed,
                Some("hard") => GitResetMode::Hard,
                _ => return Err("reset mode 不合法".into()),
            };
            Ok(GitOpInput::Reset { rev, mode })
        }
        "merge" | "rebase" => {
            let rev = as_rev(g("rev")).ok_or("rev 不合法")?;
            Ok(if op == "merge" { GitOpInput::Merge { rev } } else { GitOpInput::Rebase { rev } })
        }
        "restore" => {
            let (Some(paths), Some(untracked)) = (as_paths(g("paths")), as_paths(g("untracked"))) else {
                return Err("路径不合法".into());
            };
            if paths.is_empty() && untracked.is_empty() {
                return Err("缺少路径".into());
            }
            if pathspec_too_long(&[paths.as_slice(), untracked.as_slice()].concat()) {
                return Err("选中的文件太多，路径拼不进一条命令行。少选几个再丢弃。".into());
            }
            Ok(GitOpInput::Restore { paths, untracked: Some(untracked) })
        }
        "push" => Ok(GitOpInput::Push { force_with_lease: Some(is_true(g("forceWithLease"))) }),
        "drop" | "squash" => {
            let sha = as_sha(g("sha")).ok_or("sha 参数不合法")?;
            Ok(if op == "drop" { GitOpInput::Drop { sha } } else { GitOpInput::Squash { sha } })
        }
        "reword" => {
            let sha = as_sha(g("sha"));
            let message = as_trimmed(g("message"));
            let sha = sha.ok_or("sha 参数不合法")?;
            let message = message.ok_or("缺少提交信息")?.to_string();
            Ok(GitOpInput::Reword { sha, message })
        }
        "continue" => Ok(GitOpInput::Continue),
        "abort" => Ok(GitOpInput::Abort),
        "take" => {
            let side = match g("side").and_then(Value::as_str) {
                Some("ours") => GitTakeSide::Ours,
                Some("theirs") => GitTakeSide::Theirs,
                _ => return Err("side 不合法".into()),
            };
            let paths = as_paths(g("paths")).filter(|p| !p.is_empty()).ok_or("缺少路径")?;
            if pathspec_too_long(&paths) {
                return Err("选中的文件太多，路径拼不进一条命令行。".into());
            }
            Ok(GitOpInput::Take { side, paths })
        }
        _ => Err("未知操作".into()),
    }
}

// ---------------- 命令行拼装 ----------------

/// 把 git argv 拼成一条可交给宿主机执行的命令行。
///
/// 就是 build_command_line，只是带上 GIT_UNSET；env 通常传 [`GIT_ENV`] 或 [`GIT_ENV_RO`]
/// （TS 的默认参数是 GIT_ENV）。
///
/// git 对底座有两条硬要求，都已经在 zellij/host.rs 里满足了，这里只记为什么必须有：
/// - **退出码要如实传出**：`diff --quiet` 的全部语义就在退出码里（1 = 有改动），
///   `rev-list @{upstream}..` 的 128 表示"没有 upstream"而不是出错。压成 0/1 会让
///   "有没有未提交改动"这类判断悄悄失真——见 powershell_script 的注释。
/// - **输出编码要钉成 UTF-8**：含中文的路径经 936 代码页解码后是乱码，而护栏靠路径
///   比对——见 encode_powershell 的注释。
pub fn build_git_command_line<A, K, V>(kind: HostKind, argv: &[A], env: &[(K, V)]) -> String
where
    A: AsRef<str>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    build_command_line(kind, argv, env, GIT_UNSET)
}

/// 探测一个路径是否存在。
///
/// 退出码恒为 0、答案在 stdout 里：非零码在 ExecFn 的约定里留给"命令根本没跑起来"，
/// 混用会让"路径不存在"和"SSH 断了"变成同一个信号。
///
/// 本地远端走同一条路径而不是本地直接查文件系统：占用检查的对象是**宿主机**的
/// 文件系统，开两条分叉只会让两边的语义悄悄漂移。
pub fn exists_command(kind: HostKind, p: &str) -> String {
    if kind == HostKind::Windows {
        // -LiteralPath：-Path 会做通配符展开，路径里出现一个 [ 就足以让它什么都匹配不到
        return encode_powershell(&format!(
            "if (Test-Path -LiteralPath {}) {{ 'yes' }} else {{ 'no' }}",
            quote_powershell(p)
        ));
    }
    format!("if [ -e {} ]; then printf yes; else printf no; fi", quote_posix(p))
}

/// 一次往返探测多个路径是否存在，输出与入参同序的 yes / no 行。
///
/// 分支列表动辄几十条，每条都单发一次 exec 就是几十个 SSH 往返——一个下拉框不值这个价。
/// 命令行长度有上限（Windows 上尤其紧），所以调用方要自己分片，见 [`EXISTS_BATCH`]。
pub fn exists_many_command<S: AsRef<str>>(kind: HostKind, paths: &[S]) -> String {
    if kind == HostKind::Windows {
        let arr = paths.iter().map(|p| quote_powershell(p.as_ref())).collect::<Vec<_>>().join(",");
        return encode_powershell(&format!(
            "@({arr}) | ForEach-Object {{ if (Test-Path -LiteralPath $_) {{ 'yes' }} else {{ 'no' }} }}"
        ));
    }
    let arr = paths.iter().map(|p| quote_posix(p.as_ref())).collect::<Vec<_>>().join(" ");
    format!("for p in {arr}; do if [ -e \"$p\" ]; then echo yes; else echo no; fi; done")
}

/// 单批最多探测多少条路径。超出的分片发送，别把命令行撑爆。
pub const EXISTS_BATCH: usize = 60;

// ---------------- 批量 git ----------------

/// 哨兵行前缀。git 的输出不可能撞上它：porcelain / rev-parse / log 的每种
/// 格式都不会产出这种行；就算路径里被人恶意塞进这个串，status 行有 "XY " 前缀、
/// diff 行有 +/- 前缀，都不会整行等于哨兵。
const BATCH_MARK: &str = "__FALCON_GIT_";

/// 把多条 git 命令拼成**一次** exec：每条命令后打一行 `__FALCON_GIT_<i>_<code>__`
/// 哨兵，带序号与真实退出码。SSH 上一条 exec 就是一次 channel open/close 往返，
/// Git 面板一轮快照要跑十来条命令，逐条发就是十来个往返——与 exists_many_command
/// 是同一笔账。
///
/// 退出码的取法两边不同但语义一致：POSIX 直接 `$?`；PowerShell 沿用
/// powershell_script 的预置哨兵 127（命令没跑起来时 $LASTEXITCODE 不会被赋值，
/// 见那边的注释），跑起来了就被真实退出码覆盖。
pub fn batch_git_command_line<A, K, V>(kind: HostKind, argvs: &[Vec<A>], env: &[(K, V)]) -> String
where
    A: AsRef<str>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    if kind == HostKind::Windows {
        let mut lines: Vec<String> = GIT_UNSET.iter().map(|k| format!("$env:{k} = $null")).collect();
        lines.extend(env.iter().map(|(k, v)| format!("$env:{} = {}", k.as_ref(), quote_powershell(v.as_ref()))));
        for (i, argv) in argvs.iter().enumerate() {
            lines.push("$LASTEXITCODE = 127".into());
            // 空 argv 在 TS 里会在 quotePowerShell(undefined) 上抛；调用方从不传空的，
            // 这里与 powershell_script 一样留一条空语句，不 panic
            let call: Vec<String> = argv
                .iter()
                .enumerate()
                .map(|(j, a)| if j == 0 { format!("& {}", quote_powershell(a.as_ref())) } else { quote_powershell(a.as_ref()) })
                .collect();
            lines.push(call.join(" "));
            // [char]10 前导换行：命令输出不带结尾换行时，哨兵不能黏在同一行上
            lines.push(format!("Write-Output ([char]10 + '{BATCH_MARK}{i}_' + $LASTEXITCODE + '__')"));
        }
        return encode_powershell(&lines.join("; "));
    }
    let mut parts = vec![format!("unset {}", GIT_UNSET.join(" "))];
    parts.extend(env.iter().map(|(k, v)| format!("export {}={}", k.as_ref(), quote_posix(v.as_ref()))));
    for (i, argv) in argvs.iter().enumerate() {
        parts.push(argv.iter().map(|a| quote_posix(a.as_ref())).collect::<Vec<_>>().join(" "));
        // 前导 \n 同 PowerShell 侧；$? 在 printf 求值时仍指向上一条命令
        parts.push(format!("printf '\\n{BATCH_MARK}{i}_%s__\\n' \"$?\""));
    }
    parts.join("; ")
}

/// 批量执行里一条命令的结果
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchGitResult {
    /// None = 没找到这条命令的哨兵（整批被掐断 / 环境异常）
    pub code: Option<i32>,
    pub stdout: String,
}

/// 按哨兵切分整批输出。哨兵缺失的命令按 code: None 处理，调用方视同失败。
pub fn parse_git_batch(stdout: &str, count: usize) -> Vec<BatchGitResult> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!("^{BATCH_MARK}([0-9]+)_([0-9]+)__$")).expect("batch sentinel regex"));
    let mut out = vec![BatchGitResult::default(); count];
    let mut cur: Vec<&str> = Vec::new();
    for line in lines(stdout) {
        let Some(m) = RE.captures(js_trim(line)) else {
            cur.push(line);
            continue;
        };
        // 序号超出 usize 的数字串必然不在 [0, count) 里；退出码超出 i32（实际不会出现）
        // 记成 None，调用方同样视作失败
        if let Ok(i) = m[1].parse::<usize>()
            && i < count
        {
            out[i] = BatchGitResult { code: m[2].parse().ok(), stdout: cur.join("\n") };
        }
        cur.clear();
    }
    out
}

pub fn parse_exists_many(stdout: &str) -> Vec<bool> {
    lines(stdout).map(js_trim).filter(|l| *l == "yes" || *l == "no").map(|l| l == "yes").collect()
}

/// 读 worktree 目录里的 .git **文件**头部。
///
/// linked worktree 的 .git 是个文件（内容形如 `gitdir: /repo/.git/worktrees/x`），
/// 普通仓库那里是目录，普通目录根本没有。删除护栏拿它当降级证据——最常见的破损场景
/// 是"用户把整个源仓库删了"，此时 worktree list 永远失败，但目录本身仍该被清理。
pub fn git_file_head_command(kind: HostKind, dir: &str) -> String {
    let f = format!("{dir}{}.git", sep_for(kind));
    if kind == HostKind::Windows {
        let q = quote_powershell(&f);
        return encode_powershell(&format!(
            "if (Test-Path -LiteralPath {q} -PathType Leaf) {{ Get-Content -LiteralPath {q} -TotalCount 1 }}"
        ));
    }
    let q = quote_posix(&f);
    format!("if [ -f {q} ]; then head -c 200 {q}; fi")
}

// ---------------- 输出解析 ----------------
//
// 一律按 /\r?\n/ 切行：Windows 远端上 PowerShell 用 \r\n 拼接输出。

/// `stdout.split(/\r?\n/)`：每个 `\n` 切一刀，切出来的段去掉**一个**行尾 `\r`。
/// 空串切出一个空段（同 JS）。
fn lines(stdout: &str) -> impl Iterator<Item = &str> {
    stdout.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l))
}

/// `git worktree list --porcelain` 的一条记录
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitWorktreeEntry {
    pub path: String,
    pub head: String,
    /// refs/heads/x 去前缀；detached 时为 None
    pub branch: Option<String>,
    pub bare: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// 解析 `git worktree list --porcelain`。
///
/// 格式：记录之间空行分隔，每条含 `worktree <path>` 与 `HEAD <sha>`，外加
/// `branch refs/heads/<n>` | `detached` 之一，可能还有 `bare` / `locked [原因]` /
/// `prunable [原因]`。第一条永远是主 worktree。
///
/// 不用 -z：NUL 分隔要穿过 powershell -EncodedCommand 的输出管线，编码行为不可靠。
/// 改用默认格式 + 自己处理 C 风格引号——十行代码，换掉一整类平台不确定性。
pub fn parse_worktree_list(stdout: &str) -> Vec<GitWorktreeEntry> {
    let mut out = Vec::new();
    let mut cur: Option<GitWorktreeEntry> = None;
    // TS 的 flush：path 为空的记录不要
    let flush = |cur: &mut Option<GitWorktreeEntry>, out: &mut Vec<GitWorktreeEntry>| {
        if let Some(e) = cur.take().filter(|e| !e.path.is_empty()) {
            out.push(e);
        }
    };
    for line in lines(stdout) {
        if js_trim(line).is_empty() {
            flush(&mut cur, &mut out);
            continue;
        }
        let (key, val) = match line.find(' ') {
            None => (js_trim(line), ""),
            Some(sp) => (&line[..sp], js_trim(&line[sp + 1..])),
        };
        if key == "worktree" {
            flush(&mut cur, &mut out);
            cur = Some(GitWorktreeEntry { path: unquote_c_path(val), ..Default::default() });
            continue;
        }
        let Some(e) = cur.as_mut() else { continue };
        match key {
            "HEAD" => e.head = val.to_string(),
            "branch" => e.branch = Some(val.strip_prefix("refs/heads/").unwrap_or(val).to_string()),
            "bare" => e.bare = true,
            "locked" => e.locked = true,
            "prunable" => e.prunable = true,
            _ => {}
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// git 的 C 风格路径引号：整体加双引号，内部 \\ \" \t \n 与 \NNN（八进制）。
/// 我们已经加了 core.quotepath=false，但含双引号或控制字符的路径仍会被引起来。
///
/// 逐 UTF-16 码元照搬 TS：`\NNN` 按 `String.fromCharCode(parseInt(..., 8))` 变成**一个码元**
/// （不按 UTF-8 字节重组），八进制分支无论实际吃到几位数字都跳过 3 个码元。TS 里可能留下的
/// 孤立代理在这里变成 U+FFFD。
pub fn unquote_c_path(s: &str) -> String {
    if !s.starts_with('"') {
        return s.to_string();
    }
    let u: Vec<u16> = s.encode_utf16().collect();
    // s.slice(1, s.endsWith('"') ? -1 : undefined)：长度 1 的 `"` 切出空串
    let end = if s.ends_with('"') { u.len() - 1 } else { u.len() };
    let body: &[u16] = if end > 1 { &u[1..end] } else { &[] };
    let unit = |c: char| c as u16;
    let mut out: Vec<u16> = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if body[i] != unit('\\') {
            out.push(body[i]);
            i += 1;
            continue;
        }
        i += 1;
        match body.get(i).copied() {
            Some(c) if c == unit('n') => out.push(unit('\n')),
            Some(c) if c == unit('t') => out.push(unit('\t')),
            Some(c) if c == unit('r') => out.push(unit('\r')),
            Some(c) if (unit('0')..=unit('7')).contains(&c) => {
                // parseInt(body.slice(i, i + 3), 8)：取开头连续的八进制数字，首位已保证是
                let digits = &body[i..(i + 3).min(body.len())];
                let v = digits
                    .iter()
                    .take_while(|&&d| (unit('0')..=unit('7')).contains(&d))
                    .fold(0u16, |acc, &d| acc * 8 + (d - unit('0')));
                out.push(v);
                i += 2;
            }
            Some(c) => out.push(c),
            None => {}
        }
        i += 1;
    }
    String::from_utf16_lossy(&out)
}

/// `for-each-ref` 解析出的分支（TS 的 `ParsedBranch`），与协议里的 GitBranchRef 同形
pub type ParsedBranch = GitBranchRef;

fn is_remote_name<S: AsRef<str>>(name: &str, remotes: &[S]) -> bool {
    remotes.iter().any(|r| {
        let r = r.as_ref();
        name == r || name.strip_prefix(r).is_some_and(|rest| rest.starts_with('/'))
    })
}

pub fn parse_branch_list<S: AsRef<str>>(stdout: &str, remotes: &[S]) -> Vec<ParsedBranch> {
    let mut res = Vec::new();
    for line in lines(stdout) {
        if js_trim(line).is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let name = cols.next().unwrap_or("");
        let upstream = cols.next().unwrap_or("");
        let head = cols.next().unwrap_or("");
        let symref = cols.next().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        // 指向默认分支的 symref（origin/HEAD）不是可检出的目标，列出来只会误导。
        // 它跟自己指向的那条分支永远同时出现，丢掉不会少任何一个选项
        if !symref.is_empty() {
            continue;
        }
        res.push(GitBranchRef {
            name: name.to_string(),
            remote: is_remote_name(name, remotes),
            head: head == "*",
            upstream: Some(upstream).filter(|u| !u.is_empty()).map(str::to_string),
        });
    }
    res
}

/// [`parse_status`] 的结果
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusSummary {
    pub count: usize,
    pub files: Vec<String>,
}

/// git status --porcelain 的行数与前若干条路径（展示用，读不到不影响判断）。`sample` 缺省 8
pub fn parse_status(stdout: &str, sample: Option<usize>) -> StatusSummary {
    let entries = parse_status_entries(stdout);
    let files = entries.iter().take(sample.unwrap_or(8)).map(|e| e.path.clone()).collect();
    StatusSummary { count: entries.len(), files }
}

/// porcelain status 的一条（TS 的 `GitStatusEntry`），与协议里的 GitFileChange 同形
pub type GitStatusEntry = GitFileChange;

/// porcelain 未合并：U 出现在任一列，或双方都新增 / 都删除
pub fn is_unmerged_status(index: &str, work: &str) -> bool {
    index == "U" || work == "U" || (index == "A" && work == "A") || (index == "D" && work == "D")
}

/// [`count_status_changes`] 的结果
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusCounts {
    pub added: u32,
    pub deleted: u32,
}

/// 侧栏 +N −M：文件还在的改动算 added，工作树里消失的算 deleted。
///
/// 删了又加回来（AD）或重命名（R/C）不算删除——人看见的是一个还在的文件。
pub fn count_status_changes(entries: &[GitStatusEntry]) -> StatusCounts {
    let mut c = StatusCounts::default();
    for e in entries {
        if is_status_deleted(e) {
            c.deleted += 1;
        } else {
            c.added += 1;
        }
    }
    c
}

fn is_status_deleted(e: &GitStatusEntry) -> bool {
    if e.index != "D" && e.work != "D" {
        return false;
    }
    !["A", "R", "C"].iter().any(|s| e.index == *s || e.work == *s)
}

/// 完整解析 `git status --porcelain`。
///
/// 前两列永远是 XY，第三列是空格，后面才是路径。重命名 / 复制是
/// `XY orig -> new`；只有 XY 里真有 R/C 才按箭头拆，免得路径里刚好有 ` -> `。
///
/// 前三列按 UTF-16 码元取（同 TS 的 `line[0]` / `line[1]` / `slice(3)`）；porcelain 的
/// 这三列永远是 ASCII，走快路径。
pub fn parse_status_entries(stdout: &str) -> Vec<GitStatusEntry> {
    let mut out = Vec::new();
    for line in lines(stdout) {
        let (index, work, tail) = if line.len() >= 3 && line.as_bytes()[..3].is_ascii() {
            (line[0..1].to_string(), line[1..2].to_string(), line[3..].to_string())
        } else {
            let u: Vec<u16> = line.encode_utf16().collect();
            if u.len() < 3 {
                continue;
            }
            (String::from_utf16_lossy(&u[0..1]), String::from_utf16_lossy(&u[1..2]), String::from_utf16_lossy(&u[3..]))
        };
        let rest = unquote_c_path(js_trim(&tail));
        if rest.is_empty() {
            continue;
        }
        let renamed = ["R", "C"].iter().any(|s| index == *s || work == *s);
        let arrow = if renamed { rest.find(" -> ") } else { None };
        match arrow {
            Some(a) => out.push(GitFileChange {
                path: unquote_c_path(js_trim(&rest[a + 4..])),
                orig_path: Some(unquote_c_path(js_trim(&rest[..a]))),
                index,
                work,
            }),
            None => out.push(GitFileChange { path: rest, orig_path: None, index, work }),
        }
    }
    out
}

/// `git remote -v`：每个 remote 只取 fetch 那一行
pub fn parse_remotes(stdout: &str) -> Vec<GitRemote> {
    // /^(\S+)\s+(\S+)\s+\((fetch|push)\)/，\s 用 JS 的空白集合
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        let ws = JS_WS_CLASS;
        Regex::new(&format!(r"^([^{ws}]+)[{ws}]+([^{ws}]+)[{ws}]+\((fetch|push)\)")).expect("remote -v regex")
    });
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in lines(stdout) {
        let Some(m) = RE.captures(js_trim(line)) else { continue };
        if &m[3] != "fetch" || !seen.insert(m[1].to_string()) {
            continue;
        }
        out.push(GitRemote { name: m[1].to_string(), url: m[2].to_string() });
    }
    out
}

/// `git log` 的一条（TS 的 `GitLogEntry`），与协议里的 GitCommit 同形
pub type GitLogEntry = GitCommit;

/// unix 秒（字符串）→ 毫秒。`Number.isFinite(sec) ? sec * 1000 : 0`
fn sec_to_ms(s: &str) -> i64 {
    let sec = js_string_to_number(s);
    if sec.is_finite() { (sec * 1000.0) as i64 } else { 0 }
}

/// `git log --format=%h\t%an\t%at\t%s`。subject 里可能有 TAB，只切前三列。
pub fn parse_log(stdout: &str) -> Vec<GitLogEntry> {
    let mut out = Vec::new();
    for line in lines(stdout) {
        if line.is_empty() {
            continue;
        }
        let mut cols = line.splitn(4, '\t');
        let sha = cols.next().unwrap_or("");
        let author = cols.next().unwrap_or("");
        let at = cols.next().unwrap_or("");
        let subject = cols.next().unwrap_or("");
        if sha.is_empty() || author.is_empty() || at.is_empty() {
            continue;
        }
        out.push(GitCommit {
            sha: sha.to_string(),
            author: author.to_string(),
            authored_at: sec_to_ms(at),
            subject: subject.to_string(),
        });
    }
    out
}

// ---------------- History 面板的解析 ----------------

/// 解析 %D（decoration）。
///
/// 形如 `HEAD -> main, origin/main, tag: v1.0, origin/HEAD`。
/// - `HEAD -> x` 里的 x 是当前分支，标 head: Some(true)；
/// - 光杆 `HEAD`（detached）不是 ref，丢掉；
/// - `origin/HEAD` 是指向默认分支的 symref，与 parse_branch_list 同一个理由丢掉——
///   它跟 origin/main 永远同时出现在同一条提交上，列出来就是重复一格。
pub fn parse_ref_labels<S: AsRef<str>>(decoration: &str, remotes: &[S]) -> Vec<GitRefLabel> {
    let mut out = Vec::new();
    for raw in decoration.split(',') {
        let mut name = js_trim(raw);
        if name.is_empty() {
            continue;
        }
        if let Some(tag) = name.strip_prefix("tag: ") {
            out.push(GitRefLabel { name: js_trim(tag).to_string(), kind: GitRefKind::Tag, head: None });
            continue;
        }
        let mut head = false;
        if let Some(arrow) = name.find(" -> ") {
            // 左边必然是字面量 HEAD，右边才是分支名
            name = js_trim(&name[arrow + 4..]);
            head = true;
        }
        if name.is_empty() || name == "HEAD" {
            continue;
        }
        let remote = is_remote_name(name, remotes);
        if remote && name.ends_with("/HEAD") {
            continue;
        }
        out.push(GitRefLabel {
            name: name.to_string(),
            kind: if remote { GitRefKind::Remote } else { GitRefKind::Local },
            head: head.then_some(true),
        });
    }
    out
}

/// [`log_page_args`] 输出的一行
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedLogCommit {
    pub sha: String,
    pub short: String,
    pub author: String,
    pub author_email: String,
    /// unix 毫秒
    pub authored_at: i64,
    pub subject: String,
    pub parents: Vec<String>,
    pub decoration: String,
}

/// `(parents ?? "").split(" ").filter(Boolean)`
fn split_parents(s: &str) -> Vec<String> {
    s.split(' ').filter(|p| !p.is_empty()).map(str::to_string).collect()
}

/// 解析 log_page_args 的输出。列不齐的行直接丢——宁可少一条也不要画错的图
pub fn parse_log_page(stdout: &str) -> Vec<ParsedLogCommit> {
    let mut out = Vec::new();
    for line in lines(stdout) {
        if js_trim(line).is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 8 {
            continue;
        }
        let (sha, short) = (cols[0], cols[1]);
        if sha.is_empty() || short.is_empty() {
            continue;
        }
        out.push(ParsedLogCommit {
            sha: sha.to_string(),
            short: short.to_string(),
            author: cols[2].to_string(),
            author_email: cols[3].to_string(),
            authored_at: sec_to_ms(cols[4]),
            parents: split_parents(cols[5]),
            decoration: cols[6].to_string(),
            // subject 是最后一列，里面的 TAB 要原样拼回去
            subject: cols[7..].join("\t"),
        });
    }
    out
}

/// 作者采样：按出现次数降序，同次数按首次出现的顺序（log 是新→旧，即最近活跃优先）
pub fn rank_authors(stdout: &str) -> Vec<String> {
    // 插入顺序 = 首次出现顺序（TS 的 Map），稳定排序保住同次数时的先后
    let mut order: Vec<(&str, usize)> = Vec::new();
    let mut idx: HashMap<&str, usize> = HashMap::new();
    for line in lines(stdout) {
        let name = js_trim(line);
        if name.is_empty() {
            continue;
        }
        match idx.get(name) {
            Some(&i) => order[i].1 += 1,
            None => {
                idx.insert(name, order.len());
                order.push((name, 1));
            }
        }
    }
    order.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    order.into_iter().map(|(n, _)| n.to_string()).collect()
}

/// [`commit_meta_args`] 的一行输出
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCommitMeta {
    pub sha: String,
    pub short: String,
    pub author: String,
    pub author_email: String,
    /// unix 毫秒
    pub authored_at: i64,
    pub committer: String,
    /// unix 毫秒
    pub committed_at: i64,
    pub parents: Vec<String>,
    pub decoration: String,
}

/// 解析 commit_meta_args 的一行输出。列不齐返回 None，调用方当"读不到这条提交"
pub fn parse_commit_meta(stdout: &str) -> Option<ParsedCommitMeta> {
    let line = lines(stdout).find(|l| !js_trim(l).is_empty())?;
    let cols: Vec<&str> = line.split('\t').collect();
    if cols.len() < 8 {
        return None;
    }
    let (sha, short) = (cols[0], cols[1]);
    if sha.is_empty() || short.is_empty() {
        return None;
    }
    Some(ParsedCommitMeta {
        sha: sha.to_string(),
        short: short.to_string(),
        author: cols[2].to_string(),
        author_email: cols[3].to_string(),
        authored_at: sec_to_ms(cols[4]),
        committer: cols[5].to_string(),
        committed_at: sec_to_ms(cols[6]),
        parents: split_parents(cols[7]),
        decoration: cols.get(8).copied().unwrap_or("").to_string(),
    })
}

/// 提交里改动的一个文件（TS 的 `ParsedCommitFile`），与协议里的 GitCommitFile 同形
pub type ParsedCommitFile = GitCommitFile;

/// numstat 的一列：二进制文件是 `-`（记 None，不是 0——"改了但不知道多少行"和
/// "一行没改"在界面上是两回事）。
///
/// TS 是 `Number.isFinite(Number(s)) ? n : null`；协议字段是 u64，负数 / 小数在这里
/// 也记 None（git 不会输出它们）。
fn numstat_col(s: Option<&str>) -> Option<u64> {
    let s = s.filter(|s| !s.is_empty() && *s != "-")?;
    let n = js_string_to_number(s);
    (n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= u64::MAX as f64).then_some(n as u64)
}

/// 解析 commit_files_args 的两段输出。
///
/// raw 段每行以 `:` 开头：`:<旧模式> <新模式> <旧sha> <新sha> <状态>\t<路径>[\t<新路径>]`。
/// 状态可能带相似度数字（R100 / C85），只取首字母。
///
/// numstat 段是 `<增>\t<删>\t<路径>`，二进制文件两列都是 `-`（记 None，不是 0——
/// "改了但不知道多少行"和"一行没改"在界面上是两回事）。
///
/// 两段的文件顺序由 git 的同一个 diff queue 决定，一致，所以按下标配对。
/// 条数对不上（不该发生，但输出被 profile 之类污染过就会）时宁可丢掉全部数字，
/// 也不要把 A 文件的行数记到 B 文件头上。
pub fn parse_commit_files(stdout: &str) -> Vec<ParsedCommitFile> {
    let mut raw: Vec<GitCommitFile> = Vec::new();
    let mut nums: Vec<(Option<u64>, Option<u64>)> = Vec::new();
    for line in lines(stdout) {
        if js_trim(line).is_empty() {
            continue;
        }
        if line.starts_with(':') {
            // 状态与路径之间是 TAB，前面那截模式/sha 用空格分隔
            let Some(tab) = line.find('\t') else { continue };
            // head.trim().split(/\s+/) 的最后一段，再 charAt(0)
            let last = js_trim(&line[..tab]).rsplit(is_js_whitespace).next().unwrap_or("");
            let status = char_at0(last);
            let paths: Vec<String> =
                line[tab + 1..].split('\t').map(|p| unquote_c_path(js_trim(p))).filter(|p| !p.is_empty()).collect();
            if status.is_empty() || paths.is_empty() {
                continue;
            }
            let mut paths = paths.into_iter();
            let first = paths.next().unwrap_or_default();
            // R / C 有两个路径：先旧后新
            let (path, orig_path) = match paths.next() {
                Some(second) => (second, Some(first)),
                None => (first, None),
            };
            raw.push(GitCommitFile { path, orig_path, status, added: None, deleted: None });
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 3 {
            continue;
        }
        nums.push((numstat_col(cols.first().copied()), numstat_col(cols.get(1).copied())));
    }
    let aligned = nums.len() == raw.len();
    raw.into_iter()
        .enumerate()
        .map(|(i, mut f)| {
            if aligned {
                (f.added, f.deleted) = nums[i];
            }
            f
        })
        .collect()
}

/// 提交详情里最多列多少个文件。改了几千个文件的提交不该把面板撑爆
pub const COMMIT_FILE_CAP: usize = 300;

/// numstat 的一行：增删行数，二进制 / 读不出时为 None
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Numstat {
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

/// 解析 `diff --numstat`：`<增>\t<删>\t<路径>`，按路径索引。
///
/// 重命名在这里是紧凑形式（`dir/{old => new}/f`），解析回两个路径太脆，
/// 而调用方手上已经有 porcelain 给的准确新旧路径——所以这里只认能直接
/// 用的那些，配不上的按"未知行数"处理，见 parse_numstat_map 的调用点。
///
/// 用 IndexMap 而不是 HashMap：TS 的 Map 保留插入顺序（同一路径再次出现时值被覆盖、
/// 位置不变），调用方会取"第一条"（未跟踪文件一条命令只有一个结果）。
pub fn parse_numstat_map(stdout: &str) -> IndexMap<String, Numstat> {
    let mut out = IndexMap::new();
    for line in lines(stdout) {
        if js_trim(line).is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 3 {
            continue;
        }
        // 路径可能含 TAB，后面的列拼回去
        let path = unquote_c_path(js_trim(&cols[2..].join("\t")));
        if !path.is_empty() {
            out.insert(path, Numstat { added: numstat_col(Some(cols[0])), deleted: numstat_col(Some(cols[1])) });
        }
    }
    out
}

/// `status --porcelain --ignored=matching` 里 `!!` 开头的那些
pub fn count_ignored(stdout: &str) -> usize {
    lines(stdout).filter(|l| l.starts_with("!!")).count()
}

/// diff 文本的上限。锁文件之类的 diff 可以到几十 MB，浏览器不该收这么多
pub const DIFF_CAP: usize = 500_000;

/// [`truncate_diff`] 的结果
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TruncatedDiff {
    pub text: String,
    pub truncated: bool,
}

/// 超限时在行边界截断，别把最后一行剪成半句。`cap` 缺省 [`DIFF_CAP`]，按 UTF-16 码元计
/// （同 TS 的 `.length` / `slice`）；没有换行可退时原样切，切在代理对中间的那半个字符
/// 变成 U+FFFD（TS 里是孤立代理，序列化时同样会被替换）。
pub fn truncate_diff(text: &str, cap: Option<usize>) -> TruncatedDiff {
    let cap = cap.unwrap_or(DIFF_CAP);
    // UTF-16 码元数 ≤ UTF-8 字节数：字节数没超就一定没超，省掉大 diff 的转码
    if text.len() <= cap || utf16_len(text) <= cap {
        return TruncatedDiff { text: text.to_string(), truncated: false };
    }
    let cut: Vec<u16> = text.encode_utf16().take(cap).collect();
    let nl = cut.iter().rposition(|&c| c == u16::from(b'\n'));
    let kept = match nl {
        Some(n) if n > 0 => &cut[..=n],
        _ => &cut[..],
    };
    TruncatedDiff { text: String::from_utf16_lossy(kept), truncated: true }
}

// ---------------- JS 语义辅助 ----------------
//
// 与 falcon-core / falcon-theme 的 `js.rs` 同源（那两份是各自 crate 的私有模块，用不到）。

/// ECMAScript 的 WhiteSpace + LineTerminator：`trim()` 与正则 `\s` 用的就是这一套。
/// 与 `char::is_whitespace` 的差别：含 U+FEFF、不含 U+0085。
pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// 同一套空白写成正则字符类的内容（用在 `[...]` / `[^...]` 里）
const JS_WS_CLASS: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// `String.prototype.trim`
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// `s.length`：UTF-16 码元数
fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `s.charAt(0)`：第一个 UTF-16 码元。非 BMP 字符只取到半个代理，Rust 表示不了，记 U+FFFD
fn char_at0(s: &str) -> String {
    match s.chars().next() {
        None => String::new(),
        Some(c) if c.len_utf16() == 1 => c.to_string(),
        Some(_) => '\u{FFFD}'.to_string(),
    }
}

/// `Number(string)`（ECMAScript StringToNumber）：首尾空白、空串为 0、十六进制 / 八进制 /
/// 二进制前缀、`Infinity`；`str::parse::<f64>` 这些一样都不认，反而认 `inf` / `nan`。
fn js_string_to_number(s: &str) -> f64 {
    let t = js_trim(s);
    if t.is_empty() {
        return 0.0;
    }
    let bytes = t.as_bytes();
    if bytes.len() > 2 && bytes[0] == b'0' {
        let radix = match bytes[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if radix != 0 {
            let mut v = 0.0f64;
            for c in t[2..].chars() {
                match c.to_digit(radix) {
                    Some(d) => v = v * f64::from(radix) + f64::from(d),
                    None => return f64::NAN,
                }
            }
            return v;
        }
    }
    let (neg, body) = match bytes[0] {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    let v = if body == "Infinity" {
        f64::INFINITY
    } else if is_decimal_literal(body) {
        body.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        return f64::NAN;
    };
    if neg { -v } else { v }
}

/// StrUnsignedDecimalLiteral：`digits [. digits] [e[+-]digits]`，小数点两边至少一边有数字
fn is_decimal_literal(s: &str) -> bool {
    let b = s.as_bytes();
    let int_digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    let mut i = int_digits;
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        frac_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        i += frac_digits;
    }
    if int_digits == 0 && frac_digits == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let exp_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if exp_digits == 0 {
            return false;
        }
        i += exp_digits;
    }
    i == b.len()
}

/// 用例与 `packages/server/src/git/command.test.ts` 一一对应：每个 `describe` 一个子模块，
/// 每个 `it` 一个测试，输入与期望值照抄。末尾的 `ts_vectors` 是额外的对拍：期望值是
/// 用 tsx 跑 TS 实现当场落下来的，钉住 TS 测试没覆盖到的命令串与边界行为。
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `argv.slice(-n)`
    fn tail(v: &[String], n: usize) -> Vec<&str> {
        v[v.len().saturating_sub(n)..].iter().map(String::as_str).collect()
    }

    /// `argv.includes(s)`
    fn has(v: &[String], s: &str) -> bool {
        v.iter().any(|a| a == s)
    }

    /// `argv[argv.indexOf(flag) + 1]`
    fn after<'a>(v: &'a [String], flag: &str) -> &'a str {
        let i = v.iter().position(|a| a == flag).unwrap_or_else(|| panic!("{flag} not in {v:?}"));
        &v[i + 1]
    }

    fn change(path: &str, orig: Option<&str>, index: &str, work: &str) -> GitFileChange {
        GitFileChange { path: path.into(), orig_path: orig.map(Into::into), index: index.into(), work: work.into() }
    }

    const NO_PATHS: &[&str] = &[];

    mod ls_files_index_args {
        use super::*;

        #[test]
        fn lists_cached_plus_others_excludes_gitignore_stays_relative_to_c() {
            let argv = ls_files_index_args("/usr/bin/git", "/home/u/repo");
            assert_eq!(tail(&argv, 3), ["ls-files", "-co", "--exclude-standard"]);
            assert_eq!(after(&argv, "-C"), "/home/u/repo");
        }
    }

    mod parse_ls_files {
        use super::*;

        #[test]
        fn drops_empty_lines_and_normalizes_backslashes() {
            assert_eq!(
                parse_ls_files("src/a.ts\r\n\nREADME.md\npackages\\web\\x.ts\n"),
                ["src/a.ts", "README.md", "packages/web/x.ts"]
            );
        }
    }

    mod count_status_changes {
        use super::*;

        #[test]
        fn counts_new_and_untracked_files_as_added_deletions_as_deleted() {
            let entries = parse_status_entries(&["A  added.ts", "?? scratch.ts", " D gone.ts", "D  staged-gone.ts"].join("\n"));
            assert_eq!(count_status_changes(&entries), StatusCounts { added: 2, deleted: 2 });
        }

        #[test]
        fn counts_modified_and_renamed_files_as_added_they_are_still_there() {
            let entries = parse_status_entries(&[" M dirty.ts", "M  staged.ts", "MM both.ts", "R  old.ts -> new.ts"].join("\n"));
            assert_eq!(count_status_changes(&entries), StatusCounts { added: 4, deleted: 0 });
        }

        #[test]
        fn does_not_count_a_delete_then_add_rewrite_as_deleted() {
            let entries = parse_status_entries("AD rewritten.ts\n");
            assert_eq!(count_status_changes(&entries), StatusCounts { added: 1, deleted: 0 });
        }

        #[test]
        fn returns_zeros_for_a_clean_tree() {
            assert_eq!(count_status_changes(&parse_status_entries("")), StatusCounts { added: 0, deleted: 0 });
        }
    }

    mod truncate_diff {
        use super::*;

        #[test]
        fn passes_short_text_through_untouched() {
            assert_eq!(
                truncate_diff("+a\n-b\n", Some(10)),
                TruncatedDiff { text: "+a\n-b\n".into(), truncated: false }
            );
        }

        #[test]
        fn cuts_at_a_line_boundary_not_mid_line() {
            let TruncatedDiff { text, truncated } = truncate_diff("+aaaa\n+bbbb\n+cccc\n", Some(14));
            assert_eq!(text, "+aaaa\n+bbbb\n");
            assert!(truncated);
        }

        #[test]
        fn keeps_the_raw_cut_when_there_is_no_newline_to_fall_back_to() {
            let TruncatedDiff { text, truncated } = truncate_diff(&"x".repeat(20), Some(5));
            assert_eq!(text, "xxxxx");
            assert!(truncated);
        }
    }

    mod log_page_args {
        use super::*;

        #[test]
        fn looks_at_branches_remotes_tags_rather_than_all() {
            // --all 会把 IDE 写在 refs/jb/* 下的 Local History 提交也算进来
            let args = log_page_args("git", "/repo", &LogQuery::default());
            assert!(has(&args, "--branches"));
            assert!(has(&args, "--remotes"));
            assert!(has(&args, "--tags"));
            assert!(!has(&args, "--all"));
        }

        #[test]
        fn takes_one_extra_row_so_the_caller_can_tell_whether_there_is_a_next_page() {
            let args = log_page_args("git", "/repo", &LogQuery { limit: Some(20), ..Default::default() });
            assert_eq!(after(&args, "-n"), "21");
        }

        #[test]
        fn treats_the_search_box_as_a_literal_not_a_regex() {
            let args = log_page_args("git", "/repo", &LogQuery { grep: Some("fix(".into()), ..Default::default() });
            assert!(has(&args, "--fixed-strings"));
            assert!(has(&args, "--regexp-ignore-case"));
            assert!(has(&args, "--grep=fix("));
        }

        #[test]
        fn terminates_a_branch_filter_with_dashdash_so_a_leading_dash_cannot_become_an_option() {
            let args = log_page_args("git", "/repo", &LogQuery { rev: Some("-weird-branch".into()), ..Default::default() });
            assert_eq!(tail(&args, 2), ["-weird-branch", "--"]);
            assert!(!has(&args, "--branches"));
        }
    }

    mod parse_ref_labels {
        use super::*;

        fn label(name: &str, kind: GitRefKind, head: Option<bool>) -> GitRefLabel {
            GitRefLabel { name: name.into(), kind, head }
        }

        #[test]
        fn splits_decoration_into_local_remote_and_tag_labels() {
            let refs = parse_ref_labels("HEAD -> main, origin/main, tag: v1.0", &["origin"]);
            assert_eq!(
                refs,
                [
                    label("main", GitRefKind::Local, Some(true)),
                    label("origin/main", GitRefKind::Remote, None),
                    label("v1.0", GitRefKind::Tag, None),
                ]
            );
        }

        #[test]
        fn drops_origin_head_and_a_detached_bare_head() {
            assert_eq!(
                parse_ref_labels("HEAD, origin/HEAD, origin/main", &["origin"]),
                [label("origin/main", GitRefKind::Remote, None)]
            );
        }

        #[test]
        fn returns_nothing_for_an_undecorated_commit() {
            assert_eq!(parse_ref_labels("", &["origin"]), []);
        }
    }

    mod parse_log_page {
        use super::*;

        fn row(cols: &[&str]) -> String {
            cols.join("\t")
        }

        #[test]
        fn parses_parents_refs_and_a_subject_containing_tabs() {
            let sha = "a".repeat(40);
            let out = parse_log_page(&row(&[&sha, "aaaaaaa", "fay", "f@x.io", "1700000000", "", "HEAD -> main", "fix:\tthing"]));
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].parents, Vec::<String>::new());
            assert_eq!(out[0].subject, "fix:\tthing");
            assert_eq!(out[0].authored_at, 1_700_000_000_000);
            assert_eq!(out[0].decoration, "HEAD -> main");
        }

        #[test]
        fn keeps_both_parents_of_a_merge_in_git_order() {
            let (m, p, q) = ("m".repeat(40), "p".repeat(40), "q".repeat(40));
            let parents = format!("{p} {q}");
            let out = parse_log_page(&row(&[&m, "mmmmmmm", "fay", "f@x.io", "1", &parents, "", "Merge"]));
            assert_eq!(out[0].parents, [p, q]);
        }

        #[test]
        fn drops_short_rows_instead_of_drawing_a_wrong_graph() {
            assert_eq!(parse_log_page("deadbeef\tfay\tbroken"), []);
        }
    }

    mod rank_authors {
        use super::*;

        #[test]
        fn orders_by_commit_count_most_active_first() {
            assert_eq!(rank_authors("fay\nlee\nfay\nfay\nlee\nzhu\n"), ["fay", "lee", "zhu"]);
        }
    }

    mod parse_commit_meta {
        use super::*;

        #[test]
        fn reads_author_and_committer_timestamps_separately() {
            let sha = "s".repeat(40);
            let meta =
                parse_commit_meta(&[sha.as_str(), "sssssss", "fay", "f@x.io", "1700000000", "lee", "1700000060", "", ""].join("\t"))
                    .unwrap();
            assert_eq!(meta.authored_at, 1_700_000_000_000);
            assert_eq!(meta.committed_at, 1_700_000_060_000);
            assert_eq!(meta.committer, "lee");
        }

        #[test]
        fn returns_null_when_the_row_is_malformed() {
            assert_eq!(parse_commit_meta("nope"), None);
        }
    }

    mod parse_commit_files {
        use super::*;

        fn file(path: &str, orig: Option<&str>, status: &str, added: Option<u64>, deleted: Option<u64>) -> GitCommitFile {
            GitCommitFile { path: path.into(), orig_path: orig.map(Into::into), status: status.into(), added, deleted }
        }

        #[test]
        fn pairs_the_raw_section_with_the_numstat_section_by_position() {
            let files = parse_commit_files(
                &[
                    ":000000 100644 0000000 353b768 A\tCLAUDE.md",
                    ":100644 100644 f35f9b5 e5a4e35 M\tpackage.json",
                    "75\t0\tCLAUDE.md",
                    "2\t1\tpackage.json",
                ]
                .join("\n"),
            );
            assert_eq!(
                files,
                [file("CLAUDE.md", None, "A", Some(75), Some(0)), file("package.json", None, "M", Some(2), Some(1))]
            );
        }

        #[test]
        fn keeps_both_paths_of_a_rename_and_strips_the_similarity_score() {
            let files = parse_commit_files(&[":100644 100644 aaa bbb R100\told.ts\tnew.ts", "0\t0\tsrc/{old.ts => new.ts}"].join("\n"));
            assert_eq!(files, [file("new.ts", Some("old.ts"), "R", Some(0), Some(0))]);
        }

        #[test]
        fn records_a_binary_file_as_unknown_line_counts_not_zero() {
            let files = parse_commit_files(&[":100644 100644 aaa bbb M\tlogo.png", "-\t-\tlogo.png"].join("\n"));
            assert_eq!(files[0], file("logo.png", None, "M", None, None));
        }

        #[test]
        fn drops_every_line_count_when_the_two_sections_disagree_rather_than_misattributing_them() {
            let files = parse_commit_files(
                &[":000000 100644 0000000 353b768 A\ta.ts", ":100644 100644 f35f9b5 e5a4e35 M\tb.ts", "5\t1\tb.ts"].join("\n"),
            );
            let got: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.added, f.deleted)).collect();
            assert_eq!(got, [("a.ts", None, None), ("b.ts", None, None)]);
        }

        #[test]
        fn returns_nothing_for_a_commit_with_no_diff() {
            assert_eq!(parse_commit_files(""), []);
        }
    }

    mod parse_branch_list {
        use super::*;

        fn row(name: &str, upstream: &str, head: &str, symref: &str) -> String {
            [name, upstream, head, symref].join("\t")
        }

        #[test]
        fn drops_origin_head_which_git_shortens_to_a_bare_remote_name() {
            // refs/remotes/origin/HEAD 的 %(refname:short) 是 "origin"，不是 "origin/HEAD"
            let out = parse_branch_list(
                &[
                    row("main", "origin/main", "*", ""),
                    row("origin", "", "", "refs/remotes/origin/main"),
                    row("origin/main", "", "", ""),
                ]
                .join("\n"),
                &["origin"],
            );
            assert_eq!(out.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), ["main", "origin/main"]);
        }

        #[test]
        fn marks_the_checked_out_branch_and_keeps_its_upstream() {
            let out = parse_branch_list(&row("main", "origin/main", "*", ""), &["origin"]);
            assert_eq!(
                out,
                [GitBranchRef { name: "main".into(), remote: false, upstream: Some("origin/main".into()), head: true }]
            );
        }
    }

    mod branch_exists_args {
        use super::*;

        #[test]
        fn verifies_refs_heads_branch_quietly_anchored_to_c_repo() {
            let argv = branch_exists_args("/usr/bin/git", "/home/u/repo", "feat/x");
            assert_eq!(tail(&argv, 4), ["rev-parse", "--verify", "--quiet", "refs/heads/feat/x"]);
            assert_eq!(after(&argv, "-C"), "/home/u/repo");
        }
    }

    mod history_write_argv {
        use super::*;

        #[test]
        fn fetches_every_remote_and_prunes_stale_tracking_branches() {
            assert_eq!(tail(&fetch_args("git", "/repo"), 3), ["fetch", "--all", "--prune"]);
        }

        #[test]
        fn detaches_head_at_the_revision_instead_of_guessing_a_branch() {
            assert_eq!(tail(&checkout_detach_args("git", "/repo", "abc"), 3), ["checkout", "--detach", "abc"]);
        }

        #[test]
        fn checks_out_a_local_branch_without_dashdash_so_git_does_not_treat_it_as_a_pathspec() {
            let argv = checkout_branch_args("git", "/repo", "main");
            assert_eq!(tail(&argv, 2), ["checkout", "main"]);
            assert!(!has(&argv, "--"));
        }

        #[test]
        fn creates_a_tracking_branch_from_the_remote_ref() {
            assert_eq!(
                tail(&checkout_track_args("git", "/repo", "main", "origin/main"), 5),
                ["checkout", "-b", "main", "--track", "origin/main"]
            );
        }

        #[test]
        fn cherry_picks_and_reverts_by_full_sha_revert_without_opening_an_editor() {
            let sha = "a".repeat(40);
            assert_eq!(tail(&cherry_pick_args("git", "/repo", &sha), 2), ["cherry-pick", sha.as_str()]);
            assert_eq!(tail(&revert_args("git", "/repo", &sha), 3), ["revert", "--no-edit", sha.as_str()]);
        }

        #[test]
        fn creates_a_branch_with_dashdash_so_a_leading_dash_name_cannot_become_an_option() {
            assert_eq!(tail(&branch_create_args("git", "/repo", "feat", "HEAD"), 4), ["branch", "--", "feat", "HEAD"]);
        }

        #[test]
        fn creates_and_checks_out_a_branch_in_one_checkout_b() {
            assert_eq!(tail(&checkout_new_branch_args("git", "/repo", "feat", "abc"), 4), ["checkout", "-b", "feat", "abc"]);
        }

        #[test]
        fn writes_the_reset_mode_into_argv_instead_of_relying_on_git_defaults() {
            assert_eq!(tail(&reset_args("git", "/repo", "abc", GitResetMode::Hard), 3), ["reset", "--hard", "abc"]);
            assert!(has(&reset_args("git", "/repo", "abc", GitResetMode::Mixed), "--mixed"));
        }

        #[test]
        fn merges_without_opening_an_editor_and_rebases_onto_the_revision() {
            assert_eq!(tail(&merge_args("git", "/repo", "abc"), 3), ["merge", "--no-edit", "abc"]);
            assert_eq!(tail(&rebase_args("git", "/repo", "abc"), 2), ["rebase", "abc"]);
        }

        #[test]
        fn restores_tracked_files_from_head_and_cleans_untracked_paths_behind_dashdash() {
            assert_eq!(
                tail(&restore_args("git", "/repo", &["a.ts"]), 6),
                ["restore", "--source=HEAD", "--staged", "--worktree", "--", "a.ts"]
            );
            assert_eq!(tail(&clean_paths_args("git", "/repo", &["b.ts"]), 4), ["clean", "-f", "--", "b.ts"]);
        }

        #[test]
        fn amends_with_no_edit_when_the_message_is_kept() {
            let argv = commit_args("git", "/repo", "", NO_PATHS, CommitOpts { amend: true, no_edit: true });
            assert!(has(&argv, "--amend"));
            assert!(has(&argv, "--no-edit"));
            assert!(!has(&argv, "-m"));
        }

        #[test]
        fn force_pushes_with_lease_never_force() {
            assert!(!push_args("git", "/repo", false).iter().any(|a| a == "--force" || a == "--force-with-lease"));
            let forced = push_args("git", "/repo", true);
            assert!(has(&forced, "--force-with-lease"));
            assert!(!has(&forced, "--force"));
        }

        #[test]
        fn drops_a_commit_by_rebasing_later_commits_onto_its_parent() {
            let sha = "a".repeat(40);
            let parent = format!("{sha}~1");
            assert_eq!(tail(&drop_commit_args("git", "/repo", &sha), 4), ["rebase", "--onto", parent.as_str(), sha.as_str()]);
        }

        #[test]
        fn continues_a_rebase_without_opening_an_editor() {
            let argv = continue_conflict_args("git", "/repo", GitConflictKind::Rebase);
            assert!(has(&argv, "core.editor=true"));
            assert_eq!(tail(&argv, 2), ["rebase", "--continue"]);
        }
    }

    mod is_safe_ref_name {
        use super::*;

        #[test]
        fn accepts_ordinary_branch_and_tag_names() {
            assert!(is_safe_ref_name("main"));
            assert!(is_safe_ref_name("feat/x"));
            assert!(is_safe_ref_name("v1.2.3"));
            assert!(is_safe_ref_name("origin/main"));
        }

        #[test]
        fn rejects_names_that_would_change_argv_meaning() {
            assert!(!is_safe_ref_name(""));
            assert!(!is_safe_ref_name("-n"));
            assert!(!is_safe_ref_name("a..b"));
            assert!(!is_safe_ref_name("a~1"));
            assert!(!is_safe_ref_name("foo bar"));
            assert!(!is_safe_ref_name("a^{}"));
        }
    }

    mod parse_default_worktree_branch {
        use super::*;

        #[test]
        fn treats_empty_and_head_as_unset() {
            assert_eq!(parse_default_worktree_branch(None), Ok(None));
            assert_eq!(parse_default_worktree_branch(Some(&json!(null))), Ok(None));
            assert_eq!(parse_default_worktree_branch(Some(&json!(""))), Ok(None));
            assert_eq!(parse_default_worktree_branch(Some(&json!("  "))), Ok(None));
            assert_eq!(parse_default_worktree_branch(Some(&json!("HEAD"))), Ok(None));
            assert_eq!(parse_default_worktree_branch(Some(&json!(" HEAD "))), Ok(None));
        }

        #[test]
        fn keeps_a_safe_ref_name() {
            assert_eq!(parse_default_worktree_branch(Some(&json!("main"))), Ok(Some("main".into())));
            assert_eq!(parse_default_worktree_branch(Some(&json!(" origin/main "))), Ok(Some("origin/main".into())));
        }

        #[test]
        fn rejects_names_that_would_change_argv_meaning() {
            assert!(parse_default_worktree_branch(Some(&json!("a..b"))).is_err());
            assert!(parse_default_worktree_branch(Some(&json!("-n"))).is_err());
            assert!(parse_default_worktree_branch(Some(&json!(1))).is_err());
        }
    }

    mod tracking_local_name {
        use super::*;

        #[test]
        fn strips_the_longest_matching_remote_prefix() {
            assert_eq!(tracking_local_name("origin/main", &["origin"]).as_deref(), Some("main"));
            assert_eq!(tracking_local_name("origin/feat/x", &["origin"]).as_deref(), Some("feat/x"));
            assert_eq!(tracking_local_name("origin-backup/x", &["origin", "origin-backup"]).as_deref(), Some("x"));
        }

        #[test]
        fn returns_null_when_the_name_is_not_under_a_known_remote() {
            assert_eq!(tracking_local_name("main", &["origin"]), None);
            assert_eq!(tracking_local_name("origin", &["origin"]), None);
        }
    }

    mod parse_git_op_input {
        use super::*;

        #[test]
        fn accepts_fetch_with_no_extra_fields() {
            assert_eq!(parse_git_op_input(&json!({ "op": "fetch" })), Ok(GitOpInput::Fetch));
        }

        #[test]
        fn rejects_an_unsafe_branch_name_instead_of_letting_it_become_an_option() {
            assert!(parse_git_op_input(&json!({ "op": "checkout-branch", "branch": "-n" })).is_err());
        }

        #[test]
        fn requires_a_hex_sha_for_cherry_pick_and_revert() {
            assert!(parse_git_op_input(&json!({ "op": "cherry-pick", "sha": "HEAD" })).is_err());
            assert_eq!(
                parse_git_op_input(&json!({ "op": "revert", "sha": "abcd" })),
                Ok(GitOpInput::Revert { sha: "abcd".into() })
            );
        }

        #[test]
        fn accepts_reset_merge_restore_and_rejects_an_empty_restore() {
            assert_eq!(
                parse_git_op_input(&json!({ "op": "reset", "rev": "abcd", "mode": "hard" })),
                Ok(GitOpInput::Reset { rev: "abcd".into(), mode: GitResetMode::Hard })
            );
            assert_eq!(
                parse_git_op_input(&json!({ "op": "merge", "rev": "main" })),
                Ok(GitOpInput::Merge { rev: "main".into() })
            );
            assert!(parse_git_op_input(&json!({ "op": "restore", "paths": [] })).is_err());
            assert_eq!(
                parse_git_op_input(&json!({ "op": "restore", "untracked": ["scratch.ts"] })),
                Ok(GitOpInput::Restore { paths: vec![], untracked: Some(vec!["scratch.ts".into()]) })
            );
        }

        #[test]
        fn parses_force_with_lease_push_drop_and_conflict_take() {
            assert_eq!(
                parse_git_op_input(&json!({ "op": "push", "forceWithLease": true })),
                Ok(GitOpInput::Push { force_with_lease: Some(true) })
            );
            assert_eq!(
                parse_git_op_input(&json!({ "op": "drop", "sha": "abcd" })),
                Ok(GitOpInput::Drop { sha: "abcd".into() })
            );
            assert_eq!(
                parse_git_op_input(&json!({ "op": "take", "side": "ours", "paths": ["a.ts"] })),
                Ok(GitOpInput::Take { side: GitTakeSide::Ours, paths: vec!["a.ts".into()] })
            );
        }
    }

    /// 期望值来自 TS 实现本身（tsx 跑 command.ts 落盘），不是手写推出来的。
    mod ts_vectors {
        use super::*;
        use crate::zellij::host::tests::decode;

        fn batch_argvs() -> Vec<Vec<String>> {
            vec![version_args("git"), status_args("git", "/home/u/it's repo")]
        }

        #[test]
        fn batch_command_line_posix() {
            assert_eq!(
                batch_git_command_line(HostKind::Posix, &batch_argvs(), GIT_ENV_RO),
                "unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_ASKPASS SSH_ASKPASS; export LC_ALL='C'; \
                 export GIT_TERMINAL_PROMPT='0'; export GIT_OPTIONAL_LOCKS='0'; 'git' '--version'; \
                 printf '\\n__FALCON_GIT_0_%s__\\n' \"$?\"; 'git' '-c' 'core.quotepath=false' '--no-pager' '-C' \
                 '/home/u/it'\\''s repo' 'status' '--porcelain'; printf '\\n__FALCON_GIT_1_%s__\\n' \"$?\""
            );
        }

        #[test]
        fn batch_command_line_windows() {
            assert_eq!(
                decode(&batch_git_command_line(HostKind::Windows, &batch_argvs(), GIT_ENV_RO)),
                "$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
                 $env:GIT_DIR = $null; $env:GIT_WORK_TREE = $null; $env:GIT_INDEX_FILE = $null; \
                 $env:GIT_ASKPASS = $null; $env:SSH_ASKPASS = $null; $env:LC_ALL = 'C'; \
                 $env:GIT_TERMINAL_PROMPT = '0'; $env:GIT_OPTIONAL_LOCKS = '0'; $LASTEXITCODE = 127; \
                 & 'git' '--version'; Write-Output ([char]10 + '__FALCON_GIT_0_' + $LASTEXITCODE + '__'); \
                 $LASTEXITCODE = 127; & 'git' '-c' 'core.quotepath=false' '--no-pager' '-C' '/home/u/it''s repo' \
                 'status' '--porcelain'; Write-Output ([char]10 + '__FALCON_GIT_1_' + $LASTEXITCODE + '__')"
            );
        }

        #[test]
        fn single_command_line_unsets_inherited_git_vars_instead_of_blanking_them() {
            assert_eq!(
                build_git_command_line(HostKind::Posix, &head_branch_args("git", "/r"), GIT_ENV),
                "env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_ASKPASS -u SSH_ASKPASS LC_ALL='C' \
                 GIT_TERMINAL_PROMPT='0' 'git' '-c' 'core.quotepath=false' '--no-pager' '-C' '/r' 'rev-parse' \
                 '--abbrev-ref' 'HEAD'"
            );
            assert_eq!(
                decode(&build_git_command_line(HostKind::Windows, &head_branch_args("git", "C:\\r"), GIT_ENV)),
                "$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
                 $LASTEXITCODE = 127; $env:GIT_DIR = $null; $env:GIT_WORK_TREE = $null; $env:GIT_INDEX_FILE = $null; \
                 $env:GIT_ASKPASS = $null; $env:SSH_ASKPASS = $null; $env:LC_ALL = 'C'; $env:GIT_TERMINAL_PROMPT = '0'; \
                 & 'git' '-c' 'core.quotepath=false' '--no-pager' '-C' 'C:\\r' 'rev-parse' '--abbrev-ref' 'HEAD'; \
                 exit $LASTEXITCODE"
            );
        }

        #[test]
        fn exists_commands() {
            assert_eq!(
                exists_command(HostKind::Posix, "/a b/[x]"),
                "if [ -e '/a b/[x]' ]; then printf yes; else printf no; fi"
            );
            assert_eq!(
                decode(&exists_command(HostKind::Windows, "C:\\a'b")),
                "$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
                 if (Test-Path -LiteralPath 'C:\\a''b') { 'yes' } else { 'no' }"
            );
            assert_eq!(
                exists_many_command(HostKind::Posix, &["/a", "/b c"]),
                "for p in '/a' '/b c'; do if [ -e \"$p\" ]; then echo yes; else echo no; fi; done"
            );
            assert_eq!(
                decode(&exists_many_command(HostKind::Windows, &["C:\\a", "C:\\b'c"])),
                "$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
                 @('C:\\a','C:\\b''c') | ForEach-Object { if (Test-Path -LiteralPath $_) { 'yes' } else { 'no' } }"
            );
        }

        #[test]
        fn git_file_head_commands() {
            assert_eq!(
                git_file_head_command(HostKind::Posix, "/w/x"),
                "if [ -f '/w/x/.git' ]; then head -c 200 '/w/x/.git'; fi"
            );
            assert_eq!(
                decode(&git_file_head_command(HostKind::Windows, "C:\\w\\x")),
                "$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
                 if (Test-Path -LiteralPath 'C:\\w\\x\\.git' -PathType Leaf) \
                 { Get-Content -LiteralPath 'C:\\w\\x\\.git' -TotalCount 1 }"
            );
        }

        #[test]
        fn batch_output_is_split_by_sentinels_and_out_of_range_sentinels_still_reset() {
            let out = parse_git_batch(
                "a\r\n\r\n__FALCON_GIT_0_0__\r\nfoo\nbar\n__FALCON_GIT_5_1__\n  __FALCON_GIT_1_128__ \nbaz",
                3,
            );
            assert_eq!(
                out,
                [
                    BatchGitResult { code: Some(0), stdout: "a\n".into() },
                    BatchGitResult { code: Some(128), stdout: String::new() },
                    BatchGitResult { code: None, stdout: String::new() },
                ]
            );
        }

        #[test]
        fn unquote_c_path_follows_ts_code_unit_semantics() {
            // \NNN 逐字节变成一个码元（不按 UTF-8 重组）；八进制分支固定跳 3 个码元
            assert_eq!(unquote_c_path(r#""a\"b\\c\tq\346\226\207""#), "a\"b\\c\tq\u{e6}\u{96}\u{87}");
            assert_eq!(unquote_c_path("\""), "");
            assert_eq!(unquote_c_path(r#""\1ab""#), "\u{1}");
            assert_eq!(unquote_c_path("\"x\\"), "x");
            assert_eq!(unquote_c_path("\"\\1\u{1F600}x\""), "\u{1}x");
            assert_eq!(unquote_c_path("plain"), "plain");
        }

        #[test]
        fn status_entries_with_quoted_and_non_ascii_paths() {
            // 第一条的 origPath 多出一个 `"`：TS 先整行 unquote 再按箭头拆，照搬
            assert_eq!(
                parse_status_entries("R  \"old\\\"n\" -> \"new\\\"n\"\n?? \"tab\\there\"\nMM é.ts\nx"),
                [
                    change("new\"n", Some("old\"n\""), "R", " "),
                    change("tab\there", None, "?", "?"),
                    change("é.ts", None, "M", "M"),
                ]
            );
        }

        #[test]
        fn remotes_keep_one_fetch_url_each() {
            assert_eq!(
                parse_remotes("origin\tgit@x:y.git (fetch)\norigin\tgit@x:y.git (push)\nup  https://u (push)\nup https://u (fetch)\n"),
                [
                    GitRemote { name: "origin".into(), url: "git@x:y.git".into() },
                    GitRemote { name: "up".into(), url: "https://u".into() },
                ]
            );
        }

        #[test]
        fn worktree_list_porcelain() {
            let out = parse_worktree_list(
                "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\nworktree \"/a\\\"b\"\nHEAD def\ndetached\nlocked reason\nprunable\n\nworktree \nHEAD x\n",
            );
            assert_eq!(
                out,
                [
                    GitWorktreeEntry { path: "/repo".into(), head: "abc".into(), branch: Some("main".into()), ..Default::default() },
                    GitWorktreeEntry {
                        path: "/a\"b".into(),
                        head: "def".into(),
                        locked: true,
                        prunable: true,
                        ..Default::default()
                    },
                ]
            );
        }

        #[test]
        fn log_uses_js_number_for_timestamps() {
            assert_eq!(
                parse_log("abc\tfay\t1700000000\tfix:\tthing\r\nbad\n \t \t \nx\ty\t0x10\ts"),
                [
                    GitCommit { sha: "abc".into(), author: "fay".into(), authored_at: 1_700_000_000_000, subject: "fix:\tthing".into() },
                    GitCommit { sha: " ".into(), author: " ".into(), authored_at: 0, subject: String::new() },
                    GitCommit { sha: "x".into(), author: "y".into(), authored_at: 16_000, subject: "s".into() },
                ]
            );
        }

        #[test]
        fn numstat_map_keeps_first_insertion_position_and_last_value() {
            let got: Vec<_> = parse_numstat_map("1\t2\ta\n-\t-\tb.png\n3\t4\ta\n5\t6\t\"q\\tz\"\n").into_iter().collect();
            assert_eq!(
                got,
                [
                    ("a".to_string(), Numstat { added: Some(3), deleted: Some(4) }),
                    ("b.png".to_string(), Numstat { added: None, deleted: None }),
                    ("q\tz".to_string(), Numstat { added: Some(5), deleted: Some(6) }),
                ]
            );
        }

        #[test]
        fn truncate_counts_utf16_units() {
            // TS 切出 "ab\ud83d"（孤立代理）；Rust 表示不了，落成 U+FFFD
            assert_eq!(truncate_diff("ab\u{1F600}cd", Some(3)), TruncatedDiff { text: "ab\u{FFFD}".into(), truncated: true });
            assert_eq!(truncate_diff("ab\u{1F600}", Some(4)), TruncatedDiff { text: "ab\u{1F600}".into(), truncated: false });
        }

        #[test]
        fn small_parsers() {
            assert_eq!(parse_tag_list("v1\n\n v2 \nv3", Some(0)), ["v1"]);
            assert_eq!(parse_tag_list("v1\n\n v2 \nv3", None), ["v1", "v2", "v3"]);
            assert_eq!(rank_authors("b\na\na\nb\nc\n"), ["b", "a", "c"]);
            assert_eq!(parse_exists_many("yes\r\nno\n\n junk \n yes "), [true, false, true]);
            assert_eq!(count_ignored("!! a\n?? b\n!! c/\n"), 2);
            assert_eq!(
                parse_status(" M a\n M b\n M c\n", Some(2)),
                StatusSummary { count: 3, files: vec!["a".into(), "b".into()] }
            );
        }

        #[test]
        fn pathspec_budget_counts_utf16_units() {
            assert!(!pathspec_too_long(&["x".repeat(1997)]));
            assert!(pathspec_too_long(&["x".repeat(1998)]));
            assert!(!pathspec_too_long(&["\u{1F600}".repeat(998)]));
            assert!(pathspec_too_long(&["\u{1F600}".repeat(999)]));
        }
    }
}

//! 宿主机路径运算与分支 slug。移植自 `packages/server/src/git/path.ts`。全部是纯函数，零 I/O。
//!
//! **不用 `std::path`**——与 zellij/host.rs 手工拼 sep 的决定同一个理由：后端跑在
//! Windows 上也可能要为一台 Linux 远端构造路径，`std::path` 会用后端自己的平台规则，
//! 方向直接反了。kind 一律由本机平台或 SshLink 的探测给出。
//!
//! 这里还住着删除护栏的静态断言（veto_removal，见 remove.rs 调用处）所依赖的全部
//! 比较原语。它们必须能被单独盯着读、单独手工构造脏数据去打，所以刻意保持纯净：
//! 可审查性比复用度重要。

use std::collections::HashMap;
use std::sync::LazyLock;

use falcon_proto::{HostKind, WorktreeFailure};
use regex::Regex;

pub fn sep_for(kind: HostKind) -> &'static str {
    if kind == HostKind::Windows { "\\" } else { "/" }
}

fn trim_trailing_seps(s: &str) -> &str {
    s.trim_end_matches(['\\', '/'])
}

/// 分隔符归一。
///
/// windows：/ → \ 并合并重复分隔符（开头的 \\ 是 UNC，保留两根）。
/// posix：**什么都不做**——反斜杠在 Linux 上是合法文件名字符，顺手把它换成 /
/// 会当场把一个叫 `a\b` 的目录变成 `a/b`。
pub fn normalize_sep(kind: HostKind, p: &str) -> String {
    if kind != HostKind::Windows {
        return p.to_string();
    }
    let unc = p.starts_with("\\\\") || p.starts_with("//");
    let mut body = String::with_capacity(p.len());
    let mut prev_sep = false;
    for c in p.chars() {
        let c = if c == '/' { '\\' } else { c };
        if c == '\\' {
            if prev_sep {
                continue;
            }
            prev_sep = true;
        } else {
            prev_sep = false;
        }
        body.push(c);
    }
    if unc { format!("\\{body}") } else { body }
}

pub fn is_absolute(kind: HostKind, p: &str) -> bool {
    if kind == HostKind::Windows {
        let n = normalize_sep(HostKind::Windows, p);
        let b = n.as_bytes();
        let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
        return drive || n.starts_with("\\\\");
    }
    p.starts_with('/')
}

/// UNC 路径（\\server\share\...）。v1 不支持派生：\\server\share 才是"根"，深度规则不同
pub fn is_unc(kind: HostKind, p: &str) -> bool {
    kind == HostKind::Windows && normalize_sep(HostKind::Windows, p).starts_with("\\\\")
}

/// 去掉尾部分隔符后按 sep 切成非空段。windows 的第一段是盘符（`C:`）
pub fn segments(kind: HostKind, p: &str) -> Vec<String> {
    let n = normalize_sep(kind, p);
    trim_trailing_seps(&n).split(sep_for(kind)).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// 盘符 / 根之后还剩几段。C:\ → 0，C:\a → 1，/ → 0，/home → 1。
///
/// 删除护栏的深度门槛用它：真实的 worktree 目录都是"某个仓库目录的同级"，
/// 而仓库不会直接躺在盘符根上。门槛设在 2 能挡掉绝大多数脏数据造成的灾难，
/// 代价只是拒绝一种没人真会用的布局。
pub fn path_depth(kind: HostKind, p: &str) -> usize {
    let segs = segments(kind, p).len();
    if kind == HostKind::Windows { segs.saturating_sub(1) } else { segs }
}

pub fn basename_of(kind: HostKind, p: &str) -> String {
    segments(kind, p).pop().unwrap_or_default()
}

pub fn dirname_of(kind: HostKind, p: &str) -> String {
    let normalized = normalize_sep(kind, p);
    let n = trim_trailing_seps(&normalized);
    let sep = sep_for(kind);
    let Some(i) = n.rfind(sep) else {
        return n.to_string();
    };
    if kind != HostKind::Windows {
        return if i == 0 { "/".to_string() } else { n[..i].to_string() };
    }
    // windows：C:\a → C:\（保留根的那根分隔符），C:\a\b → C:\a
    let head = &n[..i];
    let hb = head.as_bytes();
    if hb.len() == 2 && hb[0].is_ascii_alphabetic() && hb[1] == b':' { format!("{head}\\") } else { head.to_string() }
}

pub fn join_path<S: AsRef<str>>(kind: HostKind, parts: &[S]) -> String {
    let joined = parts
        .iter()
        .map(AsRef::as_ref)
        .filter(|s| !s.is_empty())
        .enumerate()
        .map(|(i, s)| if i == 0 { trim_trailing_seps(s) } else { s.trim_matches(['\\', '/']) })
        .collect::<Vec<_>>()
        .join(sep_for(kind));
    normalize_sep(kind, &joined)
}

/// 路径相等。windows 上**大小写不敏感**，尾分隔符不计。
///
/// 必须有：git 在 Windows 上返回正斜杠（D:/code/falcon），而 DB 里存的是用户输入的
/// D:\code\falcon，直接 == 恒不成立。真正的危险不是功能不工作，而是有人为了让它
/// 工作去放宽断言——所以归一化集中在这里一处，断言只比归一化后的形式。
pub fn same_path(kind: HostKind, a: &str, b: &str) -> bool {
    canon_key(kind, a) == canon_key(kind, b)
}

/// 比较用的规范形式。只用于比较，不要拿它当路径发给宿主机。
pub fn canon_key(kind: HostKind, p: &str) -> String {
    let n = normalize_sep(kind, p);
    let n = trim_trailing_seps(&n);
    if kind == HostKind::Windows { n.to_lowercase() } else { n.to_string() }
}

/// ancestor 是不是 child 的**严格**祖先（相等返回 false）
pub fn is_ancestor(kind: HostKind, ancestor: &str, child: &str) -> bool {
    let a = segments(kind, &canon_key(kind, ancestor));
    let c = segments(kind, &canon_key(kind, child));
    if a.len() >= c.len() {
        return false;
    }
    a.iter().zip(&c).all(|(x, y)| x == y)
}

/// 去掉头部的 `-` / `.`、尾部的 `-` / `.`，按码点截到 48，再去一次尾部
fn finish_slug(cleaned: &str, fallback: &str) -> String {
    let cleaned = cleaned.trim_start_matches(['-', '.']).trim_end_matches(['-', '.']);
    let truncated: String = cleaned.chars().take(48).collect();
    let s = truncated.trim_end_matches(['-', '.']);
    if s.is_empty() { fallback.to_string() } else { s.to_string() }
}

fn collapse_dashes(s: &str) -> String {
    static DASHES: LazyLock<Regex> = LazyLock::new(|| Regex::new("-{2,}").unwrap());
    DASHES.replace_all(s, "-").into_owned()
}

/// 分支名 → 目录名后缀。
///
/// 只替换**文件系统真的不接受**的字符，不做 ASCII 白名单。
/// git 的 check-ref-format 已经挡掉了空格、`~ ^ : ? * [`、反斜杠和控制字符，
/// 剩下必须自己处理的只有 `/`（路径分隔符）和 Windows 独有的 `" < > |`。
///
/// 曾经写成"只保留 [A-Za-z0-9._-]"，结果 `功能/中文` 这类纯非 ASCII 分支名会整条
/// 塌成同一个兜底值——两条中文分支互撞、目录名还完全看不出是哪条分支。CJK 在 NTFS
/// 与 ext4 上都是合法文件名；命令行方向经 quote_posix / -EncodedCommand 都是字节安全的，
/// 而回读方向即使真的乱码，落库的也是 **git 自己报的路径**（见创建流程），
/// 于是护栏比对仍然自洽——最坏是拒删并告警，不会删错。
///
/// 尾部的 . 和空格必须去掉：Windows 创建时会**静默**吃掉它们，于是 DB 里记的是
/// `foo-feat.`、磁盘上是 `foo-feat`，删除护栏拿路径比对就永远对不上，最后表现为
/// "目录删不掉，只给了个 warning"。截断之后要再去一次，否则截口可能又露出一个 -。
/// 截断按**码点**，免得把一个字符劈成两半（TS 那边是防代理对被劈开）。
///
/// 注意这个映射**不是单射**：feature/foo 与 feature-foo 会撞同一个目录名。撞了就
/// 报 path-occupied 让用户自己改名——自动加后缀会让目录名与分支名失去对应关系。
pub fn branch_slug(branch: &str) -> String {
    static BAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[/"<>|\s]+"#).unwrap());
    finish_slug(&collapse_dashes(&BAD.replace_all(branch, "-")), "wt")
}

/// 同级平铺路径：<仓库根的父目录>/<仓库根基名>-<分支 slug>
///
/// 基准点是**仓库根**，不是项目的 workingDir。项目工作目录完全可能是仓库里的一个
/// 子目录（monorepo 里指到 packages/server 很常见），在那儿旁边建 worktree 会把它
/// 建进仓库自己里面——源仓库的 git status 里立刻多出一坨未跟踪文件。
pub fn sibling_worktree_path(kind: HostKind, repo_root: &str, branch: &str) -> String {
    let normalized = normalize_sep(kind, repo_root);
    let root = trim_trailing_seps(&normalized);
    join_path(
        kind,
        &[dirname_of(kind, root), format!("{}-{}", basename_of(kind, root), branch_slug(branch))],
    )
}

/// 容器名 → 集中目录名的前半段。比 branch_slug 更狠：分支名有 git 的
/// check-ref-format 兜底（空格、`~ ^ : ? * [`、反斜杠、控制字符都进不来），
/// 而容器名是任意用户字符串，什么都可能有——所以 POSIX/Windows 两边的非法与
/// 危险字符（`/ \ : * ? " < > |`、空白、控制字符）全部替换成 -。
/// 尾部 . 与空格必须去掉、截断按码点，理由同 branch_slug（Windows 静默吞字符、
/// 别把字符劈成两半）。空了兜底 "multi"。
pub fn name_slug(name: &str) -> String {
    static BAD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"[/\\:*?"<>|\s\x00-\x1f]+"#).unwrap());
    finish_slug(&collapse_dashes(&BAD.replace_all(name, "-")), "multi")
}

/// 批量派生的集中目录默认位置：<第一个成员仓库根的父目录>/<容器名slug>-<分支slug>。
/// 基准取仓库根而不是成员配置的路径，与 sibling_worktree_path 的理由相同。
pub fn multi_central_path(kind: HostKind, first_repo_root: &str, container_name: &str, branch: &str) -> String {
    let normalized = normalize_sep(kind, first_repo_root);
    let root = trim_trailing_seps(&normalized);
    join_path(
        kind,
        &[dirname_of(kind, root), format!("{}-{}", name_slug(container_name), branch_slug(branch))],
    )
}

/// 成员 worktree 的落点：<集中目录>/<仓库根 basename>。basename 不做 slug——它已经是一个真实存在的目录名
pub fn member_worktree_path(kind: HostKind, central_dir: &str, repo_root: &str) -> String {
    join_path(kind, &[central_dir.to_string(), basename_of(kind, repo_root)])
}

/// 批量派生前对 N 个已解析出的仓库根做的容器级否决。返回给用户看的一句话，None = 通过。
///
/// - 仓库判重按 canon_key：两个成员配置成同一仓库的不同子目录、或 Windows 上的
///   大小写别名，rev-parse 之后都会在这里现形。
/// - basename 撞名也按 canon_key：两棵 worktree 要以仓库根 basename 平铺进同一个
///   集中目录，Windows 上 Repo 与 repo 是同一个目录。
///
/// 这两条都只能在派生时判——容器创建时成员可以是仓库子目录、宿主 kind 也未必已知。
pub fn veto_multi_roots<S: AsRef<str>>(kind: HostKind, roots: &[S]) -> Option<String> {
    let mut seen_root: HashMap<String, String> = HashMap::new();
    let mut seen_base: HashMap<String, String> = HashMap::new();
    for root in roots.iter().map(AsRef::as_ref) {
        if !is_absolute(kind, root) {
            return Some(format!("仓库根不是绝对路径：{root}"));
        }
        let root_key = canon_key(kind, root);
        if let Some(dup) = seen_root.get(&root_key) {
            return Some(format!("两个成员指向同一个仓库（{dup}），请去掉一个"));
        }
        seen_root.insert(root_key, root.to_string());
        let base_key = canon_key(kind, &basename_of(kind, root));
        if let Some(dup) = seen_base.get(&base_key) {
            return Some(format!(
                "成员仓库目录同名（{dup} 与 {root}），无法在同一个集中目录里平铺，请先给仓库目录改名"
            ));
        }
        seen_base.insert(base_key, root.to_string());
    }
    None
}

/// Windows 路径长度上限。
///
/// <repo>-<长分支名>\node_modules\... 极易超过 MAX_PATH(260)，而超了之后 rd /s 直接
/// 失败——宁可在创建时就拒绝，也不要建完发现删不掉。留 60 字符给仓库内部的深路径。
pub const WINDOWS_PATH_BUDGET: usize = 200;

/// 目标目录的静态否决。返回 None 表示可以建。
///
/// 只管"这个位置本身合不合适"，不碰文件系统——占用检查是另一回事（要发命令）。
/// 与删除护栏共用同一套比较原语，免得两边的"相等/包含"语义悄悄漂移。
pub fn veto_target_dir(kind: HostKind, main_worktree: &str, dir: &str) -> Option<WorktreeFailure> {
    // 建在仓库里面：源仓库的 git status 里会立刻多出一坨未跟踪文件。
    // 等于仓库根更糟——那是要把仓库自己盖掉。
    if same_path(kind, dir, main_worktree) || is_ancestor(kind, main_worktree, dir) {
        return Some(WorktreeFailure::PathInsideRepo);
    }
    // 按 UTF-16 码元数（= TS 的 .length），Windows 的 MAX_PATH 也是按它算的
    if kind == HostKind::Windows && dir.encode_utf16().count() > WINDOWS_PATH_BUDGET {
        return Some(WorktreeFailure::PathTooLong);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use HostKind::{Posix, Windows};

    #[test]
    fn name_slug_keeps_cjk_and_punctuation() {
        assert_eq!(name_slug("我的组合项目"), "我的组合项目");
        assert_eq!(name_slug("app_v2.core"), "app_v2.core");
    }

    #[test]
    fn name_slug_replaces_illegal_chars() {
        assert_eq!(name_slug("a/b\\c:d*e?f\"g<h>i|j k"), "a-b-c-d-e-f-g-h-i-j-k");
        assert_eq!(name_slug("tab\there"), "tab-here");
    }

    #[test]
    fn name_slug_strips_dots_and_dashes() {
        assert_eq!(name_slug("..proj.."), "proj");
        assert_eq!(name_slug("--proj--"), "proj");
        assert_eq!(name_slug("proj. "), "proj");
    }

    #[test]
    fn name_slug_truncates_by_code_point() {
        let slug = name_slug(&"😀".repeat(60));
        assert_eq!(slug.chars().count(), 48);
        assert!(slug.chars().all(|c| c == '😀'));
    }

    #[test]
    fn name_slug_fallback() {
        assert_eq!(name_slug("///"), "multi");
        assert_eq!(name_slug("  "), "multi");
        assert_eq!(name_slug(""), "multi");
    }

    #[test]
    fn multi_central_path_posix_and_windows() {
        assert_eq!(multi_central_path(Posix, "/home/u/code/web", "我的组合", "feat/x"), "/home/u/code/我的组合-feat-x");
        assert_eq!(multi_central_path(Windows, "D:\\code\\web\\", "App Suite", "feat/x"), "D:\\code\\App-Suite-feat-x");
    }

    #[test]
    fn member_worktree_path_appends_basename() {
        assert_eq!(
            member_worktree_path(Posix, "/home/u/code/app-feat-x", "/srv/repos/中文仓库"),
            "/home/u/code/app-feat-x/中文仓库"
        );
        assert_eq!(member_worktree_path(Windows, "D:\\code\\app-feat-x", "D:\\repos\\Web"), "D:\\code\\app-feat-x\\Web");
    }

    #[test]
    fn veto_multi_roots_cases() {
        assert_eq!(veto_multi_roots(Posix, &["/a/web", "/a/server", "/b/shared"]), None);
        assert!(veto_multi_roots(Posix, &["/a/web", "code/server"]).unwrap().contains("绝对路径"));
        assert!(veto_multi_roots(Posix, &["/a/web", "/a/web"]).unwrap().contains("同一个仓库"));
        assert!(veto_multi_roots(Windows, &["D:\\code\\Web", "d:/code/web"]).unwrap().contains("同一个仓库"));
        assert!(veto_multi_roots(Posix, &["/a/web", "/b/web"]).unwrap().contains("同名"));
        assert!(veto_multi_roots(Windows, &["C:\\a\\Web", "C:\\b\\web"]).unwrap().contains("同名"));
        assert_eq!(veto_multi_roots(Posix, &["/a/Web", "/b/web"]), None);
    }

    #[test]
    fn primitives() {
        assert_eq!(normalize_sep(Windows, "C:/a//b\\\\c"), "C:\\a\\b\\c");
        assert_eq!(normalize_sep(Windows, "//srv/share/x"), "\\\\srv\\share\\x");
        assert_eq!(normalize_sep(Posix, "a\\b//c"), "a\\b//c");
        assert!(is_absolute(Windows, "c:/x") && is_absolute(Windows, "\\\\srv\\s") && !is_absolute(Windows, "x\\y"));
        assert!(is_unc(Windows, "//srv/share") && !is_unc(Posix, "//srv/share"));
        assert_eq!(path_depth(Windows, "C:\\"), 0);
        assert_eq!(path_depth(Windows, "C:\\a"), 1);
        assert_eq!(path_depth(Posix, "/"), 0);
        assert_eq!(path_depth(Posix, "/home"), 1);
        assert_eq!(dirname_of(Windows, "C:\\a"), "C:\\");
        assert_eq!(dirname_of(Windows, "C:\\a\\b\\"), "C:\\a");
        assert_eq!(dirname_of(Posix, "/a"), "/");
        assert_eq!(dirname_of(Posix, "rel"), "rel");
        assert_eq!(join_path(Posix, &["/a/", "/b/", "", "c"]), "/a/b/c");
        assert_eq!(join_path(Windows, &["C:\\a\\", "b/c"]), "C:\\a\\b\\c");
        assert!(same_path(Windows, "D:/Code/Falcon/", "d:\\code\\falcon"));
        assert!(!same_path(Posix, "/a/B", "/a/b"));
        assert!(is_ancestor(Posix, "/a", "/a/b") && !is_ancestor(Posix, "/a", "/a") && !is_ancestor(Posix, "/a/b", "/a"));
        assert!(!is_ancestor(Posix, "/a/b", "/a/bc"));
    }

    #[test]
    fn branch_slugs() {
        assert_eq!(branch_slug("feature/foo"), "feature-foo");
        assert_eq!(branch_slug("功能/中文"), "功能-中文");
        assert_eq!(branch_slug("a//b"), "a-b");
        assert_eq!(branch_slug("./weird./"), "weird");
        assert_eq!(branch_slug("///"), "wt");
        assert_eq!(sibling_worktree_path(Posix, "/code/app/", "feat/x"), "/code/app-feat-x");
        assert_eq!(sibling_worktree_path(Windows, "D:/code/app", "feat/x"), "D:\\code\\app-feat-x");
    }

    #[test]
    fn veto_target_dir_cases() {
        assert_eq!(veto_target_dir(Posix, "/code/app", "/code/app"), Some(WorktreeFailure::PathInsideRepo));
        assert_eq!(veto_target_dir(Posix, "/code/app", "/code/app/sub"), Some(WorktreeFailure::PathInsideRepo));
        assert_eq!(veto_target_dir(Posix, "/code/app", "/code/app-feat"), None);
        let long = format!("C:\\{}", "x".repeat(200));
        assert_eq!(veto_target_dir(Windows, "C:\\repo", &long), Some(WorktreeFailure::PathTooLong));
    }
}

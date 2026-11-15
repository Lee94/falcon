//! 把一串文件路径拼成目录树。对应旧 React 版的 `lib/fileTree.ts`，纯函数。
//!
//! 「修改」面板与 History 的提交详情共用它——两处列的都是"一组改动文件"，
//! 只是来源不同。

use crate::js::locale_compare;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileViewMode {
    List,
    Tree,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FileRow {
    Dir { name: String, path: String, depth: usize, open: bool, paths: Vec<String> },
    File { index: usize, depth: usize, name_only: bool },
}

/// 展平改动文件树；折叠不影响目录勾选作用的完整子树。
pub fn build_file_rows(paths: &[String], mode: FileViewMode, root: Option<&str>, collapsed: &HashSet<String>) -> Vec<FileRow> {
    if mode == FileViewMode::List {
        return (0..paths.len()).map(|index| FileRow::File { index, depth: 0, name_only: false }).collect();
    }
    fn subtree(nodes: &[TreeNode<usize>], paths: &[String], out: &mut Vec<String>) {
        for node in nodes {
            match node {
                TreeNode::File(file) => out.push(paths[file.item].clone()),
                TreeNode::Dir(dir) => subtree(&dir.children, paths, out),
            }
        }
    }
    fn level(nodes: &[TreeNode<usize>], paths: &[String], depth: usize, collapsed: &HashSet<String>, out: &mut Vec<FileRow>) {
        for node in nodes {
            match node {
                TreeNode::File(file) => out.push(FileRow::File { index: file.item, depth, name_only: true }),
                TreeNode::Dir(dir) => {
                    let open = !collapsed.contains(&dir.path);
                    let mut children = Vec::with_capacity(dir.file_count);
                    subtree(&dir.children, paths, &mut children);
                    out.push(FileRow::Dir { name: dir.name.clone(), path: dir.path.clone(), depth, open, paths: children });
                    if open { level(&dir.children, paths, depth + 1, collapsed, out); }
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Some(root) = root {
        let open = !collapsed.contains("");
        out.push(FileRow::Dir { name: root.to_string(), path: String::new(), depth: 0, open, paths: paths.to_vec() });
        if !open { return out; }
    }
    let tree = build_file_tree((0..paths.len()).collect(), |&index| paths[index].clone());
    level(&tree, paths, usize::from(root.is_some()), collapsed, &mut out);
    out
}

/// 树里的一个节点。调用方带什么额外字段（增删行数、状态）都行，泛型透传
#[derive(Debug, Clone, PartialEq)]
pub enum TreeNode<T> {
    File(TreeFile<T>),
    Dir(TreeDir<T>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreeFile<T> {
    /// 展示名：路径最后一段
    pub name: String,
    /// 仓库根相对的完整路径，也当视图的 key 用
    pub path: String,
    pub item: T,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreeDir<T> {
    /// 展示名。单子目录链会被压成一格，所以这里可能是 `server/src` 而不是 `src`
    /// ——见 [`build_file_tree`] 的注释。
    pub name: String,
    /// 这一层的完整路径前缀，当视图的 key 与展开状态的键用
    pub path: String,
    pub children: Vec<TreeNode<T>>,
    /// 子树里的文件总数（含更深层）
    pub file_count: usize,
}

impl<T> TreeNode<T> {
    pub fn name(&self) -> &str {
        match self {
            TreeNode::File(f) => &f.name,
            TreeNode::Dir(d) => &d.name,
        }
    }

    pub fn path(&self) -> &str {
        match self {
            TreeNode::File(f) => &f.path,
            TreeNode::Dir(d) => &d.path,
        }
    }
}

struct Draft<T> {
    /// 保持插入顺序（JS 的 Map），`finish` 里会重排
    dirs: Vec<(String, Draft<T>)>,
    files: Vec<TreeFile<T>>,
}

impl<T> Draft<T> {
    fn new() -> Self {
        Draft { dirs: Vec::new(), files: Vec::new() }
    }

    fn dir(&mut self, seg: &str) -> &mut Draft<T> {
        let i = match self.dirs.iter().position(|(n, _)| n == seg) {
            Some(i) => i,
            None => {
                self.dirs.push((seg.to_string(), Draft::new()));
                self.dirs.len() - 1
            }
        };
        &mut self.dirs[i].1
    }
}

/// 建树。
///
/// **单子目录链会被压缩成一格**：`packages/server/src/git/x.ts` 里的
/// `packages` 只有一个子目录、`server` 也只有一个，于是显示成
/// `packages` → `server/src` → `git`，而不是每层一格缩进。一个只有几个
/// 改动文件的仓库，不压缩的话大半个面板都在画空目录的缩进。
///
/// 顺序：目录在前、文件在后，各自按名字排（React 版用 localeCompare，中文路径才不会
/// 按码位乱序；这里的近似见 [`crate::js`]）。git 给的顺序本身是按路径排的，但树化
/// 之后必须重排。
pub fn build_file_tree<T>(items: Vec<T>, path_of: impl Fn(&T) -> String) -> Vec<TreeNode<T>> {
    let mut root: Draft<T> = Draft::new();
    for item in items {
        let path = path_of(&item);
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let Some((name, dirs)) = parts.split_last() else { continue };
        let name = name.to_string();
        let mut cur = &mut root;
        for seg in dirs {
            cur = cur.dir(seg);
        }
        cur.files.push(TreeFile { name, path: path.clone(), item });
    }
    finish(root, "")
}

fn finish<T>(draft: Draft<T>, prefix: &str) -> Vec<TreeNode<T>> {
    let mut dirs: Vec<TreeDir<T>> = draft
        .dirs
        .into_iter()
        .map(|(name, sub)| {
            let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            compact(name, sub, path)
        })
        .collect();
    dirs.sort_by(|a, b| locale_compare(&a.name, &b.name));
    let mut files = draft.files;
    files.sort_by(|a, b| locale_compare(&a.name, &b.name));
    dirs.into_iter().map(TreeNode::Dir).chain(files.into_iter().map(TreeNode::File)).collect()
}

/// 只有一个子目录、且本层没有文件时，把两层并成一格：`server` + `src` → `server/src`
fn compact<T>(name: String, draft: Draft<T>, path: String) -> TreeDir<T> {
    let mut label = name;
    let mut cur = draft;
    let mut full = path;
    while cur.files.is_empty() && cur.dirs.len() == 1 {
        let (child_name, child) = cur.dirs.pop().expect("len == 1");
        label = format!("{label}/{child_name}");
        full = format!("{full}/{child_name}");
        cur = child;
    }
    let children = finish(cur, &full);
    let file_count = count_files(&children);
    TreeDir { name: label, path: full, children, file_count }
}

fn count_files<T>(nodes: &[TreeNode<T>]) -> usize {
    nodes
        .iter()
        .map(|n| match n {
            TreeNode::File(_) => 1,
            TreeNode::Dir(d) => d.file_count,
        })
        .sum()
}

/// 列表视图里每个文件显示的目录（文件名右边那截灰字）。顶层文件没有目录，返回空串。
pub fn dir_of(path: &str) -> &str {
    path.rfind('/').map(|i| &path[..i]).unwrap_or("")
}

/// 路径最后一段
pub fn base_of(path: &str) -> &str {
    path.rfind('/').map(|i| &path[i + 1..]).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_rows_keep_indices_and_include_collapsed_subtree_selection() {
        let paths = ["z.txt", "src/b.txt", "src/a.txt"].map(String::from);
        assert_eq!(build_file_rows(&paths, FileViewMode::List, None, &HashSet::new()), vec![
            FileRow::File { index: 0, depth: 0, name_only: false },
            FileRow::File { index: 1, depth: 0, name_only: false },
            FileRow::File { index: 2, depth: 0, name_only: false },
        ]);
        let rows = build_file_rows(&paths, FileViewMode::Tree, Some("repo"), &HashSet::from(["src".into()]));
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], FileRow::Dir { name: "src".into(), path: "src".into(), depth: 1, open: false,
            paths: vec!["src/a.txt".into(), "src/b.txt".into()] });
        assert_eq!(rows[2], FileRow::File { index: 0, depth: 1, name_only: true });
        assert_eq!(build_file_rows(&paths, FileViewMode::Tree, Some("repo"), &HashSet::from([String::new()])).len(), 1);
    }

    #[test]
    fn tree_rows_expand_new_directories_and_compact_single_child_chains() {
        let paths = ["src/deep/a.txt", "src/deep/b.txt"].map(String::from);
        let rows = build_file_rows(&paths, FileViewMode::Tree, None, &HashSet::from(["old".into()]));
        assert!(matches!(&rows[0], FileRow::Dir { name, path, open: true, .. } if name == "src/deep" && path == "src/deep"));
        assert_eq!(rows[1], FileRow::File { index: 0, depth: 1, name_only: true });
    }

    fn tree(paths: &[&str]) -> Vec<TreeNode<String>> {
        build_file_tree(paths.iter().map(|p| p.to_string()).collect(), |p| p.clone())
    }

    /// 把树压成 `dir[...]` / `file` 的字符串，断言起来比嵌套结构好读
    fn shape(nodes: &[TreeNode<String>]) -> String {
        nodes
            .iter()
            .map(|n| match n {
                TreeNode::File(f) => f.name.clone(),
                TreeNode::Dir(d) => format!("{}[{}]", d.name, shape(&d.children)),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn compacts_a_single_child_directory_chain_into_one_row() {
        assert_eq!(
            shape(&tree(&["packages/server/src/git/command.ts", "packages/server/src/routes.ts"])),
            "packages/server/src[git[command.ts] routes.ts]"
        );
    }

    #[test]
    fn stops_compacting_where_the_tree_actually_branches() {
        assert_eq!(
            shape(&tree(&["packages/server/src/a.ts", "packages/web/src/b.ts"])),
            "packages[server/src[a.ts] web/src[b.ts]]"
        );
    }

    #[test]
    fn does_not_compact_past_a_directory_that_holds_files_of_its_own() {
        assert_eq!(shape(&tree(&["a/b/c.ts", "a/top.ts"])), "a[b[c.ts] top.ts]");
    }

    #[test]
    fn puts_directories_before_files_and_sorts_each_group_by_name() {
        assert_eq!(shape(&tree(&["z.ts", "a.ts", "dir/x.ts"])), "dir[x.ts] a.ts z.ts");
    }

    #[test]
    fn counts_files_across_the_whole_subtree_not_just_the_immediate_level() {
        let nodes = tree(&["a/b/1.ts", "a/b/2.ts", "a/c/3.ts"]);
        match &nodes[0] {
            TreeNode::Dir(a) => assert_eq!(a.file_count, 3),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn keeps_the_full_path_on_every_node_so_it_can_key_the_view_and_open_a_diff() {
        let nodes = tree(&["packages/web/src/api.ts"]);
        let TreeNode::Dir(top) = &nodes[0] else { panic!() };
        assert_eq!(top.path, "packages/web/src");
        assert_eq!(top.children[0].path(), "packages/web/src/api.ts");
    }

    #[test]
    fn handles_a_root_level_file_and_an_empty_input() {
        assert_eq!(shape(&tree(&["README.md"])), "README.md");
        assert!(tree(&[]).is_empty());
    }

    #[test]
    fn dir_of_base_of_split_a_nested_path() {
        assert_eq!(dir_of("packages/web/src/api.ts"), "packages/web/src");
        assert_eq!(base_of("packages/web/src/api.ts"), "api.ts");
    }

    #[test]
    fn dir_of_base_of_give_an_empty_directory_for_a_root_level_file() {
        assert_eq!(dir_of("README.md"), "");
        assert_eq!(base_of("README.md"), "README.md");
    }
}

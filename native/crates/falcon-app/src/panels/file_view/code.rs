//! 源码视图：gpui-component 的代码编辑器，只读（能选、能复制、⌘F 能搜，不能改——改文件是
//! 终端里的事）。语法高亮是 tree-sitter，颜色映射到主题派生出的语法色（`Ui::syntax`，与 web
//! 的 shiki css-variables 主题同一组变量），换主题不用重新解析。
//!
//! 与 web `lib/highlight.ts` 对齐的几条规则：
//! - 语言按路径认（扩展名 / 特殊文件名），认不出保持纯文本，不瞎猜；
//! - 超过 [`HIGHLIGHT_CAP`] 字符不高亮：再大的多半是生成物或日志，上色撑不起那个开销；
//! - 末尾一个换行吞掉（POSIX 文本以 `\n` 结尾，不吞就多一个幽灵空行）。

use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, IntoElement, Render, Subscription, Window};

use crate::theme::Ui;
use crate::zoom::zpx;

/// 超过这个字符数不高亮（web 的 HIGHLIGHT_CAP）
pub const HIGHLIGHT_CAP: usize = 512_000;

pub struct CodePane {
    editor: Entity<EditorState>,
    _theme: Subscription,
}

impl CodePane {
    pub fn new(text: &str, path: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let lang = if text.len() > HIGHLIGHT_CAP { None } else { lang_for_path(path) };
        let body = text.strip_suffix('\n').unwrap_or(text).to_string();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(lang.unwrap_or("text"))
                .line_number(true)
                .folding(false)
                .soft_wrap(false)
                .default_value(body)
        });
        // 语法色映射由 theme.rs 随主题一起装进组件库，这里只管换主题时重画
        let theme = cx.observe_global_in::<Ui>(window, |_, _, cx| cx.notify());
        Self { editor, _theme: theme }
    }
}

impl Render for CodePane {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // web 的源码视图是 text-xs（12px）等宽；只读 = 能选能复制，拒绝一切修改
        Editor::new(&self.editor).readonly(true).bordered(false).h_full().text_size(zpx(12.))
    }
}


/// 文件路径 → tree-sitter 语言名；认不出返回 None（保持纯文本）。
///
/// 规则照 web `langForPath`：先认特殊文件名，再按扩展名；`.env.*` 这类带后缀的变体算 dotenv
/// （这边没有 dotenv 语法，按纯文本）。这里只列 gpui-component 真带着语法的语言。
pub fn lang_for_path(path: &str) -> Option<&'static str> {
    let base = path.rsplit(['/', '\\']).next().unwrap_or("").to_lowercase();
    match base.as_str() {
        "makefile" | "gnumakefile" => return Some("make"),
        ".bashrc" | ".bash_profile" | ".zshrc" | ".zprofile" | ".zshenv" => return Some("bash"),
        "dockerfile" => return None,
        _ => {}
    }
    let dot = base.rfind('.')?;
    // dot == 0 同时排除 .gitignore 这类纯点开头（上面的文件名表没接住的）
    if dot == 0 || dot == base.len() - 1 {
        return None;
    }
    lang_for_word(&base[dot + 1..])
}

/// 扩展名 / Markdown fence 的语言词 → 语言名（web 的 ALIASES + LANGS 的交集）
pub fn lang_for_word(word: &str) -> Option<&'static str> {
    Some(match word.to_lowercase().as_str() {
        "ts" | "mts" | "cts" | "typescript" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" | "javascript" => "javascript",
        "json" | "jsonc" | "json5" => "json",
        "css" | "scss" | "less" => "css",
        "html" | "htm" | "xhtml" => "html",
        "md" | "markdown" | "mdx" => "markdown",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "py" | "pyi" | "python" => "python",
        "rs" | "rust" => "rust",
        "go" => "go",
        "sh" | "bash" | "zsh" | "shell" | "shellscript" => "bash",
        "sql" => "sql",
        "java" => "java",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "c++" => "cpp",
        "cs" | "csharp" => "csharp",
        "rb" | "ruby" => "ruby",
        "php" => "php",
        "kt" | "kts" | "kotlin" => "kotlin",
        "swift" => "swift",
        "lua" => "lua",
        "graphql" | "gql" => "graphql",
        "svelte" => "svelte",
        "diff" | "patch" => "diff",
        "zig" => "zig",
        "proto" => "proto",
        "make" | "mk" => "make",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_by_path() {
        assert_eq!(lang_for_path("src/main.rs"), Some("rust"));
        assert_eq!(lang_for_path("a/b/Index.TSX"), Some("tsx"));
        assert_eq!(lang_for_path("Makefile"), Some("make"));
        assert_eq!(lang_for_path(".zshrc"), Some("bash"));
        assert_eq!(lang_for_path(".gitignore"), None);
        assert_eq!(lang_for_path("notes."), None);
        assert_eq!(lang_for_path("LICENSE"), None);
        assert_eq!(lang_for_path("docs/README.md"), Some("markdown"));
    }

    #[test]
    fn fence_words() {
        assert_eq!(lang_for_word("TS"), Some("typescript"));
        assert_eq!(lang_for_word("sh"), Some("bash"));
        assert_eq!(lang_for_word("kdl"), None);
    }
}

//! 终端里粘贴 / 拖入图片的判定。对应旧 React 版的 `lib/pasteImage.ts` 的纯判定部分。
//!
//! 图片走上传 → 宿主机落盘 → 把路径粘进输入框（Claude Code 认输入框里的图片路径，
//! CONTEXT.md「Image Paste」）。判定规则：剪贴板里**有文本就贴文本**——Excel / 网页
//! 复制经常同时带 text 与渲染好的位图，用户要的是文本；截图与复制的图片文件没有
//! 纯文本，才当图片处理。
//!
//! React 版的输入是 DataTransfer；这里把它拆成最小面：剪贴板的纯文本 + 条目的
//! (kind, MIME) 列表，拖放的文件 MIME。读剪贴板 / 读文件是 app 层的事。
//!
//! GPUI 客户端眼下只用到 [`quote_for_prompt`]：剪贴板与拖放拿到的是 GPUI 的
//! `ClipboardEntry` / 本机路径，判定在 falcon-ui 的 `terminal/view.rs` 里按同样的规则写了。
//! DataTransfer 那一套留给浏览器版接 DOM 的 paste / drop 事件（rust-unification.md 附录 B）。

use crate::js::has_js_whitespace;

/// 剪贴板 / 拖放里的一项（DataTransferItem 的 kind 与 type）
pub trait TransferItem {
    /// `"file"` / `"string"`
    fn kind(&self) -> &str;
    fn mime(&self) -> &str;
}

/// 拖放进来的文件（只看 MIME）
pub trait TransferFile {
    fn mime(&self) -> &str;
}

/// `/^image\//`
pub fn is_image_type(mime: &str) -> bool {
    mime.starts_with("image/")
}

/// 粘贴里应当按图片处理的那一项（下标）；应走原有文本粘贴时返回 `None`。
///
/// - `text_plain`：剪贴板的 `text/plain`，没有或读不到给 `None` / 空串；
/// - `items`：剪贴板条目，`None` = 拿不到条目列表（React 版的 `!dt?.items`）。
pub fn image_from_clipboard<I: TransferItem>(text_plain: Option<&str>, items: Option<&[I]>) -> Option<usize> {
    let items = items?;
    if text_plain.is_some_and(|t| !t.is_empty()) {
        return None;
    }
    items.iter().position(|it| it.kind() == "file" && is_image_type(it.mime()))
}

/// 拖放里的图片文件；拖动是显式动作，不看文本、可以多张
pub fn images_from_drop<F: TransferFile>(files: Option<&[F]>) -> Vec<&F> {
    files.map(|fs| fs.iter().filter(|f| is_image_type(f.mime())).collect()).unwrap_or_default()
}

/// 粘进终端前给含空白的路径包上双引号（Windows 用户名带空格很常见），
/// 对齐终端拖拽文件的习惯——Claude Code 按这个约定剥引号。
pub fn quote_for_prompt(p: &str) -> String {
    if has_js_whitespace(p) { format!("\"{p}\"") } else { p.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Item(&'static str, &'static str);
    impl TransferItem for Item {
        fn kind(&self) -> &str {
            self.0
        }
        fn mime(&self) -> &str {
            self.1
        }
    }

    struct File(&'static str);
    impl TransferFile for File {
        fn mime(&self) -> &str {
            self.0
        }
    }

    #[test]
    fn treats_a_screenshot_as_an_image_paste() {
        let items = [Item("file", "image/png")];
        assert_eq!(image_from_clipboard(Some(""), Some(&items[..])), Some(0));
    }

    #[test]
    fn prefers_text_when_both_are_present() {
        let items = [Item("file", "image/png")];
        assert_eq!(image_from_clipboard(Some("A1\tB1"), Some(&items[..])), None);
    }

    #[test]
    fn ignores_plain_text_and_non_image_files() {
        let none: [Item; 0] = [];
        assert_eq!(image_from_clipboard(Some("hello"), Some(&none[..])), None);
        let pdf = [Item("file", "application/pdf")];
        assert_eq!(image_from_clipboard(Some(""), Some(&pdf[..])), None);
        assert_eq!(image_from_clipboard::<Item>(None, None), None);
    }

    #[test]
    fn drop_keeps_only_image_files_and_keeps_all_of_them() {
        let files = [File("image/png"), File("text/plain"), File("image/jpeg")];
        let kept: Vec<&str> = images_from_drop(Some(&files[..])).iter().map(|f| f.0).collect();
        assert_eq!(kept, ["image/png", "image/jpeg"]);
        assert!(images_from_drop::<File>(None).is_empty());
    }

    #[test]
    fn quotes_only_when_the_path_contains_whitespace() {
        assert_eq!(quote_for_prompt("/home/u/.falcon/paste/img-1.png"), "/home/u/.falcon/paste/img-1.png");
        assert_eq!(quote_for_prompt("C:\\Users\\a b\\.falcon\\paste\\i.png"), "\"C:\\Users\\a b\\.falcon\\paste\\i.png\"");
    }
}

//! Ghostty 主题 vendoring（原 scripts/vendor-ghostty-themes.mjs）。
//!
//! 把 Ghostty 内置主题（iTerm2-Color-Schemes 的 ghostty/ 目录）压成 falcon-theme 的内置目录：
//! `native/crates/falcon-theme/data/ghostty-themes.tsv`（数据）+ `ghostty-themes.meta.rs`
//! （来源与条数）+ `LICENSE.txt`。
//!
//! ```text
//! cargo xtask vendor-themes                # 优先用本机 Ghostty.app 里的主题
//! cargo xtask vendor-themes --from <目录>  # 指定主题目录
//! cargo xtask vendor-themes --github       # 从 GitHub 稀疏克隆
//! ```
//!
//! 本机 Ghostty.app 的主题目录就是"用户装的那个 Ghostty 认的主题"，名字与
//! `ghostty +list-themes` 一字不差，优先用它；没装 Ghostty 才去 GitHub 拉。
//! 产物每行一个主题：`名字 \t bg fg cursor cursorText selBg selFg p0..p15`，
//! 22 个不带 # 的 rrggbb 用空格隔开——比 JSON 小一半，解析在 falcon-theme 的 catalog.rs
//! （坏行直接报错，测试按 meta 里的条数核对行数）。重新 vendor 之后跑一遍
//! `cargo test -p falcon-theme`：派生结果的金标准 fixture 是删 React 前端时冻结的，
//! 新增的主题不在里面，条数变了要同步更新那份 fixture 的期望。
//!
//! 产物与 JS 版逐字节相同（移植时拿 Ghostty.app 1.3.1 实跑核对过 tsv / LICENSE），
//! meta.rs 只有头一行的"由谁生成"改了。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use icu_collator::options::CollatorOptions;
use icu_collator::{Collator, CollatorBorrowed};
use icu_locale_core::locale;

use crate::util::{self, TempDir, download, native, output, root, write_reporting};

const APP_THEMES: &str = "/Applications/Ghostty.app/Contents/Resources/ghostty/themes";
const APP_PLIST: &str = "/Applications/Ghostty.app/Contents/Info.plist";
const REPO: &str = "https://github.com/mbadolato/iTerm2-Color-Schemes";
const LICENSE_URL: &str = "https://raw.githubusercontent.com/mbadolato/iTerm2-Color-Schemes/master/LICENSE";

/// 键的顺序就是产物里 22 个颜色的顺序，falcon-theme 的 catalog.rs 按同一顺序解析
const KEYS: [&str; 6] = [
    "background",
    "foreground",
    "cursor-color",
    "cursor-text",
    "selection-background",
    "selection-foreground",
];

fn out_dir() -> PathBuf {
    native().join("crates/falcon-theme/data")
}

pub fn run(args: &[String]) -> Result<()> {
    let mut from: Option<String> = None;
    let mut github = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--from" => from = Some(it.next().context("--from 后面要跟主题目录")?.clone()),
            "--github" => github = true,
            other => bail!("未知参数 {other}"),
        }
    }

    // 克隆出来的临时目录要活到读完主题为止
    let mut _clone: Option<TempDir> = None;
    // --from 与 --github 同时给时 --from 优先（与 JS 版一致）
    let (dir, origin) = if let Some(dir) = from {
        // 来源原样写进 meta（JS 版就是命令行上给的那个字符串，不做绝对化）
        (PathBuf::from(&dir), dir)
    } else if !github && Path::new(APP_THEMES).is_dir() {
        let version = ghostty_version().unwrap_or_else(|| "?".into());
        (PathBuf::from(APP_THEMES), format!("Ghostty.app {version}"))
    } else {
        let tmp = TempDir::new("ghostty-themes")?;
        let (dir, commit) = sparse_clone(tmp.path())?;
        _clone = Some(tmp);
        (dir, format!("{REPO} @ {commit}"))
    };

    let names = sorted_theme_names(&dir)?;
    let mut tsv = String::new();
    for name in &names {
        if name.contains(['\t', '\n', '\r', '"', '\\']) {
            bail!("主题名含 TSV / Rust 字面量容不下的字符：{name}");
        }
        let bytes = fs::read(dir.join(name)).with_context(|| format!("读不了主题 {name}"))?;
        // Node 的 readFileSync(…, "utf8") 遇到坏字节也是替换成 U+FFFD，不报错
        let colors = parse_theme(name, &String::from_utf8_lossy(&bytes))?;
        tsv.push_str(name);
        tsv.push('\t');
        tsv.push_str(&colors.join(" "));
        tsv.push('\n');
    }

    let out = out_dir();
    write_reporting(&out.join("ghostty-themes.tsv"), tsv.as_bytes())?;
    // 来源用 Rust 的 Debug 转义写成字面量（JS 版用 JSON.stringify：对普通来源两者输出一样，
    // 但 JSON 的 \u00XX 不是合法的 Rust 转义，Debug 的 \u{..} 才是）
    let meta = format!(
        "// 由 cargo xtask vendor-themes 生成，勿手改。\n\
         \n\
         /// 内置 Ghostty 主题的来源（设置页显示用）\n\
         pub const GHOSTTY_THEMES_ORIGIN: &str = {origin:?};\n\
         /// 内置 Ghostty 主题条数；测试按它核对 data/ghostty-themes.tsv 的行数\n\
         pub const GHOSTTY_THEMES_COUNT: usize = {};\n",
        names.len()
    );
    write_reporting(&out.join("ghostty-themes.meta.rs"), meta.as_bytes())?;
    println!("{} 个主题，来源 {origin}", names.len());
    fetch_license(&out.join("LICENSE.txt"))
}

fn ghostty_version() -> Option<String> {
    output("/usr/libexec/PlistBuddy", ["-c", "Print CFBundleShortVersionString", APP_PLIST], None)
        .ok()
        .map(|s| s.trim().to_string())
}

/// 稀疏克隆只取 ghostty/ 目录；返回 (主题目录, 短 commit)
fn sparse_clone(tmp: &Path) -> Result<(PathBuf, String)> {
    let repo = tmp.join("repo");
    println!("稀疏克隆 {REPO} 到 {}", repo.display());
    util::run(
        "git",
        ["clone", "--depth", "1", "--filter=blob:none", "--sparse", REPO, &repo.to_string_lossy()],
        None,
    )?;
    util::run("git", ["sparse-checkout", "set", "ghostty"], Some(&repo))?;
    let commit = output("git", ["rev-parse", "--short", "HEAD"], Some(&repo))?.trim().to_string();
    Ok((repo.join("ghostty"), commit))
}

/// 目录里的主题文件名（跳过点文件与子目录），按 JS 的 `a.localeCompare(b, "en")` 排序。
///
/// localeCompare 走的是 ICU 的 en 排序（= CLDR 根排序：三级强度，标点 / 空格不忽略），
/// 这里用 ICU4X 的同一套数据。字节序排不出同样的结果：ICU 先不分大小写地比字母
/// （大小写只在第三级才分），所以 `branch` 夹在 `Box` 与 `Breadog` 之间、`CGA` 在
/// `Catppuccin Mocha` 之后、`iTerm2 …` 在 `IRIX Terminal` 与 `Jackie Brown` 之间；
/// 字节序会把小写开头的全排到最末，`CGA` / `HaX0R …` 这种大写字母也会提前。
/// 移植时拿 `--github` 那份（769 个名字，带撇号）与 Node 26 的 localeCompare 逐个对过，零差异。
fn sorted_theme_names(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("读不了主题目录 {}", dir.display()))? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|n| anyhow::anyhow!("主题名不是 UTF-8：{}", n.to_string_lossy()))?;
        // statSync 跟随符号链接：链到文件的也算主题
        if name.starts_with('.') || !fs::metadata(entry.path()).map(|m| m.is_file()).unwrap_or(false) {
            continue;
        }
        names.push(name);
    }
    let collator = en_collator()?;
    // ICU 判相等（只差可忽略字符）时 JS 的稳定排序保留 readdir 的顺序，那个顺序随文件系统变；
    // 这里退到字节序，产物不随机器变。真实数据里没有这种名字
    names.sort_by(|a, b| collator.compare(a, b).then_with(|| a.cmp(b)));
    Ok(names)
}

fn en_collator() -> Result<CollatorBorrowed<'static>> {
    // Intl.Collator("en") 的默认值：usage sort、sensitivity variant（三级）、不忽略标点、
    // 不按数值比、caseFirst 跟 locale（en 没有定制）——都是 CollatorOptions::default()
    Collator::try_new(locale!("en").into(), CollatorOptions::default()).map_err(|e| anyhow::anyhow!("ICU 排序器：{e}"))
}

/// 只认内置主题这种规整写法（每个键都有、全是 #rrggbb）。用户手写主题的宽松解析
/// （不带 #、X11 颜色名、cell-foreground 之类特殊值）在 falcon-theme 的 ghostty.rs。
fn parse_theme(name: &str, text: &str) -> Result<Vec<String>> {
    let mut values: [Option<String>; 6] = Default::default();
    let mut palette: [Option<String>; 16] = Default::default();
    for raw in text.split('\n') {
        let line = js_trim(raw);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (js_trim(key), js_trim(value));
        if key == "palette" {
            // /^(\d+)\s*=\s*#?([0-9a-fA-F]{6})$/
            let (idx, hex) = parse_palette_value(value).with_context(|| format!("{name}: 看不懂的 palette 行「{raw}」"))?;
            // 16 以上的扩展色不进目录（JS 的 Number() 对超长数字给 1e20 之类，同样 > 15）
            if let Some(slot) = idx.filter(|&i| i <= 15) {
                palette[slot] = Some(hex);
            }
        } else if let Some(i) = KEYS.iter().position(|k| *k == key) {
            let hex = parse_hex6(value.strip_prefix('#').unwrap_or(value))
                .with_context(|| format!("{name}: {key} 不是 #rrggbb：「{value}」"))?;
            values[i] = Some(hex);
        }
    }
    let mut out = Vec::with_capacity(22);
    for (key, v) in KEYS.iter().zip(values) {
        out.push(v.with_context(|| format!("{name}: 缺 {key}"))?);
    }
    for (i, v) in palette.into_iter().enumerate() {
        out.push(v.with_context(|| format!("{name}: 缺 palette {i}"))?);
    }
    Ok(out)
}

/// palette 的值 `N = #rrggbb`：返回 (下标, 小写 hex)；下标超出 usize 给 None（当成 > 15 跳过）
fn parse_palette_value(value: &str) -> Option<(Option<usize>, String)> {
    let digits_end = value.find(|c: char| !c.is_ascii_digit()).unwrap_or(value.len());
    if digits_end == 0 {
        return None;
    }
    let idx = value[..digits_end].parse::<usize>().ok();
    let rest = value[digits_end..].trim_start_matches(is_js_whitespace).strip_prefix('=')?;
    let rest = rest.trim_start_matches(is_js_whitespace);
    let hex = parse_hex6(rest.strip_prefix('#').unwrap_or(rest))?;
    Some((idx, hex))
}

/// 恰好 6 位十六进制（整串），转小写
fn parse_hex6(s: &str) -> Option<String> {
    (s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

/// ECMAScript 的 WhiteSpace + LineTerminator（`trim()` 与正则 `\s` 的集合）：含 U+FEFF、
/// 不含 U+0085，与 `str::trim` 不同。同 falcon-theme 的 js.rs，xtask 不为这一个函数依赖它
fn is_js_whitespace(c: char) -> bool {
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

fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// LICENSE 下载失败时沿用已有的副本（离线也能重新 vendor），没有副本才算失败
fn fetch_license(dest: &Path) -> Result<()> {
    let tmp = TempDir::new("ghostty-license")?;
    let file = tmp.path().join("LICENSE");
    match download(LICENSE_URL, &file).and_then(|()| Ok(fs::read(&file)?)) {
        Ok(bytes) => write_reporting(dest, &bytes),
        Err(e) if dest.exists() => {
            let rel = dest.strip_prefix(root()).unwrap_or(dest);
            eprintln!("LICENSE 下载失败（{e:#}），沿用已有的 {}", rel.display());
            Ok(())
        }
        Err(e) => Err(e.context("LICENSE 下载失败且本地没有副本")),
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;

    #[test]
    fn palette_value() {
        assert_eq!(parse_palette_value("0=#1A2b3C"), Some((Some(0), "1a2b3c".into())));
        assert_eq!(parse_palette_value("15 = 000000"), Some((Some(15), "000000".into())));
        assert_eq!(parse_palette_value("99999999999999999999999=#000000"), Some((None, "000000".into())));
        assert_eq!(parse_palette_value("=#000000"), None);
        assert_eq!(parse_palette_value("1=#00000"), None);
        assert_eq!(parse_palette_value("1=#0000000"), None);
    }

    #[test]
    fn collation_matches_locale_compare() {
        // 取自真实目录：字节序会把每一对都排反
        let c = en_collator().unwrap();
        for (a, b) in [
            ("branch", "Breadog"),
            ("Catppuccin Mocha", "CGA"),
            ("Havn Skumring", "HaX0R Blue"),
            ("iTerm2 Tango Light", "Jackie Brown"),
        ] {
            assert_eq!(c.compare(a, b), Ordering::Less, "{a} < {b}");
            assert_eq!(a.cmp(b), Ordering::Greater, "字节序 {a} > {b}");
        }
    }
}

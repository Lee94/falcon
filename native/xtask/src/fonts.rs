//! 字体 vendoring（原 scripts/vendor-berkeley-mono.mjs / vendor-ioskeley-mono.mjs /
//! vendor-maple-mono.mjs / vendor-nerd-symbols.mjs）。
//!
//! ```text
//! cargo xtask vendor-fonts berkeley [zip] [--out <目录>]
//! cargo xtask vendor-fonts ioskeley [--out <目录>]
//! cargo xtask vendor-fonts maple    [--out <目录>]
//! cargo xtask vendor-fonts nerd     [--out <目录>]
//! ```
//!
//! 产物落在 `native/assets/fonts/<字族>/`、已进仓库：falcon-app 的 build.rs 把这里的 woff2
//! 解成 TTF（原生嵌进二进制，浏览器版按需拉）。只在升级字库时跑。`--out` 把产物写到别处——
//! 试跑 / 比对时用，免得直接覆盖仓库里那份。
//!
//! 外部工具（都不是 JS，缺了先报、不白下载）：
//! - TTF → WOFF2：Google 的 `woff2_compress`。JS 版用的 npm ttf2woff2 / wawoff2 就是同一个
//!   编码器编成的 addon / wasm，默认参数一样（brotli 11、开 glyf / loca 变换）；编码器的 build
//!   不同，产物不保证与仓库里的逐字节相同，字形覆盖一样。
//! - Ioskeley 的拉丁子集：HarfBuzz 的 `hb-subset`。JS 版的 npm subset-font 是 HarfBuzz 编成的
//!   wasm，参数怎么对上的见 [`ioskeley`]。
//! - 解 zip：`unzip`（macOS / 多数 Linux 自带）；下载：curl。

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::util::{self, TempDir, download, native, output, require_tool, root, write_reporting};

const USAGE: &str = "用法：cargo xtask vendor-fonts <berkeley|ioskeley|maple|nerd> [参数]
  berkeley [zip] [--out <目录>]   从本机持有的 Berkeley Mono TX-02 发行包抽四切（默认找仓库根的 Berkeley-Mono-TX-02-*.zip）
  ioskeley [--out <目录>]         下载 IoskeleyMono，切拉丁子集
  maple    [--out <目录>]         下载 Maple Mono NL NF CN 的 Regular
  nerd     [--out <目录>]         下载 Symbols Nerd Font Mono
--out 把产物写到别的目录（默认 native/assets/fonts/<字族>/）";

const WOFF2_HINT: &str = "TTF → WOFF2 要 Google 的 woff2 工具：brew install woff2（Debian / Ubuntu：apt install woff2）";
const HB_SUBSET_HINT: &str = "切子集要 HarfBuzz 的 hb-subset：brew install harfbuzz（Debian / Ubuntu：apt install libharfbuzz-bin）";
const UNZIP_HINT: &str = "macOS 自带；Linux：apt install unzip";

pub fn run(args: &[String]) -> Result<()> {
    let Some((family, rest)) = args.split_first() else { bail!("{USAGE}") };
    let mut out: Option<PathBuf> = None;
    let mut positional = Vec::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = Some(it.next().context("--out 后面要跟目录")?.into()),
            s if s.starts_with("--") => bail!("未知参数 {s}\n\n{USAGE}"),
            s => positional.push(s),
        }
    }
    let out_for = |dir: &str| out.clone().unwrap_or_else(|| native().join("assets/fonts").join(dir));
    let no_positional = || match positional.first() {
        Some(p) => bail!("{family} 不收位置参数：{p}\n\n{USAGE}"),
        None => Ok(()),
    };
    match family.as_str() {
        "berkeley" => {
            if positional.len() > 1 {
                bail!("berkeley 只收一个 zip 路径\n\n{USAGE}");
            }
            berkeley(positional.first().copied(), &out_for("berkeley-mono"))
        }
        "ioskeley" => no_positional().and_then(|()| ioskeley(&out_for("ioskeley-mono"))),
        "maple" => no_positional().and_then(|()| maple(&out_for("maple-mono"))),
        "nerd" => no_positional().and_then(|()| nerd(&out_for("nerd-symbols"))),
        other => bail!("不认识的字族 {other}\n\n{USAGE}"),
    }
}

// ---- Berkeley Mono -------------------------------------------------------------------------

/// 从本机持有的 Berkeley Mono TX-02 zip 抽出 Regular / Bold / Oblique / BoldOblique，压成 woff2。
///
/// 默认读仓库根目录的 Berkeley-Mono-TX-02-*-FONToMASS.zip。zip 是 U.S. Graphics 的商业
/// 发行物，不进仓库；产物 woff2 作为产品内嵌默认字体进二进制。
///
/// 只要这四切：界面的 font-medium（500）落到 Regular，font-semibold（600）落到 Bold。
/// Medium / SemiBold 那几份几乎一样大，加进来只是多一倍下载。Oblique 当 italic 挂，
/// 终端 ANSI 斜体才能命中真切，而不是渲染端把 Regular 拉斜。
fn berkeley(zip_arg: Option<&str>, out: &Path) -> Result<()> {
    const ZIP_PREFIX: &str = "Berkeley Mono TX-02/TX-02 2.002/";
    const FACES: [&str; 4] = ["TX-02-Regular.ttf", "TX-02-Bold.ttf", "TX-02-Oblique.ttf", "TX-02-BoldOblique.ttf"];
    const NOTICE: &str = "Berkeley Mono TX-02 (2.002)
Copyright (c) 2022-2024, U.S. Graphics LLC. All Rights Reserved.
https://usgraphics.com/products/berkeley-mono

商业字体，只作为本产品内嵌默认字体分发。升级字库时在 native/ 下用
  cargo xtask vendor-fonts berkeley <zip>
从官方发行包重新抽出，不要把 zip 提交进仓库。
";

    // 先找 zip：多数机器上没有这份商业发行包，这是最该先说清楚的错
    let zip = find_berkeley_zip(zip_arg)?;
    require_tool("unzip", UNZIP_HINT)?;
    require_tool("woff2_compress", WOFF2_HINT)?;

    let tmp = TempDir::new("berkeley")?;
    println!("源 zip {}", zip.file_name().unwrap_or(zip.as_os_str()).to_string_lossy());
    let entries: Vec<String> = FACES.iter().map(|f| format!("{ZIP_PREFIX}{f}")).collect();
    let ttfs = unzip_flat(&zip, &entries, tmp.path())?;

    write_reporting(&out.join("NOTICE.txt"), NOTICE.as_bytes())?;
    for (face, ttf) in FACES.iter().zip(&ttfs) {
        let woff2 = ttf_to_woff2(ttf)?;
        let name = face.replace(".ttf", ".woff2");
        write_reporting(&out.join(&name), &woff2)?;
        println!("  {face} {} → {name} {}", kb(fs::metadata(ttf)?.len()), kb(woff2.len() as u64));
    }
    println!("已写入 {}", out.display());
    Ok(())
}

/// 命令行给了就用那个（相对当前目录）；没给就在仓库根找 Berkeley-Mono-TX-02-*.zip（不分大小写），
/// 有多份取文件名排序的最后一份（版本号大的）
fn find_berkeley_zip(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        let abs = std::path::absolute(p)?;
        if !abs.is_file() {
            bail!("找不到 zip：{}", abs.display());
        }
        return Ok(abs);
    }
    let root = root();
    let mut matches: Vec<String> = fs::read_dir(&root)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("berkeley-mono-tx-02-") && l.ends_with(".zip")
        })
        .collect();
    matches.sort();
    match matches.pop() {
        Some(name) => Ok(root.join(name)),
        None => bail!(
            "仓库根目录（{}）没有 Berkeley-Mono-TX-02-*.zip：把 U.S. Graphics 的发行包放在那里，\
             或把路径当参数传入（cargo xtask vendor-fonts berkeley <zip>）",
            root.display()
        ),
    }
}

// ---- IoskeleyMono --------------------------------------------------------------------------

/// IoskeleyMono 官方 Release 的 IoskeleyMonoTerm Nerd Font Mono Regular，子集化后压成 woff2。
///
/// IoskeleyMono = Iosevka 的自定义 build（Ioskeley = Iosevka + Berkeley），照着 Berkeley Mono
/// 的骨架调的；Term 变体把符号窄化成 1 格，正是终端要的。
/// https://github.com/ahatem/IoskeleyMono —— SIL OFL 1.1，全文见同目录 LICENSE.txt
///
/// 上游 zip 一份 4.7MB，18037 个码位，其中 10523 个是 Nerd Font 的 PUA 图标。图标区**整个剔除**：
/// 终端字体栈里 Symbols Nerd Font Mono 排在它前面，这份内嵌图标从来不会被用到（图标统一由
/// Symbols Nerd Font 出）。剩下按 [`IOSKELEY_RANGES`] 切，只留终端真正会画的字形。
///
/// 只要 Regular：Bold / Italic 由渲染端合成，等宽字体的合成粗体不改变字符步进，省掉再来
/// 19 个字重的下载。CJK 上游本来就没有，落到栈后面的 Maple。
///
/// 子集参数与 JS 版一一对应：subset-font 不带选项时只做两件事——把 layout feature 集合
/// 清空再取反（= `--layout-features=*`，保留全部 OpenType 特性，默认只留一小撮）、把文本里的
/// 每个码位加进 unicode 集合（= `--unicodes=<范围表>`，JS 版是把范围展开成字符串再喂进去，
/// 码位集合完全一样，含 U+0000–001F 这些控制字符）。其余（name ID、hinting、丢哪些表、
/// layout closure）两边都是 HarfBuzz 的库默认值；只是 subset-font 2.9 内嵌的 harfbuzzjs 与
/// 本机 hb-subset 版本不同，默认丢表之类的细节可能随版本略有出入，不影响字形覆盖。
fn ioskeley(out: &Path) -> Result<()> {
    const VERSION: &str = "v2.1.0";
    const ENTRY: &str = "Normal/IoskeleyMonoTermNerdFontMono-Regular.ttf";
    const OUT_FILE: &str = "IoskeleyMonoTermNFM-Latin.woff2";
    let zip_url = format!("https://github.com/ahatem/IoskeleyMono/releases/download/{VERSION}/IoskeleyMono-Term-NerdFont.zip");
    let license_url = format!("https://raw.githubusercontent.com/ahatem/IoskeleyMono/{VERSION}/LICENSE");

    require_tool("unzip", UNZIP_HINT)?;
    require_tool("hb-subset", HB_SUBSET_HINT)?;
    require_tool("woff2_compress", WOFF2_HINT)?;

    let tmp = TempDir::new("ioskeley")?;
    let zip = tmp.path().join("ioskeley.zip");
    download(&zip_url, &zip)?;
    fetch_text(&license_url, &out.join("LICENSE.txt"))?;

    let ttf = unzip_flat(&zip, &[ENTRY.to_string()], tmp.path())?.remove(0);
    println!("源字体 {}：{}", file_name(&ttf), kb(fs::metadata(&ttf)?.len()));

    let subset = tmp.path().join("subset.ttf");
    let unicodes = IOSKELEY_RANGES
        .iter()
        .map(|&(lo, hi)| if lo == hi { format!("{lo:x}") } else { format!("{lo:x}-{hi:x}") })
        .collect::<Vec<_>>()
        .join(",");
    let mut output_file = OsString::from("--output-file=");
    output_file.push(&subset);
    let args: [OsString; 4] = [
        "--layout-features=*".into(),
        format!("--unicodes={unicodes}").into(),
        output_file,
        ttf.clone().into(),
    ];
    util::run("hb-subset", &args, None)?;

    let woff2 = ttf_to_woff2(&subset)?;
    write_reporting(&out.join(OUT_FILE), &woff2)?;
    println!("→ {OUT_FILE}：{}", kb(woff2.len() as u64));
    println!("已写入 {}", out.display());
    Ok(())
}

/// Ioskeley 的保留范围（闭区间）。与原 Maple 拉丁子集（已随 React 前端删除）同一套取舍——
/// 终端真会画的骨架字形。PUA 不在表里就是刻意的。只有一片，没有按需分片可言；表里没覆盖到的
/// 字符按 cmap 落空后自动回退到字体栈下一位（Maple / 系统 CJK / emoji）。
const IOSKELEY_RANGES: &[(u32, u32)] = &[
    (0x0000, 0x024f), // Basic Latin + Latin-1 + Extended-A/B
    (0x0250, 0x02af), // IPA（man 页偶尔用）
    (0x0370, 0x03ff), // 希腊（数学 / 日志常见）
    (0x0400, 0x04ff), // 西里尔
    (0x1e00, 0x1eff), // Latin Extended Additional
    (0x2000, 0x206f), // 通用标点
    (0x2070, 0x209f), // 上下标
    (0x20a0, 0x20cf), // 货币
    (0x2100, 0x218f), // 字母式符号 + 数字形式
    (0x2190, 0x21ff), // 箭头
    (0x2200, 0x22ff), // 数学运算符
    (0x2300, 0x23ff), // 杂项技术（⌘ ⏎ 等）
    (0x2400, 0x243f), // 控制图片
    (0x2500, 0x257f), // 制表符（TUI 边框）
    (0x2580, 0x259f), // 方块元素（进度条）
    (0x25a0, 0x25ff), // 几何图形
    (0x2600, 0x27bf), // 杂项符号 + 装饰符号
    (0x2b00, 0x2bff), // 杂项符号与箭头
    (0xfffd, 0xfffd), // replacement character
];

// ---- Maple Mono ----------------------------------------------------------------------------

/// maple-font 官方 Release 的 Maple Mono NL NF CN，只抽 Regular 压成 woff2。
///
/// 完整 CN 包上百 MB，不能整包提交。粗体由渲染端合成（等宽字体的合成粗体不改变字符步进），
/// 省一份 6.3MB 的下载。falcon-app 的 build.rs 把它解成 TTF：原生客户端嵌进二进制，浏览器版
/// 运行时按需拉。
fn maple(out: &Path) -> Result<()> {
    const VERSION: &str = "v7.9";
    /// zip 里的路径带不带目录层随发行版变，按文件名结尾（不分大小写）认
    const WANTED: &str = "MapleMonoNL-NF-CN-Regular.ttf";
    const OUT_FILE: &str = "MapleMonoNL-NF-CN-Regular.woff2";
    let zip_url =
        format!("https://github.com/subframe7536/maple-font/releases/download/{VERSION}/MapleMonoNL-NF-CN-unhinted.zip");
    let ofl_url = format!("https://raw.githubusercontent.com/subframe7536/maple-font/{VERSION}/OFL.txt");

    require_tool("unzip", UNZIP_HINT)?;
    require_tool("woff2_compress", WOFF2_HINT)?;

    let tmp = TempDir::new("maple")?;
    let zip = tmp.path().join("maple.zip");
    download(&zip_url, &zip)?;
    fetch_text(&ofl_url, &out.join("OFL.txt"))?;

    let wanted = WANTED.to_ascii_lowercase();
    let entry = zip_entries(&zip)?
        .into_iter()
        .find(|n| n.replace('\\', "/").to_ascii_lowercase().ends_with(&wanted))
        .with_context(|| format!("zip 里找不到 *{WANTED}"))?;
    let ttf = unzip_flat(&zip, &[entry], tmp.path())?.remove(0);

    println!("压缩 {} → {OUT_FILE}", file_name(&ttf));
    let woff2 = ttf_to_woff2(&ttf)?;
    write_reporting(&out.join(OUT_FILE), &woff2)?;
    println!("  {}", kb(woff2.len() as u64));
    println!("已写入 {}", out.display());
    Ok(())
}

// ---- Symbols Nerd Font ---------------------------------------------------------------------

/// Nerd Fonts 官方 Release 的 Symbols Nerd Font Mono，压成 woff2；LICENSE 一并抽出。
fn nerd(out: &Path) -> Result<()> {
    const VERSION: &str = "v3.5.0";
    const TTF: &str = "SymbolsNerdFontMono-Regular.ttf";
    const OUT_FILE: &str = "SymbolsNerdFontMono-Regular.woff2";
    let zip_url = format!("https://github.com/ryanoasis/nerd-fonts/releases/download/{VERSION}/NerdFontsSymbolsOnly.zip");

    require_tool("unzip", UNZIP_HINT)?;
    require_tool("woff2_compress", WOFF2_HINT)?;

    let tmp = TempDir::new("nerd")?;
    let zip = tmp.path().join("nf.zip");
    download(&zip_url, &zip)?;
    // LICENSE 没有也不拦（JS 版就是"有才拷"）
    let has_license = zip_entries(&zip)?.iter().any(|n| n == "LICENSE");
    let mut wanted = vec![TTF.to_string()];
    if has_license {
        wanted.push("LICENSE".to_string());
    }
    let files = unzip_flat(&zip, &wanted, tmp.path())?;
    let ttf = &files[0];
    match files.get(1) {
        Some(license) => write_reporting(&out.join("LICENSE.txt"), &fs::read(license)?)?,
        None => eprintln!("zip 里没有 LICENSE，沿用已有的 LICENSE.txt"),
    }

    println!("压缩 {TTF}");
    let woff2 = ttf_to_woff2(ttf)?;
    write_reporting(&out.join(OUT_FILE), &woff2)?;
    println!("  {}", kb(woff2.len() as u64));
    Ok(())
}

// ---- 共用 ----------------------------------------------------------------------------------

/// TTF → WOFF2。woff2_compress 只认一个文件参数、产物固定写成同目录的 `<去扩展名>.woff2`
/// （没有 stdin / -o），stdout 还要打一行 Processing…：拷进独立的临时目录再压、读回字节，
/// 不在解压目录里和别的文件抢名字。
fn ttf_to_woff2(ttf: &Path) -> Result<Vec<u8>> {
    let tmp = TempDir::new("woff2")?;
    let input = tmp.path().join("font.ttf");
    fs::copy(ttf, &input).with_context(|| format!("拷 {}", ttf.display()))?;
    output("woff2_compress", [&input], None)?;
    let woff2 = fs::read(tmp.path().join("font.woff2")).context("woff2_compress 没有产出 font.woff2")?;
    if !woff2.starts_with(b"wOF2") {
        bail!("woff2_compress 的产物不是 WOFF2（开头不是 wOF2）：{}", ttf.display());
    }
    Ok(woff2)
}

/// zip 里的条目名。`unzip -Z1` 是 zipinfo 模式、一行一个名字——比 JS 版解析 `unzip -l` 的列稳，
/// Berkeley 那种带空格的路径也不会被切断
fn zip_entries(zip: &Path) -> Result<Vec<String>> {
    let listing = output("unzip", [OsStr::new("-Z1"), zip.as_os_str()], None)?;
    Ok(listing.lines().map(str::to_string).collect())
}

/// 把 zip 里的几个条目平铺解到 `dir`（-j 丢掉目录层，-o 覆盖不问，-q 不刷 inflating 行），
/// 按 `entries` 的顺序返回解出来的文件。条目先对着清单核一遍：unzip 对不存在的条目只打一句
/// "filename not matched" 再以 11 退出，不如这里直接说缺哪个。
///
/// unzip 把条目参数当通配符（`*` `?` `[`），这几个字体包的路径里都没有，不另转义。
fn unzip_flat(zip: &Path, entries: &[String], dir: &Path) -> Result<Vec<PathBuf>> {
    let listing = zip_entries(zip)?;
    for e in entries {
        if !listing.iter().any(|n| n == e) {
            bail!("{} 里找不到 {e}", file_name(zip));
        }
    }
    let mut args: Vec<OsString> = vec!["-qjo".into(), zip.into()];
    args.extend(entries.iter().map(OsString::from));
    args.extend(["-d".into(), dir.into()]);
    util::run("unzip", &args, None)?;
    entries
        .iter()
        .map(|e| {
            // Windows 上打的 zip 可能用反斜杠分目录，unzip -j 照样只留最后一段
            let base = e.rsplit(['/', '\\']).next().unwrap_or(e);
            let path = dir.join(base);
            if path.is_file() { Ok(path) } else { bail!("解压后找不到 {base}（条目 {e}）") }
        })
        .collect()
}

/// 下载一个文本文件（LICENSE / OFL）到产物目录，报告新建 / 未变 / 更新
fn fetch_text(url: &str, dest: &Path) -> Result<()> {
    let tmp = TempDir::new("font-license")?;
    let file = tmp.path().join("download");
    download(url, &file)?;
    write_reporting(dest, &fs::read(&file)?)
}

fn file_name(p: &Path) -> String {
    p.file_name().unwrap_or(p.as_os_str()).to_string_lossy().into_owned()
}

fn kb(n: u64) -> String {
    format!("{} KB", (n as f64 / 1024.0).round())
}

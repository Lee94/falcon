//! 应用更新里与网络、磁盘无关的部分：版本比较、GitHub Release 里选哪一个安装包、
//! 下载地址是不是这次发布的、把新的应用目录换到目标位置。
//!
//! 安装包文件名与 `cargo xtask pkg` / `cargo xtask win` 的产物一致。发布仓库是
//! GitHub Actions 在 `v*` tag 上挂安装包的那个仓库。

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// 打安装包并挂到 GitHub Release 的仓库（`.github/workflows/package.yml`）
pub const RELEASE_REPO: &str = "Lee94/falcon";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

/// `v0.3.19` / `0.3.19`。预发布（`0.3.20-rc.1`）与构建元数据不认：
/// `/releases/latest` 本来就跳过预发布，标错了也不当成稳定版往下装
pub fn parse_version(text: &str) -> Option<Version> {
    let text = text.trim().strip_prefix('v').unwrap_or(text.trim());
    if text.contains('-') || text.contains('+') {
        return None;
    }
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Version { major, minor, patch })
}

pub fn version_string(version: Version) -> String {
    format!("{}.{}.{}", version.major, version.minor, version.patch)
}

/// `latest` 比正在跑的版本新才算有更新。解析不了的两边都不升级，也不降级
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// 与 `xtask` 的 `host_platform` 同一套名字。没有客户端安装包的系统返回 None
pub fn release_platform(os: &str, arch: &str) -> Option<String> {
    let os = match os {
        "macos" => "darwin",
        "windows" => "win32",
        _ => return None,
    };
    let arch = match arch {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    Some(format!("{os}-{arch}"))
}

/// `Falcon-v0.3.19-darwin-arm64.pkg` / `Falcon-v0.3.19-win32-x64-setup.exe`
pub fn asset_name(version: &str, platform: &str) -> String {
    if platform.starts_with("win32-") {
        format!("Falcon-v{version}-{platform}-setup.exe")
    } else {
        format!("Falcon-v{version}-{platform}.pkg")
    }
}

pub fn releases_latest_url(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

/// 只接受「这个仓库的 Release 下载地址」。查询串、多出来的路径段和 `..` 都拒掉，
/// 避免发布 JSON 里被塞进一个别的主机
pub fn download_url_allowed(url: &str, repo: &str) -> bool {
    let prefix = format!("https://github.com/{repo}/releases/download/");
    let Some(rest) = url.strip_prefix(&prefix) else {
        return false;
    };
    if rest.is_empty() || rest.contains(['?', '#', '\\', ' ', '\n', '\r']) || rest.contains("..") {
        return false;
    }
    let mut parts = rest.split('/');
    let tag = parts.next().unwrap_or("");
    let file = parts.next().unwrap_or("");
    parts.next().is_none() && !tag.is_empty() && !file.is_empty()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// `major.minor.patch`，没有 `v` 前缀
    pub version: String,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseRelease {
    Ready(Release),
    /// 草稿或预发布。`/releases/latest` 不该返回这种，返回了就当没有可装的版本
    Skipped,
    Invalid,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

pub fn parse_release(json: &str) -> ParseRelease {
    let parsed: GhRelease = match serde_json::from_str(json) {
        Ok(parsed) => parsed,
        Err(_) => return ParseRelease::Invalid,
    };
    if parsed.draft || parsed.prerelease {
        return ParseRelease::Skipped;
    }
    let Some(version) = parse_version(&parsed.tag_name) else {
        return ParseRelease::Invalid;
    };
    ParseRelease::Ready(Release {
        version: version_string(version),
        assets: parsed
            .assets
            .into_iter()
            .map(|asset| ReleaseAsset { name: asset.name, url: asset.browser_download_url })
            .collect(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    UpToDate,
    Available {
        version: String,
        asset_name: String,
        download_url: String,
        /// macOS 的包里带着服务端，装完要按原来的参数重新 `service install`
        installs_local_service: bool,
    },
    NoPackage {
        platform: String,
    },
    BadDownloadUrl,
}

pub fn decide(current: &str, release: &Release, platform: &str, repo: &str) -> Decision {
    if !is_newer(&release.version, current) {
        return Decision::UpToDate;
    }
    let name = asset_name(&release.version, platform);
    let Some(asset) = release.assets.iter().find(|asset| asset.name == name) else {
        return Decision::NoPackage { platform: platform.to_string() };
    };
    if !download_url_allowed(&asset.url, repo) {
        return Decision::BadDownloadUrl;
    }
    Decision::Available {
        version: release.version.clone(),
        asset_name: asset.name.clone(),
        download_url: asset.url.clone(),
        installs_local_service: platform.starts_with("darwin-"),
    }
}

/// `…/Falcon.app/Contents/MacOS/Falcon` 时返回那个 `.app`。开发构建的可执行文件不在包里
pub fn running_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let app = contents.parent()?;
    if app.extension()? != "app" {
        return None;
    }
    Some(app.to_path_buf())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacTarget {
    pub dest: PathBuf,
    /// 正在跑的进程就在将被替换的这个包里。换的是目录项，进程继续用已经映射的旧二进制
    pub relaunch: bool,
}

/// 从 `.app` 里跑就换这一份（用户可能把它移出「应用程序」）。否则换 `applications/Falcon.app`
pub fn mac_install_target(current_exe: &Path, applications: &Path) -> MacTarget {
    if let Some(bundle) = running_bundle(current_exe) {
        MacTarget { dest: bundle, relaunch: true }
    } else {
        MacTarget { dest: applications.join("Falcon.app"), relaunch: false }
    }
}

pub fn backup_app_path(dest: &Path, stamp: &str) -> PathBuf {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("Falcon.backup-{stamp}.app"))
}

/// Info.plist 里 `CFBundleShortVersionString` 后面的那一个 `<string>`
pub fn plist_short_version(xml: &str) -> Option<String> {
    let key = "<key>CFBundleShortVersionString</key>";
    let rest = xml.get(xml.find(key)? + key.len()..)?;
    let start = rest.find("<string>")? + "<string>".len();
    let end = start + rest[start..].find("</string>")?;
    let value = rest[start..end].trim();
    if value.is_empty() { None } else { Some(value.to_string()) }
}

/// 展开后的 pkg 里找 `Falcon.app`。只认带 `Contents/MacOS/Falcon` 的那一层，
/// 避免把备份或半截目录当成应用
pub fn find_payload_app(root: &Path) -> Option<PathBuf> {
    fn is_app(path: &Path) -> bool {
        path.file_name().is_some_and(|name| name == "Falcon.app") && path.join("Contents/MacOS/Falcon").is_file()
    }
    if is_app(root) {
        return Some(root.to_path_buf());
    }
    let direct = root.join("Falcon.app");
    if is_app(&direct) {
        return Some(direct);
    }
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if is_app(&path) {
            return Some(path);
        }
        let nested = path.join("Falcon.app");
        if is_app(&nested) {
            return Some(nested);
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
pub enum SwapError {
    /// `source` 与 `dest` 不在同一文件系统，调用方改走复制
    CrossDevice,
    Failed(String),
}

/// 同一文件系统上用改名换入。`dest` 已存在时先改名为 `backup`，第二步失败再改回去。
/// 不覆盖正在映射的文件：换的是目录项
pub fn swap_tree(source: &Path, dest: &Path, backup: &Path) -> Result<(), SwapError> {
    if !source.exists() {
        return Err(SwapError::Failed(format!("找不到 {}", source.display())));
    }
    if backup.exists() {
        return Err(SwapError::Failed(format!("备份已存在 {}", backup.display())));
    }
    let had_dest = dest.exists();
    if had_dest {
        rename_tree(dest, backup)?;
    }
    if let Err(err) = rename_tree(source, dest) {
        if had_dest {
            let _ = std::fs::rename(backup, dest);
        }
        return Err(err);
    }
    Ok(())
}

fn rename_tree(from: &Path, to: &Path) -> Result<(), SwapError> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(err) if err.raw_os_error() == Some(18) => Err(SwapError::CrossDevice),
        Err(err) => Err(SwapError::Failed(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_app(root: &Path) {
        let exe = root.join("Contents/MacOS");
        std::fs::create_dir_all(&exe).unwrap();
        std::fs::write(exe.join("Falcon"), b"bin").unwrap();
    }

    #[test]
    fn version_order_ignores_v_prefix_and_rejects_prerelease() {
        assert_eq!(parse_version("v0.3.19"), Some(Version { major: 0, minor: 3, patch: 19 }));
        assert_eq!(parse_version("0.3.19"), parse_version("v0.3.19"));
        assert_eq!(parse_version("0.3.20-rc.1"), None);
        assert_eq!(parse_version("0.3.19+build"), None);
        assert!(is_newer("v0.3.20", "0.3.19"));
        assert!(is_newer("1.0.0", "v0.9.9"));
        assert!(!is_newer("0.3.19", "0.3.19"));
        assert!(!is_newer("0.3.18", "0.3.19"));
        assert!(!is_newer("0.3.20-rc.1", "0.3.19"));
        assert!(!is_newer("not-a-version", "0.3.19"));
    }

    #[test]
    fn platform_and_asset_names_match_the_packager() {
        assert_eq!(release_platform("macos", "aarch64").as_deref(), Some("darwin-arm64"));
        assert_eq!(release_platform("macos", "x86_64").as_deref(), Some("darwin-x64"));
        assert_eq!(release_platform("windows", "x86_64").as_deref(), Some("win32-x64"));
        assert_eq!(release_platform("linux", "x86_64"), None);
        assert_eq!(asset_name("0.3.19", "darwin-arm64"), "Falcon-v0.3.19-darwin-arm64.pkg");
        assert_eq!(asset_name("0.3.19", "win32-x64"), "Falcon-v0.3.19-win32-x64-setup.exe");
    }

    #[test]
    fn download_url_stays_on_the_release() {
        let ok = "https://github.com/Lee94/falcon/releases/download/v0.3.20/Falcon-v0.3.20-darwin-arm64.pkg";
        assert!(download_url_allowed(ok, RELEASE_REPO));
        assert!(!download_url_allowed("http://github.com/Lee94/falcon/releases/download/v0.3.20/a.pkg", RELEASE_REPO));
        assert!(!download_url_allowed("https://evil.example/Falcon.pkg", RELEASE_REPO));
        assert!(!download_url_allowed(
            "https://github.com/Lee94/falcon/releases/download/v0.3.20/../../secret",
            RELEASE_REPO
        ));
        assert!(!download_url_allowed(&format!("{ok}?token=1"), RELEASE_REPO));
        assert!(!download_url_allowed(
            "https://github.com/other/falcon/releases/download/v0.3.20/Falcon.pkg",
            RELEASE_REPO
        ));
    }

    #[test]
    fn decide_picks_the_exact_asset_and_refuses_a_foreign_url() {
        let release = Release {
            version: "0.3.20".into(),
            assets: vec![
                ReleaseAsset {
                    name: "Falcon-v0.3.20-darwin-arm64.pkg.sha256".into(),
                    url: "https://github.com/Lee94/falcon/releases/download/v0.3.20/note.txt".into(),
                },
                ReleaseAsset {
                    name: "Falcon-v0.3.20-darwin-arm64.pkg".into(),
                    url: "https://github.com/Lee94/falcon/releases/download/v0.3.20/Falcon-v0.3.20-darwin-arm64.pkg".into(),
                },
            ],
        };
        match decide("0.3.19", &release, "darwin-arm64", RELEASE_REPO) {
            Decision::Available { asset_name, installs_local_service, .. } => {
                assert_eq!(asset_name, "Falcon-v0.3.20-darwin-arm64.pkg");
                assert!(installs_local_service);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(decide("0.3.20", &release, "darwin-arm64", RELEASE_REPO), Decision::UpToDate);
        assert_eq!(decide("0.4.0", &release, "darwin-arm64", RELEASE_REPO), Decision::UpToDate);
        assert!(matches!(
            decide("0.3.19", &release, "win32-x64", RELEASE_REPO),
            Decision::NoPackage { .. }
        ));

        let mut foreign = release.clone();
        foreign.assets[1].url = "https://evil.example/Falcon.pkg".into();
        assert_eq!(decide("0.3.19", &foreign, "darwin-arm64", RELEASE_REPO), Decision::BadDownloadUrl);
    }

    #[test]
    fn parse_release_reads_tag_and_skips_prerelease() {
        let json = r#"{"tag_name":"v0.3.20","assets":[{"name":"Falcon-v0.3.20-darwin-arm64.pkg","browser_download_url":"https://github.com/Lee94/falcon/releases/download/v0.3.20/Falcon-v0.3.20-darwin-arm64.pkg","size":1}]}"#;
        match parse_release(json) {
            ParseRelease::Ready(release) => assert_eq!(release.version, "0.3.20"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(parse_release(r#"{"tag_name":"v0.3.20","prerelease":true}"#), ParseRelease::Skipped);
        assert_eq!(parse_release(r#"{"tag_name":"nightly"}"#), ParseRelease::Invalid);
        assert_eq!(parse_release("not-json"), ParseRelease::Invalid);
    }

    #[test]
    fn plist_version_and_running_bundle() {
        let xml = r#"<key>CFBundleShortVersionString</key><string>0.3.20</string>"#;
        assert_eq!(plist_short_version(xml).as_deref(), Some("0.3.20"));
        assert_eq!(plist_short_version("<key>CFBundleName</key><string>Falcon</string>"), None);
        let exe = Path::new("/Applications/Falcon.app/Contents/MacOS/Falcon");
        assert_eq!(running_bundle(exe), Some(PathBuf::from("/Applications/Falcon.app")));
        assert_eq!(running_bundle(Path::new("/tmp/falcon-desktop")), None);
        let target = mac_install_target(exe, Path::new("/Applications"));
        assert!(target.relaunch);
        let dev = mac_install_target(Path::new("/src/target/release/falcon-desktop"), Path::new("/Applications"));
        assert_eq!(dev.dest, PathBuf::from("/Applications/Falcon.app"));
        assert!(!dev.relaunch);
    }

    #[test]
    fn find_and_swap_an_app_tree() {
        let root = std::env::temp_dir().join(format!("falcon-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let extracted = root.join("expanded");
        let apps = root.join("Applications");
        std::fs::create_dir_all(&apps).unwrap();
        touch_app(&extracted.join("Falcon.app"));
        std::fs::write(extracted.join("Falcon.app/Contents/Resources-marker"), b"new").unwrap();
        let found = find_payload_app(&extracted).unwrap();
        assert_eq!(found.file_name().unwrap(), "Falcon.app");

        let old = apps.join("Falcon.app");
        touch_app(&old);
        std::fs::write(old.join("Contents/MacOS/Falcon"), b"old").unwrap();
        let backup = backup_app_path(&old, "20261109-120000");
        swap_tree(&found, &old, &backup).unwrap();
        assert_eq!(std::fs::read(old.join("Contents/Resources-marker")).unwrap(), b"new");
        assert_eq!(std::fs::read(backup.join("Contents/MacOS/Falcon")).unwrap(), b"old");
        assert!(!found.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_leaves_both_trees_when_the_backup_path_is_taken() {
        let root = std::env::temp_dir().join(format!("falcon-update-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("new");
        let dest = root.join("Falcon.app");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("marker"), b"new").unwrap();
        touch_app(&dest);
        let backup = root.join("taken.app");
        std::fs::create_dir_all(&backup).unwrap();
        let err = swap_tree(&source, &dest, &backup).unwrap_err();
        assert!(matches!(err, SwapError::Failed(_)));
        assert!(dest.join("Contents/MacOS/Falcon").is_file(), "old app stays in place");
        assert!(source.join("marker").is_file(), "new app stays put");
        let _ = std::fs::remove_dir_all(&root);
    }
}

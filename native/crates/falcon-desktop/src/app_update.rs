//! 从 GitHub Release 更新本机应用（ADR 0026）。
//!
//! macOS：下载 pkg，展开出 Falcon.app，校验版本与签名，改名换掉正在运行的那一份
//! （开发构建则换 `/Applications/Falcon.app`），再按 LaunchAgent 里原来的参数执行
//! `service install`。正在跑的就是这份应用时，界面再走正常退出（没保存的修改会先问），
//! 退出后重新打开新的 Falcon.app。`service install` 失败时把应用改回去，并用旧应用里的
//! 服务端再装一次，避免界面已经是新的、服务还停在半截。
//!
//! Windows 的安装包会在覆盖前关掉正在运行的程序，绕过退出时对未保存修改的确认，所以只下载，
//! 不启动安装程序。

use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};

use falcon_core::app_update::{self, Decision, ParseRelease};
use falcon_platform::{AppUpdater, AvailableUpdate, InstalledUpdate, UpdateCheck, UpdateFailure};

pub struct GithubUpdater;

impl AppUpdater for GithubUpdater {
    fn check(&self, current_version: &str) -> UpdateCheck {
        check_release(current_version)
    }

    fn install(&self, update: &AvailableUpdate) -> Result<InstalledUpdate, UpdateFailure> {
        install_release(update)
    }
}

fn check_release(current: &str) -> UpdateCheck {
    let Some(platform) = app_update::release_platform(std::env::consts::OS, std::env::consts::ARCH) else {
        return UpdateCheck::Failed(UpdateFailure::NoPackage {
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        });
    };
    let url = app_update::releases_latest_url(app_update::RELEASE_REPO);
    let body = match http_get(&url) {
        Ok(body) => body,
        Err(err) => return UpdateCheck::Failed(err),
    };
    let release = match app_update::parse_release(&body) {
        ParseRelease::Ready(release) => release,
        ParseRelease::Skipped => return UpdateCheck::Failed(UpdateFailure::NoRelease),
        ParseRelease::Invalid => {
            log::warn!("GitHub 发布信息解析失败");
            return UpdateCheck::Failed(UpdateFailure::Network("发布信息解析失败".into()));
        }
    };
    match app_update::decide(current, &release, &platform, app_update::RELEASE_REPO) {
        Decision::UpToDate => UpdateCheck::UpToDate { version: current.to_string() },
        Decision::Available { version, asset_name, download_url, installs_local_service } => {
            UpdateCheck::Available(AvailableUpdate { version, asset_name, download_url, installs_local_service })
        }
        Decision::NoPackage { platform } => UpdateCheck::Failed(UpdateFailure::NoPackage { platform }),
        Decision::BadDownloadUrl => UpdateCheck::Failed(UpdateFailure::BadDownloadUrl),
    }
}

fn install_release(update: &AvailableUpdate) -> Result<InstalledUpdate, UpdateFailure> {
    if !app_update::download_url_allowed(&update.download_url, app_update::RELEASE_REPO) {
        return Err(UpdateFailure::BadDownloadUrl);
    }
    #[cfg(target_os = "macos")]
    {
        return install_macos(update);
    }
    #[cfg(windows)]
    {
        return install_windows(update);
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = update;
        Err(UpdateFailure::NoPackage { platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH) })
    }
}

fn user_agent() -> String {
    format!("falcon-native/{}", env!("CARGO_PKG_VERSION"))
}

fn curl() -> &'static str {
    if cfg!(windows) { "curl" } else { "/usr/bin/curl" }
}

fn http_get(url: &str) -> Result<String, UpdateFailure> {
    let out = Command::new(curl())
        .args([
            "-sS",
            "-L",
            "--proto",
            "=https",
            "--retry",
            "3",
            "--retry-delay",
            "2",
            "--connect-timeout",
            "15",
            "--max-time",
            "30",
            "-A",
            &user_agent(),
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
            "-w",
            "\n%{http_code}",
            url,
        ])
        .output()
        .map_err(|err| UpdateFailure::Network(format!("起不来 curl：{err}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (body, code) = split_http(&stdout);
    if code == 404 {
        return Err(UpdateFailure::NoRelease);
    }
    if !out.status.success() || !(200..300).contains(&code) {
        let stderr = one_line(&String::from_utf8_lossy(&out.stderr));
        let message = github_message(&body).unwrap_or(stderr);
        return Err(UpdateFailure::Network(if message.is_empty() { format!("HTTP {code}") } else { message }));
    }
    Ok(body)
}

fn download(url: &str, dest: &Path) -> Result<(), UpdateFailure> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| UpdateFailure::Install(err.to_string()))?;
    }
    let part = part_path(dest);
    let out = Command::new(curl())
        .args([
            "-fL",
            "--proto",
            "=https",
            "--retry",
            "3",
            "--retry-delay",
            "2",
            "--connect-timeout",
            "15",
            "--max-time",
            "600",
            "-A",
            &user_agent(),
            "-o",
        ])
        .arg(&part)
        .arg(url)
        .output()
        .map_err(|err| UpdateFailure::Network(format!("起不来 curl：{err}")))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&part);
        let stderr = one_line(&String::from_utf8_lossy(&out.stderr));
        return Err(UpdateFailure::Network(if stderr.is_empty() {
            format!("下载失败（{}）", out.status)
        } else {
            stderr
        }));
    }
    std::fs::rename(&part, dest).map_err(|err| UpdateFailure::Install(format!("保存安装包失败：{err}")))?;
    Ok(())
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

fn split_http(stdout: &str) -> (String, u16) {
    let trimmed = stdout.trim_end_matches(['\n', '\r']);
    let Some((body, code)) = trimmed.rsplit_once('\n') else {
        return (trimmed.to_string(), 0);
    };
    (body.to_string(), code.trim().parse().unwrap_or(0))
}

fn github_message(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let message = value.get("message")?.as_str()?.trim();
    if message.is_empty() { None } else { Some(one_line(message)) }
}

fn one_line(text: &str) -> String {
    let line = text.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim();
    let mut chars = line.chars();
    let head: String = chars.by_ref().take(180).collect();
    if chars.next().is_some() { format!("{head}…") } else { head }
}

#[cfg(windows)]
fn install_windows(update: &AvailableUpdate) -> Result<InstalledUpdate, UpdateFailure> {
    let dest = downloads_dir().join(&update.asset_name);
    log::info!("下载 {}", dest.display());
    download(&update.download_url, &dest)?;
    Ok(InstalledUpdate {
        version: update.version.clone(),
        backup_path: None,
        relaunch: false,
        relaunch_bundle: None,
        staged_path: Some(dest.display().to_string()),
    })
}

#[cfg(windows)]
fn downloads_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(target_os = "macos")]
fn install_macos(update: &AvailableUpdate) -> Result<InstalledUpdate, UpdateFailure> {
    let temp = TempTree::new().map_err(UpdateFailure::Install)?;
    let pkg = temp.path.join(&update.asset_name);
    log::info!("下载 {}", update.asset_name);
    download(&update.download_url, &pkg)?;
    let expanded = temp.path.join("expanded");
    log::info!("展开 {}", pkg.display());
    expand_pkg(&pkg, &expanded).map_err(UpdateFailure::Install)?;
    let app = app_update::find_payload_app(&expanded)
        .ok_or_else(|| UpdateFailure::Install("安装包里没有 Falcon.app".into()))?;
    // 浏览器式下载会带上隔离属性，下次打开会被系统标成已损坏
    let _ = Command::new("/usr/bin/xattr").args(["-dr", "com.apple.quarantine"]).arg(&app).status();
    make_exec(&app.join("Contents/MacOS/Falcon"));
    make_exec(&app.join("Contents/Resources/falcon"));
    if !app.join("Contents/Resources/falcon").is_file() {
        return Err(UpdateFailure::Install("安装包里没有捆绑的服务端".into()));
    }
    let plist = std::fs::read_to_string(app.join("Contents/Info.plist"))
        .map_err(|err| UpdateFailure::Install(format!("读不到 Info.plist：{err}")))?;
    let found = app_update::plist_short_version(&plist)
        .ok_or_else(|| UpdateFailure::Install("安装包里读不到版本号".into()))?;
    if found != update.version {
        return Err(UpdateFailure::Install(format!("安装包里的版本是 {found}，发布信息写的是 {}", update.version)));
    }
    ensure_signature(&app).map_err(UpdateFailure::Install)?;

    let exe = std::env::current_exe().map_err(|err| UpdateFailure::Install(err.to_string()))?;
    let target = app_update::mac_install_target(&exe, Path::new("/Applications"));
    let had_dest = target.dest.exists();
    let backup = unused_backup(&target.dest, &local_stamp());
    log::info!("替换 {}", target.dest.display());
    place_app(&app, &target.dest, &backup).map_err(UpdateFailure::Install)?;

    // 读在换应用之后也一样：参数在 LaunchAgent 里，不在 .app 里。先清掉不可修改标记，
    // 否则 service install 换不掉正在跑的那份二进制
    if let Some(old) = crate::local_service::installed_program_path() {
        let _ = Command::new("/usr/bin/chflags").arg("nouchg").arg(&old).status();
    }
    let keep = crate::local_service::installed_service_args().map(|args| args.to_install_args()).unwrap_or_default();
    log::info!("service install {}", keep.join(" "));
    if let Err(err) = service_install(&target.dest, &keep) {
        if had_dest && backup.exists() {
            return match rollback(&target.dest, &backup, &keep) {
                Ok(()) => Err(UpdateFailure::Service(format!("本机服务没有装上，已改回更新前的应用：{err}"))),
                Err(rollback_err) => Err(UpdateFailure::Service(format!("{err}；回滚也失败了：{rollback_err}"))),
            };
        }
        return Err(UpdateFailure::Service(format!("应用已换成 v{}，本机服务没有装上：{err}", update.version)));
    }
    Ok(InstalledUpdate {
        version: update.version.clone(),
        backup_path: had_dest.then(|| backup.display().to_string()),
        relaunch: target.relaunch,
        // 开发构建的进程不在包里，不退出它。路径必须是合法 UTF-8，open 的参数按字节原样传
        relaunch_bundle: if target.relaunch { target.dest.to_str().map(str::to_string) } else { None },
        staged_path: None,
    })
}

#[cfg(target_os = "macos")]
fn expand_pkg(pkg: &Path, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|err| err.to_string())?;
    }
    let out = Command::new("/usr/sbin/pkgutil")
        .arg("--expand-full")
        .arg(pkg)
        .arg(dest)
        .output()
        .map_err(|err| format!("起不来 pkgutil：{err}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let raw = if !stderr.trim().is_empty() { stderr } else { stdout };
    Err(if one_line(raw.trim()).is_empty() { format!("pkgutil 退出 {}", out.status) } else { one_line(raw.trim()) })
}

#[cfg(target_os = "macos")]
fn ensure_signature(app: &Path) -> Result<(), String> {
    if verify_signature(app) {
        return Ok(());
    }
    // 发布包没有 Developer ID，本来就是 ad-hoc。展开或清掉隔离属性后对不上就按同样的方式重签
    let out = Command::new("/usr/bin/codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(app)
        .output()
        .map_err(|err| format!("起不来 codesign：{err}"))?;
    if !out.status.success() {
        return Err(one_line(&String::from_utf8_lossy(&out.stderr)));
    }
    if verify_signature(app) { Ok(()) } else { Err("重签之后校验仍未通过".into()) }
}

#[cfg(target_os = "macos")]
fn verify_signature(app: &Path) -> bool {
    Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "macos")]
fn make_exec(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if path.is_file() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
}

#[cfg(target_os = "macos")]
fn place_app(source: &Path, dest: &Path, backup: &Path) -> Result<(), String> {
    match app_update::swap_tree(source, dest, backup) {
        Ok(()) => Ok(()),
        Err(app_update::SwapError::CrossDevice) => place_by_copy(source, dest, backup),
        Err(app_update::SwapError::Failed(message)) => Err(message),
    }
}

/// `/tmp` 与 `/Applications` 不在同一卷时，先复制到目标目录旁边再改名
#[cfg(target_os = "macos")]
fn place_by_copy(source: &Path, dest: &Path, backup: &Path) -> Result<(), String> {
    let parent = dest.parent().ok_or_else(|| "应用没有父目录".to_string())?;
    let partial = parent.join(".Falcon.app.partial");
    if partial.exists() {
        std::fs::remove_dir_all(&partial).map_err(|err| err.to_string())?;
    }
    let copied = Command::new("/usr/bin/ditto").arg(source).arg(&partial).output().map_err(|err| format!("起不来 ditto：{err}"))?;
    if !copied.status.success() {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(one_line(&String::from_utf8_lossy(&copied.stderr)));
    }
    match app_update::swap_tree(&partial, dest, backup) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(source);
            Ok(())
        }
        Err(err) => {
            let _ = std::fs::remove_dir_all(&partial);
            Err(match err {
                app_update::SwapError::Failed(message) => message,
                app_update::SwapError::CrossDevice => "复制到目标目录后仍然无法改名".into(),
            })
        }
    }
}

#[cfg(target_os = "macos")]
fn service_install(app: &Path, keep: &[String]) -> Result<(), String> {
    let bin = app.join("Contents/Resources/falcon");
    if !bin.is_file() {
        return Err("应用里没有捆绑的服务端".into());
    }
    let out = Command::new(&bin)
        .args(["service", "install"])
        .args(keep)
        .output()
        .map_err(|err| format!("起不来服务程序：{err}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let raw = if !stderr.trim().is_empty() { stderr } else { stdout };
    let message = one_line(raw.trim());
    Err(if message.is_empty() { format!("service install 退出 {}", out.status) } else { message })
}

/// 新应用已经占着目标位置。先挪开它，把备份改回原名，再用旧应用里的服务端装回去
#[cfg(target_os = "macos")]
fn rollback(dest: &Path, backup: &Path, keep: &[String]) -> Result<(), String> {
    let parent = dest.parent().ok_or("应用没有父目录")?;
    let hold = parent.join(format!(".Falcon.update-failed-{}", std::process::id()));
    if hold.exists() {
        std::fs::remove_dir_all(&hold).map_err(|err| err.to_string())?;
    }
    std::fs::rename(dest, &hold).map_err(|err| format!("挪开新应用失败：{err}"))?;
    if let Err(err) = std::fs::rename(backup, dest) {
        return Err(format!("新应用留在 {}，改回旧应用失败：{err}", hold.display()));
    }
    if let Err(err) = service_install(dest, keep) {
        log::warn!("用旧应用重新安装本机服务失败：{err}");
    }
    let _ = std::fs::remove_dir_all(&hold);
    Ok(())
}

#[cfg(target_os = "macos")]
fn local_stamp() -> String {
    if let Ok(out) = Command::new("/bin/date").arg("+%Y%m%d-%H%M%S").output() {
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit() || ch == '-') {
            return text;
        }
    }
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs().to_string()).unwrap_or_else(|_| "backup".into())
}

#[cfg(target_os = "macos")]
fn unused_backup(dest: &Path, stamp: &str) -> PathBuf {
    let parent = dest.parent().unwrap_or_else(|| Path::new("/"));
    let mut path = parent.join(format!("Falcon.backup-{stamp}.app"));
    let mut n = 2u32;
    while path.exists() && n < 100 {
        path = parent.join(format!("Falcon.backup-{stamp}-{n}.app"));
        n += 1;
    }
    path
}

#[cfg(target_os = "macos")]
struct TempTree {
    path: PathBuf,
}

#[cfg(target_os = "macos")]
impl TempTree {
    fn new() -> Result<Self, String> {
        let millis = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let path = std::env::temp_dir().join(format!("falcon-update-{}-{millis}", std::process::id()));
        std::fs::create_dir_all(&path).map_err(|err| err.to_string())?;
        Ok(Self { path })
    }
}

#[cfg(target_os = "macos")]
impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 等本进程退出再 `open` 新的 Falcon.app。脚本只读 `$1` `$2`，路径不拼进命令字符串。
/// 先脱离会话：应用退出时不能把这个帮手一起带走。
pub(crate) fn schedule_relaunch(bundle: &Path) -> Result<(), String> {
    if bundle.file_name().is_none_or(|name| name != "Falcon.app") {
        return Err(format!("不是 Falcon.app：{}", bundle.display()));
    }
    let exe = bundle.join("Contents/MacOS/Falcon");
    if !exe.is_file() {
        return Err(format!("找不到 {}", exe.display()));
    }
    #[cfg(target_os = "macos")]
    {
        return spawn_relaunch_helper(bundle);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = exe;
        Err("这个平台不会在退出后重新打开应用".into())
    }
}

#[cfg(target_os = "macos")]
fn spawn_relaunch_helper(bundle: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(RELAUNCH_SCRIPT)
        .arg("falcon-relaunch")
        .arg(std::process::id().to_string())
        .arg(bundle)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // setsid：帮手成为新会话的首领，父进程退出时收不到 SIGHUP
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().map(|_| ()).map_err(|err| format!("起不来重新打开的帮手：{err}"))
}

/// `$1` 是要等的 pid，`$2` 是 Falcon.app。死后再等一会儿，菜单栏状态项才收得掉
#[cfg(target_os = "macos")]
const RELAUNCH_SCRIPT: &str =
    "while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; sleep 0.4; exec /usr/bin/open \"$2\"";

#[cfg(test)]
mod tests {
    use super::split_http;

    #[test]
    fn http_status_is_the_last_line() {
        let (body, code) = split_http("{\"tag_name\":\"v0.3.20\"}\n200\n");
        assert_eq!(code, 200);
        assert_eq!(body, "{\"tag_name\":\"v0.3.20\"}");
        let (body, code) = split_http("no-status");
        assert_eq!(code, 0);
        assert_eq!(body, "no-status");
    }

    #[test]
    fn relaunch_refuses_anything_except_the_app_bundle() {
        let err = super::schedule_relaunch(std::path::Path::new("/tmp")).unwrap_err();
        assert!(err.contains("Falcon.app"), "{err}");
    }
}

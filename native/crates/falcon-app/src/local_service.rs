//! 本机服务托管：`scripts/macos-pkg/launcher.sh` 的原生版。
//!
//! Falcon.app 的 Resources 里带着 SEA 单文件服务程序。启动时：
//! 1. `falcon service install`——幂等，兼作升级（它会把 Resources/falcon 挪进
//!    `<dataDir>/bin/falcon` 并注册 / 重启用户级 LaunchAgent）。**带上已安装服务原来的
//!    `--host` / `--port` / `--data-dir`**（从 LaunchAgent plist 里读）：不带参数的 install
//!    会写默认配置，装过自定义端口 / 数据目录的人一升级，服务就换了个家，会话全看不见了
//!    （pkg 的 postinstall 与 launcher.sh 用 `scripts/macos-pkg/service-args.sh` 做同一件事）；
//! 2. 等端口起来：SEA 首次启动要解压 runtime，端口起来之前连会被拒；401 也算起来了
//!    （设过访问密码）。"本机"配置连的端口同样取自 plist（见 [`installed_service_args`]）。
//!
//! 会话活在 launchd 服务里，不在 App 进程里——**退出 App = 所有窗口 Detach**，永远不会
//! Terminate。开发时（不在 .app 里跑）找不到捆绑的服务程序，就假定服务已经在跑
//! （`pnpm dev:server` 或已装好的服务），只做第 2 步。
//!
//! Windows 上没有本机服务（SEA 不支持 Windows 目标，见 README），这一段只有 macOS 实现。

use std::time::Duration;

/// 捆绑的服务程序：`Falcon.app/Contents/Resources/falcon`
#[cfg(target_os = "macos")]
fn bundled_server() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let contents = exe.parent()?.parent()?;
    let bin = contents.join("Resources").join("falcon");
    bin.is_file().then_some(bin)
}

pub enum ServiceStatus {
    /// 端口起来了
    Ready,
    /// 等了一轮还是连不上（服务装不上 / 起不来）
    Unreachable(String),
}

/// 已经注册过的用户级 LaunchAgent（服务端 `service.ts` 的 `com.falcon.server`）的启动参数。
/// 没装过、读不出来都是 None。用系统自带的 plutil 转 JSON，不为读一个 plist 加依赖
pub fn installed_service_args() -> Option<falcon_core::service_args::ServiceArgs> {
    #[cfg(target_os = "macos")]
    {
        let plist = directories::BaseDirs::new()?
            .home_dir()
            .join("Library/LaunchAgents/com.falcon.server.plist");
        if !plist.is_file() {
            return None;
        }
        let out = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", "ProgramArguments", "json", "-o", "-"])
            .arg(&plist)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let argv: Vec<String> = serde_json::from_slice(&out.stdout).ok()?;
        Some(falcon_core::service_args::parse_service_args(&argv))
    }
    #[cfg(not(target_os = "macos"))]
    None
}

/// 阻塞执行，放在后台线程里调。
pub fn ensure_running(url: &str) -> ServiceStatus {
    #[cfg(target_os = "macos")]
    if let Some(bin) = bundled_server() {
        let keep = installed_service_args().map(|a| a.to_install_args()).unwrap_or_default();
        match std::process::Command::new(&bin).args(["service", "install"]).args(&keep).output() {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                log::warn!("service install 失败：{stderr}");
            }
            Err(err) => log::warn!("拉不起捆绑的服务程序 {}：{err}", bin.display()),
        }
    }
    wait_for_port(url)
}

fn wait_for_port(url: &str) -> ServiceStatus {
    let Ok(parsed) = url::Url::parse(url) else {
        return ServiceStatus::Unreachable(format!("基址无效：{url}"));
    };
    let host = parsed.host_str().unwrap_or("127.0.0.1").to_string();
    let port = parsed.port_or_known_default().unwrap_or(4923);
    let mut last_err = String::new();
    // 与 launcher.sh 同一个节奏：最多 40 × 0.5s
    for _ in 0..40 {
        match std::net::TcpStream::connect_timeout(
            &format!("{host}:{port}")
                .parse()
                .unwrap_or_else(|_| std::net::SocketAddr::from(([127, 0, 0, 1], port))),
            Duration::from_millis(500),
        ) {
            Ok(_) => return ServiceStatus::Ready,
            Err(err) => last_err = err.to_string(),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    ServiceStatus::Unreachable(last_err)
}

#[cfg(test)]
mod tests {
    /// 装没装过服务都成立：读得出来就必须能拼出一个 http 基址（本机开发时顺便看一眼读到了什么）
    #[test]
    fn installed_args_yield_a_url() {
        if let Some(args) = super::installed_service_args() {
            let url = args.local_url();
            eprintln!("已安装服务：{args:?} → {url}");
            assert!(url.starts_with("http://"));
        }
    }
}

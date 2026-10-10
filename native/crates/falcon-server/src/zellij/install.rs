//! 宿主机上的 Zellij 安装编排。移植自 `packages/server/src/zellij/install.ts`。
//!
//! 二进制**由宿主机自己下载**，不经后端中转：不占后端带宽、不走 SFTP
//! （顺带绕开 sftp-server 被禁用的整类问题）、宿主机在海外时也不必绕道。
//! 代价是宿主机必须能出网——下不了的机器（内网、无出口）走手动预置：
//! 用户自己把二进制放到 bin_dir 即可，安装流程会先检查它。
//!
//! 不做完整性校验：走默认 GitHub 源时 HTTPS 已保证来源与完整性；
//! 配置了自定义 base URL 的用户会在 UI 上看到明确警告。

use std::time::Duration;

use falcon_proto::{ZellijInstallFailure, ZellijInstallStage, is_auto_retryable};
use tokio_util::sync::CancellationToken;

use super::command::{CONFIG_BODY, LAYOUT_BODY, scroll_config_body, version_args, version_matches};
use super::host::{HostKind, HostLayout, build_command_line, encode_powershell, host_layout, quote_posix, quote_powershell};
use super::version::{ZellijTarget, download_url};
use crate::exec::{Exec, ExecResult};
use crate::zellij::host::Downloader;

/// 阶段回调：(阶段, 第几轮, 可读的远端命令)。attempt 从 1 起，>1 表示这一轮是自动重试
pub type StageFn<'a> = &'a dyn Fn(ZellijInstallStage, u32, Option<String>);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct InstallError {
    pub reason: ZellijInstallFailure,
    pub message: String,
    pub detail: Option<String>,
}

impl InstallError {
    pub fn new(reason: ZellijInstallFailure, detail: Option<String>) -> Self {
        InstallError { reason, message: failure_text(reason).to_string(), detail }
    }

    fn cancelled() -> Self {
        Self::new(ZellijInstallFailure::Cancelled, None)
    }
}

pub fn failure_text(reason: ZellijInstallFailure) -> &'static str {
    match reason {
        ZellijInstallFailure::ArchUnsupported => "该架构没有官方 Zellij 构建",
        ZellijInstallFailure::NoDownloader => "宿主机缺少 curl / wget",
        ZellijInstallFailure::NoTar => "宿主机缺少 tar",
        ZellijInstallFailure::DirNotWritable => "无法在宿主机家目录创建 .falcon 目录",
        ZellijInstallFailure::ProbeFailed => "无法探测宿主机（命令未跑通或系统未识别）",
        ZellijInstallFailure::DownloadFailed => "下载 Zellij 失败",
        ZellijInstallFailure::ExtractFailed => "解压 Zellij 失败",
        ZellijInstallFailure::VerifyFailed => "Zellij 无法在宿主机上执行（可能是 noexec 挂载或架构不兼容）",
        ZellijInstallFailure::Cancelled | ZellijInstallFailure::Unknown => "已取消",
    }
}

/// 单次安装请求内的自动重试次数上限（含首次尝试）
pub const MAX_ATTEMPTS: u32 = 3;

/// 各次重试前的退避。够长到熬过一次网络抖动，又不至于让用户以为卡死了。
const BACKOFF_MS: [u64; 2] = [1500, 4000];

pub struct InstallOptions<'a> {
    pub kind: HostKind,
    /// falcon 根目录：远端为 remote_root(kind, home)，本地为后端的 --data-dir
    pub root: String,
    pub target: ZellijTarget,
    /// 该主机配置的下载源，缺省为 GitHub 官方地址
    pub base_url: Option<String>,
    pub downloader: Downloader,
    /// Windows 才需要判断；POSIX 上 tar 视为必然存在
    pub has_tar: Option<bool>,
    pub on_stage: Option<StageFn<'a>>,
    pub cancel: Option<&'a CancellationToken>,
}

impl InstallOptions<'_> {
    fn stage(&self, stage: ZellijInstallStage, attempt: u32, command: String) {
        if let Some(f) = self.on_stage {
            f(stage, attempt, Some(command));
        }
    }
}

/// 确保宿主机上有可用的 Zellij，返回其布局。
/// 已存在（含用户手动预置）则只做一次 --version 握手就返回。
///
/// 下载/解压这一段会自动重试（见 with_install_retry）：远端安装最常见的失败
/// 就是网络抖一下或 SSH 通道半路断开，让用户回去点重试是把机器该干的活推给人。
pub async fn ensure_zellij(exec: &dyn Exec, opts: &InstallOptions<'_>) -> Result<HostLayout, InstallError> {
    let layout = host_layout(opts.kind, &opts.root, opts.target);

    // 目录与 layout 文件先就位——二进制装没装好都需要它们
    let dirs_cmd = ensure_dirs(opts.kind, &layout);
    opts.stage(ZellijInstallStage::Verifying, 1, display_cmd(opts.kind, &dirs_cmd, &ensure_dirs_script(opts.kind, &layout)));
    run(exec, &dirs_cmd, ZellijInstallFailure::DirNotWritable, opts.cancel).await?;

    // 已装或已预置：握手通过就直接用，省掉整个下载流程
    opts.stage(ZellijInstallStage::Verifying, 1, verify_display(opts.kind, &layout.bin));
    if verify(exec, opts.kind, &layout.bin, opts.cancel, false).await? {
        return Ok(layout);
    }

    // 前置条件不随重试改变，放在重试循环外先挡掉
    if opts.downloader == Downloader::None {
        return Err(InstallError::new(
            ZellijInstallFailure::NoDownloader,
            Some(format!("可手动将 Zellij 二进制放到 {}", layout.bin)),
        ));
    }
    if opts.kind == HostKind::Windows && opts.has_tar == Some(false) {
        return Err(InstallError::new(ZellijInstallFailure::NoTar, Some(format!("可手动将 Zellij 二进制放到 {}", layout.bin))));
    }

    let url = download_url(opts.target, opts.base_url.as_deref());
    with_install_retry(|attempt| install_once(exec, opts, &layout, &url, attempt), opts.cancel).await?;
    Ok(layout)
}

/// 一轮完整的下载 → 解压 → 验证 → 就位。任一步失败即整轮作废，由调用方决定要不要再来。
async fn install_once(
    exec: &dyn Exec,
    opts: &InstallOptions<'_>,
    layout: &HostLayout,
    url: &str,
    attempt: u32,
) -> Result<(), InstallError> {
    let sep = if opts.kind == HostKind::Windows { "\\" } else { "/" };
    // 每轮都用全新的临时目录：上一轮留下的半截文件绝不会被下一轮当成好包
    let mut rand = [0u8; 4];
    let _ = getrandom::fill(&mut rand);
    let tmp_dir = format!("{}{sep}.tmp-{}", layout.bin_dir, hex::encode(rand));

    let result = async {
        let dl = download(opts.kind, &tmp_dir, url, opts.downloader);
        opts.stage(
            ZellijInstallStage::Downloading,
            attempt,
            display_cmd(opts.kind, &dl, &download_script(opts.kind, &tmp_dir, url, opts.downloader)),
        );
        run(exec, &dl, ZellijInstallFailure::DownloadFailed, opts.cancel).await?;

        let unpack = extract(opts.kind, &tmp_dir);
        opts.stage(ZellijInstallStage::Extracting, attempt, display_cmd(opts.kind, &unpack, &extract_script(opts.kind, &tmp_dir)));
        run(exec, &unpack, ZellijInstallFailure::ExtractFailed, opts.cancel).await?;

        // 原子替换：先在临时目录里验证，通过了才 rename 到正式路径。
        // 半截文件、并发安装都不会污染正式路径（后到者覆盖，内容相同无害）。
        let staged = format!("{tmp_dir}{sep}{}", if opts.kind == HostKind::Windows { "zellij.exe" } else { "zellij" });
        opts.stage(ZellijInstallStage::Verifying, attempt, verify_display(opts.kind, &staged));
        if !verify(exec, opts.kind, &staged, opts.cancel, true).await? {
            return Err(InstallError::new(ZellijInstallFailure::VerifyFailed, None));
        }
        let mv = promote(opts.kind, &staged, &layout.bin);
        opts.stage(ZellijInstallStage::Verifying, attempt, display_cmd(opts.kind, &mv, &promote_script(opts.kind, &staged, &layout.bin)));
        run(exec, &mv, ZellijInstallFailure::ExtractFailed, opts.cancel).await?;
        Ok(())
    }
    .await;
    // 不带 cancel：取消时更要清干净，否则临时目录会一直留在用户机器上
    let _ = exec.exec(&cleanup(opts.kind, &tmp_dir), None).await;
    result
}

/// 有限次自动重试，只对瞬时故障生效（见 falcon_proto::AUTO_RETRY_FAILURES）。
///
/// 取消在退避等待期间立即生效——用户点了取消就不该再等几秒。
pub async fn with_install_retry<T, F, Fut>(mut attempt_fn: F, cancel: Option<&CancellationToken>) -> Result<T, InstallError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, InstallError>>,
{
    let mut attempt = 1;
    loop {
        match attempt_fn(attempt).await {
            Ok(v) => return Ok(v),
            Err(err) => {
                let aborted = cancel.is_some_and(CancellationToken::is_cancelled);
                if !is_auto_retryable(err.reason) || aborted || attempt >= MAX_ATTEMPTS {
                    return Err(err);
                }
                let ms = BACKOFF_MS.get(attempt as usize - 1).copied().unwrap_or(BACKOFF_MS[BACKOFF_MS.len() - 1]);
                sleep(Duration::from_millis(ms), cancel).await?;
                attempt += 1;
            }
        }
    }
}

/// 可被取消打断的等待；打断时报 cancelled，与安装流程其余部分一致
async fn sleep(d: Duration, cancel: Option<&CancellationToken>) -> Result<(), InstallError> {
    match cancel {
        Some(token) if token.is_cancelled() => Err(InstallError::cancelled()),
        Some(token) => tokio::select! {
            _ = tokio::time::sleep(d) => Ok(()),
            _ = token.cancelled() => Err(InstallError::cancelled()),
        },
        None => {
            tokio::time::sleep(d).await;
            Ok(())
        }
    }
}

async fn run(
    exec: &dyn Exec,
    command_line: &str,
    reason: ZellijInstallFailure,
    cancel: Option<&CancellationToken>,
) -> Result<ExecResult, InstallError> {
    let aborted = || cancel.is_some_and(CancellationToken::is_cancelled);
    if aborted() {
        return Err(InstallError::cancelled());
    }
    let res = match exec.exec(command_line, cancel).await {
        Ok(r) => r,
        Err(_) if aborted() => return Err(InstallError::cancelled()),
        Err(e) => return Err(InstallError::new(reason, Some(format!("{e:#}")))),
    };
    if aborted() {
        return Err(InstallError::cancelled());
    }
    if res.code != Some(0) {
        // 退出码兜底：curl/wget 在某些远端上一个字都不往 stderr 写，只留一个退出码
        // （curl 6=域名解析不了、7=连不上、22=HTTP 错误、28=超时），
        // 而"该改网络还是该换下载源"全靠这一句话。空着等于让用户去猜。
        let detail = [res.stderr.trim(), res.stdout.trim()]
            .into_iter()
            .find(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("远端命令退出码 {}", res.code.map_or("null".to_string(), |c| c.to_string())));
        return Err(InstallError::new(reason, Some(detail)));
    }
    Ok(res)
}

/// 握手验证：跑得起来且版本对得上才算数。能同时挡住 noexec、架构不符、文件截断。
///
/// strict = 命令本身没跑起来时报错而不是当作"验证不通过"。装完那一次必须用它：
/// 链路在握手瞬间断掉与"二进制跑不动"是两码事，前者重试有救，后者重试白搭，
/// 混成一个 false 会让用户收到一句冤枉的"noexec 挂载或架构不兼容"。
async fn verify(
    exec: &dyn Exec,
    kind: HostKind,
    bin: &str,
    cancel: Option<&CancellationToken>,
    strict: bool,
) -> Result<bool, InstallError> {
    let argv: Vec<String> = std::iter::once(bin.to_string()).chain(version_args()).collect();
    let no_env: &[(&str, &str)] = &[];
    match exec.exec(&build_command_line(kind, &argv, no_env, &[]), cancel).await {
        Ok(res) => Ok(res.code == Some(0) && version_matches(&res.stdout)),
        Err(_) if !strict => Ok(false),
        Err(_) if cancel.is_some_and(CancellationToken::is_cancelled) => Err(InstallError::cancelled()),
        Err(e) => Err(InstallError::new(
            ZellijInstallFailure::ProbeFailed,
            Some(format!("验证握手时与宿主机失去联系：{e:#}")),
        )),
    }
}

// ---------------- 各阶段命令 ----------------

/// 下载超时。没有这几个开关，重试就是空头支票：curl 默认既不限连接时间也不管
/// 传输停滞，遇上黑洞路由（连得上、握完手、然后一个字节都不来——被墙时最常见的
/// 表现）会一直挂着不返回，失败根本不会发生，用户看到的是一个转到天荒地老的进度条。
///
/// 连接 20 秒放弃；连上之后若 30 秒内平均速率低于 1 KiB/s 也放弃。不设总时长上限：
/// 14 MiB 在慢线路上跑十几分钟是正常的，只要还在动就不该被打断。
///
/// wget 侧还要显式 --tries=1：它自带 20 次重试，会把我们的退避与取消全都架空。
const CURL_LIMITS: &str = "--connect-timeout 20 --speed-limit 1024 --speed-time 30";
const WGET_LIMITS: &str = "--tries=1 --connect-timeout=20 --read-timeout=30";

/// Windows 上建目录（含中间层，语义同 `mkdir -p`）。
///
/// 这里用 `-Path` 是可以的，尽管 New-Item 没有 `-LiteralPath` 参数：实测含 `[]`
/// 的路径照样建得出来（创建路径不像 Remove-Item 那样先做通配符匹配），
/// 而 `[ ]` 是 Windows 文件名里唯一合法的通配字符（`* ?` 本就非法）。
/// **删除**那边则完全不同，必须 `-LiteralPath`——见 cleanup。
fn mkdir_ps(dir: &str) -> String {
    format!("New-Item -ItemType Directory -Force -Path {} | Out-Null", quote_powershell(dir))
}

/// Windows 实际跑的是 -EncodedCommand；UI 展示未编码的脚本，才读得懂
fn display_cmd(kind: HostKind, encoded: &str, script: &str) -> String {
    if kind == HostKind::Windows { script.replace("; ", "\n") } else { encoded.to_string() }
}

fn verify_display(kind: HostKind, bin: &str) -> String {
    if kind == HostKind::Windows {
        format!("& {} --version", quote_powershell(bin))
    } else {
        format!("{} --version", quote_posix(bin))
    }
}

fn download_script(kind: HostKind, tmp_dir: &str, url: &str, downloader: Downloader) -> String {
    if kind == HostKind::Windows {
        return [
            mkdir_ps(tmp_dir),
            format!(
                "curl.exe -fsSL {CURL_LIMITS} {} -o {}",
                quote_powershell(url),
                quote_powershell(&format!("{tmp_dir}\\a.zip"))
            ),
            "if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }".to_string(),
        ]
        .join("; ");
    }
    let dir = quote_posix(tmp_dir);
    let out = quote_posix(&format!("{tmp_dir}/a.tar.gz"));
    let u = quote_posix(url);
    let fetch = if downloader == Downloader::Curl {
        format!("curl -fsSL {CURL_LIMITS} {u} -o {out}")
    } else {
        format!("wget -q {WGET_LIMITS} {u} -O {out}")
    };
    format!("mkdir -p {dir} && {fetch}")
}

fn wrap(kind: HostKind, script: String) -> String {
    if kind == HostKind::Windows { encode_powershell(&script) } else { script }
}

fn download(kind: HostKind, tmp_dir: &str, url: &str, downloader: Downloader) -> String {
    wrap(kind, download_script(kind, tmp_dir, url, downloader))
}

fn extract_script(kind: HostKind, tmp_dir: &str) -> String {
    if kind == HostKind::Windows {
        return [
            format!("tar.exe -xf {} -C {}", quote_powershell(&format!("{tmp_dir}\\a.zip")), quote_powershell(tmp_dir)),
            "if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }".to_string(),
        ]
        .join("; ");
    }
    format!(
        "tar -xzf {} -C {} && chmod 700 {}",
        quote_posix(&format!("{tmp_dir}/a.tar.gz")),
        quote_posix(tmp_dir),
        quote_posix(&format!("{tmp_dir}/zellij"))
    )
}

/// 包内只有单个可执行文件（POSIX 为 zellij，Windows 为 zellij.exe）
fn extract(kind: HostKind, tmp_dir: &str) -> String {
    wrap(kind, extract_script(kind, tmp_dir))
}

fn promote_script(kind: HostKind, staged: &str, bin: &str) -> String {
    if kind == HostKind::Windows {
        format!("Move-Item -Force -LiteralPath {} -Destination {}", quote_powershell(staged), quote_powershell(bin))
    } else {
        format!("mv -f {} {}", quote_posix(staged), quote_posix(bin))
    }
}

fn promote(kind: HostKind, staged: &str, bin: &str) -> String {
    wrap(kind, promote_script(kind, staged, bin))
}

fn cleanup(kind: HostKind, tmp_dir: &str) -> String {
    if kind == HostKind::Windows {
        // **必须 -LiteralPath**：`-Path` 会先把路径当通配符去匹配已存在的项，路径里有一个
        // `[` 就会被当成字符类而匹配不到任何东西。实测 `Remove-Item -Recurse -Force
        // -EA SilentlyContinue -Path 'C:\...\rm[1]old'` 退出码 0、目录纹丝不动——
        // 静默报成功是最坏的一种失败。远端 home 含 `[]` 并不罕见（`C:\Users\a[1]b`）。
        //
        // -EA SilentlyContinue 保留：对象是我们自己造的 .tmp-<随机>，清不掉最多留点
        // 垃圾，不该盖住真正的安装错误（这是在收尾时跑的）。
        return encode_powershell(&format!(
            "Remove-Item -Recurse -Force -EA SilentlyContinue -LiteralPath {}",
            quote_powershell(tmp_dir)
        ));
    }
    format!("rm -rf {}", quote_posix(tmp_dir))
}

/// 建目录并写入 layout 文件。
///
/// 单独一步执行，好把"家目录不可写"跟"下载失败"区分开来报给用户。
/// 每次都跑（哪怕二进制已经装好）：成本是一次往返，换来的是用户误删
/// layout 文件后能自愈——少了它 Zellij 会直接以 IoError 退出。
///
/// POSIX 上顺带写滚动位置插件的会话配置（scroll.kdl）：带插件的会话接回时 `--config`
/// 指着它，文件不在 zellij 直接起不来，所以同样每次都写。插件本体（.wasm）不在这里，
/// 它要经 stdin 推、还要预写授权，见 sessions::scroll_plugin。Windows 远端不走插件。
fn ensure_dirs_script(kind: HostKind, layout: &HostLayout) -> String {
    let dirs = [&layout.bin_dir, &layout.socket_dir, &layout.config_dir, &layout.data_dir, &layout.cache_dir, &layout.layout_dir];
    if kind == HostKind::Windows {
        let mut mk: Vec<String> = dirs.iter().map(|d| mkdir_ps(d)).collect();
        mk.push(format!(
            "Set-Content -LiteralPath {} -Value {}",
            quote_powershell(&layout.layout_file),
            quote_powershell(LAYOUT_BODY.trim())
        ));
        mk.push(format!(
            "Set-Content -LiteralPath {} -Value {}",
            quote_powershell(&layout.config_file),
            quote_powershell(CONFIG_BODY.trim())
        ));
        return mk.join("; ");
    }
    format!(
        "mkdir -p {} && printf %s {} > {} && printf %s {} > {} && printf %s {} > {}",
        dirs.iter().map(|d| quote_posix(d)).collect::<Vec<_>>().join(" "),
        quote_posix(LAYOUT_BODY),
        quote_posix(&layout.layout_file),
        quote_posix(&CONFIG_BODY),
        quote_posix(&layout.config_file),
        quote_posix(&scroll_config_body(&layout.scroll_plugin_file)),
        quote_posix(&layout.scroll_config_file),
    )
}

pub fn ensure_dirs(kind: HostKind, layout: &HostLayout) -> String {
    wrap(kind, ensure_dirs_script(kind, layout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::LocalBoxFuture;
    use std::cell::RefCell;

    /// 按命令前缀给出预设结果的假执行器，记下所有命令
    struct Script {
        calls: RefCell<Vec<String>>,
        respond: Box<dyn Fn(&str, usize) -> anyhow::Result<ExecResult>>,
    }

    impl Exec for Script {
        fn exec<'a>(&'a self, cmd: &'a str, _cancel: Option<&'a CancellationToken>) -> LocalBoxFuture<'a, anyhow::Result<ExecResult>> {
            let n = self.calls.borrow().len();
            self.calls.borrow_mut().push(cmd.to_string());
            let r = (self.respond)(cmd, n);
            Box::pin(async move { r })
        }
    }

    fn ok(stdout: &str) -> anyhow::Result<ExecResult> {
        Ok(ExecResult { code: Some(0), stdout: stdout.into(), stderr: String::new() })
    }

    fn opts(downloader: Downloader) -> InstallOptions<'static> {
        InstallOptions {
            kind: HostKind::Posix,
            root: "/home/u/.falcon".into(),
            target: ZellijTarget::X86_64LinuxMusl,
            base_url: None,
            downloader,
            has_tar: None,
            on_stage: None,
            cancel: None,
        }
    }

    #[tokio::test]
    async fn already_installed_only_handshakes() {
        let ver = format!("zellij {}\n", crate::zellij::version::ZELLIJ_VERSION);
        let ex = Script { calls: RefCell::default(), respond: Box::new(move |_, _| ok(&ver)) };
        let layout = ensure_zellij(&ex, &opts(Downloader::Curl)).await.unwrap();
        assert_eq!(layout.root, "/home/u/.falcon");
        let calls = ex.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].starts_with("mkdir -p '/home/u/.falcon/bin' "));
        assert!(calls[1].ends_with("'--version'"));
    }

    #[tokio::test]
    async fn no_downloader_fails_fast_with_manual_hint() {
        let ex = Script {
            calls: RefCell::default(),
            respond: Box::new(|cmd, _| if cmd.contains("--version") { ok("nope") } else { ok("") }),
        };
        let err = ensure_zellij(&ex, &opts(Downloader::None)).await.unwrap_err();
        assert_eq!(err.reason, ZellijInstallFailure::NoDownloader);
        assert!(err.detail.unwrap().contains("/home/u/.falcon/bin/zellij-"));
    }

    #[tokio::test(start_paused = true)]
    async fn download_failure_retries_then_reports_exit_code() {
        let ex = Script {
            calls: RefCell::default(),
            respond: Box::new(|cmd, _| {
                if cmd.contains("curl -fsSL") {
                    Ok(ExecResult { code: Some(28), stdout: String::new(), stderr: String::new() })
                } else if cmd.contains("--version") {
                    ok("")
                } else {
                    ok("")
                }
            }),
        };
        let err = ensure_zellij(&ex, &opts(Downloader::Curl)).await.unwrap_err();
        assert_eq!(err.reason, ZellijInstallFailure::DownloadFailed);
        assert_eq!(err.detail.as_deref(), Some("远端命令退出码 28"));
        let calls = ex.calls.borrow();
        assert_eq!(calls.iter().filter(|c| c.contains("curl -fsSL")).count(), MAX_ATTEMPTS as usize);
        // 每轮都清理临时目录
        assert_eq!(calls.iter().filter(|c| c.starts_with("rm -rf ")).count(), MAX_ATTEMPTS as usize);
    }

    #[test]
    fn windows_scripts_use_literal_path_for_removal() {
        let c = cleanup(HostKind::Windows, "C:\\Users\\a[1]b\\.falcon\\bin\\.tmp-x");
        let inner = crate::zellij::host::tests::decode(&c);
        assert!(inner.contains("-LiteralPath 'C:\\Users\\a[1]b\\.falcon\\bin\\.tmp-x'"));
        assert_eq!(
            display_cmd(HostKind::Windows, "enc", "a; b; c"),
            "a\nb\nc"
        );
    }
}

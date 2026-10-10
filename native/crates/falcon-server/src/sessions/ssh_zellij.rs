//! SSH 链路的会话部分：远端探测、Zellij 安装、会话查询与附着、askpass / agent 包装。
//! 移植自 `packages/server/src/sessions/ssh.ts` 的后半（传输层在 ssh.rs）。

use std::rc::Rc;

use falcon_proto::{NonDurableReason, SessionAgent, TermAppearance, ZellijInstallFailure, ZellijInstallStage};
use russh::ChannelMsg;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::agent::{launcher_dir, remote_launcher_path, write_remote_launcher_command};
use super::backend::{AttachResult, Backend, BackendCallbacks, SessionGoneError, normalize_captured};
use super::scroll_plugin::ensure_remote_scroll_plugin;
use super::ssh::SshLink;
use crate::askpass::hub::AskpassHub;
use crate::askpass::install::{askpass_bin_dir, posix_write_askpass_command};
use crate::exec::{Exec as _, ExecResult, Utf8Decoder};
use crate::term_env::apply_term_pty_env;
use crate::zellij::command::{self as zcmd, ScrollPosition, SessionProfile};
use crate::zellij::host::{
    Downloader, HostKind, HostLayout, POSIX_PROBE_LINES, WINDOWS_PROBE_SCRIPT, build_command_line,
    build_detached_command_line, build_pty_command_line, encode_powershell, legacy_remote_root, merge_env,
    migrate_remote_root_command, parse_migrated_root, parse_posix_probe, parse_windows_probe, posix_probe,
    quote_posix, quote_powershell, remote_root, windows_probe,
};
use crate::zellij::install::{InstallError, InstallOptions, StageFn, ensure_zellij, with_install_retry};
use crate::zellij::version::{ZELLIJ_VERSION, ZellijTarget, target_from_uname, target_from_windows_arch};

/// 远端宿主机探测结果
#[derive(Debug, Clone)]
pub struct RemoteProbe {
    pub kind: HostKind,
    pub home: String,
    /// 远端 falcon 根目录（已把 ~/.mojito 迁到 ~/.falcon，迁不了则仍是旧路径）
    pub root: String,
    pub target: Option<ZellijTarget>,
    pub downloader: Downloader,
    pub has_tar: bool,
    /// 远端登录 shell。项目没指定 shell 时用它，绝不留给 Zellij 自己猜——见 POSIX_PROBE
    pub shell: String,
    /// POSIX 的 `uname -sm` 原文；Windows 为 None。Zellij 以外的二进制（px0）按它选资产
    pub uname: Option<String>,
}

/// 远端 Zellij 就绪状态。durable=false 时 reason 必定有值。
#[derive(Debug, Clone)]
pub struct RemoteZellij {
    pub durable: bool,
    pub reason: Option<NonDurableReason>,
    /// 失败详情（远端 stderr 或异常消息），供用户判断该修什么再重试
    pub detail: Option<String>,
    pub layout: Option<HostLayout>,
    pub kind: HostKind,
    /// 滚动位置插件已就位且已预授权（ADR 0019）；新会话据此决定用哪套配置
    pub scroll: bool,
}

#[derive(Default)]
pub struct RemoteCache {
    pub probed: Option<RemoteProbe>,
    pub zellij: Option<RemoteZellij>,
}

/// 宿主机类型、家目录与默认 shell，供 git 层与 shell 侦测使用
#[derive(Debug, Clone)]
pub struct HostFacts {
    pub kind: HostKind,
    pub home: String,
    pub shell: String,
    pub root: String,
    pub uname: Option<String>,
}

/// 附着参数（本地 / SSH 共用）
#[derive(Debug, Clone, Default)]
pub struct AttachOptions {
    pub session_id: String,
    pub cwd: Option<String>,
    pub shell: Option<String>,
    pub durable: bool,
    /// 持久会话所用的 Zellij 布局；durable=true 时必须提供
    pub layout: Option<HostLayout>,
    /// true = 只接回已存在的会话，不隐式新建
    pub reattach: bool,
    pub cols: u16,
    pub rows: u16,
    /// 当前 Viewer 的终端深浅；接回时内层 shell 的 env 已经冻住，只影响新会话
    pub appearance: Option<TermAppearance>,
    /// sudo askpass 包装所在目录，会插到 PATH 最前；缺省不注入
    pub askpass_bin: Option<String>,
    /// 会话用带滚动位置插件的配置（ADR 0019），见 zellij::command::SessionProfile
    pub scroll: bool,
}

/// 安装失败原因与"非持久原因"共用同一套字面量（shared 里前者是后者的子集）
fn non_durable(reason: ZellijInstallFailure) -> NonDurableReason {
    NonDurableReason::from_wire(reason.as_str()).unwrap_or(NonDurableReason::Unknown)
}

impl SshLink {
    fn kind(&self) -> HostKind {
        self.remote.borrow().probed.as_ref().map(|p| p.kind).unwrap_or(HostKind::Posix)
    }

    fn probed_shell(&self) -> Option<String> {
        self.remote.borrow().probed.as_ref().map(|p| p.shell.clone())
    }

    /// 在远端落下 sudo 包装，并开一条 127.0.0.1 反向转发让 helper 能打到 Falcon 的
    /// /api/askpass。失败不报错——会话照开，只是 agent 的 sudo 还是没 tty。
    pub async fn ensure_askpass(self: &Rc<Self>, hub: &AskpassHub) -> Option<String> {
        let probe = self.probe(None, 1).await.ok()?;
        if probe.kind != HostKind::Posix {
            return None;
        }
        let bin_dir = askpass_bin_dir(HostKind::Posix, &probe.root);
        let helper_url = match self.ensure_askpass_tunnel(hub).await {
            Some(port) => format!("http://127.0.0.1:{port}/api/askpass"),
            None => hub.helper_url(),
        };
        let res = self.exec(&posix_write_askpass_command(&bin_dir, hub, &helper_url), None).await.ok()?;
        res.ok().then_some(bin_dir)
    }

    /// 在远端落下 agent 启动脚本，返回它的绝对路径。
    ///
    /// 与 ensure_askpass 同样是"写一个包装再把路径交出去"，但失败**不能**静默吞掉：
    /// 路径拿不到时调用方要退回普通 shell，不能把一个不存在的文件当 shell 传给
    /// Zellij——那样 pane 起不来，会话看起来就是空白一片。
    pub async fn ensure_agent_launcher(self: &Rc<Self>, agent: SessionAgent, shell: Option<&str>) -> Option<String> {
        let probe = self.probe(None, 1).await.ok()?;
        let dir = launcher_dir(probe.kind, &probe.root);
        let shell = shell.map(str::to_string).unwrap_or_else(|| probe.shell.clone());
        let res = self.exec(&write_remote_launcher_command(probe.kind, &dir, agent, &shell), None).await.ok()?;
        res.ok().then(|| remote_launcher_path(probe.kind, &probe.root, agent))
    }

    async fn ensure_askpass_tunnel(self: &Rc<Self>, hub: &AskpassHub) -> Option<u16> {
        if let Some(p) = self.askpass_port.get()
            && self.is_connected()
        {
            return Some(p);
        }
        let dest = url::Url::parse(&hub.helper_url()).ok()?;
        let dest_port = dest.port_or_known_default()?;
        let dest_host = dest.host_str().filter(|h| !h.is_empty()).unwrap_or("127.0.0.1").to_string();
        for _ in 0..8 {
            let mut r = [0u8; 2];
            let _ = getrandom::fill(&mut r);
            let port = 41200 + (u16::from_le_bytes(r) % 800);
            let Ok(mut incoming) = self.add_remote_forward("127.0.0.1", port).await else {
                // 远端端口占用，换一个
                continue;
            };
            self.askpass_port.set(Some(port));
            let host = dest_host.clone();
            tokio::task::spawn_local(async move {
                while let Some(channel) = incoming.recv().await {
                    let host = host.clone();
                    tokio::spawn(async move {
                        let Ok(mut socket) = tokio::net::TcpStream::connect((host.as_str(), dest_port)).await else {
                            return;
                        };
                        let mut stream = channel.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut stream, &mut socket).await;
                    });
                }
            });
            return Some(port);
        }
        None
    }

    /// 宿主机类型、家目录与默认 shell。probe 自带缓存，重复调用不产生往返
    pub async fn host_facts(self: &Rc<Self>) -> Result<HostFacts, InstallError> {
        let p = self.probe(None, 1).await?;
        Ok(HostFacts { kind: p.kind, home: p.home, shell: p.shell, root: p.root, uname: p.uname })
    }

    // ---- 探测与安装 ----

    /// 一次往返拿齐远端信息。先按 POSIX 试，失败再按 Windows 试——
    /// Windows 上没有 uname，POSIX 探测必然失败，这本身就是最可靠的判据。
    ///
    /// 失败一律报 probe-failed：探测跑不通最常见的原因是 SSH 连接本身出了问题（可重试），
    /// 跟"系统认不出"（不可重试）混成一句"无法识别远端操作系统"会让用户查错方向。
    /// 两者用 detail 区分。
    pub async fn probe(self: &Rc<Self>, on_stage: Option<StageFn<'_>>, attempt: u32) -> Result<RemoteProbe, InstallError> {
        if let Some(p) = self.remote.borrow().probed.clone() {
            return Ok(p);
        }
        let mut link_error: Option<String> = None;
        if let Some(f) = on_stage {
            f(ZellijInstallStage::Probing, attempt, Some(POSIX_PROBE_LINES.join("\n")));
        }
        let posix = match self.exec(&posix_probe(), None).await {
            Ok(r) => Some(r),
            Err(e) => {
                link_error = Some(format!("{e:#}"));
                None
            }
        };
        if let Some(p) = posix.filter(ExecResult::ok).and_then(|r| parse_posix_probe(&r.stdout)) {
            let root = self.resolve_remote_root(HostKind::Posix, &p.home).await;
            let probed = RemoteProbe {
                kind: HostKind::Posix,
                target: target_from_uname(&p.uname),
                home: p.home,
                root,
                downloader: p.downloader,
                has_tar: true,
                shell: p.shell,
                uname: Some(p.uname),
            };
            self.remote.borrow_mut().probed = Some(probed.clone());
            return Ok(probed);
        }

        if let Some(f) = on_stage {
            f(ZellijInstallStage::Probing, attempt, Some(WINDOWS_PROBE_SCRIPT.to_string()));
        }
        let win = match self.exec(&windows_probe(), None).await {
            Ok(r) => Some(r),
            Err(e) => {
                link_error.get_or_insert(format!("{e:#}"));
                None
            }
        };
        if let Some(w) = win.filter(ExecResult::ok).and_then(|r| parse_windows_probe(&r.stdout)) {
            let root = self.resolve_remote_root(HostKind::Windows, &w.home).await;
            let probed = RemoteProbe {
                kind: HostKind::Windows,
                target: target_from_windows_arch(&w.arch),
                home: w.home,
                root,
                downloader: w.downloader,
                has_tar: w.has_tar,
                shell: w.shell,
                uname: None,
            };
            self.remote.borrow_mut().probed = Some(probed.clone());
            return Ok(probed);
        }

        Err(InstallError::new(
            ZellijInstallFailure::ProbeFailed,
            Some(link_error.unwrap_or_else(|| "远端既不是 POSIX 也不是 Windows，或探测命令被 shell 改写".into())),
        ))
    }

    /// 把远端 ~/.mojito 迁到 ~/.falcon。探测拿到真实 home 之后再跑，路径整体加引号。
    /// 迁不了（被占用、没权限）就沿用旧目录，绝不指向一个空的新路径把会话弄丢。
    async fn resolve_remote_root(self: &Rc<Self>, kind: HostKind, home: &str) -> String {
        let next = remote_root(kind, home);
        let prev = legacy_remote_root(kind, home);
        match self.exec(&migrate_remote_root_command(kind, &next, &prev), None).await {
            Ok(res) => parse_migrated_root(&res.stdout).unwrap_or(next),
            Err(_) => next,
        }
    }

    /// 确保远端有可用的 Zellij，返回持久能力。
    ///
    /// 已授权是前提——调用方（SessionManager）负责先拿到用户对该主机的授权，
    /// 因为这会往用户的服务器上写入可执行文件。
    pub async fn prepare_zellij(self: &Rc<Self>, on_stage: Option<StageFn<'_>>, cancel: Option<&CancellationToken>) -> RemoteZellij {
        if let Some(z) = self.remote.borrow().zellij.clone() {
            return z;
        }
        let project = self.project();
        let host = project.ssh_host.clone().unwrap_or_default();
        let port = project.ssh_port.unwrap_or(22) as u16;
        let user = project.ssh_username.clone().unwrap_or_default();
        let saved = self.db().get_zellij_host(&host, port, &user);

        let result: Result<RemoteZellij, InstallError> = async {
            // 探测也走重试：这一步全靠 SSH 通道，抖一下就整个安装流程失败太亏
            let probe = with_install_retry(|attempt| self.probe(on_stage, attempt), cancel).await?;
            let Some(target) = probe.target else {
                return Ok(RemoteZellij {
                    durable: false,
                    reason: Some(NonDurableReason::ArchUnsupported),
                    detail: None,
                    layout: None,
                    kind: probe.kind,
                    scroll: false,
                });
            };
            let opts = InstallOptions {
                kind: probe.kind,
                root: probe.root.clone(),
                target,
                base_url: saved.as_ref().and_then(|s| s.base_url.clone()),
                downloader: probe.downloader,
                has_tar: Some(probe.has_tar),
                on_stage,
                cancel,
            };
            let layout = ensure_zellij(self.as_ref(), &opts).await?;
            self.db().upsert_zellij_host(
                &host,
                port,
                &user,
                crate::db::ZellijHostPatch { installed_version: Some(Some(ZELLIJ_VERSION.into())), ..Default::default() },
            );

            // Windows 远端：装好不等于能持久。Zellij server 没有脱离父 Job 的能力
            // （PR #5195 未合并），sshd 在 exec 通道关闭时就会清掉整个进程树，所以我们
            // 经 WMI 把 server 生到 sshd 树之外（见 build_detached_command_line）。这条路
            // 依赖宿主机的 DCOM/WMI 权限，只能实测，不能靠文档判断。
            //
            // 判定只在"当前锁定版本"上有效：换过 Zellij 版本（或检测手段变了）之后，
            // 旧的"非持久"结论不该把主机永远钉死在非持久上。
            if probe.kind == HostKind::Windows {
                let mut ok = saved
                    .as_ref()
                    .filter(|s| s.installed_version.as_deref() == Some(ZELLIJ_VERSION))
                    .and_then(|s| s.verified_durable);
                if ok.is_none() {
                    let v = if self.verify_durability(&layout).await { 1 } else { 0 };
                    self.db().upsert_zellij_host(
                        &host,
                        port,
                        &user,
                        crate::db::ZellijHostPatch { verified_durable: Some(Some(v)), ..Default::default() },
                    );
                    ok = Some(v);
                }
                if ok != Some(1) {
                    return Ok(RemoteZellij {
                        durable: false,
                        reason: Some(NonDurableReason::VerifyFailed),
                        detail: Some("后台 Zellij 会话没能熬过 SSH 断开（宿主机可能限制了 WMI 进程创建）".into()),
                        layout: None,
                        kind: probe.kind,
                        scroll: false,
                    });
                }
            }

            // 插件部署失败不挡开会话，新会话退回老配置（没有滚动条）而已
            let scroll = probe.kind == HostKind::Posix && ensure_remote_scroll_plugin(self, &layout, target, &probe.home).await;
            Ok(RemoteZellij { durable: true, reason: None, detail: None, layout: Some(layout), kind: probe.kind, scroll })
        }
        .await;

        let state = match result {
            Ok(z) => z,
            Err(err) => {
                let failed = RemoteZellij {
                    durable: false,
                    reason: Some(non_durable(err.reason)),
                    detail: err.detail.clone(),
                    layout: None,
                    kind: self.kind(),
                    scroll: false,
                };
                // 取消不是这台主机的属性，是用户这一次的选择——缓存它会让后续建会话
                // 全部莫名其妙地非持久，直到后端重启
                if err.reason == ZellijInstallFailure::Cancelled {
                    return failed;
                }
                failed
            }
        };
        self.remote.borrow_mut().zellij = Some(state.clone());
        state
    }

    /// 重试安装前清掉缓存的判定。
    ///
    /// verified_durable 必须连 DB 一起清：它是持久化的，只清内存的话用户点一百次
    /// 重试都是同一个秒回的旧结论——比如修好了 WMI 权限之后仍被钉在"非持久"上。
    pub fn reset_zellij(&self) {
        *self.remote.borrow_mut() = RemoteCache::default();
        let p = self.project();
        if let (Some(host), Some(user)) = (p.ssh_host.as_deref().filter(|h| !h.is_empty()), p.ssh_username.as_deref().filter(|u| !u.is_empty())) {
            self.db().upsert_zellij_host(
                host,
                p.ssh_port.unwrap_or(22) as u16,
                user,
                crate::db::ZellijHostPatch { verified_durable: Some(None), ..Default::default() },
            );
        }
    }

    /// 真实断线验证：建一个后台会话 → 断开 SSH → 重连 → 看它还在不在。
    ///
    /// 这是唯一能回答"这台机器上的会话到底能不能熬过断线"的办法。只在每台主机上
    /// 跑一次，结果持久化到 zellij_hosts。
    ///
    /// 创建走 WMI（build_detached_command_line），与真实会话的创建路径完全一致——
    /// 验证的就是这条路本身：普通 exec 拉起的 server 会随 exec 通道关闭被 sshd
    /// 连坐杀掉，根本活不到断线那一步。
    async fn verify_durability(self: &Rc<Self>, layout: &HostLayout) -> bool {
        let kind = self.kind();
        let name = format!("mj-verify-{}", &crate::askpass::hub::uuid_v4()[..8]);
        let env = zcmd::zellij_env(layout);
        let argv = [layout.bin.as_str(), "--data-dir", &layout.data_dir, "attach", &name, "--create-background"];
        // 后台建一个 detached session，无需 PTY；经 WMI 生到 sshd 进程树之外
        let created = self.exec(&build_detached_command_line(&argv, &env, None), None).await;
        if !created.as_ref().is_ok_and(ExecResult::ok) {
            return false;
        }
        self.dispose();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let run = |args: &[&str]| {
            let argv: Vec<&str> = std::iter::once(layout.bin.as_str()).chain(args.iter().copied()).collect();
            build_command_line(kind, &argv, &env, &[])
        };
        let alive = match self.exec(&run(&["--data-dir", &layout.data_dir, "list-sessions", "--no-formatting"]), None).await {
            Ok(ls) => ls.ok() && zcmd::parse_live_sessions(&ls.stdout).contains(&name),
            Err(_) => false,
        };
        let _ = self.exec(&run(&["--data-dir", &layout.data_dir, "delete-session", &name, "--force"]), None).await;
        alive
    }

    // ---- 会话查询 ----

    async fn zellij_exec(self: &Rc<Self>, layout: &HostLayout, args: Vec<String>) -> anyhow::Result<ExecResult> {
        let argv: Vec<String> = std::iter::once(layout.bin.clone()).chain(args).collect();
        let cmd = build_command_line(self.kind(), &argv, &zcmd::zellij_env(layout), &[]);
        self.exec(&cmd, None).await
    }

    pub async fn has_session(self: &Rc<Self>, layout: &HostLayout, session_id: &str) -> anyhow::Result<bool> {
        // 一个 session 都没有时 `ls` 退出码为 1，不代表出错
        let res = self.zellij_exec(layout, zcmd::list_args(layout)).await?;
        Ok(res.ok() && zcmd::parse_live_sessions(&res.stdout).contains(&zcmd::zellij_session_name(session_id)))
    }

    /// 会话聚焦 pane 的前台命令；空闲或问不到时为 None。见 parse_client_running_command
    pub async fn foreground(self: &Rc<Self>, layout: &HostLayout, session_id: &str) -> anyhow::Result<Option<String>> {
        let res = self.zellij_exec(layout, zcmd::list_clients_args(layout, session_id)).await?;
        Ok(if res.ok() { zcmd::parse_client_running_command(&res.stdout) } else { None })
    }

    pub async fn capture(self: &Rc<Self>, layout: &HostLayout, session_id: &str) -> anyhow::Result<String> {
        // 多一次往返换来的是接回时真能拿到内容，理由见 dump_screen_args
        let panes = self.zellij_exec(layout, zcmd::list_panes_args(layout, session_id)).await?;
        let Some(pane_id) = panes.ok().then(|| zcmd::parse_terminal_pane_id(&panes.stdout)).flatten() else {
            return Ok(String::new());
        };
        let res = self.zellij_exec(layout, zcmd::dump_screen_args(layout, session_id, &pane_id)).await?;
        if !res.ok() {
            return Ok(String::new());
        }
        Ok(normalize_captured(&format!("{}\n", crate::term_env::js::trim_end(&res.stdout))))
    }

    /// 会话里 terminal pane 的数字 id（滚动位置插件按它找 pane）；问不到为 None
    pub async fn terminal_pane(self: &Rc<Self>, layout: &HostLayout, session_id: &str) -> anyhow::Result<Option<u32>> {
        let res = self.zellij_exec(layout, zcmd::list_panes_args(layout, session_id)).await?;
        Ok(res.ok().then(|| zcmd::parse_terminal_pane_id(&res.stdout)).flatten().and_then(|id| zcmd::parse_terminal_pane_number(&id)))
    }

    /// 问滚动位置插件（ADR 0019）。会话里没有插件时回 None。
    ///
    /// 走 exec_with_input 而不是 exec：前者写完即 EOF，`zellij pipe` 在 stdin 不是终端时
    /// 要读到 EOF 才退出（见 scroll_pipe_args），exec 的 stdin 一直开着会挂住通道。
    /// 每次一条短命 channel，不常驻：sshd 默认 MaxSessions 10，会话的 PTY 已经各占一条。
    pub async fn scroll_pipe(
        self: &Rc<Self>,
        layout: &HostLayout,
        session_id: &str,
        pane: u32,
        seek: Option<f64>,
    ) -> anyhow::Result<Option<ScrollPosition>> {
        let argv: Vec<String> = std::iter::once(layout.bin.clone()).chain(zcmd::scroll_pipe_args(layout, session_id, pane, seek)).collect();
        let cmd = build_command_line(self.kind(), &argv, &zcmd::zellij_env(layout), &[]);
        let res = self.exec_with_input(&cmd, b"").await?;
        Ok(zcmd::parse_scroll_reply(&res.stdout))
    }

    pub async fn kill_session(self: &Rc<Self>, layout: &HostLayout, session_id: &str) {
        let _ = self.zellij_exec(layout, zcmd::delete_session_args(layout, session_id)).await;
    }

    /// 该主机上所有 falcon 建的会话名（孤儿会话检测用）
    pub async fn list_falcon_sessions(self: &Rc<Self>, layout: &HostLayout) -> Vec<String> {
        match self.zellij_exec(layout, zcmd::list_args(layout)).await {
            Ok(res) if res.ok() => zcmd::parse_live_sessions(&res.stdout).into_iter().filter(|n| zcmd::is_falcon_session(n)).collect(),
            _ => Vec::new(),
        }
    }

    // ---- 附着 ----

    pub async fn attach_session(self: &Rc<Self>, opts: &AttachOptions, cb: BackendCallbacks) -> anyhow::Result<AttachResult> {
        self.get_client().await?;
        let kind = self.kind();
        let probed_shell = self.probed_shell();

        let mut captured_history = None;
        if opts.durable && opts.reattach {
            let layout = opts.layout.as_ref().ok_or(SessionGoneError)?;
            if !self.has_session(layout, &opts.session_id).await? {
                return Err(SessionGoneError.into());
            }
            captured_history = Some(self.capture(layout, &opts.session_id).await?);
        }

        let mut term_env = apply_term_pty_env(Vec::<(String, String)>::new(), opts.appearance);
        if opts.askpass_bin.is_some() {
            merge_env(&mut term_env, [("FALCON_SESSION_ID", opts.session_id.as_str())]);
        }

        // Windows 持久会话：server 必须生在 sshd 进程树之外，否则创建它的 PTY 通道
        // 一关（关标签、断网）server 就被 sshd 连坐杀掉，"持久"名存实亡。先经 WMI
        // 后台建好——带全部会话级 options，它们只在创建时生效——下面的 PTY attach
        // 就是纯附着，attach 客户端死掉不影响 server。reattach 时会话已存在，跳过。
        if opts.durable && kind == HostKind::Windows && !opts.reattach {
            let layout = opts.layout.as_ref().ok_or(SessionGoneError)?;
            if !self.has_session(layout, &opts.session_id).await? {
                // Zellij 0.44.3 的 create-background 路径会丢掉 `attach ... options` 里的
                // 会话级选项：start_server_detached 的 New 分支发给 server 的是
                // cli_args.options()——它只认顶层 `zellij options ...` 子命令，attach 下的
                // options 解析完就地蒸发（Resurrect 分支反而正确地带上了合并结果）。
                // 其余选项在我们写的 config.kdl 里都有副本所以看不出来，唯独
                // default-shell / default-cwd 只在 CLI 上传：表现为 Windows 持久会话
                // 落进 cmd（get_default_shell 退到 COMSPEC）、目录落在 WmiPrvSE 的
                // System32。兜底：get_default_shell 在 Windows 上先查 $SHELL 再退
                // COMSPEC，把 shell 塞进 server 的环境变量；cwd 走 WMI 的
                // CurrentDirectory 让 server 生在项目目录里（初始 pane 继承 server 目录）。
                // argv 里的 --default-shell / --default-cwd 保留：上游修好后它们才是正路。
                let shell = opts.shell.clone().or(probed_shell.clone());
                let profile = SessionProfile { cwd: opts.cwd.clone(), shell: shell.clone(), scroll: opts.scroll };
                let argv: Vec<String> =
                    std::iter::once(layout.bin.clone()).chain(zcmd::create_background_args(layout, &opts.session_id, &profile)).collect();
                let mut env = zcmd::zellij_env(layout);
                merge_env(&mut env, term_env.clone());
                if let Some(sh) = &shell {
                    merge_env(&mut env, [("SHELL", sh.as_str())]);
                }
                let created = self.exec(&build_detached_command_line(&argv, &env, opts.cwd.as_deref()), None).await?;
                // WMI 的退出码只是尽力而为（见 build_detached_command_line），
                // 会话真建出来没有以 has_session 的事实为准
                if !self.has_session(layout, &opts.session_id).await? {
                    let out = [created.stderr.trim(), created.stdout.trim()].into_iter().find(|s| !s.is_empty()).unwrap_or("无输出").to_string();
                    anyhow::bail!(
                        "无法在 Windows 远端后台创建 Zellij 会话（退出码 {}）：{out}",
                        created.code.map_or("null".to_string(), |c| c.to_string())
                    );
                }
            }
        }

        let cmd = if opts.durable {
            let layout = opts.layout.as_ref().ok_or(SessionGoneError)?;
            // 项目没指定就用探测到的登录 shell，不能不传
            let profile =
                SessionProfile { cwd: opts.cwd.clone(), shell: opts.shell.clone().or(probed_shell.clone()), scroll: opts.scroll };
            let argv: Vec<String> = std::iter::once(layout.bin.clone()).chain(zcmd::attach_args(layout, &opts.session_id, &profile)).collect();
            let mut env = zcmd::zellij_env(layout);
            merge_env(&mut env, term_env.clone());
            // 包一层登录 shell，否则 ~/.profile 里的 PATH 全丢——详见 build_pty_command_line
            build_pty_command_line(kind, &argv, &env, probed_shell.as_deref(), opts.askpass_bin.as_deref())
        } else if kind == HostKind::Windows {
            // 非持久：直接起 shell。一律走 exec + env，不能用 shell 请求——塞不进 COLORFGBG / COLORTERM
            let mut parts: Vec<String> = term_env.iter().map(|(k, v)| format!("$env:{k} = {}", quote_powershell(v))).collect();
            if let Some(cwd) = opts.cwd.as_deref().filter(|c| !c.is_empty()) {
                parts.push(format!("Set-Location -LiteralPath {}", quote_powershell(cwd)));
            }
            parts.push(match opts.shell.as_deref().filter(|s| !s.is_empty()) {
                Some(sh) => format!("& {}", quote_powershell(sh)),
                None => "powershell".into(),
            });
            encode_powershell(&parts.join("; "))
        } else {
            // 非持久会话同样要走登录 shell，否则 PATH 与持久会话不一致
            let assigns: Vec<String> = term_env.iter().map(|(k, v)| format!("{k}={}", quote_posix(v))).collect();
            let prefix = if assigns.is_empty() { String::new() } else { format!("env {} ", assigns.join(" ")) };
            let path_prepend = opts.askpass_bin.as_deref().map(|b| format!("PATH={}:$PATH ", quote_posix(b))).unwrap_or_default();
            let cd = opts.cwd.as_deref().filter(|c| !c.is_empty()).map(|c| format!("cd {} && ", quote_posix(c))).unwrap_or_default();
            let sh = quote_posix(opts.shell.as_deref().or(probed_shell.as_deref()).unwrap_or("/bin/sh"));
            format!("{cd}exec {path_prepend}{prefix}{sh} -l")
        };

        let channel = self.exec_pty(&cmd, opts.cols, opts.rows).await?;
        Ok(AttachResult { backend: spawn_channel_backend(channel, cb), durable: opts.durable, captured_history })
    }
}

enum ChannelCmd {
    Data(Vec<u8>),
    Resize(u16, u16),
    Close,
}

/// PTY 通道 → Backend。读写分两个任务：读端按块流式解码（TCP 分包会把 UTF-8 多字节字符
/// 切开，stdout / stderr 各一个解码器）后回调，写端从队列里取（Backend::write 是同步的）
fn spawn_channel_backend(channel: russh::Channel<russh::client::Msg>, cb: BackendCallbacks) -> Rc<dyn Backend> {
    let (mut read, write) = channel.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ChannelCmd>();
    tokio::task::spawn_local(async move {
        let mut out = Utf8Decoder::default();
        let mut err = Utf8Decoder::default();
        loop {
            match read.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    let s = out.write(&data);
                    if !s.is_empty() {
                        (cb.on_data)(s);
                    }
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    let s = err.write(&data);
                    if !s.is_empty() {
                        (cb.on_data)(s);
                    }
                }
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            }
        }
        (cb.on_exit)();
    });
    tokio::task::spawn_local(async move {
        while let Some(cmd) = rx.recv().await {
            let ok = match cmd {
                ChannelCmd::Data(d) => write.data_bytes(d).await.is_ok(),
                ChannelCmd::Resize(cols, rows) => write.window_change(cols as u32, rows as u32, 0, 0).await.is_ok(),
                ChannelCmd::Close => {
                    let _ = write.close().await;
                    break;
                }
            };
            if !ok {
                break;
            }
        }
    });
    Rc::new(ChannelBackend { tx })
}

struct ChannelBackend {
    tx: mpsc::UnboundedSender<ChannelCmd>,
}

impl Backend for ChannelBackend {
    fn write(&self, data: &str) {
        let _ = self.tx.send(ChannelCmd::Data(data.as_bytes().to_vec()));
    }

    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.tx.send(ChannelCmd::Resize(cols, rows));
    }

    fn destroy(&self) {
        let _ = self.tx.send(ChannelCmd::Close);
    }
}

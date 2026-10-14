//! SSH 链路的会话部分：远端探测、会话守护进程的部署与验证、会话附着、askpass / agent 包装。
//! 移植自 `packages/server/src/sessions/ssh.ts` 的后半（传输层在 ssh.rs）；Zellij 退役后
//! 持久会话改由宿主机上的 falcon-sessiond 承担（ADR 0020）。

use std::rc::Rc;
use std::time::{Duration, Instant};

use falcon_proto::sessiond::{ListEntry, SpawnSpec, sid_of};
use falcon_proto::{HostKind, InstallFailure, InstallStage, NonDurableReason, SessionAgent, TermAppearance};
use russh::ChannelMsg;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::agent::{launcher_dir, remote_launcher_path, write_remote_launcher_command};
use super::backend::{AttachResult, Backend, BackendCallbacks};
use super::ssh::SshLink;
use crate::askpass::hub::AskpassHub;
use crate::askpass::install::{askpass_bin_dir, posix_write_askpass_command};
use crate::exec::{Exec as _, ExecResult, Utf8Decoder};
use crate::host::{
    POSIX_PROBE_LINES, WINDOWS_PROBE_SCRIPT, encode_powershell, legacy_remote_root, migrate_remote_root_command,
    parse_migrated_root, parse_posix_probe, parse_windows_probe, posix_probe, quote_posix, quote_powershell, remote_root,
    windows_probe,
};
use crate::sessiond::backend::{ConnectError, SessiondBackend, Transport, connect};
use crate::sessiond::command::{
    SessiondTarget, bridge_argv, bridge_command, exec_command, kill_argv, list_argv, spawn_argv, target_from_uname,
    target_from_windows_arch,
};
use crate::sessiond::deploy::{Deployed, InstallError, StageFn, ensure_remote, with_install_retry};
use crate::term_env::apply_term_pty_env;

/// 远端宿主机探测结果
#[derive(Debug, Clone)]
pub struct RemoteProbe {
    pub kind: HostKind,
    pub home: String,
    /// 远端 falcon 根目录（已把 ~/.mojito 迁到 ~/.falcon，迁不了则仍是旧路径）
    pub root: String,
    /// 会话守护进程的构建目标；None = 这个系统 / 架构没有
    pub target: Option<SessiondTarget>,
    /// 远端登录 shell。项目没指定 shell 时用它
    pub shell: String,
    /// POSIX 的 `uname -sm` 原文；Windows 为 None。守护进程以外的二进制（px0）按它选资产
    pub uname: Option<String>,
}

/// 远端的持久能力。durable=false 时 reason 必定有值
#[derive(Debug, Clone)]
pub struct RemoteSessiond {
    pub durable: bool,
    pub reason: Option<NonDurableReason>,
    /// 失败详情（远端 stderr 或异常消息），供用户判断该修什么再重试
    pub detail: Option<String>,
    pub deployed: Option<Deployed>,
    pub kind: HostKind,
}

#[derive(Default)]
pub struct RemoteCache {
    pub probed: Option<RemoteProbe>,
    pub sessiond: Option<RemoteSessiond>,
    /// sessiond 是什么时候定下的：接回时放凉了的失败不作数（[`SshLink::forget_failed_sessiond`]）
    pub sessiond_at: Option<Instant>,
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
    /// 持久会话：宿主机上已就位的守护进程；durable=true 时必须提供
    pub deployed: Option<Deployed>,
    /// true = 只接回已存在的会话，不隐式新建
    pub reattach: bool,
    pub cols: u16,
    pub rows: u16,
    /// 当前 Viewer 的终端深浅；会话的 env 在创建时冻住，只影响新会话
    pub appearance: Option<TermAppearance>,
    /// 当前 Viewer 的前景 / 背景色（`#rrggbb`）：守护进程会话的 VT 进程一起来就按它答 OSC 10/11
    pub foreground: Option<String>,
    pub background: Option<String>,
    /// sudo askpass 包装所在目录，会插到 PATH 最前；缺省不注入
    pub askpass_bin: Option<String>,
    /// 建会话的 falcon 实例 id（孤儿清理只动自己的）
    pub owner: String,
}

/// 部署失败原因与"非持久原因"共用同一套字面量（前者是后者的子集）
fn non_durable(reason: InstallFailure) -> NonDurableReason {
    NonDurableReason::from(reason)
}

/// 守护进程会话的启动参数（设计文档 4、10.4）
pub fn spawn_spec(opts: &AttachOptions, argv: Vec<String>, env: Vec<(String, String)>, env_clear: bool) -> SpawnSpec {
    SpawnSpec {
        session_id: opts.session_id.clone(),
        argv,
        env,
        env_clear,
        cwd: opts.cwd.clone().filter(|c| !c.is_empty()),
        cols: opts.cols,
        rows: opts.rows,
        scrollback_lines: 10_000,
        owner: opts.owner.clone(),
        appearance: Some(falcon_proto::sessiond::Appearance {
            scheme: opts.appearance.map(|a| match a {
                TermAppearance::Light => falcon_proto::sessiond::Scheme::Light,
                _ => falcon_proto::sessiond::Scheme::Dark,
            }),
            foreground: opts.foreground.clone(),
            background: opts.background.clone(),
            cursor: None,
        }),
    }
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
    /// 路径拿不到时调用方要退回普通 shell，不能把一个不存在的文件当 shell 交给守护进程——
    /// 那样会话起来就是一片空白。
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

    // ---- 探测与部署 ----

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
            f(InstallStage::Probing, attempt, Some(POSIX_PROBE_LINES.join("\n")));
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
                shell: p.shell,
                uname: Some(p.uname),
            };
            self.remote.borrow_mut().probed = Some(probed.clone());
            return Ok(probed);
        }

        if let Some(f) = on_stage {
            f(InstallStage::Probing, attempt, Some(WINDOWS_PROBE_SCRIPT.to_string()));
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
                shell: w.shell,
                uname: None,
            };
            self.remote.borrow_mut().probed = Some(probed.clone());
            return Ok(probed);
        }

        Err(InstallError::new(
            InstallFailure::ProbeFailed,
            Some(link_error.unwrap_or_else(|| "远端既不是 POSIX 也不是 Windows，或探测命令被 shell 改写".into())),
        ))
    }

    /// 把远端 ~/.mojito 迁到 ~/.falcon。探测拿到真实 home 之后再跑，路径整体加引号。
    /// 迁不了（被占用、没权限）就沿用旧目录，绝不指向一个空的新路径把东西弄丢。
    async fn resolve_remote_root(self: &Rc<Self>, kind: HostKind, home: &str) -> String {
        let next = remote_root(kind, home);
        let prev = legacy_remote_root(kind, home);
        match self.exec(&migrate_remote_root_command(kind, &next, &prev), None).await {
            Ok(res) => parse_migrated_root(&res.stdout).unwrap_or(next),
            Err(_) => next,
        }
    }

    /// 确保远端有可用的会话守护进程，返回持久能力。
    ///
    /// 已授权是前提——调用方（SessionManager）负责先拿到用户对该主机的授权，
    /// 因为这会往用户的服务器上写入可执行文件。
    pub async fn prepare_sessiond(self: &Rc<Self>, on_stage: Option<StageFn<'_>>, cancel: Option<&CancellationToken>) -> RemoteSessiond {
        if let Some(z) = self.remote.borrow().sessiond.clone() {
            return z;
        }
        let project = self.project();
        let host = project.ssh_host.clone().unwrap_or_default();
        let port = project.ssh_port.unwrap_or(22) as u16;
        let user = project.ssh_username.clone().unwrap_or_default();

        let result: Result<RemoteSessiond, InstallError> = async {
            // 探测也走重试：这一步全靠 SSH 通道，抖一下就整个流程失败太亏
            let probe = with_install_retry(|attempt| self.probe(on_stage, attempt), cancel).await?;
            let Some(target) = probe.target else {
                return Ok(RemoteSessiond {
                    durable: false,
                    reason: Some(NonDurableReason::ArchUnsupported),
                    detail: probe.uname.clone(),
                    deployed: None,
                    kind: probe.kind,
                });
            };
            let deployed = with_install_retry(
                |attempt| ensure_remote(self, probe.kind, &probe.root, target, on_stage, cancel, attempt),
                cancel,
            )
            .await?;
            let saved = self.db().get_session_host(&host, port, &user);
            // 记 falcon 的版本号：客户端拿它与 required_version 比，对不上就弹出部署进度
            let version = falcon_proto::sessiond::FALCON_VERSION.to_string();
            self.db().upsert_session_host(
                &host,
                port,
                &user,
                crate::db::SessionHostPatch { sessiond_version: Some(Some(version.clone())), ..Default::default() },
            );

            // Windows 远端：推上去不等于能持久。sshd 在 exec 通道关闭时清掉整个进程树（Job Object），
            // 守护进程要靠 breakaway 或 WMI 生到 sshd 树之外（falcon-sessiond 的 spawn.rs）。这条路
            // 依赖宿主机的 Job 设置与 DCOM/WMI 权限，只能实测：建一个后台会话、断开 SSH、重连看它还在不在。
            // 判定只对当前这份二进制有效：换了版本旧结论不该把主机永远钉死。
            if probe.kind == HostKind::Windows {
                let mut ok = saved.as_ref().filter(|s| s.sessiond_version.as_deref() == Some(version.as_str())).and_then(|s| s.verified_durable);
                if ok.is_none() {
                    let v = if self.verify_durability(&deployed, &probe).await { 1 } else { 0 };
                    self.db().upsert_session_host(
                        &host,
                        port,
                        &user,
                        crate::db::SessionHostPatch { verified_durable: Some(Some(v)), ..Default::default() },
                    );
                    ok = Some(v);
                }
                if ok != Some(1) {
                    return Ok(RemoteSessiond {
                        durable: false,
                        reason: Some(NonDurableReason::VerifyFailed),
                        detail: Some("后台会话没能熬过 SSH 断开（宿主机的 Job 不允许脱离、WMI 也拉不起进程）".into()),
                        deployed: None,
                        kind: probe.kind,
                    });
                }
            }
            Ok(RemoteSessiond { durable: true, reason: None, detail: None, deployed: Some(deployed), kind: probe.kind })
        }
        .await;

        let state = match result {
            Ok(z) => z,
            Err(err) => {
                let failed = RemoteSessiond {
                    durable: false,
                    reason: Some(non_durable(err.reason)),
                    detail: err.detail.clone(),
                    deployed: None,
                    kind: self.kind(),
                };
                // 取消不是这台主机的属性，是用户这一次的选择——缓存它会让后续建会话
                // 全部莫名其妙地非持久，直到后端重启
                if err.reason == InstallFailure::Cancelled {
                    return failed;
                }
                failed
            }
        };
        let mut remote = self.remote.borrow_mut();
        remote.sessiond = Some(state.clone());
        remote.sessiond_at = Some(Instant::now());
        state
    }

    /// 已经定下的持久能力（不触发探测与部署）
    pub fn cached_sessiond(&self) -> Option<RemoteSessiond> {
        self.remote.borrow().sessiond.clone()
    }

    /// 缓存的部署失败定下超过 `cooldown` 了就丢掉，下次 prepare 重新推。
    ///
    /// 失败缓存着是给建会话用的：每建一个都重推一遍太亏，用户点「重试」才清（[`Self::reset_sessiond`]）。
    /// 等着接回的会话却不能被一次失败钉死，不然它只能一直黄着，直到后端重启。冷却挡的是同一轮里
    /// 排队接回的几个会话、以及退避到顶后每 30 秒一轮的重连，别各推一遍几 MB 的二进制。
    /// 只丢内存里的：Windows 断线验证的结论在库里，换了版本才重验（见 prepare_sessiond）
    pub fn forget_failed_sessiond(&self, cooldown: Duration) {
        let mut remote = self.remote.borrow_mut();
        let failed = remote.sessiond.as_ref().is_some_and(|s| !s.durable);
        if failed && remote.sessiond_at.is_none_or(|at| at.elapsed() >= cooldown) {
            remote.sessiond = None;
            remote.sessiond_at = None;
        }
    }

    /// 重试部署前清掉缓存的判定。
    ///
    /// verified_durable 必须连 DB 一起清：它是持久化的，只清内存的话用户点一百次
    /// 重试都是同一个秒回的旧结论——比如修好了 WMI 权限之后仍被钉在"非持久"上。
    pub fn reset_sessiond(&self) {
        *self.remote.borrow_mut() = RemoteCache::default();
        let p = self.project();
        if let (Some(host), Some(user)) =
            (p.ssh_host.as_deref().filter(|h| !h.is_empty()), p.ssh_username.as_deref().filter(|u| !u.is_empty()))
        {
            self.db().upsert_session_host(
                host,
                p.ssh_port.unwrap_or(22) as u16,
                user,
                crate::db::SessionHostPatch { verified_durable: Some(None), ..Default::default() },
            );
        }
    }

    /// 真实断线验证：建一个后台会话 → 断开 SSH → 重连 → 看它还在不在。
    ///
    /// 这是唯一能回答"这台机器上的会话到底能不能熬过断线"的办法。只在每台主机上
    /// 跑一次（每份二进制），结果持久化到 session_hosts。
    async fn verify_durability(self: &Rc<Self>, deployed: &Deployed, probe: &RemoteProbe) -> bool {
        let kind = deployed.kind;
        let mut r = [0u8; 8];
        let _ = getrandom::fill(&mut r);
        let sid = hex::encode(r);
        let spec = SpawnSpec {
            session_id: format!("verify-{sid}"),
            argv: vec![probe.shell.clone(), "-NoProfile".into(), "-Command".into(), "Start-Sleep -Seconds 600".into()],
            cols: 80,
            rows: 24,
            scrollback_lines: 100,
            owner: self.db().instance_id(),
            ..Default::default()
        };
        let spawned = self.exec(&exec_command(kind, &spawn_argv(&deployed.bin, &deployed.root, &sid, &spec)), None).await;
        if !spawned.as_ref().is_ok_and(ExecResult::ok) {
            log::warn!("Windows 断线验证：起不来后台会话：{spawned:?}");
            return false;
        }
        self.dispose();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let alive = self.list_sessions(deployed).await.is_some_and(|l| l.iter().any(|e| e.sid == sid && e.alive));
        let _ = self.exec(&exec_command(kind, &kill_argv(&deployed.bin, &deployed.root, &[sid])), None).await;
        alive
    }

    /// 宿主机上这个根下的全部会话（孤儿清理用）；问不到为 None
    pub async fn list_sessions(self: &Rc<Self>, deployed: &Deployed) -> Option<Vec<ListEntry>> {
        let res = self.exec(&exec_command(deployed.kind, &list_argv(&deployed.bin, &deployed.root)), None).await.ok()?;
        if !res.ok() {
            return None;
        }
        res.stdout.lines().find_map(|l| serde_json::from_str(l.trim()).ok())
    }

    /// 连不上 holder 时按 pid 结束会话
    pub async fn kill_sessions(self: &Rc<Self>, deployed: &Deployed, sids: &[String]) {
        if sids.is_empty() {
            return;
        }
        let _ = self.exec(&exec_command(deployed.kind, &kill_argv(&deployed.bin, &deployed.root, sids)), None).await;
    }

    // ---- 附着 ----

    /// 持久会话：经 bridge 连上宿主机上的 holder（新建时带 `--create`，由它拉起会话）
    pub async fn open_sessiond(
        self: &Rc<Self>,
        opts: &AttachOptions,
        cb: BackendCallbacks,
    ) -> Result<(Rc<SessiondBackend>, falcon_proto::sessiond::Welcome), ConnectError> {
        self.get_client().await.map_err(|e| ConnectError::Failed(e.message))?;
        let deployed = opts.deployed.as_ref().ok_or_else(|| ConnectError::Failed("缺少宿主机上的守护进程".into()))?;
        let kind = deployed.kind;
        let sid = sid_of(&opts.session_id);
        let spec = (!opts.reattach).then(|| {
            // 项目没指定就用探测到的登录 shell，不能不传
            let shell = opts.shell.clone().or_else(|| self.probed_shell()).unwrap_or_else(|| "/bin/sh".into());
            let mut env = apply_term_pty_env(Vec::<(String, String)>::new(), opts.appearance, kind);
            if opts.askpass_bin.is_some() {
                env.push(("FALCON_SESSION_ID".into(), opts.session_id.clone()));
            }
            env.push(("FALCON_SESSIOND_ID".into(), sid.clone()));
            // SSH：在 holder 自己的环境（登录 shell 给的）上叠加，不清空
            spawn_spec(opts, vec![shell], env, false)
        });
        let argv = bridge_argv(&deployed.bin, &deployed.root, &sid, spec.as_ref());
        let login = self.probed_shell();
        let command = bridge_command(kind, &argv, login.as_deref(), opts.askpass_bin.as_deref());
        connect(Transport::Ssh { link: self.clone(), command }, cb).await
    }

    /// 非持久会话：一条普通的 PTY channel 直接跑 shell
    pub async fn attach_session(self: &Rc<Self>, opts: &AttachOptions, cb: BackendCallbacks) -> anyhow::Result<AttachResult> {
        self.get_client().await?;
        let kind = self.kind();
        let probed_shell = self.probed_shell();

        let mut term_env = apply_term_pty_env(Vec::<(String, String)>::new(), opts.appearance, kind);
        if opts.askpass_bin.is_some() {
            term_env.push(("FALCON_SESSION_ID".into(), opts.session_id.clone()));
        }

        let cmd = if kind == HostKind::Windows {
            // 一律走 exec + env，不能用 shell 请求——塞不进 COLORFGBG / COLORTERM
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
            // 同样要走登录 shell，否则 PATH 与持久会话不一致
            let assigns: Vec<String> = term_env.iter().map(|(k, v)| format!("{k}={}", quote_posix(v))).collect();
            let prefix = if assigns.is_empty() { String::new() } else { format!("env {} ", assigns.join(" ")) };
            let path_prepend = opts.askpass_bin.as_deref().map(|b| format!("PATH={}:$PATH ", quote_posix(b))).unwrap_or_default();
            let cd = opts.cwd.as_deref().filter(|c| !c.is_empty()).map(|c| format!("cd {} && ", quote_posix(c))).unwrap_or_default();
            let sh = quote_posix(opts.shell.as_deref().or(probed_shell.as_deref()).unwrap_or("/bin/sh"));
            format!("{cd}exec {path_prepend}{prefix}{sh} -l")
        };

        let channel = self.exec_pty(&cmd, opts.cols, opts.rows).await?;
        Ok(AttachResult { backend: spawn_channel_backend(channel, cb), durable: false })
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

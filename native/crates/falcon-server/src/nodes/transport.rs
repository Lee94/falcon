//! SSH 只负责发现、部署和转发宿主机节点；HTTP / WebSocket 的业务请求由节点自己处理。
//! 先验证已有描述与身份，再选择二进制；兼容的旧节点不下载、不替换、不重启。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, ensure};
use falcon_proto::HostKind;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::bootstrap::{self, Descriptor, Identity};
use crate::exec::Exec as _;
use crate::git::path::join_path;
use crate::host::{encode_powershell, quote_posix, quote_powershell};
use crate::sessiond::command::{self, SessiondTarget};
use crate::sessions::ssh::SshLink;

#[derive(Clone)]
pub struct NodeEndpoint {
    pub node_id: String,
    pub port: u16,
    pub version: String,
    /// 只供节点间迁移使用，不能放进公共节点状态响应或日志。
    pub token: String,
}

impl std::fmt::Debug for NodeEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeEndpoint")
            .field("node_id", &self.node_id)
            .field("port", &self.port)
            .field("version", &self.version)
            .field("token", &"[redacted]")
            .finish()
    }
}

/// 在引擎 LocalSet 中调用。拿到端点后仍应经同一链路的 forward_out 连接节点回环地址。
pub async fn ensure_remote(link: Rc<SshLink>) -> anyhow::Result<NodeEndpoint> {
    let probe = link
        .probe(None, 1)
        .await
        .context("无法探测宿主机节点的系统信息")?;
    let kind = probe.kind;
    let root_path = canonical_remote_root(&link, kind, &probe.root).await?;
    let root = root_path.as_str();
    let descriptor_path = join_path(kind, &[root, bootstrap::DESCRIPTOR]);
    let existing = timed_exec(&link, &read_command(kind, &descriptor_path)).await?;
    if existing.ok() {
        let descriptor = parse_descriptor(&existing.stdout)
            .context("宿主机节点描述无法识别；保留现有服务，请检查 node.json")?;
        ensure!(
            descriptor.protocol_version == 1,
            "宿主机节点协议不兼容：需要1，现有描述为{}；请手动升级节点，不会自动替换",
            descriptor.protocol_version
        );
        if remote_pid_alive(&link, kind, descriptor.pid).await? {
            return verify_endpoint(&link, descriptor, root).await;
        }
    } else {
        ensure!(
            existing.code == Some(3),
            "读取宿主机节点描述失败：{}",
            output_detail(&existing)
        );
    }
    // 没有描述的旧服务仍可能正在使用该根目录。先探测进程，再部署到内容命名的新文件，
    // 绝不覆盖 root/bin/falcon；新二进制的 node ensure 还会再次核查进程与跨进程锁。
    let legacy = timed_exec(&link, &legacy_probe_command(kind, root)).await?;
    ensure!(
        legacy.ok(),
        "无法检查宿主机现有服务：{}",
        output_detail(&legacy)
    );
    if legacy
        .stdout
        .lines()
        .any(|l| l.trim() == "FALCON_NODE_STARTING")
    {
        return wait_for_remote_node(&link, kind, root, &descriptor_path).await;
    }
    ensure!(
        !legacy
            .stdout
            .lines()
            .any(|l| l.trim() == "FALCON_NODE_LEGACY"),
        "宿主机该数据目录已有旧版 Falcon 服务，但没有可验证的节点描述；请手动升级原服务，不会部署或启动第二个节点"
    );

    let target = probe
        .target
        .context("宿主机系统或架构没有 Falcon 节点构建")?;
    let binary = binary_for(target).await?;
    ensure!(
        binary
            .windows(bootstrap::CAPABILITY_MARKER.len())
            .any(|w| w == bootstrap::CAPABILITY_MARKER),
        "所选 Falcon 二进制尚不支持节点协议1；请发布包含节点模块的稳定版或设置 FALCON_NODE_BIN_DIR，不会运行旧版的未知子命令"
    );
    let hash = hex::encode(Sha256::digest(&binary));
    let name = format!(
        "falcon-node-{}{}",
        hash,
        if target.is_windows() { ".exe" } else { "" }
    );
    let remote_bin = join_path(kind, &[root, "bin", &name]);
    let exists = timed_exec(&link, &command::exists_command(kind, &remote_bin)).await?;
    if !exists.ok() || !exists.stdout.lines().any(|l| l.trim() == "yes") {
        let (cmd, input) = match kind {
            HostKind::Windows => {
                let mut encoder = crate::transfer::Base64LineEncoder::new();
                let mut lines = encoder.feed(&binary);
                lines.extend(encoder.finish());
                (
                    command::windows_install_command(&remote_bin, binary.len() as u64),
                    lines,
                )
            }
            _ => (command::posix_install_command(&remote_bin), binary),
        };
        let result =
            tokio::time::timeout(Duration::from_secs(180), link.exec_with_input(&cmd, &input))
                .await
                .context("推送宿主机节点二进制超时")??;
        if !result.ok() {
            // A、C 可以同时部署同一内容；Windows 已执行的目标文件无法再次 rename 覆盖。
            // 另一方已完成落位时继续 node ensure，让宿主机文件锁合并启动。
            let installed = timed_exec(&link, &command::exists_command(kind, &remote_bin)).await?;
            ensure!(
                installed.ok() && installed.stdout.lines().any(|l| l.trim() == "yes"),
                "推送宿主机节点二进制失败：{}",
                output_detail(&result)
            );
        }
    }
    let args = ensure_argv(&remote_bin, root);
    let result = tokio::time::timeout(
        Duration::from_secs(80),
        link.exec(&command::exec_command(kind, &args), None),
    )
    .await
    .context("等待宿主机节点启动超时")??;
    ensure!(result.ok(), "宿主机节点未就绪：{}", output_detail(&result));
    let descriptor = parse_descriptor(&result.stdout)
        .context("node ensure 没有返回有效节点描述（远端版本可能不支持节点模式）")?;
    ensure!(
        remote_pid_alive(&link, kind, descriptor.pid).await?,
        "节点返回的进程已经退出"
    );
    verify_endpoint(&link, descriptor, root).await
}

async fn wait_for_remote_node(
    link: &Rc<SshLink>,
    kind: HostKind,
    root: &str,
    descriptor_path: &str,
) -> anyhow::Result<NodeEndpoint> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut last_error = None;
    loop {
        let out = timed_exec(link, &read_command(kind, descriptor_path)).await?;
        if out.ok() {
            let descriptor = parse_descriptor(&out.stdout)?;
            if remote_pid_alive(link, kind, descriptor.pid).await? {
                match verify_endpoint(link, descriptor, root).await {
                    Ok(endpoint) => return Ok(endpoint),
                    Err(e) => last_error = Some(format!("{e:#}")),
                }
            }
        } else {
            ensure!(
                out.code == Some(3),
                "读取正在启动的节点失败：{}",
                output_detail(&out)
            );
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "宿主机已有正在启动的节点，但30秒内没有提供有效身份：{}；保留该进程，不会另起服务",
            last_error.as_deref().unwrap_or("尚未发布描述文件")
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn canonical_remote_root(
    link: &Rc<SshLink>,
    kind: HostKind,
    root: &str,
) -> anyhow::Result<String> {
    let cmd = match kind {
        HostKind::Windows => encode_powershell(&format!(
            "$ErrorActionPreference = 'Stop'; $r = {}; if (Test-Path -LiteralPath $r -PathType Container) {{ $r = (Resolve-Path -LiteralPath $r).ProviderPath }}; 'FALCON_NODE_ROOT:' + $r",
            quote_powershell(root)
        )),
        _ => format!(
            "r={}; if [ -d \"$r\" ]; then r=$(cd \"$r\" && pwd -P) || exit 1; else d=${{r%/*}}; n=${{r##*/}}; if [ -d \"$d\" ]; then d=$(cd \"$d\" && pwd -P) || exit 1; r=\"$d/$n\"; fi; fi; printf 'FALCON_NODE_ROOT:%s\\n' \"$r\"",
            quote_posix(root)
        ),
    };
    let out = timed_exec(link, &cmd).await?;
    ensure!(out.ok(), "无法解析宿主机节点目录：{}", output_detail(&out));
    out.stdout
        .lines()
        .find_map(|line| line.strip_prefix("FALCON_NODE_ROOT:"))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .context("宿主机没有返回节点根目录")
}

async fn timed_exec(link: &Rc<SshLink>, command: &str) -> anyhow::Result<crate::exec::ExecResult> {
    tokio::time::timeout(Duration::from_secs(15), link.exec(command, None))
        .await
        .context("宿主机节点探测超时")?
}

fn output_detail(result: &crate::exec::ExecResult) -> String {
    let text = if result.stderr.trim().is_empty() {
        result.stdout.trim()
    } else {
        result.stderr.trim()
    };
    if text.is_empty() {
        format!("远端退出码 {:?}", result.code)
    } else if text.contains("\"token\"") {
        // 错误的远端包装器也可能把 JSON 和错误混在同一输出中，不能将迁移令牌带进日志。
        format!(
            "远端退出码 {:?}（包含私有节点描述，已省略输出）",
            result.code
        )
    } else {
        text.chars().take(2048).collect()
    }
}

fn parse_descriptor(stdout: &str) -> anyhow::Result<Descriptor> {
    // 登录 shell 可以输出提示；只接受单行 JSON，不能拿别的端口/版本文本凑节点身份。
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .next()
        .context("输出里没有节点描述 JSON")
}

async fn verify_endpoint(
    link: &Rc<SshLink>,
    descriptor: Descriptor,
    root: &str,
) -> anyhow::Result<NodeEndpoint> {
    let info = tokio::time::timeout(
        Duration::from_secs(5),
        remote_identity(link, descriptor.port),
    )
    .await
    .context("验证节点 HTTP 身份超时；保留现有节点")?
    .context("无法验证现有节点 /api/node；旧版服务请手动升级，不会启动另一份服务")?;
    bootstrap::validate_identity(&descriptor, &info, root)?;
    Ok(NodeEndpoint {
        node_id: descriptor.node_id,
        port: descriptor.port,
        version: descriptor.version,
        token: descriptor.token,
    })
}

async fn remote_identity(link: &Rc<SshLink>, port: u16) -> anyhow::Result<Identity> {
    ensure!(port != 0, "节点描述的端口无效");
    let channel = link.forward_out("127.0.0.1", port).await?;
    let mut stream = channel.into_stream();
    stream
        .write_all(bootstrap::node_request(port).as_bytes())
        .await?;
    let mut response = Vec::new();
    stream.take(65_537).read_to_end(&mut response).await?;
    bootstrap::parse_identity_response(&response)
}

fn read_command(kind: HostKind, file: &str) -> String {
    match kind {
        HostKind::Windows => encode_powershell(&format!(
            "$ErrorActionPreference = 'Stop'; $p = {}; if (-not (Test-Path -LiteralPath $p)) {{ exit 3 }}; [IO.File]::ReadAllText($p)",
            quote_powershell(file)
        )),
        _ => format!(
            "p={}; if [ ! -e \"$p\" ]; then exit 3; fi; cat \"$p\"",
            quote_posix(file)
        ),
    }
}

async fn remote_pid_alive(link: &Rc<SshLink>, kind: HostKind, pid: u32) -> anyhow::Result<bool> {
    if pid == 0 {
        return Ok(false);
    }
    let cmd = match kind {
        HostKind::Windows => encode_powershell(&format!(
            "if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ 'alive' }}"
        )),
        _ => format!(
            "if kill -0 {pid} 2>/dev/null; then s=$(ps -p {pid} -o stat=); case \"$s\" in *Z*) :;; ?*) printf 'alive\\n';; esac; fi"
        ),
    };
    let output = timed_exec(link, &cmd).await?;
    ensure!(output.ok(), "无法检查节点进程：{}", output_detail(&output));
    Ok(output.stdout.lines().any(|line| line.trim() == "alive"))
}

fn ensure_argv(bin: &str, root: &str) -> Vec<String> {
    vec![
        bin.into(),
        "node".into(),
        "ensure".into(),
        "--data-dir".into(),
        root.into(),
    ]
}

/// 只做读取，既不调用旧二进制的未知参数，也不使用 service install/start。未知参数会让
/// 旧服务按默认配置启动，因此探测不能靠 `falcon --version` 或 `falcon node ...`。
fn legacy_probe_command(kind: HostKind, root: &str) -> String {
    if kind == HostKind::Windows {
        return encode_powershell(&format!(
            r#"$ErrorActionPreference = 'Stop'; $root = {}; $found = Get-CimInstance Win32_Process | Where-Object {{ $_.Name -match '^falcon(?:-server|-v[^ ]+|-node-[0-9a-f]+)?\.exe$' -and $_.CommandLine -and $_.CommandLine -notmatch '\s(node|service|respawn)\s' }}; foreach ($p in $found) {{ $dirs = [regex]::Matches($p.CommandLine, '(?:^|\s)--data-dir\s+(?:"([^"]+)"|(\S+))'); $owns = $false; if ($dirs.Count) {{ $m = $dirs[$dirs.Count - 1]; $d = $m.Groups[1].Value; if (-not $d) {{ $d = $m.Groups[2].Value }}; $owns = $d.TrimEnd('\', '/') -ieq $root.TrimEnd('\', '/') }} elseif ($p.ExecutablePath) {{ $owns = $p.ExecutablePath.StartsWith((Join-Path $root 'bin') + '\', [StringComparison]::OrdinalIgnoreCase) }}; if ($owns) {{ if ($p.Name -like 'falcon-node-*') {{ 'FALCON_NODE_STARTING' }} else {{ 'FALCON_NODE_LEGACY' }} }} }}"#,
            quote_powershell(root)
        ));
    }
    // 预探测只认明确的 --data-dir 或标准安装路径。没有目录参数的另一个服务可能使用
    // 不同 HOME；更完整的进程环境判断由新二进制的本机 ensure 在拿锁后执行。
    let script = format!(
        "r={}; ps -ax -o command= | while IFS= read -r c; do \
         case \"$c\" in *' node ensure '*|*' service '*|*' respawn '*|*'ps -ax -o command='*|*'falcon-sessiond'*|*'falcon-desktop'*) continue;; esac; \
         x=${{c%% *}}; e=${{x##*/}}; marker=FALCON_NODE_LEGACY; case \"$e\" in falcon-node-*) marker=FALCON_NODE_STARTING;; esac; \
         case \"$e\" in falcon|falcon-server|falcon-v*|falcon-node-*) \
         case \"$c\" in *'--data-dir '*) d=${{c##*--data-dir }}; case \"$d\" in \"$r\"|\"$r \"*) printf '%s\\n' \"$marker\";; esac;; \
         *) case \"$x\" in \"$r\"/bin/*) printf '%s\\n' \"$marker\";; esac;; esac;; esac; done",
        quote_posix(root)
    );
    format!("sh -c {}", quote_posix(&script))
}

fn platform(target: SessiondTarget) -> &'static str {
    match target {
        SessiondTarget::X86_64Linux => "linux-x64",
        SessiondTarget::Aarch64Linux => "linux-arm64",
        SessiondTarget::X86_64Mac => "darwin-x64",
        SessiondTarget::Aarch64Mac => "darwin-arm64",
        SessiondTarget::X86_64Windows => "win32-x64",
    }
}

async fn binary_for(target: SessiondTarget) -> anyhow::Result<Vec<u8>> {
    let platform = platform(target);
    if let Some(dir) = std::env::var_os("FALCON_NODE_BIN_DIR") {
        let name = crate::server_update::asset_name(env!("CARGO_PKG_VERSION"), platform);
        let path = PathBuf::from(dir).join(name);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("FALCON_NODE_BIN_DIR 缺少 {}", path.display()))?;
        ensure!(!bytes.is_empty(), "开发节点二进制为空：{}", path.display());
        return Ok(bytes);
    }
    // 本机正在跑的 server 就是可信开发产物；只认明确的服务端文件名，test harness 与
    // falcon-desktop 即使链接了本 crate，也不能被推到宿主机执行。
    if command::local_target() == Some(target) {
        let exe = std::env::current_exe()?;
        if current_server(&exe) {
            return std::fs::read(&exe).context("读取当前 Falcon 服务端失败");
        }
    }
    let release = crate::server_update::fetch_release(None)
        .await
        .context("读取 Falcon 官方稳定版失败")?;
    let download = DownloadDir::new()?;
    let file = download
        .0
        .join(crate::server_update::asset_name(&release.version, platform));
    crate::server_update::download_verified_release(&release, platform, &file)
        .await
        .context("下载并校验宿主机节点失败")?;
    let bytes = std::fs::read(&file)?;
    ensure!(!bytes.is_empty(), "官方节点二进制为空");
    Ok(bytes)
}

fn current_server(path: &Path) -> bool {
    !path.components().any(|part| part.as_os_str() == "deps")
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(bootstrap::is_server_name)
}

struct DownloadDir(PathBuf);
impl DownloadDir {
    fn new() -> anyhow::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "falcon-node-download-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for DownloadDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_and_commands_cover_all_targets_and_quote_paths() {
        for target in SessiondTarget::ALL {
            let name = crate::server_update::asset_name("0.3.24", platform(target));
            assert!(name.starts_with("falcon-v0.3.24-"));
            assert_eq!(name.ends_with(".exe"), target.is_windows());
        }
        let argv = ensure_argv("/tmp/a'b/falcon", "/tmp/a b/.falcon");
        assert_eq!(&argv[1..4], ["node", "ensure", "--data-dir"]);
        assert!(command::exec_command(HostKind::Posix, &argv).contains("'\\''"));
        let cmd = command::exec_command(HostKind::Windows, &argv);
        assert!(cmd.starts_with("powershell -NoProfile -NonInteractive -EncodedCommand "));
    }

    #[test]
    fn current_executable_selection_excludes_tests_and_clients() {
        assert!(!current_server(Path::new(
            "/tmp/target/debug/deps/falcon_server-aabbccdd"
        )));
        assert!(!current_server(Path::new("/tmp/falcon-desktop")));
        assert!(current_server(Path::new("/tmp/target/debug/falcon-server")));
        assert!(current_server(Path::new("/tmp/falcon-v0.3.24-linux-x64")));
    }

    #[test]
    fn descriptor_parser_skips_shell_noise() {
        let json = serde_json::json!({ "nodeId": "n", "pid": 1, "port": 4950, "version": "0.1.0", "protocolVersion": 1,
            "platform": "linux", "arch": "x86_64", "dataDir": "/tmp/n", "token": "b".repeat(64) });
        assert_eq!(
            parse_descriptor(&format!("welcome\n{json}\n"))
                .unwrap()
                .node_id,
            "n"
        );
        assert!(parse_descriptor("welcome\n").is_err());
    }

    /// 只对 SetEnv HOME 指向短临时目录的隔离 sshd 执行；开发二进制目录必须显式提供，
    /// 因而测试不会访问 GitHub，也不会探测或修改用户真实家目录。
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "需要 FALCON_NODE_SSH_TEST=隔离环境manifest.json 和 FALCON_NODE_BIN_DIR"]
    async fn real_ssh_concurrent_ensure_reuses_node_after_disconnect() -> anyhow::Result<()> {
        use crate::crypto::SecretBox;
        use crate::db::{Db, ProjectRow};
        use std::sync::Arc;
        let manifest = std::env::var("FALCON_NODE_SSH_TEST").context("缺少隔离 SSH manifest")?;
        ensure!(
            std::env::var_os("FALCON_NODE_BIN_DIR").is_some(),
            "测试必须提供本地节点二进制，禁止联网下载"
        );
        let spec: serde_json::Value = serde_json::from_slice(&std::fs::read(manifest)?)?;
        let expected_home = spec["home"]
            .as_str()
            .context("manifest 缺少home")?
            .to_string();
        ensure!(
            expected_home.starts_with("/tmp/fn-ssh-")
                || expected_home.starts_with("/private/tmp/fn-ssh-"),
            "只允许专用临时家目录"
        );
        let project = ProjectRow {
            id: "isolated-node".into(),
            name: "isolated-node".into(),
            project_type: "ssh".into(),
            ssh_host: Some("127.0.0.1".into()),
            ssh_port: Some(spec["port"].as_i64().context("缺少port")?),
            ssh_username: Some(spec["user"].as_str().context("缺少user")?.into()),
            ssh_auth_method: Some("key".into()),
            ssh_key_path: Some(spec["key"].as_str().context("缺少key")?.into()),
            ..Default::default()
        };
        let a_dir = tempfile::tempdir()?;
        let c_dir = tempfile::tempdir()?;
        let a = SshLink::new(
            project.clone(),
            Arc::new(Db::open(a_dir.path())?),
            Arc::new(SecretBox::open(a_dir.path())?),
        );
        let c = SshLink::new(
            project,
            Arc::new(Db::open(c_dir.path())?),
            Arc::new(SecretBox::open(c_dir.path())?),
        );
        tokio::task::LocalSet::new().run_until(async move {
            // 必须先核实 HOME，再运行会迁移旧目录的 SshLink::probe。
            for link in [&a, &c] {
                let home = link.exec("printf '%s\\n' \"$HOME\"; (cd \"$HOME\" && pwd -P)", None).await?;
                ensure!(home.ok(), "无法读取隔离 HOME");
                let lines: Vec<_> = home.stdout.lines().collect();
                ensure!(lines.len() == 2 && lines[0] == expected_home, "sshd 未隔离 HOME，拒绝探测");
                ensure!(lines[1].starts_with("/private/tmp/fn-ssh-") || lines[1].starts_with("/tmp/fn-ssh-"), "HOME 实际指向了临时目录之外");
            }
            let (first, second) = tokio::join!(ensure_remote(a.clone()), ensure_remote(c.clone()));
            let (first, second) = (first?, second?);
            ensure!(first.node_id == second.node_id && first.port == second.port && first.token == second.token,
                "两个独立入口启动了不同节点");
            let facts = a.probe(None, 1).await?;
            let root = canonical_remote_root(&a, facts.kind, &facts.root).await?;
            let descriptor_path = join_path(facts.kind, &[&root, bootstrap::DESCRIPTOR]);
            let before = timed_exec(&a, &read_command(facts.kind, &descriptor_path)).await?;
            let before = parse_descriptor(&before.stdout)?;
            a.dispose(); c.dispose();
            tokio::time::sleep(Duration::from_millis(150)).await;
            let reattached = ensure_remote(a.clone()).await?;
            let after = timed_exec(&a, &read_command(facts.kind, &descriptor_path)).await?;
            let after = parse_descriptor(&after.stdout)?;
            ensure!(reattached.node_id == first.node_id && after.pid == before.pid && after.port == before.port,
                "SSH 断开后节点被重启或替换");
            // 只终止已核实属于本测试根目录的节点；测试目录由调用方移除。
            ensure!(bootstrap::same_root(&after.data_dir, &root), "拒绝清理不属于测试的节点");
            let stopped = a.exec(&format!("kill -TERM {}", after.pid), None).await?;
            ensure!(stopped.ok(), "无法停止隔离节点");
            a.dispose();
            println!("isolated SSH node reused by two independent entries and survived disconnect; pid={}, port={}", after.pid, after.port);
            anyhow::Ok(())
        }).await
    }

    #[test]
    fn legacy_probe_is_read_only_and_keeps_root_quoted() {
        let root = "/tmp/a'b $HOME/node";
        let cmd = legacy_probe_command(HostKind::Posix, root);
        assert!(cmd.starts_with("sh -c '"));
        assert!(cmd.contains("ps -ax"));
        assert!(!cmd.contains("--version"));
        assert!(!cmd.contains("service start"));
    }
}

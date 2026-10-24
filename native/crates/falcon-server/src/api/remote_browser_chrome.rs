//! 远端浏览器 · Chrome 形态：`type: "chrome"` 的目标不走 Neko，走 headless Chrome + CDP。
//!
//! 链路：宿主机上一个 `--headless --remote-debugging-port` 的 Chrome（系统已有就用系统的，
//! 没有就把 Chrome for Testing 的 zip 经 SSH 推上去），Falcon 照旧经 `forward_out` 把
//! `/remote-browser/<id>/cdp/*` 反代到它的调试端口；画面用 `Page.startScreencast` 的
//! JPEG 帧、输入用 `Input.dispatch*`，媒体与控制全走已有 SSH/HTTP 通道，不开新端口。
//!
//! 部署/运行状态全在宿主机 `$DIR` 下的文件上（deploy.pid / deploy.log / chrome.log），
//! 服务端不存任务表：每次连接先跑 `ensure_script`（顺带报部署进度），缺 Chrome 才 kick
//! `deploy_script`（detached，下载解压耗时以分钟计，页面轮询刷新）。

/// 锁定的 Chrome for Testing 版本；URL 形如
/// `https://storage.googleapis.com/chrome-for-testing-public/<ver>/<plat>/chrome-<plat>.zip`
pub const CHROME_VERSION: &str = "156.0.8078.47";
/// linux64 zip 的 sha256（发版时锁定；arm64 包单独核，见部署脚本注释）
pub const CHROME_SHA256_LINUX64: &str = "e6c94490f0b6b337225aeb2d2a5034858516b7017356980ba94f5176d8774a50";

pub const CHROME_DIR: &str = "$HOME/.falcon/chrome";

/// 探测 + 启动。退出码与输出是协议：
/// `UP` 已在监听 / `STARTED` 本次拉起 / `DEPLOYING:<行>` 部署在跑 / `NO_CHROME` 没装 /
/// `DEPLOY_FAILED:<行>` 上次部署死了 / `NEED_LIBS`（随后每行一个缺失的 .so）/ `LAUNCH_TIMEOUT:<日志>`
pub fn ensure_script(port: u16) -> String {
    format!(
        r#"set -u
port={port}
dir="{dir}"
probe() {{
  ( curl -fsS --max-time 3 "http://127.0.0.1:$port/json/version" 2>/dev/null \
    || wget -q -T 3 -O- "http://127.0.0.1:$port/json/version" 2>/dev/null ) || true
}}
probe | grep -q '"Browser"' && {{ echo UP; exit 0; }}
if [ -f "$dir/deploy.pid" ] && kill -0 "$(cat "$dir/deploy.pid")" 2>/dev/null; then
  echo "DEPLOYING:$(tail -n 1 "$dir/deploy.log" 2>/dev/null)"; exit 0
fi
bin=""
for c in google-chrome google-chrome-stable chrome chromium chromium-browser microsoft-edge; do
  command -v "$c" >/dev/null 2>&1 && {{ bin="$c"; break; }}
done
if [ -z "$bin" ]; then
  for c in "$dir"/chrome-*/chrome; do
    [ -x "$c" ] && {{ bin="$c"; break; }}
  done
fi
if [ -z "$bin" ]; then
  if [ -f "$dir/deploy.log" ]; then echo "DEPLOY_FAILED:$(tail -n 1 "$dir/deploy.log")"; else echo NO_CHROME; fi
  exit 3
fi
"$bin" --version >/dev/null 2>&1 || {{
  echo NEED_LIBS
  ldd "$bin" 2>/dev/null | awk '/not found/{{print $1}}'
  exit 6
}}
[ -f "$dir/pid" ] && kill "$(cat "$dir/pid")" 2>/dev/null
mkdir -p "$dir"
setsid nohup "$bin" --headless --remote-debugging-port="$port" \
  --remote-debugging-address=127.0.0.1 --remote-allow-origins='*' \
  --no-first-run --no-default-browser-check --disable-gpu \
  --user-data-dir="$dir/profile" </dev/null >>"$dir/chrome.log" 2>&1 &
echo $! > "$dir/pid"
i=0
while [ "$i" -lt 40 ]; do
  probe | grep -q '"Browser"' && {{ echo STARTED; exit 0; }}
  sleep 0.5; i=$((i+1))
done
echo "LAUNCH_TIMEOUT:$(tail -n 3 "$dir/chrome.log" 2>/dev/null | tr '\n' ' ')"
exit 7
"#,
        port = port,
        dir = CHROME_DIR
    )
}

/// 下载 + 解压 Chrome for Testing。detached 跑，进度逐行写进 deploy.log（ensure 摘最后一行）。
/// 末尾直接 exec ensure：起来了这一步就报 STARTED，deploy.pid 由外层包装写。
pub fn deploy_script(port: u16) -> String {
    format!(
        r#"set -u
ver="{ver}"
dir="{dir}"
mkdir -p "$dir"
echo "$$" > "$dir/deploy.pid"
: > "$dir/deploy.log"
arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) plat=linux64 ; sha="{sha64}" ;;
  aarch64|arm64) plat=linux-arm64 ; sha="" ;;
  *) echo "不支持的架构：$arch"; exit 9 ;;
esac
if [ ! -x "$dir/chrome-$plat/chrome" ]; then
  zip="$dir/chrome-$plat.zip"
  url="https://storage.googleapis.com/chrome-for-testing-public/$ver/$plat/chrome-$plat.zip"
  echo "下载 Chrome for Testing $ver（~150MB）"
  if command -v curl >/dev/null 2>&1; then
    curl -fSL --retry 3 -o "$zip" "$url" || {{ echo "下载失败：curl $?"; rm -f "$dir/deploy.pid"; exit 4; }}
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$zip" "$url" || {{ echo "下载失败：wget $?"; rm -f "$dir/deploy.pid"; exit 4; }}
  else
    echo "下载失败：没有 curl / wget"; rm -f "$dir/deploy.pid"; exit 4
  fi
  if [ -n "$sha" ] && command -v sha256sum >/dev/null 2>&1; then
    echo "$sha  $zip" | sha256sum -c - >/dev/null 2>&1 || {{ echo "校验失败"; rm -f "$zip" "$dir/deploy.pid"; exit 4; }}
  fi
  echo "解压"
  if command -v unzip >/dev/null 2>&1; then
    unzip -o -q "$zip" -d "$dir" || {{ echo "解压失败"; rm -f "$dir/deploy.pid"; exit 5; }}
  elif command -v python3 >/dev/null 2>&1; then
    python3 -m zipfile -e "$zip" "$dir" || {{ echo "解压失败"; rm -f "$dir/deploy.pid"; exit 5; }}
  else
    echo "解压失败：没有 unzip / python3"; rm -f "$dir/deploy.pid"; exit 5
  fi
  rm -f "$zip"
  chmod +x "$dir/chrome-$plat/chrome"
fi
echo "启动"
exec sh -c '{ensure}'
"#,
        ver = CHROME_VERSION,
        sha64 = CHROME_SHA256_LINUX64,
        dir = CHROME_DIR,
        ensure = ensure_script(port).replace('\'', r#"'\''"#),
    )
}

/// kick 部署：把 deploy.sh 落盘（base64，免转义），detached 起来。deploy.lock 挡重复 kick；
/// ensure 里 pid 死掉即视为结束，锁不会永久卡住（下次 NO_CHROME/DEPLOY_FAILED 时会重判）。
pub fn kick_script(port: u16) -> String {
    let body = base64(&deploy_script(port));
    format!(
        r#"dir="{dir}"
mkdir -p "$dir"
printf %s "{body}" | base64 -d > "$dir/deploy.sh"
if [ -d "$dir/deploy.lock" ]; then
  echo DEPLOYING
elif mkdir "$dir/deploy.lock" 2>/dev/null; then
  ( sh "$dir/deploy.sh" >>"$dir/deploy.log" 2>&1; rmdir "$dir/deploy.lock" ) </dev/null &
  echo KICKED
else
  echo DEPLOYING
fi
"#,
        dir = CHROME_DIR,
        body = body
    )
}

/// 读部署进度（轮询用）：UP / DEPLOYING:<行> / NO_CHROME / DEPLOY_FAILED:<行>。
/// 就是 ensure 的前半段——直接跑完整 ensure，碰巧能起来还省一跳。
fn base64(text: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(text)
}

/// ensure 的输出归类
#[derive(Debug, PartialEq)]
pub enum Ensure {
    Up,
    Started,
    Deploying(String),
    NoChrome,
    DeployFailed(String),
    NeedLibs(String),
    LaunchTimeout(String),
    Unknown(i32, String),
}

pub fn parse_ensure(code: Option<i32>, stdout: &str, stderr: &str) -> Ensure {
    let mut lines = stdout.lines();
    match lines.next().unwrap_or("").trim() {
        "UP" => Ensure::Up,
        "STARTED" => Ensure::Started,
        "NO_CHROME" => Ensure::NoChrome,
        "NEED_LIBS" => Ensure::NeedLibs(lines.map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(", ")),
        s if s.starts_with("DEPLOYING:") => Ensure::Deploying(s[10..].into()),
        s if s.starts_with("DEPLOY_FAILED:") => Ensure::DeployFailed(s[14..].into()),
        s if s.starts_with("LAUNCH_TIMEOUT:") => Ensure::LaunchTimeout(s[15..].into()),
        other => Ensure::Unknown(code.unwrap_or(-1), format!("{other} {}", stderr.trim()).trim().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_script_ports_and_dir_baked() {
        let s = ensure_script(9333);
        assert!(s.contains("port=9333"));
        assert!(s.contains("$HOME/.falcon/chrome"));
        for marker in ["UP", "STARTED", "NO_CHROME", "NEED_LIBS", "DEPLOYING", "LAUNCH_TIMEOUT"] {
            assert!(s.contains(marker), "{marker}");
        }
        // 新 Chrome 必须放行远端来源，否则 CDP WS 403
        assert!(s.contains("--remote-allow-origins"));
    }

    #[test]
    fn deploy_script_embeds_ensure_and_pins_version() {
        let s = deploy_script(9222);
        assert!(s.contains("156.0.8078.47"));
        assert!(s.contains("chrome-$plat.zip"));
        assert!(s.contains("exec sh -c '"));
        assert!(s.contains("port=9222"));
        assert!(s.contains(CHROME_SHA256_LINUX64));
    }

    #[test]
    fn kick_script_base64_roundtrip_and_lock() {
        let s = kick_script(9222);
        assert!(s.contains("base64 -d"));
        assert!(s.contains("deploy.lock"));
    }

    /// 生成的脚本必须过 `sh -n`：ensure 里带单引号（--remote-allow-origins='*'、tr '\n'），
    /// deploy 把它嵌进 exec sh -c '...'，kick 再包一层 base64——任何一层漏转义这里就会炸
    #[cfg(unix)]
    fn sh_syntax_ok(script: String) {
        let path = std::env::temp_dir().join(format!("falcon-rb-{}.sh", std::process::id()));
        std::fs::write(&path, script).unwrap();
        let out = std::process::Command::new("sh").arg("-n").arg(&path).output().unwrap();
        std::fs::remove_file(&path).ok();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    #[cfg(unix)]
    fn generated_scripts_are_valid_sh() {
        sh_syntax_ok(ensure_script(9222));
        sh_syntax_ok(deploy_script(9222));
        sh_syntax_ok(kick_script(9222));
    }

    #[test]
    fn parse_ensure_markers() {
        assert_eq!(parse_ensure(Some(0), "UP\n", ""), Ensure::Up);
        assert_eq!(parse_ensure(Some(0), "STARTED\n", ""), Ensure::Started);
        assert_eq!(parse_ensure(Some(3), "NO_CHROME\n", ""), Ensure::NoChrome);
        assert_eq!(parse_ensure(Some(0), "DEPLOYING:下载 Chrome\n", ""), Ensure::Deploying("下载 Chrome".into()));
        assert_eq!(parse_ensure(Some(3), "DEPLOY_FAILED:解压失败\n", ""), Ensure::DeployFailed("解压失败".into()));
        assert_eq!(parse_ensure(Some(6), "NEED_LIBS\nlibnss3.so\nlibgbm.so.1\n", ""), Ensure::NeedLibs("libnss3.so, libgbm.so.1".into()));
        assert!(matches!(parse_ensure(Some(1), "", "boom"), Ensure::Unknown(1, _)));
    }
}

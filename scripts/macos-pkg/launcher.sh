#!/bin/bash
# Falcon.app 入口：把捆绑的服务端注册成用户级服务，然后打开 UI。
# 服务跑在 <dataDir>/bin/falcon（launchd），不是这个启动器进程——关掉
# App 不会 Recreate 会话。再点一次 App 也是升级：service install 会把
# Resources/falcon rename 进 dataDir（见 service.ts installServiceBinary）。
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/Resources/falcon"
ARGS_SH="$ROOT/Resources/service-args.sh"

alert() {
  osascript -e "display alert \"Falcon\" message \"$1\" as critical" >/dev/null 2>&1 || true
}

if [ ! -x "$BIN" ] || [ ! -f "$ARGS_SH" ]; then
  alert "安装不完整：找不到服务程序。"
  exit 1
fi

# 装过服务就带上它原来的 --host / --port / --data-dir（为什么见 service-args.sh），
# 打开的地址也按它的端口算，不写死 4923
. "$ARGS_SH"
read_service_args "$HOME/Library/LaunchAgents/com.falcon.server.plist"
URL=$(service_local_url)

if ! "$BIN" service install ${SERVICE_ARGS[@]+"${SERVICE_ARGS[@]}"}; then
  alert "无法注册后台服务。"
  exit 1
fi

# 等端口起来再 open（服务刚被 launchd 拉起时还没 listen），避免浏览器先看到连接拒绝。
# 401 也算起来了（设过访问密码）。-g：IPv6 字面量的方括号别被 curl 当成 glob
for _ in $(seq 1 40); do
  code=$(curl -g -s -o /dev/null -w '%{http_code}' --max-time 1 "$URL" 2>/dev/null || true)
  if [ -n "$code" ] && [ "$code" != "000" ]; then
    break
  fi
  sleep 0.5
done
open "$URL"

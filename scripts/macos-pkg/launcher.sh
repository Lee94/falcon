#!/bin/bash
# Falcon.app 入口：把捆绑的 SEA 注册成用户级服务，然后打开 UI。
# 服务跑在 <dataDir>/bin/falcon（launchd），不是这个启动器进程——关掉
# App 不会 Recreate 会话。再点一次 App 也是升级：service install 会把
# Resources/falcon rename 进 dataDir（见 service.ts installServiceBinary）。
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/Resources/falcon"
URL="http://127.0.0.1:4923"

alert() {
  osascript -e "display alert \"Falcon\" message \"$1\" as critical" >/dev/null 2>&1 || true
}

if [ ! -x "$BIN" ]; then
  alert "安装不完整：找不到服务程序。"
  exit 1
fi

if ! "$BIN" service install; then
  alert "无法注册后台服务。"
  exit 1
fi

# SEA 首次启动要解压 runtime，等端口起来再 open，避免浏览器先看到连接拒绝。
# 401 也算起来了（设过访问密码）。
for _ in $(seq 1 40); do
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 1 "$URL" 2>/dev/null || true)
  if [ -n "$code" ] && [ "$code" != "000" ]; then
    break
  fi
  sleep 0.5
done
open "$URL"

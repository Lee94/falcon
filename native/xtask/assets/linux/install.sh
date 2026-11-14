#!/bin/sh
# falcon 服务端安装脚本（自解包）。用法：
#   sudo ./falcon-v*-install.sh [--host <地址>] [--port <端口>] [--data-dir <目录>]
# 把 falcon 装到 /usr/local/bin（无 root 时 ~/.local/bin），再注册 systemd 服务；
# 已装过 = 升级，不带参数即沿用原启动参数。卸载：falcon service uninstall 后删掉该文件。
set -eu

DEST=/usr/local/bin
UNIT=/etc/systemd/system/falcon.service
if [ "$(id -u)" != "0" ]; then
  DEST="${HOME}/.local/bin"
  UNIT="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user/falcon.service"
fi

line=$(awk '/^__FALCON_BINARY_BELOW__$/ { print NR; exit }' "$0")
if [ -z "$line" ]; then
  echo "安装脚本损坏：找不到二进制段" >&2
  exit 1
fi
mkdir -p "$DEST"
tail -n "+$((line + 1))" "$0" > "$DEST/falcon.new"
chmod 755 "$DEST/falcon.new"
mv "$DEST/falcon.new" "$DEST/falcon"

if [ -d /run/systemd/system ]; then
  if [ -f "$UNIT" ]; then
    "$DEST/falcon" service install || true
  elif [ "$#" -gt 0 ]; then
    "$DEST/falcon" service install "$@" || true
  elif [ "$(id -u)" = "0" ]; then
    "$DEST/falcon" service install --data-dir /var/lib/falcon || true
  else
    "$DEST/falcon" service install || true
  fi
else
  echo "未检测到 systemd：已装到 $DEST/falcon，请自行接入 init。" >&2
fi
echo "已安装：$DEST/falcon"
exit 0

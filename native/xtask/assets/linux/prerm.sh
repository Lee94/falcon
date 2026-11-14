#!/bin/sh
# 卸包前停掉并摘掉服务（deb prerm；upgrade 不走这条路——新包的 postinst 会原地重装）。
# service uninstall 摘 unit 并停进程；不动 /var/lib/falcon 里的数据与 bin/。
if [ "$1" = "remove" ] || [ "$1" = "deconfigure" ]; then
  /usr/bin/falcon service uninstall || true
fi

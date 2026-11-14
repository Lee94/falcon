#!/bin/sh
# 装好 falcon 后注册 systemd 服务（deb postinst / rpm %post 共用）。
# 已装过 falcon.service = 升级：service install 不带参数，沿用原 --host / --port /
# --data-dir（服务端从旧 unit 的 ExecStart 读回）；首装用 --data-dir /var/lib/falcon。
# install 失败不拖垮包管理（比如没有 systemd 的容器：它只是把 unit 打出来）。
if [ -d /run/systemd/system ] && [ -x /usr/bin/falcon ]; then
  if [ -f /etc/systemd/system/falcon.service ]; then
    /usr/bin/falcon service install || true
  else
    /usr/bin/falcon service install --data-dir /var/lib/falcon || true
  fi
fi

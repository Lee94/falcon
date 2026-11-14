#!/bin/sh
# rpm %preun：$1 = 0 是卸载，>= 1 是升级（升级交给新包的 %post 原地重装，这里不动）。
if [ "$1" = "0" ]; then
  /usr/bin/falcon service uninstall || true
fi

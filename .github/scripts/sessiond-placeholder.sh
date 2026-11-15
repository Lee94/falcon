#!/usr/bin/env bash
# 在 native/target-sessiond/embed/ 放 4 个占位文件，让带 embed-sessiond 的 cargo build / check
# 不等守护进程就能开工（include_bytes! 只要文件存在）。
#
# 占位内容刻意不是 gzip：真文件换上后重编只动 falcon-server 的 main.rs；万一没换上或没重编，
# `xtask server` 出包前的内嵌核对会报错，占位内容不会被发出去。
set -euo pipefail

dir="native/target-sessiond/embed"
rm -rf "$dir"
mkdir -p "$dir"
for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin x86_64-pc-windows-gnu; do
  printf 'FALCON-SESSIOND-PLACEHOLDER %s\n' "$target" > "$dir/$target.gz"
done

#!/usr/bin/env bash
# Falcon 浏览器版构建（docs/design/rust-unification.md C 线）：
#   cargo build（wasm32，profile web）→ wasm-bindgen → [wasm-opt] → 拼产物目录 → 报体积
#
# 产物在 native/target-wasm/dist/。开发构建的服务端缺省就托管这个目录；
# `cargo xtask web` 就是跑本脚本；`cargo xtask server` 先跑它，再把产物编进服务端二进制。
#
# 工具链：
# - rustup 的 stable + wasm32-unknown-unknown target。这台机器 PATH 上排前面的是 Homebrew 的
#   rustc（没有 wasm 的标准库），所以这里显式用 rustup 工具链的 bin；绕过 rustup 代理时
#   rust-lld 找不到 libLLVM，DYLD_FALLBACK_LIBRARY_PATH 要自己补。
# - wasm-bindgen-cli，版本必须与 Cargo.lock 里的 wasm-bindgen 一致（不一致它会直接报错）。
# - wasm-opt（binaryen）可选，默认不用（见下）。
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here"

toolchain="$(rustup toolchain list | awk '/^stable/ {print $1; exit}')"
tc_dir="$(rustup show home)/toolchains/$toolchain"
export PATH="$tc_dir/bin:$PATH"
export DYLD_FALLBACK_LIBRARY_PATH="$tc_dir/lib"

lock_ver="$(awk '/^name = "wasm-bindgen"$/ {getline; gsub(/version = |"/, ""); print; exit}' Cargo.lock)"
cli_ver="$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')"
if [[ "$lock_ver" != "$cli_ver" ]]; then
  echo "wasm-bindgen-cli 版本是 ${cli_ver:-（没装）}，Cargo.lock 要 $lock_ver：" >&2
  echo "  cargo install wasm-bindgen-cli --version $lock_ver --locked" >&2
  exit 1
fi

target_dir="$here/target-wasm"
out="$target_dir/dist"

# 从 cargo 的 JSON 消息里取 falcon-app 构建脚本的 OUT_DIR（解好的 TTF 在那儿）
fonts_dir="$(
  cargo build --profile web --target wasm32-unknown-unknown --target-dir "$target_dir" -p falcon-web \
    --message-format=json-render-diagnostics |
    python3 -c '
import json, sys
out = ""
for line in sys.stdin:
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("reason") == "build-script-executed" and m.get("package_id", "").find("falcon-app") >= 0:
        out = m["out_dir"]
print(out)
'
)"
[[ -n "$fonts_dir" ]] || { echo "没拿到 falcon-app 的 OUT_DIR" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out/fonts" "$out/assets"
wasm-bindgen --target web --no-typescript --out-dir "$out" \
  "$target_dir/wasm32-unknown-unknown/web/falcon_web.wasm"

# wasm-opt 默认不跑（FALCON_WASM_OPT=1 打开）：2026-10 实测（binaryen 133，-Os / -Oz / -O3），
# 原始体积能小 10–14%，但 gzip / brotli 之后反而大 0.1–0.2MB——rustc 的 opt-level=s + LTO 已经
# 做完了能做的，它再改写只是打乱了字节分布。传给浏览器的是压缩后的字节，按那个算它是负收益
if [[ "${FALCON_WASM_OPT:-}" == 1 ]]; then
  # gpui / wgpu 用到的这些 wasm 特性 rustc 默认就开着，wasm-opt 要显式认
  wasm-opt -Os --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
    --enable-mutable-globals --enable-reference-types --enable-multivalue \
    -o "$out/falcon_web_bg.opt.wasm" "$out/falcon_web_bg.wasm"
  mv "$out/falcon_web_bg.opt.wasm" "$out/falcon_web_bg.wasm"
fi

cp web/index.html "$out/"
# 内置应用图标（cargo xtask icons 的产物）：服务端的 /api/app-icon/* 与 PWA 清单都指向 /icons/<id>/…
cp -R web/icons "$out/icons"
# 浏览器版只嵌了正文字体，其余按需拉（falcon-app/src/fonts.rs）
for f in IoskeleyMonoTerm.ttf MapleMonoNL-NF-CN.ttf SymbolsNerdFontMono.ttf; do
  cp "$fonts_dir/$f" "$out/fonts/"
done
# gpui-kit-assets 在 wasm 上按需取 <源>/assets/icons/*.svg
assets_src="$(cargo metadata --format-version 1 --filter-platform wasm32-unknown-unknown |
  python3 -c 'import json,sys; m=json.load(sys.stdin); print(next(p["manifest_path"] for p in m["packages"] if p["name"]=="gpui-kit-assets"))')"
cp -R "$(dirname "$assets_src")/assets/icons" "$out/assets/"

wasm="$out/falcon_web_bg.wasm"
raw=$(wc -c <"$wasm")
gz=$(gzip -9 -c "$wasm" | wc -c)
br=$( (command -v brotli >/dev/null && brotli -q 11 -c "$wasm" | wc -c) || echo "?")
printf 'wasm: 原始 %.1f MB · gzip %.1f MB · brotli %s\n' \
  "$(echo "$raw / 1048576" | bc -l)" "$(echo "$gz / 1048576" | bc -l)" \
  "$( [[ "$br" == "?" ]] && echo "?" || printf '%.1f MB' "$(echo "$br / 1048576" | bc -l)")"
echo "产物：$out"

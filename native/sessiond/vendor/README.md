# Ghostty 构建依赖归档

`translate_c-0.0.0-Q_BUWhVNBwDOEcIqub4VFPJPB6D9dgwzUMHTX5KWr8Xr.tar.gz` 是 `zig fetch` 规范化后的包缓存归档，内容对应：

- 上游提交：`https://codeberg.org/vancluever/translate-c/archive/4e879eb8aba615de112eabd1231ea6e01920cead.tar.gz`
- Zig 包哈希：`translate_c-0.0.0-Q_BUWhVNBwDOEcIqub4VFPJPB6D9dgwzUMHTX5KWr8Xr`（与 `ghostty.pin` 所选 Ghostty 的 `pkg/translate-c/build.zig.zon` 一致）
- 归档 SHA-256：`f59dab4e74b399e9d00c44f5d667c5c0d212ddfaf72537b593d76514dd59ec8c`

Codeberg 域名在 2026-11-18 返回 NXDOMAIN，导致 GitHub Actions 的四个 `sessiond-build` 任务全部失败。`xtask` 会在编译 `libghostty-vt` 前校验此归档并复制到 Zig 全局包缓存，因此缓存未命中时也不必访问 Codeberg。升级 Ghostty 后，如果上游的 `translate_c` 包哈希变化，须从可信的 Zig 缓存重新导出此文件并同步修改 `native/xtask/src/sessiond.rs` 中的文件名与 SHA-256。

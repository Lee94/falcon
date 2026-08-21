import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  optimizeDeps: {
    // rioterm 走动态 import 且靠 import.meta.url 定位同目录的 .wasm；
    // esbuild 预打包会把产物挪进 .vite/deps，wasm 就 404 了
    exclude: ["rioterm"],
  },
  build: {
    // 默认 target（≈es2020）会让 esbuild 压缩时降级 `||=`，而 xterm.js 6.x 的
    // enum 产物 `let r;(te=>…)(r||={})` 在 rollup+esbuild 组合下会被错误重命名成
    // `void 0||(i={})`（i 未声明），zellij 一发 DECRQM 查询 requestMode 就抛
    // ReferenceError、终端全空白。es2022 下 `||=` 原样保留，绕开该压缩 bug。
    target: "es2022",
  },
  server: {
    port: 5173,
    proxy: {
      "/api": "http://localhost:4923",
      "/ws": { target: "ws://localhost:4923", ws: true },
    },
  },
});

import React from "react";
import { createRoot } from "react-dom/client";
import "./i18n.js";
import "@xterm/xterm/css/xterm.css";
import "./lib/maple-mono.css";
import "./lib/nerd-symbols.css";
import "./styles.css";
import { MAPLE_FONT_FAMILY, NERD_FONT_FAMILY } from "./lib/term.js";
import { App } from "./components/App.js";
import { initInstallListeners } from "./lib/install.js";

// 预热字体，别等终端回放时字库还没到（xterm 会把缺的字形画进 atlas）。
// 不带文本的 load 只命中拉丁片（~40KB）；5MB 的 CJK 片留到空闲时预取——
// 首屏不占带宽，等终端真输出中文时它多半已经就位，即使没就位，
// TerminalView 监听的 fonts loadingdone 也会在片子到达后重建 atlas。
void document.fonts.load(`13px "${MAPLE_FONT_FAMILY}"`);
void document.fonts.load(`13px "${NERD_FONT_FAMILY}"`);
const prefetchCjk = () => void document.fonts.load(`13px "${MAPLE_FONT_FAMILY}"`, "中");
if ("requestIdleCallback" in window) requestIdleCallback(prefetchCjk, { timeout: 5000 });
else setTimeout(prefetchCjk, 3000);
initInstallListeners();

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);

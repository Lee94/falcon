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

// Maple woff2 有 6MB+，不提前拉的话，终端回放时字库还没到，xterm 会把缺的字形
// 画进 atlas，之后 fit() 也不会清。图标字体也一起预热。
void document.fonts.load(`13px "${MAPLE_FONT_FAMILY}"`);
void document.fonts.load(`13px "${NERD_FONT_FAMILY}"`);
initInstallListeners();

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);

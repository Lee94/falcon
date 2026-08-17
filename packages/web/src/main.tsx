import React from "react";
import { createRoot } from "react-dom/client";
import "./i18n.js";
import "@xterm/xterm/css/xterm.css";
import "./lib/maple-mono.css";
import "./styles.css";
import { App } from "./components/App.js";

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);

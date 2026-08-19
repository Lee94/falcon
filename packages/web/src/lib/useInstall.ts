import { useEffect, useState } from "react";
import {
  canInstall,
  isStandalone,
  promptInstall,
  subscribeInstall,
} from "./install.js";

export function useInstall() {
  const [, bump] = useState(0);
  useEffect(() => subscribeInstall(() => bump((n) => n + 1)), []);
  return {
    standalone: isStandalone(),
    canInstall: canInstall(),
    promptInstall,
  };
}

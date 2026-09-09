/**
 * Claude Code / grok 这类 agent 的 Bash 工具用管道起子进程，sudo 打不开
 * /dev/tty 就会报 "a terminal is required to authenticate"。PTY 本身没问题
 * （isatty / 控制终端都在），缺的是一条不经过 TTY 的问密通道。
 *
 * 做法：在会话 PATH 最前面放一个 `sudo` 包装。人在 Falcon 提示符里敲 sudo
 * （stdin 是 tty）原样交给 /usr/bin/sudo；管道里调 sudo 则补 `-A` 并把
 * SUDO_ASKPASS 指到同目录的 helper。helper 读旁边的 conf（不靠环境变量——
 * Claude Code 会把 SUDO_ASKPASS 从子进程环境里抠掉），长轮询 Falcon 后端，
 * 密码弹在网页上。
 *
 * 包装必须叫 `sudo` 且靠 PATH 抢在 /usr/bin 前面：agent 几乎都是 `bash -c
 * 'sudo …'`，极少写死绝对路径。绝对路径 `/usr/bin/sudo` 这条拦不住，v1 接受。
 */

export const SUDO_SHIM_NAME = "sudo";
export const ASKPASS_NAME = "falcon-askpass";
export const ASKPASS_CONF_NAME = "askpass.conf";

export const ASKPASS_TIMEOUT_MS = 120_000;

export function renderAskpassConf(url: string, token: string): string {
  return `URL=${url}\nTOKEN=${token}\n`;
}

export function parseAskpassConf(text: string): { url: string; token: string } | null {
  let url = "";
  let token = "";
  for (const line of text.split(/\r?\n/)) {
    const t = line.trim();
    if (!t || t.startsWith("#") || !t.includes("=")) continue;
    const i = t.indexOf("=");
    const k = t.slice(0, i);
    const v = t.slice(i + 1);
    if (k === "URL") url = v;
    else if (k === "TOKEN") token = v;
  }
  if (!url || !token) return null;
  return { url, token };
}

/**
 * stdin 是 tty → 人在终端里敲的，走原 sudo。
 * 否则视为 agent / 管道，需要 -A；已经带 -n / -S / -A 的不重复加。
 *
 * 短参簇里的 n/S/A 也认（`sudo -nv`、`sudo -An`）。`-u` 的 u 不是这三位。
 */
export function sudoNeedsAskpassFlag(argv: string[]): boolean {
  for (const a of argv) {
    if (a === "--") break;
    if (a === "-n" || a === "--non-interactive") return false;
    if (a === "-S" || a === "--stdin") return false;
    if (a === "-A" || a === "--askpass") return false;
    if (a.startsWith("--")) continue;
    if (a.startsWith("-") && a.length > 1) {
      const rest = a.slice(1);
      if ([...rest].every((c) => /[A-Za-z]/.test(c))) {
        if (rest.includes("n") || rest.includes("S") || rest.includes("A")) return false;
      }
    }
  }
  return true;
}

export function renderSudoShim(): string {
  return `#!/bin/sh
# falcon sudo 包装：管道/agent 走 askpass，交互终端原样透传。
REAL="\${FALCON_REAL_SUDO:-/usr/bin/sudo}"
if [ ! -x "\$REAL" ]; then
  if [ -x /bin/sudo ]; then REAL=/bin/sudo
  else
    echo "falcon: 找不到 sudo" >&2
    exit 127
  fi
fi

# stdin 是 tty = 人在 Falcon 提示符里敲的，不要改成网页弹窗。
if [ -t 0 ]; then
  exec "\$REAL" "\$@"
fi

needs_A=1
already_A=0
for a in "\$@"; do
  if [ "\$a" = "--" ]; then break; fi
  case "\$a" in
    -n|--non-interactive|-S|--stdin) needs_A=0; break ;;
    -A|--askpass) already_A=1; needs_A=0; break ;;
    --*) ;;
    -*)
      rest=\$(printf %s "\$a" | cut -c2-)
      case "\$rest" in
        *n*|*S*) needs_A=0; break ;;
        *A*) already_A=1; needs_A=0; break ;;
      esac
      ;;
  esac
done

HERE=\$(CDPATH= cd -- "\$(dirname "\$0")" && pwd)
ASKPASS="\$HERE/${ASKPASS_NAME}"
export SUDO_ASKPASS="\$ASKPASS"
if [ "\$needs_A" = 1 ]; then
  exec "\$REAL" -A "\$@"
fi
exec "\$REAL" "\$@"
`;
}

/**
 * 纯 Python 3：远端 POSIX 没有 node，urllib 是标准库。
 * 出错只写 stderr——stdout 会被 sudo 当成密码。
 */
export function renderAskpassHelper(): string {
  return `#!/usr/bin/env python3
import json, os, sys, urllib.error, urllib.request

def load_conf(path):
    url = token = ""
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            if k == "URL":
                url = v
            elif k == "TOKEN":
                token = v
    if not url or not token:
        raise SystemExit("falcon-askpass: askpass.conf 不完整")
    return url, token

def main():
    prompt = sys.argv[1] if len(sys.argv) > 1 else "Password:"
    here = os.path.dirname(os.path.abspath(__file__))
    url, token = load_conf(os.path.join(here, "${ASKPASS_CONF_NAME}"))
    session = os.environ.get("FALCON_SESSION_ID") or None
    body = json.dumps({"prompt": prompt, "sessionId": session}).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={
            "Authorization": "Bearer " + token,
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=125) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except Exception as e:
        print(f"falcon-askpass: {e}", file=sys.stderr)
        raise SystemExit(1)
    password = data.get("password")
    if not isinstance(password, str) or password == "":
        raise SystemExit(1)
    sys.stdout.write(password if password.endswith("\\n") else password + "\\n")

if __name__ == "__main__":
    main()
`;
}

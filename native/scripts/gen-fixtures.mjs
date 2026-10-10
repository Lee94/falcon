#!/usr/bin/env node
/**
 * 协议 fixture 生成（设计文档 docs/design/gpui-client.md 决定五）。
 *
 * 拉起一个**真实的** falcon 服务端（临时数据目录、空闲端口），建一个本地项目（工作目录
 * 是一个临时 git 仓库：几个文件、两次提交、一处未提交改动、一个未跟踪文件）、SSH
 * 项目、多仓库容器、附属项目、会话……把各接口的真实响应落盘到
 * native/crates/falcon-proto/tests/fixtures/*.json，再由 falcon-proto 的
 * tests/fixtures.rs 逐个反序列化。服务端改了字段名或删了字段，重生 fixture 时 Rust
 * 测试就红——这是 shared（TS）与 falcon-proto（Rust 手写镜像）之间唯一的契约检查。
 *
 * 何时重跑：
 *   - packages/shared/src/index.ts 改了线上形状（加 / 改 / 删字段、改字面量）；
 *   - packages/server/src/routes.ts、meegle/routes.ts、ws.ts 改了某个端点的响应；
 *   - falcon-proto 改了类型之后，顺手重跑一次确认两边还对得上。
 * 重跑后 `git diff` 看一眼 fixture 的变化：id、时间戳、令牌每次都会变，那是正常的；
 * 字段的增减才是要关心的。
 *
 * 跑法（仓库根目录）：
 *   pnpm --filter @falcon/shared build && pnpm --filter @falcon/server build
 *   node native/scripts/gen-fixtures.mjs
 *   (cd native && cargo test -p falcon-proto --test fixtures)
 *
 * 对拍 Rust 服务端（S 线）：落到别的目录，再用 compare-fixtures.mjs 逐个比形状
 *   FALCON_FIXTURE_SERVER_BIN=native/target/debug/falcon-server FALCON_FIXTURE_OUT=/tmp/fx-rs \
 *     node native/scripts/gen-fixtures.mjs
 *   node native/scripts/compare-fixtures.mjs native/crates/falcon-proto/tests/fixtures /tmp/fx-rs
 *
 * 几个坑：
 *   - 数据目录必须是短路径：zellij 的 IPC socket 全路径在 macOS 上限 104 字节，
 *     长路径会让会话建完立刻 exited。所以一律放 /private/tmp/fal-fx-*。
 *   - 全新数据目录第一次建本地会话会去 GitHub 下载 zellij（14MB）。脚本先从
 *     ~/.falcon/bin 或 ~/.mojito/bin 拷一份锁定版本的二进制进去（服务端安装流程会先
 *     检查 binDir 里现成的），找不到才让它下载。也可以用 FALCON_FIXTURE_ZELLIJ 指定。
 *   - 端口只在 4940–4999 里挑，绝不碰 4923（那是日常在用、设了密码的实例）。
 *   - 从 falcon 终端里跑时环境里会带着父实例的 FALCON_* 变量（FALCON_WEB_DIST 之类），
 *     启动子服务端前全部去掉。
 *   - fixture 里不能有机密：飞书项目的登录用户（名字、邮箱、头像）脱敏；飞书的业务数据
 *     （空间、待办）一律不取。路径、用户名保留。
 */
import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const SERVER = path.join(ROOT, "packages/server/dist/index.js");
// FALCON_FIXTURE_SERVER_BIN：改起 Rust 服务端（native/target/debug/falcon-server 之类）；
// FALCON_FIXTURE_OUT：落到别的目录，好与 Node 版的 fixture 逐个比形状（S 线对拍用）
const SERVER_BIN = process.env.FALCON_FIXTURE_SERVER_BIN;
const OUT = process.env.FALCON_FIXTURE_OUT
  ? path.resolve(process.env.FALCON_FIXTURE_OUT)
  : path.join(ROOT, "native/crates/falcon-proto/tests/fixtures");

const TMP = "/private/tmp";
const DATA = `${TMP}/fal-fx-data`;
const REPO = `${TMP}/fal-fx-repo`;
const PLAIN = `${TMP}/fal-fx-plain`;
const PASSWORD = "fixture-pass";

// 1×1 PNG
const PNG = Buffer.from(
  "89504e470d0a1a0a0000000d4948445200000001000000010806000000" +
    "1f15c4890000000d49444154789c6360f8cfc0f01f0005000201e2a6a1" +
    "e90000000049454e44ae426082",
  "hex"
);

let server = null;
let base = "";
let cookie = null;
const written = [];

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});

async function main() {
  if (SERVER_BIN ? !fs.existsSync(SERVER_BIN) : !fs.existsSync(SERVER)) {
    if (SERVER_BIN) throw new Error(`没找到 ${SERVER_BIN}：先 cargo build -p falcon-server`);
    throw new Error(`没找到 ${SERVER}：先在仓库根目录跑 pnpm --filter @falcon/shared build && pnpm --filter @falcon/server build`);
  }
  cleanupDirs();
  try {
    prepareRepo();
    prepareDataDir();
    const port = await freePort(4940, 4999);
    base = `http://127.0.0.1:${port}`;
    server = startServer(port);
    await waitReady();
    fs.mkdirSync(OUT, { recursive: true });
    for (const f of fs.readdirSync(OUT, { withFileTypes: true }).filter((e) => e.name.endsWith(".json"))) {
      fs.rmSync(path.join(OUT, f.name));
    }
    await capture();
    console.log(`写了 ${written.length} 个 fixture 到 ${path.relative(ROOT, OUT)}/`);
  } finally {
    await stopServer();
    cleanupDirs();
  }
}

async function capture() {
  // ---- 认证（先不设密码：绑回环、没密码时不需要认证）/ 系统 / 目录 ----
  save("auth-status", (await api("GET", "/api/auth/status")).json);
  save("system", (await api("GET", "/api/system")).json);
  save("askpass-pending", (await api("GET", "/api/askpass/pending")).json);
  save("fs-validate-ok", (await api("POST", "/api/fs/validate", { path: REPO })).json);
  save("fs-validate-missing", (await api("POST", "/api/fs/validate", { path: `${TMP}/fal-fx-nope` })).json);
  save("fs-list", (await api("GET", `/api/fs/list?${qs({ path: REPO })}`)).json);
  save("shells", (await api("GET", "/api/shells")).json);

  // ---- 远端主机：只落库、不真连（127.0.0.1:1 连不上，正好拿到 ok:false 的探测）----
  const host = (
    await api("POST", "/api/hosts", {
      name: "fixture-host",
      host: "127.0.0.1",
      port: 1,
      username: "fixture",
      authMethod: "password",
      secret: "not-a-real-password",
    })
  ).json;
  save("host-create", host);
  save(
    "host-update",
    (
      await api("PUT", `/api/hosts/${host.id}`, {
        name: "fixture-host-renamed",
        host: "127.0.0.1",
        port: 1,
        username: "fixture",
        authMethod: "password",
      })
    ).json
  );
  save("host-test", (await api("POST", `/api/hosts/${host.id}/test`)).json);
  save(
    "host-test-draft",
    (
      await api("POST", "/api/hosts/test", {
        name: "draft",
        host: "127.0.0.1",
        port: 1,
        username: "fixture",
        authMethod: "password",
        hostId: host.id,
      })
    ).json
  );

  // ---- 项目 ----
  const local = (
    await api("POST", "/api/projects", {
      name: "fixture",
      type: "local",
      workingDir: REPO,
      defaultWorktreeBranch: "main",
    })
  ).json;
  save("project-create-local", local);
  const ssh = (
    await api("POST", "/api/projects", { name: "fixture-ssh", type: "ssh", hostId: host.id, workingDir: "/srv/app" })
  ).json;
  save("project-create-ssh", ssh);
  const multi = (await api("POST", "/api/projects", { name: "fixture-multi", type: "local", repos: [REPO] })).json;
  save("project-create-multi", multi);
  const plain = (await api("POST", "/api/projects", { name: "fixture-plain", type: "local", workingDir: PLAIN })).json;
  save(
    "project-update",
    (
      await api("PUT", `/api/projects/${plain.id}`, {
        name: "fixture-plain-renamed",
        type: "local",
        workingDir: PLAIN,
        shell: "/bin/sh",
      })
    ).json
  );
  save("hosts", (await api("GET", "/api/hosts")).json);

  // ---- 派生前探测 ----
  save("repo-info", (await api("GET", `/api/projects/${local.id}/repo`)).json);
  save("repos-multi", (await api("GET", `/api/projects/${multi.id}/repos`)).json);

  // ---- Git 面板（在动文件之前取，仓库状态最干净）----
  save("git-snapshot", (await api("GET", `/api/projects/${local.id}/git`)).json);
  save("git-snapshot-multi", (await api("GET", `/api/projects/${multi.id}/git?${qs({ repo: REPO })}`)).json);
  save("git-snapshot-unavailable", (await api("GET", `/api/projects/${plain.id}/git`)).json);
  save("git-changes", (await api("GET", `/api/projects/${local.id}/git/changes`)).json);
  save("git-changes-batch", (await api("POST", "/api/git/changes", { ids: [local.id, plain.id, "gone"] })).json);
  save("git-diff", (await api("GET", `/api/projects/${local.id}/git/diff?${qs({ path: "src/main.rs" })}`)).json);
  save(
    "git-diff-untracked",
    (await api("GET", `/api/projects/${local.id}/git/diff?${qs({ path: "notes.txt", untracked: "1" })}`)).json
  );
  save("git-diff-unavailable", (await api("GET", `/api/projects/${plain.id}/git/diff?${qs({ path: "x" })}`)).json);
  save("git-working", (await api("GET", `/api/projects/${local.id}/git/working`)).json);
  save("git-working-unavailable", (await api("GET", `/api/projects/${plain.id}/git/working`)).json);
  const log = (await api("GET", `/api/projects/${local.id}/git/log`)).json;
  save("git-log", log);
  save("git-log-unavailable", (await api("GET", `/api/projects/${plain.id}/git/log`)).json);
  save("git-refs", (await api("GET", `/api/projects/${local.id}/git/refs`)).json);
  save("git-refs-unavailable", (await api("GET", `/api/projects/${plain.id}/git/refs`)).json);
  const sha = log.commits[0].sha;
  save("git-commit-detail", (await api("GET", `/api/projects/${local.id}/git/commit?${qs({ sha })}`)).json);
  save(
    "git-commit-diff",
    (await api("GET", `/api/projects/${local.id}/git/commit/diff?${qs({ sha, path: "README.md" })}`)).json
  );
  save("git-op-fetch", (await api("POST", `/api/projects/${local.id}/git/op`, { op: "fetch" })).json);
  save("git-push", (await api("POST", `/api/projects/${local.id}/git/push`)).json);

  // ---- 文件面板（在 git 之后：会往仓库里写未跟踪文件）----
  save("files", (await api("GET", `/api/projects/${local.id}/files`)).json);
  save("files-subdir", (await api("GET", `/api/projects/${local.id}/files?${qs({ path: "src" })}`)).json);
  save("files-index", (await api("GET", `/api/projects/${local.id}/files/index`)).json);
  const text = (await api("GET", `/api/projects/${local.id}/file?${qs({ path: "README.md" })}`)).json;
  save("file-text", text);
  save("file-image", (await api("GET", `/api/projects/${local.id}/file?${qs({ path: "assets/logo.png" })}`)).json);
  save("file-binary", (await api("GET", `/api/projects/${local.id}/file?${qs({ path: "assets/blob.bin" })}`)).json);
  // 原始字节路由不是 JSON，只确认 rawBase 拼出来的地址取得到字节
  const raw = await fetch(`${base}${text.rawBase}assets/logo.png`);
  if (raw.status !== 200 || !Buffer.from(await raw.arrayBuffer()).equals(PNG)) {
    throw new Error(`原始字节路由不对：${raw.status}`);
  }
  save("mkdir", (await api("POST", `/api/projects/${local.id}/mkdir`, { path: "uploads", recursive: false })).json);
  const upload = `/api/projects/${local.id}/upload?${qs({ path: "uploads", name: "hello.txt" })}`;
  save("upload", (await api("PUT", upload, Buffer.from("hello falcon\n"))).json);
  save("error-conflict", (await api("PUT", upload, Buffer.from("again\n"), { expect: [409] })).json);
  save(
    "rename",
    (await api("POST", `/api/projects/${local.id}/rename`, { path: "uploads/hello.txt", name: "hi.txt" })).json
  );
  save(
    "remove",
    (
      await api("POST", `/api/projects/${local.id}/remove`, {
        paths: ["uploads/hi.txt", "../outside.txt"],
      })
    ).json
  );

  // ---- 中转（挂机器，ADR 0016）：enabled:false，只落库不起隧道、不连主机 ----
  const fwd = (
    await api("POST", "/api/forwards", {
      hostId: host.id,
      name: "pg",
      kind: "local",
      bindPort: 45999,
      destPort: 5432,
      enabled: false,
    })
  ).json;
  save("forward-create", fwd);
  save("forward-update", (await api("PATCH", `/api/forwards/${fwd.id}`, { name: "postgres" })).json);
  // 本机的发布没有 hostId，挂主机的有
  const share = (await api("POST", "/api/shares", { name: "vite", destPort: 5173, enabled: false })).json;
  save("share-create", share);
  const hostShare = (
    await api("POST", "/api/shares", { hostId: host.id, name: "api", destPort: 8000, enabled: false })
  ).json;
  save("share-create-host", hostShare);
  save("share-update", (await api("PATCH", `/api/shares/${share.id}`, { name: "web" })).json);
  save("relays", (await api("GET", "/api/relays")).json);
  save("forward-delete", (await api("DELETE", `/api/forwards/${fwd.id}`)).json);
  save("share-delete", (await api("DELETE", `/api/shares/${share.id}`)).json);
  await api("DELETE", `/api/shares/${hostShare.id}`);

  // ---- 宿主机 Zellij（仅 SSH 项目；只读写库，不连远端）----
  save("host-zellij-status", (await api("GET", `/api/projects/${ssh.id}/host`)).json);
  save("host-authorization", (await api("POST", `/api/projects/${ssh.id}/host`, { authorized: false })).json);
  save("host-zellij-status-denied", (await api("GET", `/api/projects/${ssh.id}/host`)).json);

  // ---- 附属项目：派生 → 预检 → 存档 → 恢复 → 删除（会在仓库旁边建一棵 worktree）----
  const wt = (
    await api("POST", `/api/projects/${local.id}/worktrees`, { mode: "new-branch", branch: "feat/fixture" })
  ).json;
  save("worktree-create", wt);
  save("worktree-status", (await api("GET", `/api/projects/${wt.id}/worktree`)).json);
  save("project-archive", (await api("POST", `/api/projects/${wt.id}/archive`)).json);
  save("project-restore", (await api("POST", `/api/projects/${wt.id}/restore`)).json);
  save("project-delete", (await api("DELETE", `/api/projects/${wt.id}?force=true`)).json);
  save("projects", (await api("GET", "/api/projects")).json);

  // ---- 会话 ----
  const session = (
    await api("POST", `/api/projects/${local.id}/sessions`, {
      appearance: "dark",
      background: "#0a0a0a",
      foreground: "#fafafa",
    })
  ).json;
  save("session-create", session);
  const wsMessages = await captureSessionWs(session.id);
  save("ws-session-messages", wsMessages.text);
  save("session-foreground-busy", wsMessages.busy);
  save("session-foreground", (await api("GET", `/api/sessions/${session.id}/foreground`)).json);
  save("session-rename", (await api("PATCH", `/api/sessions/${session.id}`, { name: "fixture shell" })).json);
  save("sessions", (await api("GET", "/api/sessions")).json);
  save("system-after-session", (await api("GET", "/api/system")).json);
  save(
    "paste-image",
    (await api("POST", `/api/sessions/${session.id}/paste-image`, PNG, { contentType: "image/png" })).json
  );
  save("session-reattach", (await api("POST", `/api/sessions/${session.id}/reattach`)).json);
  save("session-terminate", (await api("POST", `/api/sessions/${session.id}/terminate`)).json);
  // Terminate 直接删行：之后既不在列表里，也清除不了（清除只认 dead 的行）
  save("sessions-after-terminate", (await api("GET", "/api/sessions")).json);
  save("error-session-clear", (await api("DELETE", `/api/sessions/${session.id}`, undefined, { expect: [409] })).json);
  save("ws-session-missing", (await wsCollect(`/ws/sessions/${session.id}`, { ms: 800 })).text);

  // shell 自己退出 = 已丢失（dead）：这才是能"清除记录"的那种会话
  const doomed = (await api("POST", `/api/projects/${local.id}/sessions`, {})).json;
  save("ws-session-exit", await captureSessionExit(doomed.id));
  save("sessions-with-dead", (await api("GET", "/api/sessions")).json);
  save("session-reattach-dead", (await api("POST", `/api/sessions/${doomed.id}/reattach`)).json);
  save("ws-session-dead", (await wsCollect(`/ws/sessions/${doomed.id}`, { ms: 800 })).text);
  save("session-clear", (await api("DELETE", `/api/sessions/${doomed.id}`)).json);

  // ---- Zellij 安装通道：本地项目已经装好，直接 done；项目不存在是 failed ----
  save("ws-install-local", (await wsCollect(`/ws/install/${local.id}`, { untilClose: true })).text);
  save("ws-install-missing", (await wsCollect("/ws/install/no-such-project", { untilClose: true })).text);

  // ---- 飞书项目：状态脱敏；业务数据（空间、待办）一律不取；固定列表是 falcon 自己的库 ----
  const meegle = (await api("GET", "/api/meegle/status")).json;
  if (meegle.user) {
    meegle.user = { key: "user_redacted", name: "已脱敏", email: "redacted@example.com" };
  }
  save("meegle-status", meegle);
  if (!meegle.installed || !meegle.authenticated) {
    // 没装 / 没登录时业务接口是 409 + reason，正好是 app 要认的错误体
    save("error-meegle-unavailable", (await api("GET", "/api/meegle/spaces", undefined, { expect: [409] })).json);
  }
  const pin = (
    await api("POST", "/api/meegle/pins", {
      kind: "workitem",
      spaceKey: "fixture_space",
      spaceName: "Fixture",
      targetId: "1001",
      typeKey: "story",
      label: "登录页",
      url: "https://project.feishu.cn/fixture/story/detail/1001",
    })
  ).json;
  save("meegle-pin-create", pin);
  await api("POST", "/api/meegle/pins", { kind: "view", spaceKey: "fixture_space", targetId: "view_1", label: "view_1" });
  save("meegle-pin-rename", (await api("PATCH", `/api/meegle/pins/${pin.id}`, { label: "登录页（改）" })).json);
  save("meegle-pins", (await api("GET", "/api/meegle/pins")).json);
  save("meegle-pin-delete", (await api("DELETE", `/api/meegle/pins/${pin.id}`)).json);

  // ---- 错误体 ----
  save("error-not-found", (await api("GET", "/api/projects/no-such-project/git", undefined, { expect: [404] })).json);
  save(
    "error-bad-request",
    (await api("GET", `/api/projects/${local.id}/git/commit?${qs({ sha: "zz" })}`, undefined, { expect: [400] })).json
  );

  // ---- 最后才设密码：之后所有请求都要 cookie ----
  save("auth-password", (await api("POST", "/api/auth/password", { next: PASSWORD })).json);
  save("auth-status-locked", (await api("GET", "/api/auth/status")).json);
  save("error-unauthorized", (await api("GET", "/api/projects", undefined, { expect: [401] })).json);
  save("error-login", (await api("POST", "/api/auth/login", { password: "wrong-password" }, { expect: [401] })).json);
  const login = await api("POST", "/api/auth/login", { password: PASSWORD });
  cookie = /falcon_token=([^;]+)/.exec(login.headers.get("set-cookie") ?? "")?.[1] ?? null;
  if (!cookie) throw new Error("登录响应里没有 falcon_token");
  save("auth-login", login.json);
  save("auth-status-authenticated", (await api("GET", "/api/auth/status")).json);

  // 收尾：删项目（DELETE 的响应已经有 project-delete 了，这里只清理）
  for (const p of [local, ssh, multi, plain]) await api("DELETE", `/api/projects/${p.id}?force=true`);
  await api("DELETE", `/api/hosts/${host.id}`);
  save("auth-logout", (await api("POST", "/api/auth/logout")).json);
}

/** 连上会话，报尺寸与外观，敲一条 `sleep` 让前台命令变一变，收集控制消息。 */
async function captureSessionWs(id) {
  const ws = new WebSocket(`${base.replace("http", "ws")}/ws/sessions/${id}`);
  ws.binaryType = "arraybuffer";
  const text = [];
  let replay = 0;
  let output = "";
  ws.onmessage = (ev) => {
    if (typeof ev.data === "string") text.push(JSON.parse(ev.data));
    else {
      const bytes = new Uint8Array(ev.data);
      if (bytes[0] === 0x02) replay++;
      output += Buffer.from(bytes.subarray(1)).toString("utf8");
    }
  };
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = reject;
  });
  ws.send(JSON.stringify({ type: "appearance", appearance: "dark", background: "#0a0a0a", foreground: "#fafafa" }));
  ws.send(JSON.stringify({ type: "resize", cols: 100, rows: 30 }));
  await sleep(1500);
  ws.send(JSON.stringify({ type: "input", data: "sleep 4\r" }));
  await sleep(2000);
  const busy = (await api("GET", `/api/sessions/${id}/foreground`)).json;
  await sleep(5000);
  ws.close();
  if (replay === 0) throw new Error("会话 WS 没收到回放帧");
  if (!stripAnsi(output).includes("sleep 4")) {
    throw new Error(`会话 WS 没收到输入回显：${JSON.stringify(stripAnsi(output).slice(-600))}\n控制消息：${JSON.stringify(text)}`);
  }
  return { text, busy };
}

/** 连上会话敲 `exit`，收集控制消息直到服务端推 dead。 */
async function captureSessionExit(id) {
  const ws = new WebSocket(`${base.replace("http", "ws")}/ws/sessions/${id}`);
  ws.binaryType = "arraybuffer";
  const text = [];
  let resolveDead;
  const dead = new Promise((resolve) => (resolveDead = resolve));
  ws.onmessage = (ev) => {
    if (typeof ev.data !== "string") return;
    const msg = JSON.parse(ev.data);
    text.push(msg);
    if (msg.type === "state" && msg.state === "dead") resolveDead();
  };
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = reject;
  });
  ws.send(JSON.stringify({ type: "resize", cols: 100, rows: 30 }));
  await sleep(1500);
  ws.send(JSON.stringify({ type: "input", data: "exit\r" }));
  await Promise.race([dead, sleep(15_000).then(() => Promise.reject(new Error(`exit 之后 15s 没等到 dead：${JSON.stringify(text)}`)))]);
  ws.close();
  return text;
}

/** 连一条 WS，收集文本帧：到时间就关，或者等服务端关。 */
async function wsCollect(p, { ms = 1000, untilClose = false } = {}) {
  const ws = new WebSocket(`${base.replace("http", "ws")}${p}`);
  ws.binaryType = "arraybuffer";
  const text = [];
  let code = null;
  ws.onmessage = (ev) => {
    if (typeof ev.data === "string") text.push(JSON.parse(ev.data));
  };
  const closed = new Promise((resolve) => {
    ws.onclose = (ev) => {
      code = ev.code;
      resolve();
    };
  });
  if (untilClose) {
    await Promise.race([closed, sleep(30_000).then(() => Promise.reject(new Error(`${p} 30s 没关`)))]);
  } else {
    await sleep(ms);
    ws.close();
    await closed;
  }
  return { text, code };
}

// ---------------- HTTP ----------------

async function api(method, p, body, { expect = [200], contentType } = {}) {
  const headers = {};
  if (cookie) headers.cookie = `falcon_token=${cookie}`;
  let payload;
  if (Buffer.isBuffer(body)) {
    headers["content-type"] = contentType ?? "application/octet-stream";
    payload = body;
  } else if (body !== undefined) {
    headers["content-type"] = "application/json";
    payload = JSON.stringify(body);
  }
  const res = await fetch(`${base}${p}`, { method, headers, body: payload });
  const raw = await res.text();
  let json;
  try {
    json = JSON.parse(raw);
  } catch {
    json = undefined;
  }
  if (!expect.includes(res.status)) {
    throw new Error(`${method} ${p} → ${res.status}（期望 ${expect.join("/")}）：${raw.slice(0, 500)}`);
  }
  return { status: res.status, json, headers: res.headers };
}

function qs(o) {
  return new URLSearchParams(o).toString();
}

function save(name, value) {
  if (value === undefined) throw new Error(`${name}：响应不是 JSON`);
  fs.writeFileSync(path.join(OUT, `${name}.json`), `${JSON.stringify(value, null, 2)}\n`);
  written.push(name);
}

// ---------------- 环境 ----------------

function git(...args) {
  const env = {
    ...process.env,
    GIT_AUTHOR_NAME: "Falcon Fixture",
    GIT_AUTHOR_EMAIL: "fixture@example.com",
    GIT_COMMITTER_NAME: "Falcon Fixture",
    GIT_COMMITTER_EMAIL: "fixture@example.com",
  };
  // 本机的全局配置可能要求签名、挂着钩子：一律关掉，fixture 不该依赖它们
  return execFileSync(
    "git",
    ["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", "-c", "core.hooksPath=/dev/null", "-C", REPO, ...args],
    { env, encoding: "utf8" }
  );
}

function prepareRepo() {
  fs.mkdirSync(path.join(REPO, "src"), { recursive: true });
  fs.mkdirSync(path.join(REPO, "assets"), { recursive: true });
  fs.mkdirSync(PLAIN, { recursive: true });
  fs.writeFileSync(path.join(PLAIN, "readme.txt"), "not a git repo\n");
  git("init", "-q", "-b", "main");
  git("config", "user.name", "Falcon Fixture");
  git("config", "user.email", "fixture@example.com");
  fs.writeFileSync(path.join(REPO, "README.md"), "# fixture\n\n协议 fixture 用的临时仓库。\n");
  fs.writeFileSync(path.join(REPO, "src/main.rs"), 'fn main() {\n    println!("hello");\n}\n');
  fs.writeFileSync(path.join(REPO, "assets/logo.png"), PNG);
  fs.writeFileSync(path.join(REPO, "assets/blob.bin"), Buffer.from([0, 1, 2, 3, 0, 255, 254, 0, 7]));
  git("add", "-A");
  commitAt("2026-09-01T10:00:00+08:00", "feat: 初始提交");
  git("tag", "v0.1.0");
  fs.appendFileSync(path.join(REPO, "README.md"), "\n第二段说明。\n");
  git("add", "-A");
  commitAt("2026-09-02T10:00:00+08:00", "docs: 补充说明");
  // 一处未提交改动 + 一个未跟踪文件
  fs.writeFileSync(path.join(REPO, "src/main.rs"), 'fn main() {\n    println!("hello, falcon");\n}\n');
  fs.writeFileSync(path.join(REPO, "notes.txt"), "未跟踪的笔记\n");
}

function commitAt(date, message) {
  execFileSync(
    "git",
    ["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "-C", REPO, "commit", "-q", "-m", message],
    {
      env: {
        ...process.env,
        GIT_AUTHOR_NAME: "Falcon Fixture",
        GIT_AUTHOR_EMAIL: "fixture@example.com",
        GIT_COMMITTER_NAME: "Falcon Fixture",
        GIT_COMMITTER_EMAIL: "fixture@example.com",
        GIT_AUTHOR_DATE: date,
        GIT_COMMITTER_DATE: date,
      },
    }
  );
}

/** 把锁定版本的 zellij 拷进数据目录，省掉首次建会话时的下载。 */
function prepareDataDir() {
  fs.mkdirSync(path.join(DATA, "bin"), { recursive: true });
  const versionTs = fs.readFileSync(path.join(ROOT, "packages/server/src/zellij/version.ts"), "utf8");
  const version = /ZELLIJ_VERSION = "([^"]+)"/.exec(versionTs)?.[1];
  const name = `zellij-${version}`;
  const candidates = [
    process.env.FALCON_FIXTURE_ZELLIJ,
    path.join(os.homedir(), ".falcon/bin", name),
    path.join(os.homedir(), ".mojito/bin", name),
  ].filter(Boolean);
  const found = candidates.find((c) => fs.existsSync(c));
  if (!found) {
    console.warn(`没找到现成的 ${name}，服务端会自己下载（慢）`);
    return;
  }
  const dest = path.join(DATA, "bin", name);
  fs.copyFileSync(found, dest, fs.constants.COPYFILE_FICLONE);
  fs.chmodSync(dest, 0o755);
}

function startServer(port) {
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([k]) => !k.startsWith("FALCON_") && !k.startsWith("MOJITO_"))
  );
  env.LANG ??= "en_US.UTF-8";
  const log = fs.openSync(path.join(DATA, "server.log"), "a");
  const args = ["--host", "127.0.0.1", "--port", String(port), "--data-dir", DATA];
  const child = SERVER_BIN
    ? spawn(SERVER_BIN, args, { env, stdio: ["ignore", log, log] })
    : spawn(process.execPath, [SERVER, ...args], { env, stdio: ["ignore", log, log] });
  child.on("exit", (code, signal) => {
    if (server === child) console.error(`服务端提前退出：code=${code} signal=${signal}，日志在 ${DATA}/server.log`);
  });
  return child;
}

async function waitReady() {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${base}/api/auth/status`);
      if (res.ok) return;
    } catch {
      // 还没起来
    }
    await sleep(200);
  }
  throw new Error(`服务端 20s 没起来，日志在 ${DATA}/server.log`);
}

async function stopServer() {
  const child = server;
  server = null;
  if (!child || child.exitCode != null) return;
  const exited = new Promise((resolve) => child.once("exit", resolve));
  child.kill("SIGTERM");
  await Promise.race([exited, sleep(5000)]);
  if (child.exitCode == null) child.kill("SIGKILL");
}

function cleanupDirs() {
  // 中途失败时会话没来得及 Terminate，zellij server 是独立进程、不随后端退出：
  // 按二进制路径把数据目录里起的那些收掉（只匹配我们自己的数据目录）
  try {
    execFileSync("pkill", ["-f", `${DATA}/bin/zellij`], { stdio: "ignore" });
  } catch {
    // 没有匹配的进程时 pkill 退出码是 1
  }
  // 只删自己建的：数据目录、临时仓库、非仓库目录、派生在仓库旁边的 worktree
  for (const name of fs.readdirSync(TMP)) {
    if (name === "fal-fx-data" || name === "fal-fx-plain" || name === "fal-fx-repo" || name.startsWith("fal-fx-repo-")) {
      fs.rmSync(path.join(TMP, name), { recursive: true, force: true });
    }
  }
}

function freePort(from, to) {
  return new Promise((resolve, reject) => {
    const tryPort = (p) => {
      if (p > to) return reject(new Error(`${from}–${to} 没有空闲端口`));
      const srv = net.createServer();
      srv.once("error", () => tryPort(p + 1));
      srv.listen(p, "127.0.0.1", () => srv.close(() => resolve(p)));
    };
    tryPort(from);
  });
}

/** 粗略去掉 ANSI 转义：zellij 重绘时会在字符之间插光标移动与 SGR */
function stripAnsi(s) {
  return s.replace(/\x1b\[[0-9;?]*[ -\/]*[@-~]/g, "").replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, "").replace(/\x1b[()][A-Z0-9]/g, "");
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

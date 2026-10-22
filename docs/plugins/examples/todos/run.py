"""falcon 插件示例（docs/plugins.md）：当前项目里的 TODO / FIXME / HACK。

演示：
- scope = project：请求里带着焦点项目（stdin 的 JSON 与 FALCON_PROJECT_* 环境变量），换项目就重画；
- 用 JSON 库产出面板文档：注释原文里什么字符都有，手拼 JSON 迟早拼坏；
- select 参数、数字卡片、列表行（点一下复制 `文件:行号`）、copy 动作；
- 出错 / 不适用时画一条 notice，而不是退出非零——面板照样能看，下一轮照样重画。

插件跑在 falcon **后端本机**：SSH 项目的工作目录在远端，这里读不到，如实说。只用标准库与 git。
"""

import json
import os
import subprocess
import sys

KINDS = ["TODO", "FIXME", "HACK"]
LIMIT = 200


def view(blocks, title=None):
    out = {"blocks": blocks}
    if title:
        out["title"] = title
    json.dump(out, sys.stdout, ensure_ascii=False)


def notice(text, tone="muted"):
    view([{"type": "notice", "tone": tone, "text": text}])


def main():
    req = json.load(sys.stdin)
    project = req.get("project")
    if not project:
        return notice("在侧栏选一个项目。")
    if project["type"] != "local":
        return notice("插件跑在 falcon 后端本机，看不到 SSH 项目的远端目录。", "warning")
    cwd = project.get("workingDir") or ""
    kind = req["params"].get("kind", "all")
    if kind not in KINDS:
        kind = "all"
    pattern = "|".join(KINDS) if kind == "all" else kind
    # 整词匹配用 -w，别写 \b：git grep 的 -E 是 POSIX ERE，macOS 上 \b 不报错、一条都不匹配
    res = subprocess.run(
        ["git", "-C", cwd, "grep", "-n", "-I", "-w", "-E", pattern, "--", "."],
        capture_output=True, text=True, timeout=20,
    )
    # git grep：没匹配是退出码 1，不是错误
    if res.returncode not in (0, 1):
        return notice(res.stderr.strip() or "git grep 失败（不是 git 仓库？）", "danger")
    hits = []
    for line in res.stdout.splitlines():
        path, lineno, text = (line.split(":", 2) + ["", ""])[:3]
        found = next((k for k in KINDS if k in text), "TODO")
        hits.append((found, path, lineno, text.strip()))
    counts = {k: sum(1 for h in hits if h[0] == k) for k in KINDS}
    blocks = [
        {"type": "select", "param": "kind", "value": kind,
         "options": [{"value": "all", "label": "全部"}] + [{"value": k, "label": k} for k in KINDS]},
        {"type": "stats", "items": [
            {"label": k, "value": counts[k], "tone": "warning" if k != "TODO" and counts[k] else None}
            for k in KINDS
        ]},
    ]
    if not hits:
        blocks.append({"type": "notice", "tone": "success", "text": "一条都没有。"})
    else:
        blocks.append({"type": "list", "items": [
            {
                "title": text[:120],
                "detail": f"{path}:{lineno}",
                "badge": found,
                "tone": "warning" if found != "TODO" else "muted",
                "action": {"kind": "copy", "text": f"{path}:{lineno}"},
            }
            for found, path, lineno, text in hits[:LIMIT]
        ]})
        if len(hits) > LIMIT:
            blocks.append({"type": "text", "tone": "muted", "text": f"只列出前 {LIMIT} 条，共 {len(hits)} 条。"})
        blocks.append({"type": "actions", "items": [
            {"label": "复制全部", "icon": "copy",
             "action": {"kind": "copy", "text": "\n".join(f"{p}:{n}  {t}" for _, p, n, t in hits)}},
        ]})
    view(blocks, title=f"{project['name']} · {len(hits)} 条")


if __name__ == "__main__":
    try:
        main()
    except Exception as e:  # noqa: BLE001 — 插件里什么错都该画出来
        notice(f"出错了：{e}", "danger")

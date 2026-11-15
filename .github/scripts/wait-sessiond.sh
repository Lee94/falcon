#!/usr/bin/env bash
# 在同一次 workflow 运行里等 sessiond-build 各组上传守护进程产物：
#   bash .github/scripts/wait-sessiond.sh <产物名>...
#
# 客户端 / 服务端 / 测试 job 不再 `needs` 守护进程（那样要等它整组跑完才能开始编依赖），
# 而是一开始就开工、先用占位文件编完整个工作区，到真正要内嵌 / 运行守护进程时才在这里等。
# upload-artifact v4 上传完成即可下载；产物在 sessiond-build 的检查与测试步骤之后才上传，
# 等到了就说明那一组已通过。任一 sessiond-build 失败 / 取消立即退出，最多等 60 分钟。
# 需要 GH_TOKEN 与 workflow 的 `actions: read` 权限。
#
# 只用 bash 3.2 也有的语法（macOS runner 上的 bash 可能是系统自带的 3.2）；不开 set -u：
# 3.2 在 set -u 下引用空数组会报 unbound variable。
set -o pipefail

if [ "$#" -eq 0 ]; then
  echo "用法：$0 <产物名>..." >&2
  exit 2
fi

run="repos/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
deadline=$((SECONDS + 3600))
while true; do
  # 接口偶发失败时当作还没就绪，下一轮再查，不让整个 job 因一次网络抖动失败
  names=$(gh api "${run}/artifacts?per_page=100" --jq '.artifacts[].name' 2>/dev/null) || names=""
  missing=()
  for want in "$@"; do
    printf '%s\n' "$names" | grep -qxF -- "$want" || missing+=("$want")
  done
  if [ "${#missing[@]}" -eq 0 ]; then
    echo "守护进程产物已就绪：$*"
    exit 0
  fi

  failed=$(gh api "${run}/attempts/${GITHUB_RUN_ATTEMPT}/jobs?per_page=100" --jq \
    '.jobs[] | select(.name | startswith("sessiond-build")) | select(.conclusion == "failure" or .conclusion == "cancelled" or .conclusion == "timed_out") | .name' \
    2>/dev/null) || failed=""
  if [ -n "$failed" ]; then
    echo "::error::守护进程构建未通过：$(printf '%s' "$failed" | tr '\n' ' ')"
    exit 1
  fi
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "::error::等待守护进程产物超时：${missing[*]}"
    exit 1
  fi
  echo "等待守护进程产物：${missing[*]}"
  sleep 15
done

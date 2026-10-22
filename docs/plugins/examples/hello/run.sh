#!/bin/sh
# falcon 插件示例（docs/plugins.md）：一个计数器。
#
# 演示：select 块的参数（FALCON_PARAM_STEP）、invoke 动作（FALCON_PLUGIN_ACTION）、动作回应里的
# toast、带 confirm 的危险按钮、用 FALCON_PLUGIN_DATA 存状态、读焦点项目（scope = project）。
#
# 为了不依赖 jq，这里用 printf 拼 JSON——只拼自己控制的值。项目名这类外来字符串可能带引号，
# 真插件请用有 JSON 库的语言（见 ../ports/run.py）。
set -eu

count_file="$FALCON_PLUGIN_DATA/count"
count=$(cat "$count_file" 2>/dev/null || echo 0)
case "${FALCON_PARAM_STEP:-1}" in
  1|5|10) step="${FALCON_PARAM_STEP:-1}" ;;
  *) step=1 ;;
esac

toast=null
if [ "$FALCON_PLUGIN_EVENT" = action ]; then
  case "$FALCON_PLUGIN_ACTION" in
    add) count=$((count + step)); toast="{\"tone\":\"success\",\"title\":\"加了 $step\"}" ;;
    reset) count=0; toast='{"tone":"warning","title":"清零了"}' ;;
  esac
  echo "$count" > "$count_file"
fi

if [ -n "${FALCON_PROJECT_ID:-}" ]; then project="有（${FALCON_PROJECT_TYPE}）"; else project="没有"; fi

cat <<EOF
{
  "blocks": [
    {"type": "select", "param": "step", "label": "步长", "value": "$step",
     "options": [{"value": "1", "label": "1"}, {"value": "5", "label": "5"}, {"value": "10", "label": "10"}]},
    {"type": "stats", "items": [
      {"label": "计数", "value": $count, "tone": "accent"},
      {"label": "焦点项目", "value": "$project"}
    ]},
    {"type": "actions", "items": [
      {"label": "加一步", "icon": "plus", "tone": "accent", "action": {"kind": "invoke", "id": "add"}},
      {"label": "清零", "icon": "rotate-ccw", "tone": "danger",
       "action": {"kind": "invoke", "id": "reset", "confirm": "计数清零？"}}
    ]}
  ],
  "toast": $toast
}
EOF

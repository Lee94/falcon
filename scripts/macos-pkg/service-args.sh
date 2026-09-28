# 已安装服务的启动参数：postinstall 与 launcher.sh 共用，打包时放进
# Falcon.app/Contents/Resources，两边都 source 它。两边都是 macOS 自带的 bash 3.2，
# 一边开着 set -euo pipefail、一边只开 set -u，这里的写法两种都得扛得住（空数组
# 不能直接展开、不能有会失败的裸语句）。
#
# 装过服务的话，重新 service install 必须带上它原来的 --host / --port / --data-dir
# （从 LaunchAgent plist 的 ProgramArguments 里读）：不带参数就写默认配置
# （127.0.0.1:4923、默认数据目录），装过自定义端口 / 数据目录的人一升级，服务就
# 换了个家，会话全看不见了。口径同原生客户端 native/crates/falcon-core/src/service_args.rs：
# 只认这三个开关，后出现的覆盖先出现的，端口不是合法数字当没写；本机基址也照那边的
# local_url 算。

# read_service_args <plist>：设置 SERVICE_HOST / SERVICE_PORT / SERVICE_DATA_DIR
# （没写为空）与数组 SERVICE_ARGS（重新 install 要带的参数）。plist 不存在 = 没装过，全空。
# 展开 SERVICE_ARGS 一律写 ${SERVICE_ARGS[@]+"${SERVICE_ARGS[@]}"}（bash 3.2 + set -u）
read_service_args() {
  local plist=$1 argv=() v i=0 j=0 n val
  SERVICE_HOST=
  SERVICE_PORT=
  SERVICE_DATA_DIR=
  SERVICE_ARGS=()
  if [ ! -f "$plist" ]; then return 0; fi
  while v=$(/usr/libexec/PlistBuddy -c "Print :ProgramArguments:$i" "$plist" 2>/dev/null); do
    argv+=("$v")
    i=$((i + 1))
  done
  n=${#argv[@]}
  while [ "$j" -lt "$n" ]; do
    val=
    if [ $((j + 1)) -lt "$n" ]; then val=${argv[j + 1]}; fi
    case "${argv[j]}" in
      --host) SERVICE_HOST=$val; j=$((j + 1)) ;;
      --port) SERVICE_PORT=$val; j=$((j + 1)) ;;
      --data-dir) SERVICE_DATA_DIR=$val; j=$((j + 1)) ;;
    esac
    j=$((j + 1))
  done
  case "$SERVICE_PORT" in
    '' | *[!0-9]*) SERVICE_PORT= ;;
    *) if ! [ "$SERVICE_PORT" -le 65535 ] 2>/dev/null; then SERVICE_PORT=; fi ;;
  esac
  if [ -n "$SERVICE_HOST" ]; then SERVICE_ARGS+=(--host "$SERVICE_HOST"); fi
  if [ -n "$SERVICE_PORT" ]; then SERVICE_ARGS+=(--port "$SERVICE_PORT"); fi
  if [ -n "$SERVICE_DATA_DIR" ]; then SERVICE_ARGS+=(--data-dir "$SERVICE_DATA_DIR"); fi
  return 0
}

# service_local_url：本机连它用的基址（先 read_service_args）。监听通配地址或没写时
# 连回环；IPv6 字面量加方括号；端口没写是服务端默认的 4923
service_local_url() {
  local host=$SERVICE_HOST
  case "$host" in
    '' | 0.0.0.0 | :: | '[::]') host=127.0.0.1 ;;
    '['*) ;;
    *:*) host="[$host]" ;;
  esac
  printf 'http://%s:%s' "$host" "${SERVICE_PORT:-4923}"
}

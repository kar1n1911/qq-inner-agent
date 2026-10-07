#!/usr/bin/env bash
# 启动前等 OneBot 的 WebSocket 端口就绪,避免开机时 agent 比 NapCat 先起、连接被拒,
# 造成一串 websocket_error 重试(以及可能错过启动窗口的消息)。
#
# 只在"等得到"时提前返回;超时也退出 0,让 agent 照常启动(它自身有重连逻辑)。
# 端口可用环境变量 ONEBOT_PORT 覆盖,默认 3001;总等待秒数用 WAIT_SECONDS,默认 120。
set -u

host=127.0.0.1
port="${ONEBOT_PORT:-3001}"
limit="${WAIT_SECONDS:-120}"
waited=0

while [ "$waited" -lt "$limit" ]; do
  if (exec 3<>"/dev/tcp/$host/$port") 2>/dev/null; then
    exit 0
  fi
  sleep 2
  waited=$((waited + 2))
done

exit 0

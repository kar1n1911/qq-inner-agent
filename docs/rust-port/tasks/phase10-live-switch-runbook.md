# P10 实况切换执行手册(可逆)

## 前置状态(已验证)

- 远端 `100.114.145.17`,`~/github_repo/qq-inner-agent`;
- Rust 二进制:`rust/target/release/qq-inner-core`(`selftest ok`、`config` summary 正常);
- 当前线上:Node `main.mjs`(pid 2325)已连 OneBot WS `127.0.0.1:3001`;`dashboard.mjs`(pid 2326)读 `status.json`;
- SnowLuma 的 OneBot WS **只接一个客户端**,故切换必须先停 Node。

## 切换步骤(停 Node → 起 Rust)

```bash
cd ~/github_repo/qq-inner-agent

# 1) 停 Node 引擎(保留 dashboard)
flock -n .agent.lock -c true 2>/dev/null  # 仅确认锁存在
# 实际停止:kill 掉 main.mjs 进程(由 flock 包裹)
pkill -f '\.runtime/node src/main\.mjs'   # 停引擎
# dashboard.mjs 不动(继续读 status.json)

# 2) 起 Rust 内核(后台,setsid 脱离 SSH)
export PATH="$PWD/.runtime:$PATH"   # 仅当需要 node 时
setsid nohup rust/target/release/qq-inner-core start --root "$PWD" \
  >> data/rust-core.log 2>&1 < /dev/null &

# 3) 验证连接
sleep 6
python3 -c "import json;d=json.load(open('data/status.json'));print('onebotConnected:',d.get('onebotConnected'),'selfId:',d.get('selfId'))"
```

预期:`onebotConnected: true`、`selfId: "3879337324"`。

## 真实往返验证

主账号 `1950202917` 私聊 bot `3879337324` 发一句;查:

```bash
python3 - <<'PY'
import sqlite3, json
db = sqlite3.connect('data/agent.sqlite')
for chat,sender,text,ts,self in db.execute("SELECT chat,sender,text,ts,self FROM messages WHERE chat LIKE 'private:%' ORDER BY ts DESC LIMIT 4"):
    print(('BOT' if self else f'user {sender}'), ':', text[:80])
PY
tail -30 data/rust-core.log   # 看 decision / send / 错误
```

## 回滚(停 Rust → 起 Node)

```bash
cd ~/github_repo/qq-inner-agent
pkill -f 'qq-inner-core start'            # 停 Rust 内核
# 重启 Node 引擎(原启动方式,由 flock 守护)
setsid nohup flock -n .agent.lock .runtime/node src/main.mjs \
  >> data/main.log 2>&1 < /dev/null &
```

## 判定

- Rust 能连 OneBot 且主账号私聊得到回复 → P10 通过,进入 P11(测试重写);
- 连不上/报错 → 按回滚步骤回退,读 `data/rust-core.log` 定位。

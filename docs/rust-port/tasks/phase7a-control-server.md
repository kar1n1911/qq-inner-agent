# P7a 控制套接字服务端(Rust)

你是 qq-inner-agent 的 Rust 移植工程师。工作区是隔离的 git worktree。

## 定位

新功能,**只做 Rust**(控制协议没有 JS 实现,是 Rust 内核与 Node 仪表盘之间的新接口)。

协议权威定义见 `docs/rust-port/ARCHITECTURE.md` **第 5 节**,不可变更契约见第 6.1 节。

## 必读

- `docs/rust-port/ARCHITECTURE.md` 第 5、6.1 节;
- `docs/rust-port/SURVEY.md` 第 5 节(仪表盘全部路由与 `snapshot()` 字段);
- `src/dashboard.mjs`(Node 侧要读的数据形状);
- 已合并的 `rust/src/engine.rs`(它产出 `status.json` 与各类日志)。

## 范围(只做 `control.rs`)

1. 监听 `<dataDir>/control.sock`(目录已 0700);启动时若存在旧 socket 文件**先清理**;
2. **NDJSON 分帧**,单行上限 1 MiB;
   - 请求 `{"id","method","params"}` → 成功 `{"id","ok":true,"result"}` / 失败 `{"id","ok":false,"error":{"code","message"}}`;
   - 事件 `{"event","data"}`;
3. 实现 ARCHITECTURE 第 5 节的方法表:`state.get`、`models.list`、`test.model`、`contacts.list`、
   `debug.send`、`debug.receive.start|status|stop`、`learning.reset`、`learning.list`、`logs.tail`;
4. 事件:`onebot`、`decision`、`send_assessment`、`message_sent`、`chat_learning_updated`、
   `config_applied`、`cycle_error` —— **字段与现有 `log()` 一致**;
5. **多客户端**:仪表盘与 CLI 可同时连接;**慢客户端不得阻塞引擎** ——
   每连接一个写任务 + 有界队列,队列满时**丢弃事件**(不是断开);
6. 引擎停止时**干净关闭 socket 文件**。

## 硬性约束

1. 不得引入新依赖(Unix socket 用 `tokio::net::UnixListener`,已在依赖里);
2. **不要**改 `config.json`/`secrets.json` 的写路径(那仍是 Node 侧职责,含恢复日志与 `.settings-write`);
3. **不要**做 Node 侧客户端 / 仪表盘改造(那是 P7b);
4. 中文注释,重点标注:分帧边界处理、慢客户端背压策略、断线与重连、socket 文件生命周期。

## 验收

1. `cargo build --release`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 全过;
2. 测试(用 `UnixListener` 本地回环,不联网、不写真实 `data/`):
   - 分帧:半包 / 粘包 / 超长行;
   - `id` 关联、未知方法、非法 JSON;
   - 多客户端并发;
   - 慢客户端不阻塞其他客户端;
   - socket 文件在启动前清理、停止时移除;
3. 测试强度按 `docs/prompt-and-learning-design.md` 第十八节:不变量精确,启发式用区间/方向。

## 交付

1. `git add -A && git commit`,英文。
2. 一段话回报:实现了哪些方法、分帧/背压怎么测的、与设计的偏差或不确定点。

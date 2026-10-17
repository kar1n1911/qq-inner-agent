# AGENTS.md — 本仓库的工作方法

> 这份文件记录**实际在用**的工作方式、环境位置、协作协议和已经踩过的坑。
> 任何人（包括 agent）接手这个仓库，**先读这份**，再读 `TODO.md` 和 `docs/DEVELOPMENT.md`。
> 最后更新：2026-10-17。

---

## 0. 一句话概括

Rust 内核（`rust/`，crate `qq-inner-core`）+ Node 看板（`src/dashboard.mjs`）+ 静态 WebUI（`web/`）+ 插件（`plugins/`）。
**开发在 VM 上，通过远程 codex agent 执行，外层只做裁决、收集与验证。**

---

## 1. 环境：服务器在哪、怎么连

| 项 | 值 |
| --- | --- |
| **VM 地址** | `100.114.145.17`（Tailscale） |
| **登录用户** | `kar1n1911` |
| **仓库路径（VM）** | `~/github_repo/qq-inner-agent` |
| **本地 clone** | `~/github_repos/qq-inner-agent`（macOS） |
| **远程** | `origin` = GitHub `kar1n1911/qq-inner-agent`，主分支 `main` |
| **时区** | `Europe/Stockholm`（看日志时间时注意） |

### 常驻服务（systemd `--user`）

```bash
systemctl --user status qq-inner-agent      # 机器人内核
systemctl --user status napcat              # QQ 协议端
systemctl --user status qq-inner-dashboard  # 看板 + WebUI
```

**⚠️ 重启服务是用户的操作，agent 不要自行重启**（会导致 QQ 掉线、参与状态变化）。

### 端口与入口

| 用途 | 地址 |
| --- | --- |
| NapCat WebSocket | `127.0.0.1:3001`（`reportSelfMessage=true`） |
| NapCat HTTP API | `127.0.0.1:3000` |
| 看板 `/api/*` | 由 `qq-inner-dashboard` 提供 |
| **NapCat WebUI** | `http://100.114.145.17:6099/webui?token=<见部署配置>` |

**⚠️ token 不写进仓库**（本文件也不写）。需要时问用户。
**🚫 绝对不要给用户发二维码图片** —— 只给 WebUI 的 URL + token，这是硬规则。

### QQ 身份与群

- 账号 `3879337324`（Kar1n1911）
- 群：`1127261371`（两人测试群）、`663614559`（ZJUTLUG）、`65840633`（霞之丘）
- ⚠️ **"至少惠及三人"这类研究要求在两人群里无法验证**

---

## 2. 与 codex 协作：三个 agent、三条会话

### 角色分离（**不要合并**）

| Agent | 会话 ID | 职责 | 禁止 |
| --- | --- | --- | --- |
| **Agent 1** | `01a0ed80-82b3-7242-9984-b37181533d06` | 代码 + **单元测试** | 改契约测试的断言 |
| **Agent 2** | `01a139d5-a001-7eb2-894e-a9f7550b3193` | **目标性 / 契约测试** | 读实现来迎合它 |
| **Agent 3** | `01a139e1-db41-7e11-b6e6-fadd136de604` | 审计（只读）+ 文档整理（可写 `docs/`） | 改代码 |

### 三步走（每个功能都这样）

```
① 规划（不写代码）        → 方案 + 取舍 + 验收标准 + "不做什么"
② 目标性测试（必须红）    → 先红，报告失败输出
③ 实现到绿               → 不改断言；冲突就停下报告
```

**⚠️ 永远不要把测试改成符合实现。** 断言冲突时：
- **机械的（夹具/快照字段）** → **外层（我）修**，并自证"`assert` 行未被增删"；
- **需要重新设计场景的** → **测试作者（Agent 2）修**；
- **语义上有分歧的** → **停下，由用户裁决**。

### 派发套路（**照抄即可**）

```bash
# 1) 任务书写本地文件
cat > /tmp/task-xxx.md <<'EOF'
...（见下方模板）
EOF

# 2) base64 传过去（避免中文/引号问题）
base64 -i /tmp/task-xxx.md | ssh 100.114.145.17 "base64 -d > /tmp/task-xxx.md"

# 3) 派发脚本：先杀 writer，再 resume
cat > /tmp/go-xxx.sh <<'SH'
#!/bin/bash
export PATH="$(ls -d ~/.nvm/versions/node/*/bin 2>/dev/null | head -1):$PATH"
cd ~/github_repo/qq-inner-agent
for p in $(ps -eo pid,args | grep -E "codex.*(app-server|proxy)" | grep -v grep | awk '{print $1}'); do kill "$p" 2>/dev/null; done
sleep 2
git pull --rebase origin main 2>&1 | tail -1
SID1=01a0ed80-82b3-7242-9984-b37181533d06
rm -f /tmp/c-result-xxx.txt
nohup codex exec resume "$SID1" \
  -c 'sandbox_mode="danger-full-access"' \
  -c 'approval_policy="never"' \
  -o /tmp/c-result-xxx.txt "$(cat /tmp/task-xxx.md)" \
  > /tmp/c-run-xxx.log 2>&1 &
echo "pid: $!"
SH

base64 -i /tmp/go-xxx.sh | ssh 100.114.145.17 "base64 -d > /tmp/go-xxx.sh && bash /tmp/go-xxx.sh"
```

**关键点**

- **必须** `-c 'sandbox_mode="danger-full-access"'` —— 否则沙箱会拒写，表现为莫名其妙的失败；
- **必须** `-c 'approval_policy="never"'` —— 没人会点确认；
- **用 `-o` 的结果文件判断完成**，不要靠 `pgrep`（会自匹配到 ssh 命令本身）；
- **同一会话不能有两个 writer**：会报 `thread ... already has an active writer`。
  需要并行时：**文件不重叠** + 第二个任务用**新会话**（`codex exec` **不加** `resume`）。

### 判断远程 agent 是"在跑"还是"卡住"

```bash
stat -c %y /tmp/c-run-xxx.log   # 日志最后修改时间
ps -eo pid,etime,pcpu,comm | grep codex   # CPU
```

| 现象 | 结论 |
| --- | --- |
| 日志在长 + CPU > 0 | 正常工作 |
| 日志几十分钟不动 + CPU 0.0% + 无报错 | **卡住** |
| `Reconnecting` / `at capacity` / `hit your usage limit` | **在等**（不是卡死） |

**⚠️ 额度耗尽的信息在日志末尾，但巨长日志里容易被 `tail` 截掉** ——
**永远 grep 整个日志**：

```bash
grep -icE "usage limit|at capacity" /tmp/c-run-xxx.log
```

### 任务书模板（要点）

```markdown
**目标一句话**

## 现状（已实测，给数字/行号）
## 你要做的（编号）
## ⚠️ 三条硬约束（不许改断言 / 不许改实现 / 冲突就停）
## 任务边界
- 允许：<精确文件列表>
- 禁止：<精确文件列表>
## 收尾（跑哪些测试、提交要求）
## 回报（逐条问，含"有没有必须改契约的地方"、"需用户决定的"）
```

**经验**：任务书里**必须**写"若发现超出范围 → **停下来说明**"，
否则 agent 会自作主张扩大改动。**它每次停下都暴露了一个真实的设计缺口。**

---

## 3. 构建与测试

```bash
# Rust
cd rust
cargo build
cargo test -- --test-threads=1        # ⚠️ 必须串行
cargo clippy --all-targets -- -D warnings   # 必须零警告

# Node / WebUI
node --test test/*.test.mjs
```

### ⚠️ 已知的测试陷阱

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| 多个测试并行时随机失败 | **并行执行 + 资源竞争** | 永远 `--test-threads=1` |
| `onebot_mock` 之类偶发 | 并发/负载敏感 | 同上；已修两处根因 |
| `a_memorix` gRPC 失败（`service unavailable`） | **`a-memorix` 服务未运行**（`systemctl --user is-active a-memorix` → `inactive`） | **环境问题**，不是代码问题；不要因此改代码 |
| 全量跑出 1 条无关失败 | 已知偶发 | 单跑确认；若稳定复现再查 |

**验证偶发失败的方法（有效）**：
**主动构造竞态**，而不是"多跑几次看会不会出现"。
例如控制连接：**先注入一个 `onebot` 事件帧，再发响应**，断言仍拿到正确响应。

---

## 4. Git 约定

```bash
git pull --rebase origin main     # ✅ 用它
git pull --ff-only origin main    # ❌ 会因分叉失败
```

- **⚠️ 不要 `git add -A`** —— 会扫进别人（或并行任务）的半成品。
  **用显式路径**：`git add path/a path/b`。
- **收集后必须 `git push`**，并且确认：

```bash
git log --oneline origin/main..HEAD    # 必须是空
git status --porcelain                 # 必须是空
```

- **一次提交一个逻辑**；**英文提交信息**。
- 并行任务共用一个工作区时：**提交前检查 `git show --stat HEAD`**，
  确认没有把别人的在途文件扫进去。

---

## 5. 设计原则（这个仓库反复强调的）

### 5.1 声明 ≠ 实测

**manifest / capabilities 里写的是【声明】**，页面必须**分栏显示声明与实测**。
**不得**把声明当实测能力。

```rust
// 反例：把 manifest 内容覆写成"运行证据"
status["capabilities"] = manifest.capabilities;   // ❌
```

### 5.2 不得伪装

- 骨架 → 必须标 `status: "skeleton"`，**系统要认识这个状态并拒绝启用**；
- 版本号 → 不许拿仓库版本冒充插件版本（现在统一 `0.1.0`，并在 `plugins/README.md` 说明含义）；
- 能力 → **实现之后**才把 `media` 从 `unsupported` 移到 `supported`，**顺序不能反**。

### 5.3 失败必须可见

**静默降级 = "看起来完成了但不生效"**。任何 fallback / 降级都要留下**可查询的痕迹**（带群、带原因、限频去重）。

### 5.4 `Unsupported` ≠ `Failed`

```
Unsupported  = "这个端口不做这件事"   → 正常，不是故障
Failed       = "试过了但失败"          → 故障，要查
```

### 5.5 出站的 `uncertain`

```
离线 / 未连接（action 之前）  →  Failed { uncertain: false }   确定没发出去
超时 / 断连（action 之后）    →  Failed { uncertain: true  }   可能已发出
```
**群聊里"发出去了"是撤不回的**，所以这条不能糊。

### 5.6 双路径是误用源

"有插件走插件、没有就退回内置"这种 fallback，**必须先验证、再收敛**。
删除前要确认：能力已被替代、绑定时机已解决、状态来源已切换。

### 5.7 信任模型

**声明优先**，不做机械拦截；**签名是信息，不是门禁**；
`host_permissions: []` **不得**用来阻止执行。

---

## 6. 用户偏好（硬规则，别问、别试）

1. **🚫 绝不发二维码图片** —— 只给 WebUI URL + token；
2. **模型选择是用户的**（VM 上 `model = "gpt-6-sol"`，`reasoning_effort = "medium"`）——
   **不要自行跑 `/model` 或改配置**；
3. **宿主侧 / sudo / 重启服务由用户执行**；
4. **审批已禁用**：不要请求沙箱升级，直接带 `danger-full-access` 派发；
5. **验证而非推测** —— 说"我看了"之前先 `grep -c` / `sed -n` 拿到证据；
6. **不要为了让我高兴而说"完成了"** —— 有缺口就说缺口，有折扣就说折扣。

---

## 7. 文档地图

| 文件 | 内容 |
| --- | --- |
| `TODO.md` | **台账**（唯一权威的待办与状态）。分主题编号：1 输入输出 / 2 行为与决策 / 3 端口与插件 / 4 框架与可视化 / 5 更新与部署 / 6 学习与记忆 / 7 界面 / 8 流程与规范 / 9 偶发失败 |
| `docs/DEVELOPMENT.md` | 三步走、角色分离、测试分工、git 约定 |
| `docs/working/topic-literature.md` | 群聊机器人设计文献索引（35 项，含用途/边界/术语） |
| `docs/working/topic-2plugins-plan.md` | 主动话题两插件（选择器 + 前置背景判断）边界与接口 |
| `docs/working/topic-gen-select-plan-v2.md` | 话题生成与选择（含 12:03 事故的可测判据） |
| `docs/working/outbound-closure.md` | 出站三条证据路径、时间来源、入站 `reply` 实测形态 |
| `docs/working/behavior-plugin-design.md` | 行为插件、逐步升级+EOL、声明优先、签名定位 |
| `docs/working/remaining-plan.md` | **⚠️ 2026-10-14 的历史排期快照，不要当现状用** |

### 代码地图

```
rust/src/engine/io/      ← 【契约层】types / ports / registry / router / plugin_list / factory / adapters
plugins/                 ← 【实现层】onebot / a_memorix / openai / claude / ollama / sillytavern / null
rust/src/engine/         ← 引擎（mod / policy / decision / targeting / backlog / orientation ...）
rust/src/transport/      ← 内置 OneBot WebSocket 客户端（正在搬迁进 plugins/onebot）
rust/src/store/          ← SQLite 访问层（含 .sql）
web/ + src/dashboard.mjs ← WebUI 与 API
rust/tests/*_contract.rs ← 契约测试（Agent 2 写）
test/*.test.mjs          ← Node 侧契约测试
```

---

## 8. 故障排查表

| 症状 | 原因 | 处理 |
| --- | --- | --- |
| `thread ... already has an active writer` | 同一会话两个进程 | `pkill` app-server / proxy 后再派 |
| 沙箱拒写（`denied`） | 没带 `danger-full-access` | 派发时加 `-c 'sandbox_mode="danger-full-access"'` |
| ssh 超时 / `Operation timed out` | 偶发网络 | **重试**；`ping` + `nc -z 端口` 确认仍在 |
| 日志几十分钟不动、CPU 0% | 卡住 | 杀掉重派，并检查额度 |
| `git pull` 报 "Please commit or stash" | 有并行任务的在途改动 | 正常；用显式路径提交，或等它提交 |
| 全量 Rust 只有 1 条无关失败 | 已知偶发 / 服务未运行 | 单跑确认；`a-memorix` 看 `is-active` |
| 任务书里 agent 停下没输出改动 | **它在守边界**（正常） | 读报告，通常暴露了真实设计缺口 |

---

## 9. 当前进度（2026-10-17）

```
B 线 根因收尾      ✅ T+★ · R · P
D 线 缺陷验证      ✅ AA2 · S · 控制响应首帧修复
A 线 插件系统      ✅ 工厂 · AB3(识别/暂存/启用) · AB2(状态/能力/密钥/自检)
C 线 独立功能      ✅ Q(多气泡语义去重) ／ O(评论图片) 后移到插件系统之后
插件清单           ✅ 七份 manifest 如实 + skeleton 状态 + plugins.json
插件启用生效性     ✅ 5/5（含 fallback 可观测）
ChatPort 媒体发送  ✅ 8/8（SendMedia / Segment::Image / uncertain 语义）
搬迁 WS 进插件     🔄 进行中（删除内置连接的前提）
```

**三条"看起来完成了但不生效"（已定位，待处理）**

1. `MemoryPort` 可注册，但**内核记忆读写仍走 `LayeredMemory`**（`main.rs` / `engine/mod.rs` 里 `MemoryPort` 出现 0 次）；
2. **话题学习**读写闭环存在，但 `learning.enabled` 默认 `false`，`config.json` 未覆盖；
3. 插件发现/页面可用，但**仓库自带插件是否进入运行代**依赖 manifest 与 `plugins.json`（已修，但仍需实机验证）。

---

## 10. 给下一个 agent 的三条忠告

1. **先读 `TODO.md` 的对应章节**，再动手 —— 那里记着为什么这么设计，以及**试过但失败了的路**；
2. **看到怪现象先怀疑"我读的代码不是正在跑的代码"** —— 用 `grep -c` 证实，别推断；
3. **宁可停下报告，也不要越界改**。这个仓库里，**每一次 agent 停下都值一次设计评审**。

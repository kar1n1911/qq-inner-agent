# P6i 新话题来源(§21)

设计权威:`docs/prompt-and-learning-design.md` **第二十一节**。

## 定位

全新功能,只做 Rust、开关化默认关闭(不影响已迁移行为)。

## 四块(按依赖顺序)

### 1. forward 段收发(onebot 传输,先做,最自包含)

- **发送侧**:`onebot.rs` 加 `send_forward(chat, nodes)`,发 `{"type":"forward","data":{"nodes":[...]}}`,
  每个 node = `{user_id|uin, id|message_id}`;**只接受已存在消息 id 引用,不构造新内容**;
  约束:node 只在 forward 内合法、必须有 user_id/uin + 合法 id、file 段不支持。
- **接收侧**:`normalize` 遇到 forward 段时,用 `forwardId` 调 `get_forward_msg` 拉出 nodes,
  抽 node 里的文本接入(而不是 `[聊天记录]` 占位符);
- 测试:onebot mock 收发 forward、get_forward_msg 拉取、node 校验、接收侧文本抽取。

### 2. 外部新鲜内容接入

- 配置:来源列表(GitHub topic/关键词、RSS/论坛 URL),默认空 = 关闭;
- 按群的 `traits`(兴趣)+ 高频词元,与外部条目标题/描述/tags 做规范化词元重叠,阈值以上才入选;
- 小时级节流 + 次数/字符预算 + 结果缓存;
- 测试:相关度匹配(阈值/无关不出)、节流、预算、缓存。

### 3. 群间转发

- 低风险(转发现成的聊天记录卡片/链接):正常相关度阈值;
- 高风险(自造合并转发):默认关闭,仅 ownerUin 指令或多证据+高相关度+自我审核强制 keep;
- 去重:短期记忆内没在本群发过才转;
- 测试:低风险转发、高风险门控、去重。

### 4. 责任线 + 自我审核 + 来源可溯源(横切)

- 所有外部/转发内容都要过责任线(§14)+ 自我审核(§16);
- 相关度 ≠ 安全:鼓励违法/未授权无线电的内容一律 drop;
- 带来源、不编造。

## 硬约束

1. 不引入新依赖(抓取用现有 ureq);不改 rust/ 以外文件(除 onebot 契约的 JS 同步);
2. 全部开关化默认关闭;
3. 中文注释,标注:安全闸门比"感兴趣"更靠前。

## 验收

1. build/clippy/test 全过;
2. forward 收发 + get_forward_msg + normalize 抽取各有测试;
3. 相关度匹配/节流/预算/缓存、群间转发低/高风险门控、去重各有测试;
4. 默认关闭时行为与现在逐字一致(parity 照旧全过)。

## 交付

`git add -A && git commit`(英文),一段话回报。

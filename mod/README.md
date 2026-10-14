# mod/ 插件目录与 manifest 规范（草案）

本目录是拟议的插件安装单元。当前文件仅为规范和空骨架；宿主尚未从这里自动发现、授权或加载插件。不要把占位 manifest 当作可运行声明。

## 1. 边界与目录

**契约在 `engine/io/`，实现在 `mod/`。** 精确位置为仓库的 `rust/src/engine/io/`：`types.rs` 定义六面中立类型、`PortReply` 和能力；`ports.rs` 定义六个端口 trait；`registry.rs`、`router.rs` 管注册与路由。插件只实现契约，不在 `mod/` 重定义端口、绕过注册表或直接依赖内核的业务实现。行为插件若未来也放在 `mod/`，须有独立类别与契约，不能伪装成第七个 I/O 端口。

```text
mod/
  README.md
  <plugin-id>/
    manifest.json            # 每个可安装单元一份
    ...                      # 日后实现、协议资源和平台产物
    sidecar/                 # 进程外本机服务的源文件/入口，按需使用
  onebot/ a_memorix/ ollama/ openai/ claude/ null/
```

目录名是稳定的插件 ID；`name` 应与之匹配。一个 manifest 对应一个端口面。现有 `null.rs` 同时实现多个面，因此本骨架的 `null/manifest.json` 仅是未定方案，未来应拆为多个安装单元或明确一目录多 manifest 的规则。

## 2. 如何写与如何发现

1. 选六面之一：`Chat`、`Model`、`Db`、`Memory`、`External`、`Persona`。阅读 `rust/src/engine/io/` 的类型、trait 和响应语义，填一份完整 manifest；按实际支持能力声明，不要把“暂未验证”写成支持。
2. 进程内实现只能用 Rust，接现有 trait。进程外实现可用任何语言，只需实现宿主公布的**版本化线协议**。它不能依赖 Rust SDK、内核私有类型、原始数据库句柄或进程内锁。协议及响应 schema 必须可由宿主校验。
3. 宿主将来只扫描**配置的插件根目录**下一级子目录的 `manifest.json`；读取和校验清单、API 版本、安装来源与入口后列为“已发现”，不因此执行代码。启用时再由用户绑定依赖与配置、由宿主审批权限，并加载受信任的进程内实现、启动 Sidecar 或连接 Remote。失败必须可见，不能静默启用。当前扫描器、入口字段、签名/摘要与隔离机制**尚未实现且待确认**。
4. `Sidecar` 指由宿主管理生命周期的本机进程；`Remote` 指宿主连接现成的外部服务；`InProcess` 指与内核同进程的 Rust 实现。**transport 描述运行拓扑，不描述插件作者使用的语言。** 例如 Rust gRPC 桥接器可连接 Python Remote；Python 进程若改由宿主启动，才属于 Sidecar。
5. 输出以契约规定的 `PortReply` 为准：`Ok(result)`、`Unsupported{capability}`、`Degraded{result,missing}`、`Failed{code,uncertain}`。进程外线协议必须为四种状态定义明确标签及相应字段；不能只靠 HTTP/gRPC 状态码、空对象或空字符串表示失败。宿主验证版本、tag、字段类型、长度/范围与对应端口结果 schema；非法响应按失败处理。schema 文件位置和具体编码待确认。

### 非 Rust 插件示意：A_memorix

```text
mod/a_memorix/
  manifest.json              # 当前 Rust gRPC 桥接器的占位声明
  sidecar/                   # 预留 Python 启动器、依赖锁定和协议资源的位置
    README.md                # 本骨架唯一文件；没有可执行实现
```

A_memorix 本体是 Python 服务，走 gRPC；现有 Rust 代码是 `MemoryPort` 到它的桥接器，manifest 当前标 `Remote`。如以后由宿主启动本地 Python 服务，才将部署形态改为 `Sidecar` 并填写入口、版本、资源限制及权限。不要把 Python 库复制进 Rust 源码，也不要将当前连接方式误称为已实现的 Sidecar。

## 3. manifest.json 字段

这是**目标文件规范**，不是当前 `PortManifest` 的可直接反序列化 JSON。当前 Rust 结构还没有 `version`、`capabilities`、`config_schema` 或入口字段；迁移时需明确映射，不能假装宿主已经支持。占位文件允许字符串 `"TBD"` 并含 `status: "placeholder"`；**正式发布/启用时禁止 TBD**。

| 字段 | 正式要求 | 说明 |
| --- | --- | --- |
| `name` | 必填、非空、安装单元内唯一 | 与目录 ID 一致；与注册实例名如何区分待确认 |
| `version` | 必填、确定的插件版本 | 建议语义版本；具体校验规则待确认 |
| `kind` | 必填、六面之一 | 一个安装单元一个面；多面组合另议 |
| `transport` | 必填：`InProcess` / `Sidecar` / `Remote` | 描述部署拓扑；进程外协议版本另列 |
| `core_api_version` | 必填：`{major,min_minor,max_minor}` | 宿主与端口契约兼容范围；与插件自身 `version` 分开 |
| `protocol` | 进程外必填 | 协议名、schema 版本及可校验 schema 标识；编码和文件路径待确认 |
| `entry` | Sidecar/Remote/可装载进程内插件启用前必填 | 可执行入口或远程连接描述；不可在发现阶段执行；具体格式待确认 |
| `capabilities` | 必填 | `supported`、`unsupported`、`unknown` 显式分列；运行时 `capabilities()` / `PortReply` 可收窄声明，不能静默扩大 |
| `dependencies` | 必填，可为空 | **依赖层**：每项 `{role,needed_for,required}`；说需要何种服务，例如 `ModelPort`，由用户绑定实例、宿主注入。它不授予联网/执行权限 |
| `host_permissions` | 必填，可为空 | **安全层**：`http`（限定 origin）、`spawn`（限定可执行文件）、`read_file`（限定根目录）、`log`（限字节）等请求。宿主按管理员授权独立裁决并逐调用核验目标；**不信任插件自报**，未授予即拒绝 |
| `config_schema` | 必填，可为空 | **配置层**：AB2 按字段 `key`、`label`、`type`、`required`、`secret` 等渲染表单。密钥只写入受保护配置，读取只回“已配置/未配置”，从不回显原值；保存沿用既有增量 patch 路径 |
| `status`、`tbd` | 仅骨架使用 | 说明本文件尚不可加载及未决项；不是启用开关 |

三层互不替代：`dependencies` 决定语义绑定，`host_permissions` 决定宿主服务允许的机械动作，`config_schema` 决定用户可填的值。给出 `ModelPort` 依赖不等于允许直接 HTTP；用户填了 URL 不等于获得 HTTP 授权；插件请求 `http` 也不等于已获批准。AB2 展示 requested/effective 状态与原因；发现、安装、授权、启用各是不同状态。组件可开关，但安全校验、发送闸门、锁序和隐私隔离不可配置成关闭。

### 完整示例（**设计示例，不代表已有实现**）

```json
{
  "name": "example_text_memory",
  "version": "0.1.0",
  "kind": "Memory",
  "transport": "Sidecar",
  "core_api_version": {"major": 1, "min_minor": 0, "max_minor": null},
  "protocol": {"name": "qq-inner-port-grpc", "schema_version": "1.0", "schema": "protocol/memory-v1.json"},
  "entry": {"executable": "sidecar/bin/example-memory", "args": []},
  "capabilities": {
    "supported": ["text_retrieval"],
    "unsupported": ["embeddings", "relation_extraction", "reset_scope"],
    "unknown": []
  },
  "dependencies": [
    {"role": "ModelPort", "needed_for": "optional_summary", "required": false}
  ],
  "host_permissions": [
    {"type": "spawn", "executable": "sidecar/bin/example-memory"},
    {"type": "log", "max_bytes": 4096}
  ],
  "config_schema": [
    {"key": "namespace", "label": "Namespace", "type": "string", "required": true, "secret": false},
    {"key": "access_token", "label": "Access token", "type": "string", "required": false, "secret": true}
  ]
}
```

示例里的 `protocol`/入口只是拟议形状，不表示 `protocol/memory-v1.json` 已存在。若实现不需要某权限，填空数组；不得用宽泛通配目标取代最小授权。端口响应里的成功值仍需满足 `engine/io/` 的中立类型，权限申请不能修改返回 schema。

## 4. 红线与失败

插件不得绕过宿主安全校验、最终输出校验、发送闸门、锁序、隐私隔离或审计边界。出站消息必须回到宿主的 `Command` 路径，由宿主决定是否发送。插件不能持有完整 `Io`、原始 `Store`、发送句柄、其他插件的密钥或非授权聊天数据。外部插件只拿最小必要的脱敏输入；宿主回收结果后重验 schema、权限、会话/版本和最终发送条件。无效结构、超时、权限拒绝、不支持、降级均应有显式状态；不能把错误转换为“成功但没内容”。

宿主 API 授权与进程级沙箱是两层不同控制：`host_permissions` 只限制经宿主调用的服务，不能单凭 manifest 阻止任意外部程序直接访问网络/文件。Sidecar 的进程隔离、远程数据范围、包签名与资源上限还需部署方案确定。

## 5. 当前占位文件与待确认

六份 `manifest.json` 只依据当前适配器显式声明的 kind、transport 和能力填写；未知的版本、能力或权限目标标 `TBD`。特别是 `ollama` 当前 Rust `HttpModel` 的 transport 声明为 `Sidecar`，但它连接的是本机 Ollama HTTP 服务，尚未有宿主启动该进程的实现；这个命名语义需要落地前统一。A_memorix 当前为 `Remote`，其现有 Rust manifest 的 `host_permissions` 为空，而 gRPC 客户端直接拨号；这暴露了宿主级网络授权尚未接线，不能把空数组解释成已完成隔离。模型三家当前共用 `model_http.rs` 和 `model_config.json`，拆目录不意味着已拆实现。

待确认：①真实扫描根、安装包格式、签名/摘要、入口/资源限制和启用事务；②进程外线协议与 schema 的文件格式、版本和发布位置；③ `null` 多面如何拆分；④ `ollama` 的 Sidecar 与本地服务连接如何区分；⑤ A_memorix 何时从 Remote 切换为宿主管理的 Python Sidecar，以及凭据/端点配置；⑥ manifest 中插件 ID 与运行实例名的关系、能力动态刷新和正式启用开关的持久化位置。这些未决项不得用猜测填成可加载值。

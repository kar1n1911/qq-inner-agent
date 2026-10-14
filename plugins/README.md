# plugins/ 插件目录与 manifest 规范（草案）

本目录保存插件的唯一实现源码。当前 Rust 内置适配器由 `rust/src/engine/io/adapters/mod.rs` 的 `#[path]` 声明直接编译这里的文件；宿主尚未从 manifest 自动发现、授权或动态加载插件。不要把占位 manifest 当作可运行声明。

这种静态接线保持 `engine/io/` 为唯一契约位置，也避免核心 crate 与依赖其 trait 的插件 crate 形成循环依赖。相对路径集中在一个模块文件中，移动目录时必须同步更新并以 `cargo build` 验证。独立 crate/进程加载仍是后续架构议题；当前不声称已有运行时插件发现。

方案 B 的边界前提是 **P-5 只放行 `adapters/mod.rs` 中列明的八条固定 `plugins/` 路径，并递归扫描 `plugins/` 的 Rust 源码依赖**；新增或变更路径必须同步更新断言，不能借 `#[path]` 绕过内核边界。X10 引入 JSON 清单时应重新评估这套静态路径与扫描规则。

`rust/src/engine/io/adapters/model_config.json` 是指向 `plugins/model_config.json` 的兼容符号链接，供现有 Node 配置读取路径使用；它没有第二份内容。

## 1. 边界与目录

**契约在 `engine/io/`，实现在 `plugins/`。** 精确位置为仓库的 `rust/src/engine/io/`：`types.rs` 定义六面中立类型、`PortReply` 和能力；`ports.rs` 定义六个端口 trait；`registry.rs`、`router.rs` 管注册与路由。插件只实现契约，不在 `plugins/` 重定义端口、绕过注册表或直接依赖内核的业务实现。行为插件若未来也放在 `plugins/`，须有独立类别与契约，不能伪装成第七个 I/O 端口。

```text
plugins/
  README.md
  <plugin-id>/
    manifest.json            # 每个可安装单元一份
    ...                      # 日后实现、协议资源和平台产物
    sidecar/                 # 进程外本机服务的源文件/入口，按需使用
  onebot/ a_memorix/ ollama/ openai/ claude/ null/ sillytavern/
```

目录名是稳定的插件 ID；`name` 应与之匹配。一个 manifest 对应一个端口面。现有 `null/adapter.rs` 同时实现多个面，因此 `null/manifest.json` 仅是未定方案，未来应拆为多个安装单元或明确一目录多 manifest 的规则。

## 2. 如何写与如何发现

1. 选六面之一：`Chat`、`Model`、`Db`、`Memory`、`External`、`Persona`。阅读 `rust/src/engine/io/` 的类型、trait 和响应语义，填一份完整 manifest；按实际支持能力声明，不要把“暂未验证”写成支持。
2. 进程内实现只能用 Rust，接现有 trait。进程外实现可用任何语言，只需实现宿主公布的**版本化线协议**。它不能依赖 Rust SDK、内核私有类型、原始数据库句柄或进程内锁。协议及响应 schema 必须可由宿主校验。
3. 宿主将来只扫描**配置的插件根目录**下一级子目录的 `manifest.json`；读取和校验清单、API 版本、安装来源与入口后列为“已发现”，不因此执行代码。启用时再由用户绑定依赖与配置、由宿主审批权限，并加载受信任的进程内实现、启动 Sidecar 或连接 Remote。失败必须可见，不能静默启用。当前扫描器、入口字段、签名/摘要与隔离机制**尚未实现且待确认**。
4. `Sidecar` 指由宿主管理生命周期的本机进程；`Remote` 指宿主连接现成的外部服务；`InProcess` 指与内核同进程的 Rust 实现。**transport 描述运行拓扑，不描述插件作者使用的语言。** 例如 Rust gRPC 桥接器可连接 Python Remote；Python 进程若改由宿主启动，才属于 Sidecar。
5. 输出以契约规定的 `PortReply` 为准：`Ok(result)`、`Unsupported{capability}`、`Degraded{result,missing}`、`Failed{code,uncertain}`。进程外线协议必须为四种状态定义明确标签及相应字段；不能只靠 HTTP/gRPC 状态码、空对象或空字符串表示失败。宿主验证版本、tag、字段类型、长度/范围与对应端口结果 schema；非法响应按失败处理。schema 文件位置和具体编码待确认。

### 非 Rust 插件示意：A_memorix

```text
plugins/a_memorix/
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

七份 `manifest.json` 只依据当前适配器显式声明的 kind、transport 和能力填写；未知的版本、能力或权限目标标 `TBD`。特别是 `ollama` 当前 Rust `HttpModel` 的 transport 声明为 `Sidecar`，但它连接的是本机 Ollama HTTP 服务，尚未有宿主启动该进程的实现；这个命名语义需要落地前统一。A_memorix 当前为 `Remote`，其现有 Rust manifest 的 `host_permissions` 为空，而 gRPC 客户端直接拨号；这暴露了宿主级网络授权尚未接线，不能把空数组解释成已完成隔离。模型三家当前共用 `model_http.rs` 和 `model_config.json`，拆目录不意味着已拆实现。

待确认：①真实扫描根、安装包格式、签名/摘要、入口/资源限制和启用事务；②进程外线协议与 schema 的文件格式、版本和发布位置；③ `null` 多面如何拆分；④ `ollama` 的 Sidecar 与本地服务连接如何区分；⑤ A_memorix 何时从 Remote 切换为宿主管理的 Python Sidecar，以及凭据/端点配置；⑥ manifest 中插件 ID 与运行实例名的关系、能力动态刷新和正式启用开关的持久化位置。这些未决项不得用猜测填成可加载值。

## 6. X10 清单与探测

X10 使用**每包一份** `<root>/<directory>/manifest.json` 描述包，另用可选的 **`plugins/plugins.json`** 描述部署意图。`engine/io/plugin_list.rs` 提供发现、原子预检和注入加载器的解析入口；`plugins reconcile` 提供一次性状态查询。`docs/working/behavior-plugin-design.md` §4 示例称 `plugin.json`，但仓库现有七包均为 `manifest.json`；X10 只认后者。只有 `plugin.json` 的目录须以 `manifest_missing` 报告为未识别，不得悄悄忽略或猜测格式。将来若统一命名，要做显式版本迁移。

### 部署清单 v1

```json
{
  "schema_version": 1,
  "mode": "auto",
  "plugins": [
    {
      "id": "qq-main",
      "directory": "onebot",
      "enabled": true,
      "params": {"self_id": "12345"},
      "bindings": {},
      "secret_refs": {}
    },
    {
      "id": "optional-style",
      "directory": "language-tune",
      "enabled": false,
      "params": {"language": "zh-CN"},
      "bindings": {"ModelPort": "main-model"},
      "secret_refs": {"api_key": "secrets.language_tune_key"}
    }
  ]
}
```

这是**结构示例，不是可启用配置**：`onebot` 现有 manifest 含 TBD，`language-tune` 尚不存在。`plugins` 数组顺序是用户请求的顺序；`id` 是实例 ID，`directory` 是包 ID，二者不可混用。`params` 只放可公开的实例参数，按包的 `config_schema` 验证；密钥只放受保护配置中的引用，状态输出只给 `configured: true/false`。`bindings` 以依赖角色指向实例 ID，不授权网络、文件或进程。根目录由宿主配置/CLI 传入并规范化，清单路径不能越出根目录；v1 只扫根目录的**一级真实子目录**，不跟随目录符号链接，也不扫描归档。

`mode=auto`：只读扫描全部一级目录，把每个目录列入发现结果；缺 manifest、坏 JSON、名字与目录不符、版本不兼容、签名/授权/入口不满足或仍含 `TBD` 的包分别给出原因，绝不执行。有效包也只进入 `discovered`，**新包默认关闭**；已有静态基线路径不因扫描而改变。清单中的显式项可启用、禁用或配置某实例；启用须通过依赖/权限/能力预检后才激活。`mode=manual`：仍扫描并报告所有目录，但仅清单列出的实例有激活资格；未列目录记 `not_selected`。两种模式都不能把“发现”当成“已加载”。

兼容输入接受 `schema_version: 1` 或 `version: 1`，输出统一写 `schema_version`。`plugin_list::resolve` 的包状态使用 `missing_manifest` 作为内部原因，CLI 发现视图使用 `manifest_missing`；两者都明确表示目录存在但缺清单。`config_schema` 可以是字段数组或字段映射。扫描不加载插件；解析器只对显式启用项调用宿主提供的加载器。加载器返回实测能力，声明的支持项超出实测时记 `capability_mismatch`，不列为生效。

冲突规则：显式 `enabled=false` 优先于自动发现，状态为 `disabled_by_user`，不得调用入口或注册实例；手动列出不存在目录（即使禁用）时启动前失败，错误含实例 ID、目录与 `plugin_directory_missing`，既有有效 generation 不变；目录存在但缺 manifest 时自动模式列 `unrecognized/manifest_missing`，手动引用则启动前失败并点名。重复实例 ID、重复包名、越根路径、非法顺序或启用的占位包均显式拒绝。数组顺序只决定没有语义依赖时的激活先后；行为 DAG 的固定边和依赖拓扑优先，冲突报 `invalid_order`，不能靠重排绕过校验。任何失败都不得静默退回其他插件或发言路径。

一次性验收入口为 `qq-inner-core plugins reconcile --root <path> --config <plugins.json> --json`。成功时 JSON 至少含 `mode`、`generation`、按目录列出的 `discovery`、按实例列出的 `requested_enabled/effective_state/reason`、`activation_order`、实际注册实例 ID 与脱敏的依赖/权限/配置状态；配置错误时非零退出，结构化错误含 code/id/path。CLI 与 `start` 在 `plugins/plugins.json` 存在时共用 `plugin_list::resolve` 的发现和预检逻辑；缺少该文件时旧运行路径不变。宿主须注入真实加载器并只在全部成功后发布新的 generation。现阶段二者均未绑定实际工厂，显式启用会报 `plugin_load_failed`，不会假装已注册。运行时状态查询、generation 切换和配置增量保存的接线仍待后续工作。

X6：静态声明只能给能力上限，实际 `Unsupported/Degraded` 和健康状态必须收窄 effective 能力；Sidecar/Remote 需版本化线协议、校验和宿主授权。AB2：`dependencies`、`host_permissions`、`config_schema` 三层独立；用户填的密钥不回显，清单不能直接含密钥。X7：`requested_enabled` 与 `effective_state/reason` 分开，禁用依赖实例时先拒绝或显式重绑，默认基线图不变。X8：清单版本、顺序、绑定、请求开关与脱敏状态可序列化，generation/图哈希可核对；无 schema 变更走 `apply`，涉及持久状态迁移走 `full`。安全校验和最终发送闸门不是可关闭插件。

`plugins.json` **不能直接替代 `#[path]`**：JSON 只能选择已经编译进核心的受信任 Rust 工厂，无法在运行时声明新的 Rust 模块；删除静态路径须改为稳定动态 ABI、Sidecar/Remote，或拆出中立契约 crate 以避免核心↔插件循环依赖。收益是独立发布与运行时发现；代价是 ABI/协议版本、隔离、签名、权限和迁移机制。X10 保留固定八路径与 P-5 扫描，X10 完成后按真实插件装载需求再评估，不能把读取 JSON 称为动态加载。

**验收**：无 manifest 目录出现在状态中并写明原因；自动模式能识别有效包但不默认执行新包；显式禁用不执行也不注册，且状态可查；手动引用缺目录/缺 manifest 在任何副作用前以可定位错误拒绝；有效启用实例按合法顺序、参数与绑定激活，状态与实际注册表一致；密钥和未授权权限不泄漏、不生效；保存失败保持旧 generation。当前的两套目标测试覆盖解析器的加载器边界和 CLI 状态入口；新增的 `plugin_startup_contract` 断言正式 `start` 对缺目录清单在建立运行时前拒绝。当前已实现扫描器、解析器与 CLI 状态入口；UI、动态装载、协议 runner 和结构迁移仍不在 X10 范围。现有七份占位 manifest 不可启用；CLI 没有受信任的宿主工厂时对启用请求明确报 `plugin_load_failed`，不会假装已注册。宿主调用 `plugin_list::resolve` 时须提供真实工厂，并只发布全部校验成功的 generation。

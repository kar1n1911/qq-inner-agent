# 预留：A_memorix Python sidecar

此处目前没有入口、依赖包或可执行代码。现有 Rust `MemoryPort` 桥接器连接 Python gRPC 服务，声明为 `Remote`。只有宿主负责启动和管理 Python 进程时，才把正式 manifest 的 transport 改为 `Sidecar`；入口、依赖锁定、协议 schema、权限和隔离方案均待确认。

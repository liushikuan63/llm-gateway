# 架构与模块

[返回维护入口](README.md)

架构背景见[统一网关设计](../统一LLM网关设计方案.md)和[智能路由、本地模型与搜索设计](../智能路由与本地模型设计方案.md)。这些文件包含历史阶段说明；当前行为仍以生产调用链和测试为准。

| 模块 | 当前职责与维护入口 |
| --- | --- |
| 桌面入口 | React 页面调用 Tauri IPC；注册表在 [lib.rs](../../src-tauri/src/lib.rs)，命令在 [commands.rs](../../src-tauri/src/commands.rs)，类型在 [api.ts](../../src/api.ts) |
| 聊天消费者 | [desktop_chat.rs](../../src-tauri/src/desktop_chat.rs) 通过已归属的网关 listener 发送请求，不能绕开路由、预算或审计；[Chat.tsx](../../src/pages/Chat.tsx) 管理呈现、续轮及本地导出 |
| 协议与上游 | [protocol](../../src-tauri/src/protocol/mod.rs) 保留供应商响应语义；[代理服务](../../src-tauri/src/proxy/server.rs) 负责请求生命周期，账号适配不是普通 API Key 请求的别名 |
| 独立账号 | [WorkBuddy](../../src-tauri/src/workbuddy_auth.rs)、[Qoder](../../src-tauri/src/qoder_auth.rs)、[Trae](../../src-tauri/src/trae_auth.rs) 分别维护区域、主体、认证代次与加密存储；不能互借账号或区域授权 |
| 权益 | [benefits](../../src-tauri/src/benefits.rs) 与 [WorkBuddy 领取](../../src-tauri/src/workbuddy_auth/daily_claim.rs) 使用持久预约和收据；活动积分、可用余额、模型权限是不同证据 |
| 用量 | [client_usage](../../src-tauri/src/client_usage.rs) 读取受支持日志并聚合 token；参考 API 费用不是平台账单或积分兑换 |
| 搜索 | [search](../../src-tauri/src/search/mod.rs) 在上游模型调用前预取；设置界面实际位于 [LocalModels.tsx](../../src/pages/LocalModels.tsx) |
| 路由与网络 | [router](../../src-tauri/src/router/mod.rs)、[outbound.rs](../../src-tauri/src/outbound.rs) 和系统代理生命周期是不同层，修改权重、计费政策或系统网络设置需要各自的方案与授权 |

实际 Git 根是 `llm-gateway` 子目录，邻接的 Node 项目只作参考，不能替代 Rust 生产路径。共享接线、构建、发布资源和安装目录由唯一主线管理，隔离辅助只能修改已划定范围。

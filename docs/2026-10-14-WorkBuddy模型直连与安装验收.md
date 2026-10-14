# WorkBuddy 模型直连与安装验收

本记录接续独立授权后端阶段；该阶段文档是历史结果，当前生产登录页面与模型适配已经进入新安装包。总体目标覆盖全部现有任务卡并保持 active，不以 WorkBuddy 一个平台的阶段交付代替全项目完成。

## 实现范围

WorkBuddy 国际版增加官方网页登录、等待/取消/状态、显式刷新和模型接入页面。授权链接固定官方发行方；令牌和待授权 state 使用本软件既有加密存储。共享认证服务为模型请求提供账号绑定和临期单次刷新，失败不重放。账号接入按事务新增运行时、供应商与模型，重复接入保留用户配置、价格和能力来源。

模型运行时 `workbuddy-model` 直接访问固定官方目录及聊天接口。保留多轮消息、工具、声明支持的媒体、推理/拒绝、用量和独立积分字段；五种下游协议通过本地真实 HTTP 对照验证。最终请求含覆盖参数和工具定义的上下文预算、输出限制与声明能力检查，拒绝重定向、自动重试和不完整流式终态。接口积分不换算为美元费用。

本阶段修复了运行时错误误报 500、工具名称分片/交错消息丢参数及 Gemini 工具历史转换等问题。独立复审的四条问题已经修复并重新核验。普通供应商编辑丢能力来源、聊天页仅 refusal 当空回复等后续问题已经进入 S1 和 UX5/UX6 队列，本阶段不声称修复。

## 验证结果

原始证据位于父目录 `artifacts/Project-Enhancement-20261013`；失败日志和历史收据均保留。

| 检查 | 实际结果 |
| --- | --- |
| Rust 全量 | `workbuddy-direct-full-tests-final.log` 退出 0，71 份摘要，1320 passed / 0 failed / 19 ignored。另有 10 条实际跳过消息，Ollama/edgeJev 外部环境未验证，不能把测试框架 passed 总数当成全部真实流程通过。 |
| 静态检查 | fmt、Clippy all-targets `-D warnings` 退出 0；修正后协议 29 项、账号注册数据库 10 项定向测试通过。 |
| 页面与脚本 | 前端构建、全量浏览器冒烟退出 0；29 脚本测试、19 语法文件、13 脱敏夹具通过；10 规划文档、28 手册章节校验通过。浏览器 IPC 夹具与原生 IPC 实证分开保存。 |
| 官方匿名流程 | `workbuddy-direct-live-proof.json` 验证官方 host、未知服务端 TTL、最长 300 秒本地等待、加密 pending、取消后 0 个 pending/session；没有网页同意、刷新、模型或领取。原真实资料及旧安装 EXE 在该探针前后摘要一致。 |
| 原生正式资源 | release 及新安装版各通过 5 项真实页面/IPC 检查：状态、源码一致的 64 字符输入、匿名等待、取消、未登录的刷新/接入/无效轮询拒绝。实际 origin 为 `http://tauri.localhost`，资源 `/assets/main-D3d5EJnJ.js`；自有进程已退出。 |
| 保留的开发环境失败 | debug v3/v4 页面走 `http://localhost:5173`，未满足最新输入属性断言。失败记录保留；不能把这些失败称通过。正式资源和安装版通过独立断言，未删除共享缓存或强停用户服务。 |
| 新包 | NSIS/MSI 构建退出 0。NSIS 静默实际安装退出 0；安装目录 `D:/Program Files/LLM Gateway`，更新前完整备份在 `D:/UserData/llm-gateway-backups/installed-before-workbuddy-direct-20261014-0500`。 |
| 包验证 | NSIS 安装版及 MSI 管理提取各通过 21 资源检查；规范化 EXE SHA256 为 `9284e3a45ff3b929c99f2824b4ac23359a83fb5cecd82feb4f87e9f4fca498f5`。199 个生产输入文件，源清单 SHA256 为 `eee53abf6f475f5c6b6f3c152e83b259a75b7fe435de2600c70966b3fd438028`。 |
| MSI 提取边界 | 首次 `/a` 使用正斜线路径返回 1619；保留日志。改用已解析的 Windows 原生路径后退出 0，资源校验通过。仅管理提取，未做 MSI 整机安装。 |
| 安装版真实只读验收 | `workbuddy-direct-installed-account-usage.log` 退出 0：26 日行合为 12 个来源/模型合计，1 模型生成配置价 USD 曲线；Codex partial、DSH 两版及 Qoder ok。三个加密登录查询无错误，Trae 独立 HTTP 上下文与 00:05、Qoder 12:05 保留；模型/领取请求均 0。 |
| 后台收尾 | 五项原任务 Run 自启仍不存在，双定时触发仍启用且指向安装版；自有原生测试窗口已退出。没有实际重启，不称重启验证通过。 |

## 真实流程的限制与下一步

完整网页登录同意、真实 token/expiry、真实刷新及收费模型调用仍未验证；匿名授权可达、离线完整模型协议和安装成功均不能代替这些验收。当前没有新增收费请求或重复领取。Trae 首次失败和随后原生领取 100 的两份独立收据保留；新日直接 HTTP 实际发放仍待活动日验证。

继续实现 Trae 独立网页/刷新、Qoder 两版设备/PAT/COSY 可行协议及区域绑定；需要用户网页同意和单次收费请求的步骤另取得适用授权。等待外部条件时推进能力 S1–S8、体验 UX2–UX8 与原有 N/V 等队列。源清单对应本次安装包；后续源码变更必须单独验收，不能沿用本包哈希冒充新包。


## 第三次独立授权的模型成功与正式接入（2026-10-14 11:45）

用户明确回复call_3d62e0ae8088418aa33e1cec004e0896“授权这1次WorkBuddy请求”。使用已安装EXE d84837508f145f0dfa4075cd3faf1857e82b5364fe74861eaab883d048797c9a及独立资料目录，用户正文仅“只回复 OK”、补中性system、stream、max_tokens32；目录24模型，固定deepseek-v4.1-flash只POST一次。HTTP200、回复OK、finish_reason=stop、DONE恰好1，供应商usage输入14/输出1/总15、reasoning0、credit0。该credit为供应商上报积分，未核对账号积分流水，不承诺所有模型免费。

原收据workbuddy-approved-third-request-receipt.json SHA 119ac93e5668b397d4a437b92c7cb6623786e76a6632aa8cb3590f275bcc78b5；独立数据库审计96号只有1个attempt，status200、input14/output1、总latency920ms、上游attempt849ms，trace与HTTP头一致，重试/认证复测/降级/搜索/领取均0。两份此前失败收据SHA不变；新成功不能改写旧失败。真实验证范围仅简单文本/OpenAI流式，完整历史/工具/媒体及其他协议仍只有既有合成HTTP回归，未发额外实测。

正式资料已通过生产IPC注册WorkBuddy · workbuddy独立供应商及24个官方目录模型，读取生产记录确认可选择使用；这次注册只有目录GET，不发模型。注册前私有完整备份D:/UserData/llm-gateway-backups/own-profile-before-workbuddy-direct-20261014-1100。原有14供应商/87模型/95请求与旧运行时各行完整保留，当前15供应商/111模型/95请求、2运行时；config/master不变。模型测试仍在独立资料，正式请求记录没有新增测试请求。

正式WorkBuddy保存登录随后仅一次真实refresh成功、新pair及同一identity_hash核验/读回通过，零模型/领取/自动重试。允许改变的仅自有account-login:workbuddy:global:session:workbuddy；旧三权益认证、Qoder模型/设备认证及三份模型收据仍保全。既有菜单登录刷新与模型适配使用本软件加密认证，不需WorkBuddy客户端。无需以过期/失败模型收据替代这次成功。

新安装仍为1367/0/19完整Cargo（另10外部真实跳过）、202生产源及21资源阶段，未改Workbench模型源码或重建另一包。Qoder新源码scripts/qoder-protocol的独立签名/严格codec与管道worker已11项离线测试通过，当前属于未打包/未接Rust消费的下一阶段。两区已安装认证模块相同、298606 bytes/SHA6419471effa631519def7797d76d7ede38b9fcfa9a83c2148c8ef5d43355b43d，不等同公开CLI1.1.34的旧297238-byte基线；当前模块仅作隔离合成oracle，不随本软件分发。合成目录20头、RSA/AES、canonical MD5、UTF8/custom-base64响应匹配；不读取第三方凭据或发送Qoder目录/模型。当前字段只需UID/token/org/tags/data-policy，撤回旧主程序姓名/原始邮箱作为必须签名上下文的推断。下一项接私有加密签名用途、Rust管道消费、真实两区免费目录及完整模型路由。

总体目标仍active，Trae00:05/Qoder两区12:05及全部R5/S/UX/N/V与原卡保留；未提交推送。

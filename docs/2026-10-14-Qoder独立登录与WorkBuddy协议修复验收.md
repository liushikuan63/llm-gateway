# Qoder 独立登录与 WorkBuddy 协议修复验收

2026-10-14，本阶段已完成源码、适用回归、实际 NSIS 更新、安装版关键流程与后台清理。全部待完成/可优化项仍在[持续目标索引](持续目标与任务索引.md)，不将本阶段完成记作完整目标完成。证据统一在父目录 `artifacts/Project-Enhancement-20261013`，不混用早期子仓 artifacts。

## 实现及真实状态

Qoder 国际版/CN 提供独立 PKCE 设备登录、五个 Tauri 命令、后台命令和生产页面。固定各区网页/API、公共客户端 ID，安装设备标识与 COSY 机器上下文分开；自有认证加密保存在既有 app_secrets，验证同区域 UID/组织后原子提交。取消、到期、资料变更、并发刷新和未知结果禁止覆盖或重放。有效期支持已核实的设备毫秒字段、ISO 和相对秒数；未知数字单位不猜。

安装版两区原生界面与实际 IPC 7 项通过；后台 CLI 5 项通过，两区各一次真实匿名轮询都返回 awaiting_authorization。没有打开 Qoder 授权浏览器或平台客户端，没有真实用户授权/刷新/模型请求。页面已明确本页凭据尚不用于模型调用或奖励领取，旧领取配置保持有效。当前程序的 jobToken 签发/刷新及 api2 线索继续在 R5.5 实现，不能混用旧 api1/PAT 或设备/模型相对有效期单位。

用户允许 WorkBuddy 官方网页并授权一次真实模型。完整网页登录、身份核验、官方 27 项目录已通过；账号新增一个本应用的加密会话，安装更新后原生界面可真实读回。只读界面验证没有点击接入/刷新/模型按钮。

该一次模型授权用于前一安装版本，在隔离测试资料目录关闭降级、鉴权复测、缓存、智能辅助、搜索与领取，用户正文仅“只回复 OK”，模型 deepseek-v4.1-flash，max_tokens 32。上游实际返回 HTTP 400/code11128/90ms，审计只有一个失败 attempt，没有 Done、回复或供应商 usage。没有自动重试，不能从失败审计默认0 token推断费用或额度未扣减。

随后补齐 WorkBuddy 国际版首条 system 的消息格式约定：缺首 system 时添加中性 `You are a helpful assistant.`，保留已有系统和所有用户内容、历史、工具与媒体次序，并计入输入预算。公开作者实现记录同类11128，不能据此断言它是实际拒绝的唯一原因。新版本修复已安装，修复后真实模型仍未验证；原一次授权已使用，追加一次需要新授权及独立收据。

## 适用门禁

| 项目 | 实际结果 | 证据 |
| --- | --- | --- |
| 最终完整 Rust | 72组摘要，1351 passed / 0 failed / 19 ignored，退出0；另10条明确外部环境跳过 | qoder-workbuddy-full-tests-final.log / -exit.txt |
| Qoder 后端专项 | 19/0/0；已过期令牌与明确毫秒字段均有实际首红及修复后绿 | qoder-device-extended-tests-final.log；qoder-expiry-red.log |
| WorkBuddy 首system专项 | 首红0/1/exit101；定向41/0/0/exit0，网关HTTP夹具要求首system | workbuddy-system-head-red.log / -green.log |
| Clippy / fmt | all-targets -D warnings退出0；fmt --check退出0 | qoder-workbuddy-clippy-final.log |
| 前端 | build退出0，Qoder14项浏览器交互与完整UI退出0，新截图141张 | qoder-login-ui-tests-v2.log；qoder-login-full-ui.log |
| 其他 | 脚本29/0/0、语法19、夹具脱敏13、日志扫描109、规划10、手册28章节、release配置均通过 | 相应本阶段终端输出与原脚本 |
| 独立复审 | Qoder生命周期/有效期增量与WorkBuddy首system均未确认新增P1/P2 | qoder-auth-review.md；workbuddy-direct-review.md |

19 ignored、10显式外部跳过、0 doc tests与本地mock分别保留，不当作真实供应商/设备已验。首次浏览器姓名完全匹配定位失败已保存，修正含掩码邮箱的dd定位后通过；第一次原生调用计数监听不受支持，v2明确记录null，不当成零轮询证据。

## 包与安装

- 实际 Git 根为本子仓，main，HEAD `9fe371308435c4a78d23d8374b9e2ffb9197827b`，保留既有脏工作区，没有提交/推送。
- 构建输入201个生产文件，清单 SHA256 `8705da19182eae455bc0e71479aec40c0ee594f283947a2f1846974d2c31f08b`；构建时间 `2026-10-13T23:38:23.781Z`。
- `npm run tauri:build` 退出0，两包生成；NSIS `/S` 实际更新退出0，安装在 `D:/Program Files/LLM Gateway`。更新前完整程序备份 `D:/UserData/llm-gateway-backups/installed-before-qoder-device-20261014-0742`。
- NSIS安装载荷和MSI管理提取各21资源校验通过；规范化EXE SHA256 `5065e05ab8ef0b00ded67c7ef08883631db353a862c375846cf4d618a6801656`。MSI提取另验证DLL；首次斜杠路径提取1619失败日志保留，改Windows原生路径后退出0。没有实际MSI整机安装/重启验收。
- 安装版Qoder原生7项、后台CLI5项，以及原资料中WorkBuddy登录读回3项通过，自有窗口已退出。原生计数监听不受支持，因此匿名GUI轮询数量保留未知；CLI明确只执行两次匿名GET，没有认证成功的声明。
- 新安装版实际窗口标题为LLM Gateway、Responding=True；`/healthz` 200，15721监听进程确属本任务启动的应用。额外存活检查未发送模型、领取或刷新，结束后自有应用已退出，证据 `workbuddy-installed-login-liveness/proof.json`。

## 原资料、收据和后台清理

原14供应商/87模型/95请求、原3权益认证、配置、主密钥及Trae14日首次失败收据摘要保持一致；仅增加用户本次明确授权的一个WorkBuddy加密登录会话。真实模型的96号失败审计在隔离目录，原请求历史没有增加测试数据。

WorkBuddy一次失败原收据 SHA256 `747efe9d01e8e6f1ca37e4affc58749c54670d4192d55160be06238df5f1c68d`；Trae首次00:05失败原件 SHA256 `3aec895b4df5f8d70848c01fdd276636cccf6f12b80bb86a8bf509fe70938b77`。账户保存与保护证据为 workbuddy-approved-profile-proof.json；不删除或复用任一授权/收据。

Windows任务 `LLMGW-Benefits-7261FDEA4F8BC3A8` Ready，两启用触发器00:05/12:05（北京时间），指向新安装EXE与原资料；LastRun14日00:05:01/Result1仍是原失败，NextRun14日12:05。本阶段没有重复领取。五项先前移除的自启仍不存在；自有Vite PID42112已核验身份后关闭，5173/39012/39361/39362无自有监听残留。用户授权的浏览器与其他应用不做整体关闭。

## 接续入口

完成本阶段桌面文件更新后，继续 Qoder jobToken/api2 及 Trae独立授权/刷新、WorkBuddy权益和模型实际复验，再推进S/UX/N/V全部队列。普通供应商测试按钮未走账号dispatch、失败trace及安全错误说明、静默取消结果提示与区域等待优化已写回原队列。当前聊天真实成功压缩9次，原生创建/交接/归档入口缺失及后台输入拒绝继续保留，未假报轮转。

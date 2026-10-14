# VibeCoding 任务卡 · 账号权益、签到与领奖

2026-10-14 23:15 R5 安装接续：Trae CN 普通 IDE 认证 UI 与独立后端、账号指纹绑定已随 233 源输入/26 资源新包实际 NSIS 静默安装，完整 1455/0/19（另10外部跳过）、20 浏览器场景/146 新图、安装版原生7与自有退出通过。尚未真实授权/设备公钥登记或 Trae 模型/签到直连；SDK AHA 新设备初始化和 renderer 整窗重载恢复仍待核实。WorkBuddy 页面明确国际版，CN 版及 Trae 国际版不得泛化为已实现。预算夹具原414.65秒失败及所有首红保留，按证据修补而非跳过测试。详见 [阶段验收](2026-10-14-Trae登录界面与安装阶段验收.md)。用户最新要求将日志/API费用增强优先推进，区域补齐继续同一目标；新收费授权仍为0，已有收据不重放。

2026-10-14 区域补充要求：Trae CN / 国际版、WorkBuddy CN / 国际版都纳入 R5；当前实现只证明 Trae CN 普通 IDE 与 WorkBuddy global（issuer www.workbuddy.ai、独立加密前缀 global），另一版本须单独调查官方发行方、客户端身份、认证/刷新/目录/权益/模型协议和区域隔离。不得只换域名、共享另一地区令牌或借已消费授权证明成功。Qoder global/cn 保持既有区分。缺失区域列为后续实施，不据当前单一区域通过宣称该平台两区全部完成。

2026-10-14 20:30 R5 后端阶段最终：73 组完整 1451 passed/0 failed/19 ignored，补证 10 条外部主动跳过；Trae 专项 19、独立复审、脚本 50/语法 29/夹具 13、Clippy/fmt/121 日志文件+11 样本、类型/规划/资源/28 章手册均通过。真实开发版原生 IPC 共 6 项通过，私有模型/领取/设备登记均 0；自有应用和 Vite 退出。230 源输入全部匹配，未安装本阶段代码、未测真实 Trae 授权/刷新。下一项六个已接 API 的 UI 生命周期、官方新设备登记及原生免费安装验收；不得将原有客户端设备借用称为新设备完全独立登录。首轮资源错误与前三轮探针失败原件见 [完整阶段记录](2026-10-14-Trae独立认证后端阶段验收.md)，总目标仍 active。

2026-10-14 20:12 R5 接续：Trae 普通 IDE 的 PKCE/P256、一次性回调、V2 交换与服务端主体核验、加密 pair/密钥、刷新/取消代次和六个 Tauri 命令已接入。19 项后端回归、50 脚本/29 语法/13 夹具、Clippy 和 121 Rust 日志/11 自测通过；首红和复审修补原件保留。完整构建首轮 os error 1455，降构建并发重跑仍执行中，未安装或真实登录。下一项完整回归/资源校验、UI 与原生免费验收、官方新设备登记合同；真实登记前必须明确复用旧 DeviceID 公钥变更的影响，不得伪造设备、偷读客户端私钥或复用已消费的费用授权。见 [阶段验收与接续入口](2026-10-14-Trae独立认证后端阶段验收.md)。两区每日 12:05、Trae 每日 00:05、今日原收据和正式资料保护均保持，全部队列 active。

## 2026-10-10 实施现状与本轮计划

本节是本轮实施入口；下文 2026-10-05/06 的调查、撤回记录与接口证据保留为历史，不代表当前代码状态。开工检查工作区干净；此前已有供应商额度查询与账号型运行时管理，但没有独立账号权益中心。本轮按 §九已核验的 Qoder 国际版/中国版契约实施，不重新读取第三方凭据或代登录。

原 N2/N3/N4/V3 优化方向继续保留在[运行时一致性与 VPN 集成任务卡](VibeCoding任务卡-运行时一致性与VPN集成.md)，本批账号权益是补充；总余额适配、到期提醒和更多平台权益须分别核验，不能以活动条数或领取历史代替完成声明。

当前源码已接入独立「账号权益」页面、账号配置、活动详情、手动/可选自动领取、加密 Token 与领取记录。配置定义在 `src-tauri/src/benefit_config.rs`，纯解析与应用编排合并在 `src-tauri/src/benefits.rs`，沿用 `db/repo.rs` 和 `db/migrations.rs`；没有另建历史规格中的 `benefit_center.rs`。IPC 为 `benefits_overview`、`claim_benefit_now`、`set_benefit_token`、`clear_benefit_token`、`benefit_runs`；账号与开关走既有 `update_config`。返回 `has_token` 与固定掩码，不返回 Token 原文，密钥按平台和账号分别存储。

| 能力 | 本轮范围 | 其他平台边界 |
| --- | --- | --- |
| 活动列表、逐条领取状态、积分数量与有效期 | Qoder / Qoder CN；未知金额或时间显示未知 | Trae 等平台仅列能力差异，不假装已适配 |
| 手动领取、发放与重复领取结果、执行历史 | 用户显式触发；以后端逐条 campaign 状态为准 | 不把登录、模型调用或桌面提醒当成签到成功 |
| 每日自动领取 | 总开关与自动领取均默认关闭，用户显式开启后复用手动路径 | 未核验平台不提供可操作的自动领取开关 |
| Token 管理 | 用户自行输入，仅 Tauri IPC 提交；返回只带凭据状态，保存后清空输入 | 不读客户端凭据文件，不在页面、配置、日志或导出中回显 Token |
| 账号型模型调用 | 在供应商运行时管理中独立配置 | 与本页领取凭据和赠送积分分开，不保证调用可用或额度互通 |

账号型 OpenCode 调用还须通过独立的配置预检：在同一子进程环境先执行官方 `opencode debug config`，确认唯一 primary Agent 全 deny、share 禁用；显式插件或未关闭的 MCP 会导致拒绝，未通过时不执行 run、不发送模型提示词。预检最多 10 秒并与 run 共用请求总时限。这项门禁不验证活动 Token 或积分，也不等于 OS 沙箱；CLI 自身解析、初始化与会话落盘仍有独立边界。详见另一任务卡的当前 OpenCode 能力矩阵。

实施顺序：先落实后端 DTO/IPC 与禁用不发请求、幂等/错误不封窗测试，再接入「账号权益」导航和页面；随后对齐客户端运行时能力说明，更新单一手册源 `src/content/user-manual.json`，最后运行类型检查与真实浏览器 IPC 夹具回归。手册已更新为 0.2.1、23 章；最终命令结果回填验证记录。

页面验收必须覆盖默认关闭、缺少 Token、加载失败/重试、活动详情、金额未知、`granted` 与 `replayed` 分离、手动失败可重试、自动设置保存、历史记录、导航退出后的迟到结果保护，以及 390/900 像素布局与控制台错误。浏览器 mock 不代表已真实领取；本轮实测领取另列证据，不用历史截图替代。

本轮没有使用真实平台 Token 调用活动 API，因此没有新的真实余额、实际发放或生产接口漂移验收结论。页面只展示活动权益，不承诺账户总余额；自动领取仅在应用运行时执行。托盘或到期主动提醒仍属后续方向，不能将页面内历史记录写成主动提醒已完成。

前端回归：类型检查与手册/任务卡验证退出码 0；完整浏览器 `verify:ui` 退出码 0，截图独立写入 `.ui-smoke-out-20261010/`（115 张，旧 97 场景全部保留）。新增断言覆盖默认不查询/不领取、每平台 Token 隔离与不回显、活动详情/未知值、失败后重试、发放与重复领取、自动开关/小时校验、历史、严格模式乱序及卸载后的迟到响应；390/900 布局已目视，控制台错误为空。此处“发放”是保真 IPC 夹具分支，不代表真实平台发放验收。

最终全量门禁和 0.2.1 包端验收已通过；NSIS 实装后与 MSI 提取后分别验证权益页、默认关闭、Token 加密/清除、管理 HTTP 鉴权等真实接线。两组各 12 项包含原 VPN 流程，具体证据见[本批验证记录](验证记录.md)；合成 Token 未发平台，不作为实际奖励到账结论。

---

> 需求：把「每日签到 / 活动赠送积分」这类平台权益，直接在网关里看到并领取，
> 而不是每天自己在七八个平台之间手动点一遍。

---

## 一、目标与边界

**目标**
1. 一处看全所有平台的**余额、赠送额度、到期时间、今天有没有可领**；
2. 对**官方提供接口**的平台，支持「一键领取」与**每日自动领取**；
3. 到期与未领**主动提醒**，而不是事后发现白扔了。

**明确不做**
- 不做页面爬取去「模拟点击领奖按钮」；
- 不伪造设备指纹、请求签名或验证码绕过（与
  [`VibeCoding任务卡-账号型上游包装.md`](VibeCoding任务卡-账号型上游包装.md) §3.5 的 L4 红线同一条）；
- 不代持、不过期处理任何平台账号密码（凭据只进加密的 `app_secrets`）。

---

## 二、现状（已核实 / 待核实分开写）

### 2.1 已核实（本轮查证）

| 事实 | 来源 |
|---|---|
| 网关已有平台适配层：`QuotaAdapter { Openrouter, Deepseek, Newapi, Sub2api }`，按 host 注册，未知中转站按 `NewApi → Sub2Api` 顺序尝试，**无适配时明确给出「该服务尚无已适配的额度查询接口」警告而不是假装有数据** | `src-tauri/src/provider_quota.rs:14-20, 112-126` |
| 已知端点：`openrouter /key`、`deepseek /user/balance`、`NewAPI /api/usage/token`、`Sub2API /v1/usage` | 同上 `:176-185` |
| 2026-10-05 历史现状：网关没有独立权益调度器；当时使用 `src-tauri/src/proxy/server.rs` 的维护 tick（旧估计行号 230，非当前引用） | `src-tauri/src/proxy/server.rs` |
| Qoder「每日 100 Credits」规则原文：窗口每天 10:00（UTC+8）开启、次日前夕关闭；**每窗口限领一次，必须手动领、错过不补、不结转**；**仅限在 Qoder 桌面端领取**；有效期 30 天、先到期先扣；个人用户适用、Teams/Enterprise 不适用 | <https://docs.qoder.com/events/100credits.md>（2026-10-05 取） |
| Qoder 的额度可以**官方读取**：CLI `/usage` 面板（Plan / Plan Expiration Date / Plan Credits Used / Add-on Credits Used），Agent SDK 另有 `getUsageInfo()` | <https://docs.qoder.com/cli/usage.md>；`/usage` **需要已登录或已设 Access Token** |
| 本机已配置的 8 家平台：commandcode、openrouter、zai-coding-cn、sensenova、maas-api、geeknow、shitapi、agentrouter（+ 本地 Ollama） | 2026-10-05 探测结果，见 `2026-10-05-导入DSH供应商与模型.md` |

### 2.2 平台核验结论（C1 实测，2026-10-05）

探针全部**不带凭据**，只做路由与族别识别；判定口径见 §2.3。

| 平台 | 族别证据 | 签到/领取端点 | 每日额度机制 | 所需凭据 | 分层结论 |
|---|---|---|---|---|---|
| **geeknow.ai**（GeekAI） | `/api/status` 200，85 字段，`quota_per_unit=500000` | ⚠️ `POST /api/user/checkin` 路由**存在**（401 `invalid access token`），但**功能开关关闭**：**`checkin_enabled = false`**（2026-10-05 23:52 实时取值）；官方文档站 [docs.geeknow.ai](https://docs.geeknow.ai) 目录中**无签到章节**；站点公告中也无签到条目 | **当前没有每日签到** | — | **不可领取**（曾一度误判为 L2，见 §8.1 教训 3） |
| **agentrouter**（Agent Router） | `/api/status` 200，`version=init-20260918-…` | ❌ `POST /api/user/checkin` → 404 `Invalid URL`；`/api/status` **无** `checkin_enabled` | **每日登录**（用户 2026-10-05 告知：每天要重新发登录请求才能领额度）。`POST /api/user/login` → 200「用户名或密码错误」= **登录端点存在** | **账号密码**（非 access token） | **L3**：登录是写操作且要账号密码，可能叠加验证码/风控 |
| **commandcode** | `api.commandcode.ai/` → `{"success":true,"message":"Command Code API","link":"https://commandcode.ai/docs"}`；**无** `/api/status`、**无** `/api/user/checkin` | 未确认 | 未确认 | — | **定位已改变**（见 §2.4） |
| shitapi | `/api/status` → 404 `page not found`（不是 new-api 的响应形状）；根页是 `lang=zh-CN` 的前端 SPA | ❌ `POST /api/user/checkin` → 404 | 未确认（控制台内可能有签到） | — | 待核验 |
| zai-coding-cn | 智谱官方 | ❌ `POST /api/user/checkin` → `{"code":500,"msg":"404 NOT_FOUND"}` | 活动页领取 | 账号 | 待核验 |
| sensenova | 商汤官方 | ❌ 404 `NOT_FOUND` | — | — | 未发现 |
| maas-api | 讯飞官方 | ❌ 404 `no Route matched with those values` | — | — | 未发现 |
| openrouter | 官方 | — | 促销 credits（网页端） | — | 未发现接口 |
| Qoder | 官方 | ❌ 官方明确「**只能手动领、且仅限桌面端**」（Usage panel → gift icon）；`/claim` **经双向取证不存在**（见 §2.3.2） | 每日 100 Credits，窗口 10:00（UTC+8），错过不补 | 桌面端登录态 | **L1 提醒 + 余额**；自动领取属 L3 |

### 2.3 判据修正：**401 不足以证明端点存在**（这条差点写错）

第一版打算写「GET 返回 401 ⇒ 端点存在」。同族对照组直接把它证伪：

```text
agentrouter  GET  /api/user/checkin -> 401    ← 与 geeknow 一模一样的 401，但该站根本没这个端点
agentrouter  POST /api/user/checkin -> 404    ← 真正的判据在这里
geeknow      POST /api/user/checkin -> 401    ← new-api 鉴权中间件对所有 /api/ 路径先返 401
```

**401 只说明"被鉴权中间件拦下"，不说明路由存在。** 可用口径：

1. `POST` 返回 **404 `Invalid URL` ⇒ 端点不存在**（判"无"的唯一可靠信号）；
2. `POST` 返回 401 / 400 这类业务错误 ⇒ 路由存在且方法匹配；
3. 每轮探测必须带**一个已知不存在的路径**做阴性对照（本轮用 `/api/user/claim` → 404），
   否则"全都 404"与"路径写错所以全都 404"分不开。
4. **端点存在 ≠ 功能开启**（本卡最贵的一条教训，见 §8.1）。判断某站是否支持签到，
   **先读 `/api/status` 里开关字段的值**，再决定要不要去试端点：
   `checkin_enabled == true` 才值得试；为 false 时端点即使存在也只是残留代码路径。

### 2.3.1 geeknow 现状（2026-10-05 23:52 实测，三条独立证据一致）

```text
/api/status → data.checkin_enabled = false        ← 功能开关：关
/api/status → data.turnstile_check  = false        （注册需 register_captcha = true）
docs.geeknow.ai 目录                 → 只有「账户管理（认证方式 / 余额与用量）」，无签到章节
站点公告 35 条                       → 无任何签到条目
```

**结论：geeknow 当前没有每日签到。**

#### 带真实凭据的实测（2026-10-05 23:5x）

拿本机 DSH 里**真实的 geeknow 推理 Key**（51 字符，进程内使用、不落盘、不打印）打签到端点：

```text
POST /api/user/checkin  Authorization: Bearer <sk-推理Key>                  -> 401 invalid access token
POST /api/user/checkin  Authorization: Bearer <sk-推理Key> + New-Api-User: 1 -> 401 invalid access token
```

**这条实测证明的是「推理 Key 不能用于签到」**（鉴权在功能判定之前就拦下，
所以**不能**用它反推签到开关状态）——即 §2.2 表里"所需凭据 = 控制台 access token"这句，
从推断升级为实测。要真正确认"该用户能否签到"必须用**控制台 access token** 再打一次，
**本轮没有该凭据**。

### 2.3.2 Qoder CLI `/claim`：双向取证**不存在**

用户提出「Qoder CLI 应该能用 `/claim` 领奖励」。两条独立证据都否定：

```text
官方完整 slash 命令表 /cli/slash-reference  → Account and Status 组只有
        /login /logout /status /profile /usage /upgrade /insights /privacy /permissions
        Conditional Commands（feature flag 隐藏的那批）里也没有 claim
本机 ~/.qoder/commands/                   → 只有 clean-disk.md / clean-temp.md / edge-net-check.md
```

`/claim` 既不是内置命令，也不是本机自定义命令。Qoder 官方给的领取入口是
**桌面端 Usage panel 左下角的礼物图标**（见 §2.1 引用的 100credits.md 原文），
CLI 侧只能 `/usage` **查看**额度、不能领取——且 `/usage` 需要已登录或已设
Access Token（本机当前 `qoder status` = **Not logged in**，故 CLI 侧也测不了）。

> 这次差点又犯同一个错：先看到「常见命令表里没有 claim」就下结论。
> 是官方文档自己提示"完整列表见 Slash Commands Reference"才去查了权威页——
> **凡是说"某命令/某功能没有"，必须找到那份权威完整清单 + 本机实际状态两处都查。**

顺带记录两条与本机导入配置直接相关的站点动态（公告原文）：

- 「geeknow.top 域名目前智能海外访问，可切换 geeknow.ai，部分视频和图片存储也受到
  影响，海外区域可正常访问。**codex 渠道官方封控**」
  → 域名在 `.top` / `.ai` 间迁移；`codex` 渠道已被上游封控，
  与探测时 `gpt-image-2-vip` 返回 `no available channel found` 属同一类渠道问题。
  **导入的 5 个图像模型仍全部保留**（属配置事实），但要在界面标注渠道风险。

### 2.4 定位改变：commandcode 不是普通中转站

`https://commandcode.ai/docs` 是一套完整产品文档：Plans（Go / GOAT / Pro / Max）、
Provider API、Available Models、Headless Mode、ACP、CLI Reference、Skills、Hooks、
Memory、BYOK Providers……**结构与 Qoder 官方文档同构**，属账号型 Agent 产品，
而不是"只有推理接口的中转站"。它应当由
[`VibeCoding任务卡-账号型上游包装.md`](VibeCoding任务卡-账号型上游包装.md) 的
「账号型上游」框架接管。其**每日赠送/签到是否存在**仍需核验正文
（`/docs/resources/pricing-limits`、`/docs/plans/goat`、`/docs/plans/pro`）——
本轮抓取只拿到侧栏，正文未渲染。

### 2.5 未验证的关联（**不许合并成一个结论**）

用户观察到「agentrouter 每天要重新登录才能领额度」，而我们此前测到它所有模型返回
401 `unauthorized client detected`。这两件事**方向一致但未经证明是同一件事**：
前者解释额度，后者是推理调用被拒。可能是同一原因（登录态每日过期），也可能是两回事。
**必须实测才能合并**：当天重新登录拿到 access token 后，再打一次推理、再打一次签到，
看两个现象是否同时消失。在此之前，任何"agentrouter 的 Key 已死"的结论都不成立。

### 2.6 仍未核验（不得凭记忆写实现）

| 项 | 要确认什么 |
|---|---|
| commandcode | 是否有每日赠送 / 签到；文档正文未取到 |
| shitapi | 控制台内是否有签到；站点族别（响应形状与 new-api 不同） |
| zai-coding-cn | 活动赠送的领取入口与是否需要登录态 |
| 各平台的 access token | 在控制台的哪个位置生成、有效期多长、能否长期持有 |

> 纪律：**没核实到的，一律写成「该平台暂无可自动领取的官方入口（已核对 <来源>）」，
> 不允许把「大概是 `/api/user/checkin`」这类猜测写进实现。**

---

## 三、能力分层（决定这张卡怎么做）

```text
L1 权益可见    余额 / 赠送额度 / 到期 / 今天有没有可领 —— 纯读取，零风险
                └─ 复用现有 QuotaAdapter + Qoder Agent SDK 的 getUsageInfo
                └─ 「窗口是否开启、今日是否已领、几天后过期」纯本地算，不碰接口

L2 官方领取    平台确实提供公开领取接口 → 「立即领取」+ 每日自动领取
                └─ 必须逐个平台核验到官方文档原文才准实现
                └─ 本轮唯一确认：geeknow.ai `POST /api/user/checkin`（access token 鉴权）

L2' 登录续期   agentrouter 这类「每天要重新登录才能领额度」的机制
                └─ `POST /api/user/login` 端点已验证存在，但要的是**账号密码**
                └─ 默认只做「提醒 + 一键打开登录页」，不由网关代登录

L3 UI 自动化   Qoder 已明确「仅限桌面端领取」→ 自动领取要么驱动界面、要么逆向内部接口
                └─ 默认不做。若用户坚持，按 0.7.0 的 C9 走「外部插件 + 合规警告」，
                   仓库内不实现、不出指纹/签名模板
```

Qoder 是 L3 的典型：官方明确写「必须手动领」「只能在桌面端领」，
所以能做的是 L1（提醒 + 余额）。
agentrouter 是 L2' 的典型：**端点有，但凭据是账号密码**——让网关长期持有账号密码，
风险与收益不成比例，所以默认只提醒不代劳。

> 两套凭据别搞混（实测结论）：geeknow 的**签到**要控制台 **access token**
> （错误原文 `Unauthorized, invalid access token`），而**登录**要账号密码；
> 用户手上现成的 `sk-` 推理 Key **不能**用于签到。

---

## 四、合规红线

1. 只调用平台**公开提供**的接口；需要登录态浏览器、验证码、设备指纹的一律标为「需人工」；
2. 凭据（Cookie / access token）走 `crypto::encrypt` + `repo::set_secret` 存 `app_secrets`，
   **绝不进 `config.toml`**（明文落盘且随快照传播）；
3. 领取是**写操作**，必须有幂等保护：同一领取窗口重复执行要能被识别为「已领」而不是重复领取；
4. 自动领取必须有**可见的开关与最近一次执行记录**，不允许静默失败；
5. 平台条款若禁止自动化，本功能在该平台上自动降级为「仅提醒」；
6. **网关不持有账号密码**。需要账号密码才能领取的平台（agentrouter 每日登录），
   只做「到点提醒 + 一键打开登录页」，不代登录、不存密码；
7. 签到与登录所需的 access token / Cookie 属**高敏感凭据**：存 `app_secrets` 加密、
   日志与审计只写掩码、界面上永不明文回显。

---

## 五、设计落点（沿用既有形状，不新造子系统）

| 关注点 | 落点 |
|---|---|
| 平台适配 | 与 `provider_quota.rs` 同文件同风格的 `ClaimAdapter`，共用 host → 适配器 的注册表；没有就返回「该平台暂无已适配的领取接口」 |
| 执行记录 | 新表 `claim_runs`（`provider_id / window_key / verdict / amount / message / ts`）；`window_key` 用「平台 + 本地日期」保证一天一次可判重 |
| 调度 | 历史设计：挂在 `src-tauri/src/proxy/server.rs` 已有维护 tick 上（旧估计行号 230）：到点且本地判定「窗口开启且今日未领」才跑。**不引入新的定时器子系统** |
| 手动入口 | Tauri 命令 `claim_now(provider_id)` + 供应商卡片上的按钮 |
| 界面 | 供应商卡片增一块「权益」：余额、赠送额度、最近到期、今日状态（可领 / 已领 / 不适用）、最近一次领取结果 |
| 与 R1 联动 | 供应商因鉴权失败被自动停用、或额度为 0 时，在卡片上直接提示「该平台有可领权益 / 订阅到期」 |

---

## 六、任务卡

### 【C1】平台权益核验（本卡是调研卡，不是代码卡）

**目标**：把 §2.2 那张表逐行变成有出处的结论：哪个平台**有**公开领取接口（给端点与文档链接）、哪个**只有额度查询**、哪个**明确要人工**。

**判据**
1. 每个平台一行结论，**带官方文档链接或控制台路径**；取不到原文的写「未确认」而不是留空；
2. 结论里必须区分「已确认可自动领取」与「需人工」，且 L3 的平台要写明理由原文；
3. 若某平台需要浏览器/验证码才能领，结论里引用其原文（Qoder 已有原文可作范例）。

### 【C2】权益聚合（L1）

**目标**：一处看全余额 / 赠送 / 到期 / 今日状态。含 Qoder 的 `getUsageInfo()` 接入。

**判据**
1. 每家平台的「今日状态」都能算出来，且**不调任何写接口**；
2. 到期倒计时按**平台自己的规则**算（Qoder 是 30 天且各自独立过期，不能简单按领取日推算）；
3. 拿不到权益的平台显示「该平台暂无可自动查询的接口」并说明原因，**不允许显示 0**（0 与未知必须区分，沿用计价那套约定）；
4. 对照组：把某平台适配器关掉，该平台显示为未知而不是 0。

### 【C3】领取执行（L2，仅限 C1 确认过的平台）

**目标**：「立即领取」+ 每日自动领取。

**判据**
1. **幂等**：同一 `window_key` 重复执行第二次必须判定为「已领」且**不再发请求**
   （用计数 mock 断言上游只被打一次）；
2. 自动领取失败时，界面能看到失败原因，**不许静默**；
3. 对照组：把开关关掉，当天不会自动领取（断言上游命中数为 0）；
4. 自动领取只在窗口开启时发生，窗口外手动点领取要给出明确提示而不是白跑一次。

### 【C4】到期与未领提醒

**目标**：今天有可领的、或者有快过期的，主动说一声。

**判据**
1. 提醒能对应到具体的「哪个平台、剩多少、几天后过期」；
2. 已领的不重复提醒（与 C3 的 `window_key` 同源，不能各判各的）；
3. 提醒走通知（桌面通知）或界面红点，**不依赖用户开着界面**才能看到。

### 【C5】文档与合规留档

**目标**：把「哪些平台允许自动化、依据是哪条条款」记进 `docs/`，
供以后有人问「这个能不能自动做」时有出处可查。

**判据**：`docs/` 下有一份对照表，每行有结论 + 出处；
`npm run verify:plan` 退出码 0。

### 【C6】抓包核验 Qoder 领取端点（用户 2026-10-05 提出）

**要回答的问题**：Qoder 桌面端「领取 100 Credits」背后那个 HTTP 调用，
**能不能在客户端之外重放**？——这是"能不能不在客户端领取"的唯一判据。

**动工前置**（缺一条就抓不到）

1. 本机 Qoder 桌面端（已装 Qoder / Qoder CN 0.4.3）+ **用户本人登录**
   （当前 `qoder status` = **Not logged in**，所以现在连 `/usage` 都不可用）；
2. 抓包必须发生在**领取窗口内**（每天 10:00 至次日 09:59，UTC+8）且**当天还没领**，
   否则抓不到那次请求；
3. 抓包方式先拍板（见 §七 裁决点 7）——**已定：甲**。

#### C6 前置取证：静态扫客户端（2026-10-06 00:2x，不用登录、不用抓包）

甲方案的可行性已验：`D:\Program Files\Qoder\Qoder.exe` **9/9 Electron 标志全中**
（`app.asar` / `app.asar.unpacked` / `icudtl.dat` / `snapshot_blob.bin` /
`v8_context_snapshot.bin` / `chrome_100_percent.pak` / `ffmpeg.dll` /
`LICENSES.chromium.html` / `resources\elevate.exe`），版本 **0.4.3.0**，
且 `resources\app.asar.unpacked\node_modules\@qoder-ai\qoder-agent-sdk` 在包里。
当前**没有 Qoder 进程在运行**，现在时间在窗口内（窗口 10-05 10:00 → 10-06 10:00）。

直接扫 `app.asar`（134.8 MB）拿到 **170 条接口路径**，其中与额度有关的全部是**只读**的：

```text
/sash/api/v1/ai-conversations/credits-summary      ← 额度汇总
/sash/api/v1/ai-conversations/credits-heatmap      ← 用量热力图
/sash/api/v2/me/usage
/api/v2/quota/usage                                ← 配额用量（同处出现 usageLogLink / purchaseLink）
/api/v2/user/plan                                  ← 套餐
/api/v1/me/partner_plans  /api/v1/partner_plan/authorize | revoke
```

**没有扫到任何 claim / checkin / reward / bonus / gift 形式的领取路径**
（170 条全表逐条过，领取类关键词零命中）。这条证据**支持**与**不支持**什么，必须分清：

- ✅ **支持**：Qoder 的额度可以用 REST 读——**C2（L1 权益可见）有路可走，且这条线索比抓包先到手**；
- ❌ **不支持**任何关于"领取"的结论。原因是三种可能**尚未区分**：
  1. 领取走的是**另一个 host / 服务**（不在 `/api/` 或 `/sash/api/` 前缀内）；
  2. 路径是**运行时拼**出来的，字面量不在包里；
  3. 领取入口是**内嵌 webview 打开的活动页**（官方文档写入口是 "Usage panel → gift icon"，
     这类入口常常就是 webview 而不是原生请求）。
- 顺带证伪一条：包里**没有**客户端指纹/签名模板的迹象，但也**不能**据此说没有——
  签名逻辑通常在**主进程**（Node 侧），asar 里看到的是渲染层代码。

> 以上三种可能**已由抓包区分**（见本卡末尾「C6 结果」）：答案是**第 3 种**——
> 跨源 **iframe** 打开的活动页；而且它确实用的是**另一个 host**（`openapi.qoder.sh`），
> 即第 1 种也同时成立。静态扫包扫不到的真正原因就是这两条叠加。

#### 因此甲方案要带兜底（**关键风险，先说清**）

Electron 里**渲染层**发的请求能在 CDP 的 Network 域看到，**主进程**（Node 侧 `net`/`fetch`）
发的**看不到**。所以：

```text
首选：Qoder.exe --remote-debugging-port=<port>   → 连 CDP，看主窗口 / webview 各 target 的 Network
兜底：Qoder.exe --log-net-log=<临时路径> --net-log-capture-mode=IncludeSensitive
      ← Chromium netlog，主进程 + 渲染层全收，同样不需要证书、不需要改系统代理
```

若两者都被应用屏蔽（起不来调试端口、netlog 为空），才转 §七 的**乙**（系统代理抓包）。

> **netlog 含 token/cookie 等敏感数据**：写到临时路径、用完即删，不进仓库、
> 不贴进对话（`verify:plan` 会查疑似凭据）。

**三种抓法（侵入性从低到高）**

| 方案 | 做法 | 代价 / 风险 |
|---|---|---|
| 甲（推荐）**不开证书** | Qoder 桌面端若为 Electron，用 `--remote-debugging-port` 起一份 + CDP 的 Network 域读请求（本机已有 `browser-cdp` 技能可复用）；或先用应用自带的网络诊断（官方 `qoder/network-proxy` 页） | 不需要装根证书、不改系统代理；可能需要改快捷方式加启动参数 |
| 乙 **系统代理抓包** | Fiddler / Charles / mitmproxy 解 HTTPS | **系统级修改**：装根证书 + 改系统代理，影响其它所有程序；证书必须能干净卸载 |
| 丙 **不抓包，先查公开 API** | Qoder 有 OpenAPI（`account/teams/openapi/*`，Teams/Enterprise 前提）与 Cloud Agents 一整套公开 API，先确认领取是否本就有公开端点 | 零风险；大概率查不到（官方明说"仅限桌面端"） |

**判据（每条都要能失败）**

1. **有效判据只有一条**：抓到的请求在**客户端不运行时**能重放成功。
   在客户端里能领 ≠ 能不在客户端领。
2. **对照组 A（鉴权真的在起作用）**：去掉鉴权头重放同一请求，**必须失败**。
   若去掉也能成功，说明该端点无鉴权或无副作用，之前那次"成功"不构成结论。
3. **对照组 B（服务端幂等）**：同一账号、同一窗口内**重复重放**，必须返回
   "已领取"类业务错误。这条同时验证 C3 的幂等键设计前提——
   **别把"服务端会拦"当成理所当然**。
4. 记录**请求方法 / 路径 / host / 鉴权头的名称 / 请求体 / 响应体结构**，
   以及**是否携带客户端指纹或签名参数**（这一项决定 L3 到底是不是死路）。
5. 若请求带签名/指纹：**到此为止**，结论写"不可在客户端外复现"，
   不进入"怎么复现签名"这一步（见下）。

**红线（与 0.7.0 §3.5 同一条）**

- 抓包是**只读观测**，允许；**把签名/指纹算法复现出来并落进仓库**不允许。
  确要自动化，只能走**外部插件**形态，仓库内不实现、不出模板。
- 证书 / 代理属系统级修改：动手前给方案 + **回滚步骤**（装了什么、装在哪、怎么卸），
  由用户本人确认后再做。
- 抓到的 token / cookie **只进 `app_secrets`**：文档里只写头的**名称**，不写值；
  抓包文件（`.saz` / `.har`）用完即删，不进仓库（`verify:plan` 会查疑似凭据）。
- 抓包只用于**判定可行性**，不等于平台条款允许自动化；
  条款禁止时本功能仍降级为「仅提醒」。

---

#### C6 结果：**能，但有一道设备绑定门槛**（2026-10-06 00:3x 实测抓包完成）

甲方案（CDP，不装证书、不改系统代理）成功抓到全部相关请求。整个活动页只发**两条**业务请求：

```text
GET  https://openapi.qoder.sh/sash/api/v1/me/campaigns
POST https://openapi.qoder.sh/sash/api/v1/me/campaigns/{campaignId}/claim     ← 领取
```

**鉴权构成（关键）**——`requestWillBeSentExtraInfo` 里读到的头，**没有 Cookie**：

```text
authorization            ← 账号令牌
cosy-clienttype   cosy-version        cosy-machineos
cosy-machineid    cosy-machinetoken   cosy-machinetype
cosy-machinecode  cosy-machinehostname
```

那组 `cosy-machine*` **看起来**是设备标识 / 机器令牌，当时据此推断它是门槛——
**这个推断后来被实测证伪，见下方「C6 最终结果」**。保留在此是为了留下推断→证伪的轨迹：

| 当时的推断 | 最终实测结论 |
|---|---|
| 领取**不是**原生私有协议，就是普通 REST + JSON | ✅ **成立**（标准 `POST` + `application/json`） |
| 可以在客户端之外完成，但**必须同时具备** `authorization` 与 `cosy-machinetoken` | ❌ **证伪**：只要 `authorization`；去掉 `cosy-machinetoken` 仍 200 |
| `cosy-machinetoken` 是设备绑定、绕不过 | ❌ **证伪**：领取端点不校验它 |
| "脱离客户端"= 把本机已登录状态下的这组头复制出去 | ✅ 成立，但**只需复制一个头** |

**判据验证结果**

- **判据 3（服务端幂等）✅ 通过，而且证据比预期更强**：今天这个活动已领，抓到的响应是
  `{"status":"CLAIMED","replayed":true,…,"claimedAt":"2026-09-02T05:20:18Z"}` ——
  **服务端明确把重复领取标记为 `replayed` 而不是再发一次**。这直接印证了 C3 幂等键的设计前提：
  拦重复的可靠性在服务端，客户端只做体验。
- **判据 2（去掉鉴权必须失败）✅ 通过**：见下方最终结果。
- **判据 1（客户端外重放成功）✅ 通过**：见下方最终结果。

**另一个发现：领取不需要点按钮。** 活动页加载后 1 秒内自动发出 `GET campaigns` → 紧接着
自动 `POST …/claim`（因为该活动处于可领状态），服务端用 `replayed` 兜住重复，
UI 再据此显示「已领取」+ 按钮禁用。所以"打开活动页"这个动作本身就等价于"尝试领取"。

**今天的状态（实测）**：`claimable: false`、`claimStatus: "CLAIMED"`；
权益形态 `{"kind":"CREDITS","amount":100,"validity":{"mode":"RELATIVE_DAYS","days":30}}`、
`modelScope.modelSeries.key = "ALL_MODELS"`、`actionType = "CLAIM_BENEFIT"`。
`claimable` / `claimStatus` / `benefit.amount` 这三个字段就是 C2 要显示的东西，**全部可读**。

**当时那次实测的副作用：零。** 因为当天已领，服务端把所有重放都判成 `replayed`，没有重复发放。

**方法教训（三条，都是踩过的）**

1. **关键词表会骗人**：第一版监听器按 `claim|credit|reward` 过滤，而这个活动页的接口用的是
   `campaigns` 词根，**一条都没命中**，看起来像"应用不发请求"。宽口径（记录全部 host+path）
   才是找未知接口的正确起点。
2. **`Page.reload` 对 OOPIF target 无效**：跨源 iframe 作为独立 target 出现，
   但 `Page.reload` 不会真的重载它；要在 iframe 内 `location.reload()`。
3. **Cookie 不在基础事件里**：`Network.requestWillBeSent` 的 `request.headers` 看不到 Cookie，
   必须读 `Network.requestWillBeSentExtraInfo`——否则会错误地得出"这个请求没有鉴权"。
4. **收尾关进程不能按进程名匹配**：写 `Get-Process | Where ProcessName -match 'Qoder' | Stop-Process`
   收尾时，**把用户开着的国际版 Qoder 一起关掉了**（国际版进程名是 `Qoder`，CN 版是 `Qoder CN.exe`，
   两者都命中 `Qoder`）。要关自己起的实例，必须**按启动时间或命令行特征**精确定位，
   例如匹配 `CommandLine -like '*--remote-debugging-port=<端口>*'`。已重新以正常模式启动国际版并确认窗口恢复。

#### C6 收尾准备：重放脚本已就绪并干跑通过（2026-10-06 01:1x）

脚本在**仓库外**（不随产品交付）：`D:\Software\qoder-claim-verify\replay.mjs`

```powershell
# 默认 dry-run：只抓头部、只打印头部名称，不在客户端外发 claim
node D:\Software\qoder-claim-verify\replay.mjs
# 最终验证：抓头部 + 中止客户端的 claim + 客户端外重放 + 两组负向对照
node D:\Software\qoder-claim-verify\replay.mjs --claim
```

脚本会自己用 `--remote-debugging-port` 拉起 Qoder（无需手工准备），红线和注意事项都写在文件头。

**干跑已验证的四件事**（2026-10-06 01:1x，当天已领、无副作用）：

1. 活动页 iframe target 可定位，把 Network 域挂**它自己身上**能拿到注入后的完整头部；
2. `Fetch` 拦截**在该 target 上生效**——`GET /me/campaigns` 与 `POST …/claim` 都被暂停，
   这同时证明**即使在已领状态下客户端每次打开活动页仍会发 claim**（就是那个重放尝试）；
3. 拦截 claim 并 `failRequest` 中止 = **不产生领取动作**，可以把当天额度留给
   "客户端外重放"去证明——这是判据 1 能成立的前提；
4. `campaignId` 与重放头部来源均可拿到，报告会显式写出用的是哪一组头。

**新发现（差点导致误判，必须记住）**：`authorization` 与 `cosy-machine*`
是**网络栈注入**的，不是活动页自己发出的：

| 视角 | 事件 | 能看到 `authorization` / `cosy-*`？ |
|---|---|---|
| 注入后（网络栈） | `Network.requestWillBeSentExtraInfo` | ✅ 有（完整 8 个 `cosy-*`） |
| 注入前（渲染层提交） | `Fetch.requestPaused.request.headers` | ❌ **没有**，只有 `Accept/Origin/Referer/User-Agent/sec-ch-*` |

拿 Fetch 那组头去重放会得到 401，然后会**误判成"客户端外领不了"**。重放必须用网络层头部。
脚本里这两个视角分开存（`netHeaders` / `fetchHeaders`），并显式报告用的是哪一个。

#### C6 最终结果：判据 1 通过 —— **客户端外真实领到了 100 Credits**（2026-10-06 13:08）

用户告知可领取后，用 **Qoder CN**（`openapi.qoder.com.cn`，其余闲、未启动，拉起无影响）执行：

```text
POST https://openapi.qoder.com.cn/sash/api/v1/me/campaigns/01a0f1cd-…/claim
Authorization: <账号令牌>
→ HTTP 200
{"grantId":"…","status":"CLAIMED","replayed":false,
 "benefit":{"kind":"CREDITS","amount":100,
            "modelScope":{"modelSeries":{"key":"ALL_MODELS"}},
            "validity":{"mode":"RELATIVE_DAYS","days":30}},
 "campaignKey":"act-20260930-100","campaignVersion":1,
 "claimedAt":"2026-10-06T05:08:58.441473Z",
 "grantedAt":"2026-10-06T05:08:58.543833Z",
 "expiresAt":"2026-11-05T05:08:58.441473Z"}
```

**`replayed:false` + `claimedAt` = 请求时刻 + `expiresAt` = 30 天后** ⇒ 这是一次**真实发放**，
而且整个请求由**客户端之外**的进程发出。**判据 1 通过。**

| 判据 | 结果 |
|---|---|
| 1 客户端外重放成功 | ✅ **通过**（`replayed:false`，100 Credits，`expiresAt` +30 天） |
| 2 去掉 `authorization` 必须失败 | ✅ **通过**（`401 TOKEN_INVALID: missing authorization token`） |
| 3 服务端幂等 | ✅ **通过**（重复请求返回 `replayed:true`，不重复发放） |

**结论（推翻了两条早先的推断）**

1. **能脱离客户端领取，而且只需 `authorization` 一个头。**
2. **`cosy-machine*` 那 8 个设备头不是门槛**——去掉 `cosy-machinetoken` 后请求仍返回 200。
   早先"设备绑定绕不过"的判断**是错的**；`cosy-*` 只是客户端习惯性携带的设备信息。
3. 但**必须指定"当天可领"的那个 campaignId**：客户端每次打开活动页发的 claim 打的是
   **常驻 campaign**（`act-20260901-922`，早已领过），对它重放只会得到 `replayed:true`。
   这也解释了为什么"打开活动页"看起来像在领奖、实际从来没领到当天的额度。
4. 国际版与 CN 版是**两套独立域名**（`openapi.qoder.sh` / `openapi.qoder.com.cn`），
   抓包与实现都必须按域名区分。
5. 这是 **L3 能力的实证**（0.7.0 §3.5 的 L4 红线）。**用户 2026-10-06 明确要求
   「尽可能实现在网关进行领取」**，因此按用户指令改为**在网关内实现**；
   但实现必须守住 §四 的合规红线（只调公开接口、不伪造指纹/签名）。
   > 说明：本节早先写的"仓库内不实现、走外部插件"已被该指令取代。
   > 实现交接规格见 **§九**（含可直接采用的代码与全部踩坑记录）。

**对照 A 的严格性说明（不许含糊）**：去掉 `cosy-machinetoken` 的那次返回 200 且
`replayed:true`——因为当天的额度**已被前一次重放领走**，所以这一对照证明的是
"缺机器令牌**不会被拒**"，而不是"缺机器令牌也能新领"。要做到后者必须在**未领状态下**
再跑一次（等下一个窗口）。这不影响结论 1/2（`authorization` 必需、机器令牌不拦截），
但严格性差异必须写清，不能当成同一件事。

**判据状态：1 / 2 / 3 全部通过，C6 完成。**

#### 国际版复现（2026-10-06 13:11）—— 两个账号各自独立通过

用户要求把国际版也抓一遍，于是对 `openapi.qoder.sh` 复跑了完全相同的流程：

```text
POST https://openapi.qoder.sh/sash/api/v1/me/campaigns/01a0f1db-…/claim
→ HTTP 200
{"status":"CLAIMED","replayed":false,
 "benefit":{"kind":"CREDITS","amount":100,"validity":{"mode":"RELATIVE_DAYS","days":30}},
 "campaignKey":"act-20260930-551","campaignVersion":1,
 "claimedAt":"2026-10-06T05:11:43.134053Z",
 "expiresAt":"2026-11-05T05:11:43.134053Z"}
```

**这是第二次独立复现，不是同一次结果的重复计数**：不同账号、不同域名、不同 campaign id。
端点路径**完全相同**，只有 host 不同——说明两版是同一套服务、同一套契约。

| | CN 版 | 国际版 |
|---|---|---|
| host | `openapi.qoder.com.cn` | `openapi.qoder.sh` |
| 当天可领 campaign | `act-20260930-100` | `act-20260930-551` |
| 常驻（早已领）campaign | `act-20260901-922` | `act-20260901-493` |
| 客户端外领取结果 | `replayed:false` ✅ | `replayed:false` ✅ |
| 缺 `authorization` | `401 TOKEN_INVALID` | `401 TOKEN_INVALID` |
| 缺 `cosy-machinetoken` | `200 replayed:true` | `200 replayed:true` |

**两个账号当天的 100 Credits 都已实际到账**（各 100，`expiresAt` 均为 2026-11-05），
且**两次发放都由客户端之外的进程完成**。

**结论再确认**：`authorization` 是唯一必需的凭据；`cosy-machine*` 设备头**不参与拦截**；
必须显式指定"当天可领"的 campaignId（客户端自己发的那个永远打常驻 campaign，只得到重放）。


## 七、待裁决（写文档时未定，实现前必须拍板）

| # | 裁决点 | 选项 | 倾向 |
|---|---|---|---|
| 1 | L3（UI 自动化 / 逆向领取）要不要做 | 甲：不做，只做 L1+L2（推荐）｜乙：作为外部插件提供 | **甲**；若选乙，走 0.7.0 的 C9，仓库内不实现 |
| 2 | 自动领取的默认策略 | 甲：默认**关闭**，用户手动点开（推荐）｜乙：默认开启 | **甲**：写操作默认不该自动发生 |
| 3 | 自动领取的时间 | 甲：每天固定时刻（可配）｜乙：只在该平台自己的窗口开启后首次运行时 | **乙**：各平台窗口不同，跟平台走更不容易漏领或白跑 |
| 4 | 平台凭据从哪来 | 甲：用户在界面粘贴（存 `app_secrets`）｜乙：从本地客户端目录读取 | **甲**：与「不读第三方凭据文件」的硬不变量一致 |
| 5 | agentrouter 的每日登录要不要自动化 | 甲：不代登录，只在到点时提醒 + 一键打开登录页（推荐）｜乙：网关持账号密码自动登录 | **甲**：长期保存第三方账号密码的风险远大于每天点一次 |
| 6 | 先做哪个平台 | 甲：geeknow（端点存在，但需 access token 才能验）｜乙：先做 L1 权益可见 + 提醒（当前**没有任何平台可自动领取**） | **乙**：C1 做完的结论是"暂无平台够格做自动领取"，先把 L1 做完才是有产出的路径 |
| 7 | C6 抓包用哪种方式 | **已定：甲**（不开证书：Electron 调试端口 / CDP，netlog 兜底）｜乙：系统代理抓包（要装根证书 + 改系统代理）｜丙：先只查公开 API | 用户 2026-10-06 选定**甲**；乙留作甲被屏蔽时的后备 |

---

## 八、进度回填

| 卡 | 状态 | 备注 |
|---|---|---|
| C1 | **部分完成** | 9 个平台逐个核验：**geeknow 端点存在但功能开关关闭**（`checkin_enabled=false`）；agentrouter 有登录端点（要账号密码）；Qoder 有官方规则原文且 `/claim` 经双向取证不存在；commandcode 定位为账号型 Agent 产品；shitapi / zai / sensenova / maas / openrouter 未发现端点。**当前没有任何平台可自动领取** |
| C2 | **部分取证完成**（代码未开始） | Qoder 额度读取有**实测路径**：客户端包里写死的 `/sash/api/v1/ai-conversations/credits-summary`、`/api/v2/quota/usage`、`/api/v2/user/plan`（见 C6 前置取证）；CLI 侧 `/usage` 面板字段已核到官方文档 |
| C6 | ✅ **完成**（2026-10-06 13:08） | 判据 1/2/3 **全部通过**：客户端外 `POST …/me/campaigns/{id}/claim` 真实领到 100 Credits（`replayed:false`、`expiresAt` +30 天）；缺 `authorization` 被拒（401）；重复领取被服务端判 `replayed:true`。**结论：能脱离客户端领取，且只需 `authorization`——`cosy-machine*` 设备头不是门槛（早先推断已证伪）**；但必须打"当天可领"的那个 campaignId。国际版 `openapi.qoder.sh` / CN 版 `openapi.qoder.com.cn` 是两套域名 |
| C3 | **可实施**：契约与规格见 §九（实现交给另一个任务） | Qoder 国际版/中国版的领取端点、鉴权、幂等与判据已全部实测并写入 §九；作者试写的一版已按要求撤回，工作区干净 |
| C4 | 未开始 | |
| C5 | 未开始 | |
| C6 | **未开始**（卡已写好，含判据与红线） | 阻塞在**外部条件**：需要用户本人登录 Qoder 桌面端 + 在领取窗口内抓包；且抓包方式待拍板（§七 裁决点 7）。**当前不能动工**——`qoder status` = Not logged in |

### 8.1 C1 的两个方法论教训（比结论本身更值得记）

1. **401 不等于端点存在**。new-api 系对所有 `/api/` 路径先过鉴权中间件，
   不存在的端点也会返 401。唯一可靠信号是 `POST` 的 **404 `Invalid URL`**，
   且每轮必须带一条已知不存在的路径做阴性对照。
   （第一版差点把 agentrouter 判成"有签到端点"，全靠对照组才没写错。）
2. **两件事方向一致不等于同一件事**。agentrouter「每天要重新登录领额度」
   与我们观测到的 401 `unauthorized client` 可能同源（登录态每日过期），
   也可能无关。合并结论前必须实测：重新登录后推理与签到两个现象是否**同时**消失。

### 8.2 工具侧记录

本轮 `web_search` 后端恢复注册后，返回结果与查询几乎无关（命中词典释义页），
因此**未采信任何搜索结果**；全部结论来自 `web_fetch` 官方文档原文 + 对用户自用平台的
不带凭据探测。后续若要做 §2.6 的剩余核验，优先用官方文档抓取而不是搜索。

---

## 九、实现交接：在网关内领取（C7）

> **状态：待实现，交给另一个任务。** 本节是**自包含**的交接规格——接口契约、数据结构、
> 落点、判据、踩坑全部在内，**不需要重新抓包或重新推导**。
>
> 交接前作者本人在本仓写过一版实现（`benefits.rs` 纯层 + 配置段 + `benefit_runs` 建表），
> 编译通过、模块内 10 个单测全绿；按用户要求"不在这里实现"**已撤回**，
> 工作区保持干净（`cargo check --lib` 退出码 0），因此实现任务从零开始、无历史包袱。
> 撤回的那版代码在本节 §9.3 逐字给出，可直接采用。

### 9.1 接口契约（已实测两次独立复现，勿再推导）

| 平台 | key | host |
|---|---|---|
| Qoder 国际版 | `qoder` | `openapi.qoder.sh` |
| Qoder 中国版 | `qoder_cn` | `openapi.qoder.com.cn` |

```text
读状态：GET  https://<host>/sash/api/v1/me/campaigns
领取：  POST https://<host>/sash/api/v1/me/campaigns/{campaignId}/claim
请求头：Authorization: <令牌>       ← 唯一必需；实测 authorization 必需、cosy-* 不参与校验
        Accept: application/json
```

**GET 响应**（真实字段）：

```json
{ "uid": "...", "showCampaign": true, "claimable": false,
  "campaignUrl": "https://<host>/growth-page/activity-iframe",
  "campaigns": [
    { "campaignId": "01a0f1cd-...", "campaignKey": "act-20260930-100",
      "actionType": "CLAIM_BENEFIT", "claimStatus": "CLAIMABLE",
      "startAt": 1791165600, "endAt": 1791251940,
      "benefit": { "kind": "CREDITS", "amount": 100,
                   "modelScope": { "modelSeries": { "key": "ALL_MODELS" } },
                   "validity": { "mode": "RELATIVE_DAYS", "days": 30 } },
      "placements": [ { "type": "POPUP", "campaignUrl": "...", "content": { "en": { ... } } } ] } ] }
```

**POST 响应**（发放 vs 重放，两种都要处理）：

```json
{ "grantId": "...", "status": "CLAIMED", "replayed": false,
  "benefit": { "kind": "CREDITS", "amount": 100, "validity": { "mode": "RELATIVE_DAYS", "days": 30 } },
  "campaignId": "...", "campaignKey": "act-20260930-551", "campaignVersion": 1,
  "claimedAt": "2026-10-06T05:11:43.134053Z",
  "grantedAt": "2026-10-06T05:11:43.189263Z",
  "expiresAt": "2026-11-05T05:11:43.134053Z" }
```

**401 响应**：`{"code":"TOKEN_INVALID","message":"missing authorization token"}`

**四条必须内化的语义**

1. **`replayed: false` 才是真实发放**；`true` = 平台判定重复领取、**没有再发**。
   界面与审计必须把两者分开，`replayed:true` 不能显示成"领取成功"。
2. **必须挑 `claimStatus == "CLAIMABLE"` 的那条 campaign**。客户端自己打的是
   **常驻 campaign**（永远 `CLAIMED`），对它的重放只会得到 `replayed:true`——
   这是"看着像在领、其实从没领到当天额度"的根本原因。
3. **`claimable` 总开关与逐条状态会不一致**（实测已领当天总开关仍为 `true`）→ **以逐条为准**，
   并把不一致写进 `warnings`。
4. **令牌有两种粘贴形态**：抓包复制得到的是整条头部值（可能含 `Bearer `），用户也可能只粘裸令牌。
   两种都要能用：**含空格就当已带 scheme 原样用，否则补 `Bearer `**。

### 9.2 落点与命名（照本仓既有惯例）

| 层 | 文件 | 参照物 | 职责 |
|---|---|---|---|
| 纯层 | `src-tauri/src/benefits.rs` | `provider_quota.rs` | 平台识别、HTTP、响应解析；返回 `Result<_, String>`；**不碰数据库与状态** |
| 应用层 | `src-tauri/src/benefit_center.rs` | `pricing_refresh.rs` | 读配置/密钥、编排「读状态 → 挑可领 → 领取 → 落库」；手动与自动**共用同一条路径** |
| 建表 | `src-tauri/src/db/migrations.rs` | 现有 `app_secrets` 条目 | 追加 `benefit_runs`（见 §9.6） |
| 访问 | `src-tauri/src/db/repo.rs` | `list_remote_access_keys` | `insert_benefit_run` / `list_benefit_runs` / `benefit_window_settled` |
| 配置 | `src-tauri/src/config.rs` | `SearchConfig` + `auth_failure.validate()` | 新段 `[benefits]`（见 §9.4） |
| 触发 | `src-tauri/src/proxy/server.rs` | 已有 `/gw/stats` | `/gw/benefits`、`/gw/benefits/claim`（见 §9.7） |
| 命令 | `src-tauri/src/commands.rs` | 现有 provider 命令 | 4 个命令（见 §9.7） |
| 定时 | `server.rs` 的 300s tick（现约 230 行） | `last_pricing_refresh` 的 due 模式 | 每日自动领取 |

**不要新造调度器**：tick 里已有「`Option<Instant>` + 到期才跑」的模式，照抄即可。

### 9.3 数据结构（撤回前已编译通过、10 例单测全绿，可直接采用）

```rust
pub enum ClaimPlatform { Qoder, QoderCn }        // key() / label() / host() / parse() / all()
pub fn platform_for_host(host: &str) -> Option<ClaimPlatform>

pub struct BenefitCampaign { campaign_id, campaign_key, claim_status, action_type,
                             amount: Option<f64>, kind: Option<String>, valid_days: Option<u32> }
impl BenefitCampaign { fn is_claimable(&self) -> bool   // claim_status == "CLAIMABLE"
                       fn is_claimed(&self) -> bool
                       fn describe(&self) -> String }   // "100 CREDITS"

pub struct BenefitStatus { platform, claimable: bool, campaigns: Vec<BenefitCampaign>,
                           source: String, warnings: Vec<String> }
impl BenefitStatus { fn next_claimable(&self) -> Option<&BenefitCampaign> }

pub struct ClaimOutcome { platform, campaign_id, campaign_key, status, replayed: bool,
                          granted: bool,        // status==CLAIMED && !replayed
                          amount, kind, claimed_at, expires_at, message }

pub enum BenefitVerdict { Granted, Replayed, NoClaimable, Skipped, Error }
impl BenefitVerdict { fn as_str(self) -> &'static str
                      fn parse(value: &str) -> Option<Self>
                      fn settles_window(self) -> bool }   // 只有 Granted/Replayed 为了结

pub struct BenefitRunRecord { id, account_id, platform, window_key, campaign_key,
                              verdict, amount, message, manual, created_at }

pub fn auth_value(token: &str) -> String   // 含空格原样用，否则补 "Bearer "
pub async fn fetch_status(platform, token, proxy) -> Result<BenefitStatus, String>
pub async fn claim(platform, token, proxy, campaign_id) -> Result<ClaimOutcome, String>
```

纯层实现要点（都是踩过才写的，别简化掉）：

- `ensure_token` 在**发请求之前**拦住空令牌，而不是发一个必然 401 的请求；
- 响应体**带上限读取**（`BODY_LIMIT = 512 KiB`），不把内存交给对端；
- `http_error(status, body)` 把 401/403 → 「凭据被拒 + 平台原文」，404 → 「接口不存在」，
  **不能笼统说成凭据问题**（那会把排查方向指错）；
- 401 错误信息里**绝不能带令牌原文**。

### 9.4 配置段

```toml
[benefits]
enabled = false                 # 总开关；关掉时任何路径都不发请求
auto_claim = false              # 默认关：这是写操作，会真实消耗当天名额
auto_claim_after_hour = 10      # 「不早于」本地几点（平台窗口各不相同）
[[benefits.accounts]]
id = "qoder-intl"               # 只能是字母/数字/-/_，非空，≤64
platform = "qoder"              # 必须在 ClaimPlatform::all() 的 key 里
label = "Qoder 国际版"
enabled = true
```

`BenefitsConfig::validate()` 返回**问题清单**（不静默改值），并挂到 `AppConfig::validate_local()`
（照 `self.auth_failure.validate()?` 的写法）。判据：`id` 非法/重复、平台未适配都要能报出来。

### 9.5 密钥

- 名称：`benefit:<account_id>:token`，值走 `crypto::encrypt` → `repo::set_secret`（`app_secrets`）。
- **绝不进 `config.toml`**（明文落盘且随快照传播）。
- 所有返回前端的结构体**只带掩码**，界面永不明文回显。
- 令牌写入只走 **Tauri IPC**（沿用「没有 HTTP 写接口」的既有不变量）。

### 9.6 表与幂等

```sql
CREATE TABLE IF NOT EXISTS benefit_runs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id   TEXT NOT NULL,
    platform     TEXT NOT NULL,
    window_key   TEXT NOT NULL,
    campaign_key TEXT,
    verdict      TEXT NOT NULL,
    amount       REAL,
    message      TEXT,
    manual       INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_benefit_runs_account ON benefit_runs(account_id, created_at DESC);
```

- `window_key = platform + 本地日期(YYYY-MM-DD) + campaign_key`；
- `benefit_window_settled(account, window_key)`：存在 `verdict ∈ {granted, replayed}` 的记录即为真；
- **`error` 不为了结**——否则一次网络抖动会白扔当天额度（这是本设计最容易写错的一点）。

### 9.7 触发面

| 入口 | 形态 | 要求 |
|---|---|---|
| `GET /gw/benefits` | 只读：各账号状态（令牌掩码） | 沿用 `/gw/` 已有的**仅回环**约束（`server.rs` 约 396 行） |
| `POST /gw/benefits/claim` | 手动领取：`{ accountId }` | 同一约束；未开启 `benefits.enabled` 时拒绝并说明 |
| Tauri `benefits_overview` | 同上 | |
| Tauri `claim_benefit_now` | 同上 | |
| Tauri `set_benefit_token` / `clear_benefit_token` | 写密钥 | **只走 IPC** |
| Tauri `benefit_runs` | 最近执行记录 | 供界面显示"最近一次什么时候领的、结果如何" |
| tick 每日自动 | `auto_claim && enabled && 本地时刻 >= after_hour && 该窗口未了结` | 手动与自动**共用同一实现**，保证行为一致（照 `pricing_refresh` 的做法） |

### 9.8 判据（每条都要能失败；括号里是必须同时存在的对照组）

**纯层（mock 上游，形状五项对齐：方法 / 路径 / host / 返回结构 / 判定口径）**

1. 状态解析挑出 `CLAIMABLE` 那条，而不是常驻那条；（对照：只有 `CLAIMED` 时 `next_claimable()` 为 `None`）
2. `claimable` 总开关与逐条冲突时**以逐条为准**且产出 warning；
3. `replayed:true` **不算发放**（`granted == false`），`replayed:false` 才算；
4. 空令牌在**发请求之前**就报错（用 mock 断言**一次请求都没发出**）；
5. 401 的错误信息包含平台原文且**不含令牌**；（对照：404 的信息必须指向"接口不存在"）
6. `auth_value` 对裸令牌与整条头部值都能用；
7. `platform_for_host` 对未知 host / 相似域名（`openapi.qoder.sh.evil.com`）返回 `None`。

**应用层**

8. 同一 `window_key` 第二次执行**不再发请求**（计数 mock 断言上游只被调一次）；
9. `Granted`/`Replayed` 使窗口了结，**`Error` 不了结**（下一次仍会重试）；
10. `benefits.enabled = false` 时**任何路径都不发请求**（断言上游命中 0）；
11. 自动领取在 `auto_claim = false` 时不发生（对照组：打开后才发生），且不早于 `after_hour`。

### 9.9 踩过的坑（实现时别重踩）

1. **别造 `cosy-machine*` 头**：实测去掉它请求照样 200，它不是门槛；早先"设备绑定绕不过"的推断已被证伪。
2. **别把 `claimable` 总开关当唯一依据**：实测已领当天它仍为 `true`，只看它会重复打请求。
3. **别打错 campaign**：必须显式挑 `CLAIMABLE`；否则永远只得到重放（见 §9.1 第 2 条）。
4. **两个域名是两套服务**：写抓包/探测脚本时 pattern 别写死 `.sh`，
   作者就因为写死域名导致 CN 的请求**完全没被拦截**、客户端把当天额度先领掉了。
5. **抓包头的两个视角**：`Network.requestWillBeSentExtraInfo`（注入后，含 `Authorization`/`cosy-*`）
   vs `Fetch.requestPaused.request.headers`（注入前，**没有**这两个头）。
   拿后者去重放会 401，从而误判"客户端外领不了"。
6. **`ExtraInfo` 的头部含 HTTP/2 伪头**（`:authority`/`:method`/`:path`/`:scheme`），
   拿去构造 `fetch`/`reqwest` 请求会被拒；要剔除伪头与 `host`/`content-length` 等自管头。
7. **`Page.reload` 对跨源 iframe（OOPIF）无效**，要在 iframe 内跑 `location.reload()`。
8. **收尾关进程别按进程名匹配**：国际版进程名 `Qoder`、CN 是 `Qoder CN.exe`，
   按 `Qoder` 匹配会把用户开着的国际版一起关掉（作者真踩了）。要按命令行特征定位自己起的实例。

### 9.10 明确不做（红线，不由实现者放宽）

- **不伪造设备指纹、不构造签名、不绕验证码**；只调平台公开接口；
- **不做浏览器/界面自动化**；
- **不读第三方客户端的凭据文件**：令牌由用户在界面粘贴（裁决点 4 已定）；
- `auto_claim` **默认关**，且必须有可见开关与最近一次执行记录（不允许静默失败）；
- 平台条款若禁止自动化，该平台降级为「仅提醒」。

### 9.11 交付验收

| 验收项 | 判据 |
|---|---|
| 编译 | `cargo check --lib` 退出码 0（编译退出码是验收的一部分） |
| 测试 | `cargo test --jobs 1` **0 failed**；新增用例含 §9.8 全部对照 |
| mock 保真 | 夹具的 host/path/方法/返回结构/判定口径与 §9.1 逐项对齐 |
| 真机 | 在**领取窗口内**（每日 10:00 UTC+8 后）经网关真领一次：`benefit_runs` 落 `granted` 且平台返回 `replayed:false`、`expiresAt` = +30 天 |
| 幂等真机 | 同一天再触发一次：落 `replayed` 或直接跳过，**平台不重复发放** |
| 界面（若做） | `npm run verify:ui` 退出码 0 **且**真的生成了新截图；未做视觉验证要显式写明 |

> **真机验收的时间约束**：作者抓包当天（2026-10-06）两个账号的额度**都已被领走**，
> 因此**当天只能验证 `replayed` 与"已领过"分支**；
> 要证明网关能**真实发放**，必须在下一个窗口（2026-10-07 10:00 UTC+8 之后）跑，
> 且当天**不要**先在客户端里领。

## 2026-10-13 追加：CN 领取、12:05 定时与直接账号接入

用户授权：CN 也实际领取；国际版与 CN 每天北京时间 12:05 自动领取；增加可设置时间功能；核实直接登录获取账号信息与模型调用，凡经验证可行的路径均替代桥接。模型调用实现授权不自动复用此前已经消耗的单次收费验收授权。

| 卡 | 内容 | 完成标准 | 当前状态 |
| --- | --- | --- | --- |
| R1 | CN 今日领取 | 一次目标领取与平台回读，独立收据 | 已完成：100 CREDITS，HTTP 200，replayed=false，CLAIMED；2026-10-13 19:49 北京时间 |
| R2 | 可设置分钟与时区 | 12:04:59 不查询/领取，12:05 才执行；兼容旧整点配置 | 已实现并通过边界与旧配置回归；界面保存 12:05 和北京时间通过 |
| R3 | 每日实际定时执行 | 两版独立账号；后台、幂等、登录失效和失败可见；无弹窗自启 | 已启用 Windows 每日任务；2026-10-13 22:37 实际触发退出码 0，两版返回已领取；下次北京时间 2026-10-14 12:05；不是新增发放收据 |
| R4 | 直接登录与账号资料/用量/权益 | 复用现有登录状态，经官方客户端接口取得，凭据仅内存或本软件加密存储；直连服务端，不部署 HTTP 翻译桥接 | 两版原生导入、加密保存及直连查询均通过；完整上下文不能由 Bearer 单独冒充；领取任务无需客户端常驻 |
| R5 | 直接模型调用替代 | 按平台核实官方 SDK/协议、模型和权限；现有工具禁用、安全边界与用量保持；有可验证接口才替换 | 正在核实 Qoder/WorkBuddy/Trae；不以账号接口可用推断推理可用，不伪造模型支持 |
| U1 | 本机用量范围合计 | 同一来源、供应商、模型在所选范围一行，单日使用日期筛选 | 已实现，Rust 和浏览器通过；增加快捷日期、模型排序与可展开读取详情 |
| U2 | 消耗曲线与预计费用 | 保留每日数据用于趋势；缓存计费不重复；使用现有参考价格，未知/条件价明确；币种分开 | 已实现，Rust 和浏览器通过；小额费用缩放与键盘点选日期通过，截图实际检查 |

本轮没有恢复上一批删除的五项弹窗 Run 自启。Qoder CN 使用隔离的后台桌面与现有登录态；不切换可见桌面、不输入系统鼠标。首次 Job 清理未覆盖 Electron 自重启的独立进程，已补充按启动时间、EXE 和独占调试端口验证的收尾；现存测试 CN 调试进程仍须收尾，不得漏报。

### Trae 签到追加与阶段接续（2026-10-13 23:10）

- 用户授权：过零点后测试 Trae CN 每日签到积分；每日北京时间 00:05 自动领取。Qoder 国际版和 CN 保持 12:05。不追加模型请求，不读取第三方凭据文件，不操作前台鼠标或焦点。
- T1：安装包公开代码已确认状态 POST `/trae/api/v2/ug/checkin_credits/status` 和领取 POST `/trae/api/v2/ug/checkin_credits/claim` 分离；使用客户端产生的认证与设备上下文。正在只读核验真实状态、域名和登录导出。积分金额以平台响应为准，不硬编码旧活动金额。
- T2：增加账号独立领取时间，兼容现有默认时间；同一后台任务可以有多个每日触发器，逐账号判断到期时间并保留每日收据。00:05 不得提前触发 Qoder。
- T3：零点后一次真实领取、平台状态回读、实际定时任务读回与退出结果。失败或不明收据保留，不自动重发。当前尚未到 2026-10-14，未执行领取，不算通过。
- 当前聊天 `01a1331c-9c03-7653-a296-c274d8e02da0`，2026-10-13 23:05 实查成功压缩 4 次；第 5 次按全局约定轮转并核验桌面原生接续。实际工作区为本仓，HEAD `9fe371308435c4a78d23d8374b9e2ffb9197827b`，大量已有未提交修改；不提交、不推送、不覆盖历史产物。
- 已验证本轮安装包：NSIS 实装与 MSI 管理提取各 20 资源验证通过；Rust 1248 passed / 0 failed / 19 ignored，Clippy 通过；浏览器 136 截图、24 项安装版免费原生验收通过。完整 `ci-local all` 曾失败，最终采用分项证据，不描述为整条 CI 通过。
- 权威新证据在父目录 `artifacts/Project-Enhancement-20261013`；原资料备份在 `D:/UserData/llm-gateway-backups/benefits-1205-20261013-212236`，不复制凭据到交付目录。任务 `LLMGW-Benefits-7261FDEA4F8BC3A8`，原程序资料 `C:/Users/Admin/AppData/Local/llm-gateway`。旧五项 Run 自启已清理，客户端桥接端口关闭；不恢复弹窗。
- 当前只读 Trae 探针：独占调试端口 9343、隔离桌面，统一执行会话 19095；日志 `trae-readonly-status.log`，工作脚本 `trae-checkin-inspect.cjs`。下一步读回结果、按真实原生认证适配 Trae，然后增加账号时间与回归。Vite 会话 2140 属于本任务，其余旧代理没有源码写入者。最终需更新验收文档并生成新的桌面交付目录，旧交付目录保留。

实际 CN 收据：父目录 artifacts/Project-Enhancement-20261013/cn-reward/2026-10-13-qoder_cn.json。无认证头或第三方凭据落在收据。完整登录请求头仅在内存中用于只读直连对照。

### 直接模型调用的当前证据与下一步

- Qoder 两版账号/权益 API 的直连已实测；本项目已有的原生 Qoder CLI 模型适配器无需 HTTP 桥接，但 CLI 登录和桌面登录不是已证明可互换的认证。
- [Qoder SDK 官方认证文档](https://docs.qoder.com/cli/sdk/authentication)支持 PAT、服务账号或本机 CLI 登录。SDK 会委托 CLI 运行，并非账号权益 Bearer 自动变成任意模型 API Key；不通过修改客户端凭据文件伪装登录。
- WorkBuddy 打包的公开 preload 契约声明了 getToken/getAccount/getUserInfo/getAccountUsage，现有桥接使用其原生 HTTP 代理调用文本流式接口。已发现直接接入候选；尚需核验真实认证形状、设备上下文、可用模型清单及流式/非流式转换，再在既有加密存储和上游协议边界内实现。
- 2026-10-13 追加真机核验：WorkBuddy 后台隔离实例的 window.wb 和 workbuddyDesktop 没有暴露上述 getter；只记录 API 方法名与返回形状，未得到可用 token 导出。公开打包契约与当前 renderer 能力存在差别，不能据静态方法名判定登录与模型直连已经可用；本次没有注册虚假的直连供应商。
- Trae 现有实现依赖客户端聊天接口，只能确认当前选中的桌面模型；目前没有验证成功的独立登录与推理接口证据。不能把第三方逆向接口或其他 Trae Agent 项目的自备 Key 能力当作桌面账号接口。
- R5 尚未完成，不删除原有桥接或改动用户模型启用状态。下一项先核验 WorkBuddy 原生认证与只读模型目录，再补协议回归和可审查的实现；真实模型请求需要新的单次费用授权，已使用的 Trae 授权不复用。

### 23:38 进展与接续指针

- Trae CN 原生登录 getter、只读签到状态及直接服务端 POST 状态查询均真机通过。固定域名 api.trae.cn，实际 req_source=1；原生和直连均 checked_in=true、credits=100，仅读取，不领取。授权上下文来自原生 API，不读取第三方凭据文件；只读对照不需要伪造设备头。
- 源码已实现 trae_cn 适配、严格的平台/客户端来源绑定、响应限额与反射凭据拒绝、领取最多一次 POST 并回读 checked_in；配置 account_claim_times 支持账号 HH:MM，Windows 单任务支持多个每日 CalendarTrigger。Trae 00:05、Qoder 默认 12:05；新 CLI `--setup-trae-benefits` 保持 Qoder 默认时间。
- 全量浏览器最新退出 0，137 张截图在 `trae-ui-final`，账号独立时间、恢复默认、三个平台选项已覆盖；Clippy all-targets -D warnings 最新退出 0，日志 `trae-clippy.log`。脚本 29 pass，19 语法检查，13 脱敏夹具通过；文档 9 份、手册 28 章通过。
- 首红 `trae-full-rust-tests.log`：414 pass / 1 fail，进程清理测试在 Python PID 文件创建但还未写完时读到空 JSON。测试改为临时文件写完后 os.replace 原子发布，生产逻辑未因该失败改动。正在全量重跑：统一会话 86020、Cargo PID 36640，`trae-full-rust-tests-final.log`。源码写入已停止；后续不得与该构建并行生成 Rust 产物。
- 原资料追加备份 `D:/UserData/llm-gateway-backups/trae-0005-20261013-232814`；安全收据 `trae-profile-before.json`，providers=14/models=87/requests=95、master.key 哈希不变。原资料尚未添加 Trae，新适配尚未构建安装；旧有效包和 Qoder Windows 任务仍保留。
- WorkBuddy account/models factory 只读核验返回 factory_unavailable，证据 `workbuddy-factory-inspect.log`；不能据此宣称穷尽所有登录协议，更不能注册未经验证的模型直连。R5 仍待后续独立契约核验。
- 下一步：读回全量测试结果；通过后依次 Tauri NSIS+MSI 构建、静默安装、资源与源清单验证，才用安装版 setup/import Trae 并注册 00:05/12:05 双触发器。之后运行 `native-account-usage-ui.cjs`，私有克隆本软件资料做真实配置价格/日志/UI 与三账号查询，源码、凭据文件不得放桌面；00:05 等待系统自动触发并读回新日收据，任何失败不自动重发。
- 原生接续入口已发现：Codex 应用实际进程名 ChatGPT.exe，PID 2984、窗口 68098；后台 get_window_state 需要 pid/window_id，可用 5000ms 读到完整 433 元素，已看到 llm-auto 项目新聊天入口。当前内置 thread/automation tools 不可用，不使用独立 stdio app-server 持续运行模型。轮转时按新鲜 UIA token 操作并核验新桌面轮次与写入归属，不能凭旧元素索引或标题猜聊天 ID。

### 23:58 安装与午夜验收接续

- 全量重跑已通过：`trae-full-rust-tests-final.log`，1252 pass / 0 fail / 19 ignored，退出 0；Clippy/fmt/浏览器等通过。源码已经冻结，无编译或测试进程在运行。
- 新 NSIS/MSI 构建、NSIS 实装、MSI 管理提取均退出 0，各核验 21 资源；首次 MSI 正斜线路径报 1619，保留首红，使用 Windows 路径后通过。规范化 EXE SHA256 `3f90a49584f31ee646060814ee6db3b2e1a6a5a3d51f1122f75c90d7e8a87995`；194 源文件清单 SHA256 `d1a4601ed154ada25d4f34ae6222bf84f566901cc4d94ec983d7631e5f5da18b`，逐文件核验通过。
- 原资料已新增 trae_cn 并加密导入登录，安装版 headless setup/import/task/check 均退出 0，三账号 has_session=true、error/session_error=null。原供应商/模型/请求表全部行完全一致，14/87/95；master.key 与除 benefits 外配置不变。
- Windows 专用任务 `LLMGW-Benefits-7261FDEA4F8BC3A8` 已启用，两个每日触发器 00:05/12:05（+08:00），下一次实际 2026-10-14 00:05。截止此阶段 Trae 今日已签到，仅查询；**不要在 00:05 前手动领取或启动 Trae 客户端，以免客户端抢先签到。**
- 最新安装版真实 UI 3 项检查通过：`trae-real-account-usage-ui/native-account-usage-proof.json`。实际 25 日行合为 12 模型，1 模型有 USD 参考价、曲线和范围估算正常；Codex 部分读取、DSH 两版/Qoder 完整。三登录/独立时间通过；隔离 GUI 自有进程已退出，辅助客户端和旧五项 Run 自启都没有残留。
- 午夜下一步：只读查看原资料 `benefits-receipts/2026-10-14-trae_cn.json` 与任务 LastRun/LastTaskResult，等 00:05 自动触发；若 granted，额外只读安装版 `--benefits-check-login` 核验 CLAIMED。若 error/unknown 保留，禁止自动再次 POST。Qoder 00:05 不到期，不能把提前领取记作通过。
- 原资料路径 `C:/Users/Admin/AppData/Local/llm-gateway`。新桌面交付准备目标 `E:/Users/Admin/Desktop/LLM Gateway 0.2.1 (20261014 签到与体验版)`；只复制安装包、手册、验收报告、脱敏收据、源清单、SHA256 和截图，不复制私有资料/主密钥/认证上下文。旧两交付目录保留。
- 源码没有待处理写入者。Vite 会话 2140 仍是本任务的测试服务，结束前停止；WorkBuddy factory 暂无可用实例，R5 模型直连仍未验证，保持原桥接与用户模型状态。当前聊天最后实查 23:52 成功压缩 4 次；达到 5 次即按原生桌面接续门禁轮转，优先现有 llm-auto 新聊天入口，不持续运行独立模型进程。

### 2026-10-14 00:50 原生签到修复接续

- **首个真实 00:05 自动领取失败**：任务 00:05:01 运行，LastTaskResult=1，原资料 `benefits-receipts/2026-10-14-trae_cn.json` verdict=error/id=3，原件不可覆盖。零点前后只读状态证明可领、登录有效；失败后仍 CLAIMABLE，未自动重试。其后原生接口对照成功，`trae-native-contrast-20261014-v2.json`：一次原生 claim，HTTP 200/code=0，before.checked_in=false、after.checked_in=true、credits=100，确认时间北京时间 00:19:21。第一个对照只因 ID/显示名混淆安全拒绝，claim_calls=0，原件保留。
- 已停止把 JWT-only 状态查询成功当作领取可行。生产 `trae_benefits` 领取改为调用官方客户端原生 IPC 方法，后台临时启动、真实认证/设备上下文、不伪造设备头、不使用 HTTP 桥接；状态查询仍直连。完整设备信息导出未证明可用，主进程 inspect 实际只覆盖短命 CLI 启动器，已停止该路线，不在生产包加入 inspector。
- `benefit_login::native_trae_claim` 与 worker 以已加密保存的真实 SHA256 身份、客户端来源及北京时间活动 ID 校验；SDK 最多一次领取并回读；Rust decoder 验证身份、来源、HTTP/business 状态、未领→已领和调用次数。仅已领取状态可运行新 CLI `--benefits-verify-native trae_cn --data-dir ...`，该只读验收模式不会领取。PS 空 receipt 参数会造成成对参数位移，已改为仅存在 receipt 时传递，资源副本同步。
- Rust check/Clippy all-targets -D warnings 通过；脚本 29 pass、19 语法检查、13 脱敏夹具、9 文档、28 章手册通过。新增 decoder 回归和原生委托 mock，原 HTTP claim 夹具改为禁止直接调用。**新全量尚在编译**：会话 54361，`trae-native-fix-full-tests.log`、退出文件 `trae-native-fix-full-tests-exit.txt`。当前 SDK 修复源码尚未重新打包安装；旧安装版仍是首个 JWT-only 版本。
- 下一步：等全量测试结果；通过后构建 NSIS/MSI、静默安装并核验资源/194 源清单，以安装版 `--benefits-verify-native` 做严格绑定的只读真机验证，不再次领取今天已领积分；更新 Windows 任务与真实登录回读。不要把明日 00:05 的新实现发放记为已发生；今日首次定时失败与原生实际成功分开报告。
- Vite 会话 2140 已关闭，所有辅助端口/客户端已清理。桌面目录 `E:/Users/Admin/Desktop/LLM Gateway 0.2.1 (20261014 签到与体验版)` 已准备 16 文件 + SHA 清单，但**尚非最终修复包**；旧包须保存后更新，不覆盖旧两交付目录。最终增加首个定时失败、原生 100 积分成功与只读绑定验证，重算 SHA。
- 最后实查 00:48 当前聊天成功压缩 4 次，未到 5；源码修改仅本任务，其他旧代理无写入者。暂不提交/推送；原业务表 14/87/95 和 master.key/非 benefits 配置保持一致。新全量之后源码冻结；不要并行写资源和构建/消费发布产物。

### 2026-10-14 01:10 后续目标与第 5 次压缩交接

- 用户最新要求：不仅 Trae，Qoder 国际版/CN、WorkBuddy 均联网查找成熟的独立登录、权益、用量及模型调用实现；凡验证可行就实现脱离客户端。先核验能否在登录时获取完整真实认证/设备上下文并加密保存。脱离客户端不等于脱离供应商云服务；不得将开源项目宣称或只读状态成功当作真实领取/模型成功。
- 最新全量 `trae-native-fix-full-tests-exit.txt` 实读 0，日志末尾 0 failed；下一阶段汇总所有 test result 实际通过/忽略数。Clippy/check/脚本已通过。此原生后备修复尚未打包安装；旧安装仍 JWT-only，00:05 原始失败及 00:19:21 原生成功 100 积分分别保留。今日不再 claim，也不自动重试，收费模型验收需新的适用授权。
- 已联网找到参考：https://github.com/autumnsentiment/Trae2api-cn（网页登录、直连模型/签到候选，含真实 guaranteedDeviceId 设置，同时有自动换设备/重试策略，不能照搬），https://github.com/star620/TraeTools（旧 TRAE-Automatic-sign-in 重定向，签到/账号/用量候选），https://github.com/avaritiachaos/qoder-proxy（CLI/PAT 候选，须区分无需桌面与仍需 CLI）。WorkBuddy 尚待定向搜索；DSH 官方 https://github.com/deepseek-ai/deepseek-harness 可参考。只检索公开资料，不向外发送凭据/私有源码，不执行未经审阅的第三方项目。
- Trae 完整设备导出下一步：检查公共 main.js 的 fetchWithHeaders/nativeHTTP 与 iCubeDeviceRegisterChannel(getAllIds/resolveDeviceId)，或 CDP 只读观察真实状态请求；此前 --inspect 仅附着短命 CLI 启动器并非真实 main，失败不能证明不可导出。window.vscode.ipcRenderer.invoke 原生 getUserInfo/状态/claim 已真机可用；userinfo/真实设备只在内存进入本软件加密存储，不打印原始 Token/用户 ID/设备 ID，不读取第三方凭据文件。
- 当前聊天 01a1331c-9c03-7653-a296-c274d8e02da0 在 01:08 实查 count_known=true/successful_compactions=5/rotation_due=true，最后压缩 2026-10-13T17:06:56.363Z。本任务停止源码写入，旧三个代理无写入者，无 Cargo/Vite/辅助客户端仍占用。原生 Codex PID 2984/HWND 68098 最新完整 UIA 正显示本任务“验证 VPN 与 AI 工具接管 · 接续”，内容已核对用户弹窗及最新消息。原生工具不提供 create_thread 等，使用后台 UIA 新鲜 token 创建 llm-auto 新聊天，禁止持续独立 stdio 模型替代。须新 ID、count=0、正确 cwd、桌面可操作/无占用、真实下一步工具及桌面父进程归属，旧聊天归档实测后才记录成功。
- 工程/授权/安装/计划/资料/交付证据沿上述段落与验收文档，不重建台账；实际 Git 根 D:/Java/GitHub/llm-auto/llm-gateway、HEAD 9fe371308435c4a78d23d8374b9e2ffb9197827b、大量脏修改，禁止覆盖/提交/推送。统一证据目录父目录 artifacts/Project-Enhancement-20261013。下一具体操作：新聊天执行压缩计数检查、读本段，定向搜索 WorkBuddy 开源项目并审阅 Trae/Qoder 候选的公开源码/许可，随后对真实上下文进行只读直接请求对照，再按可行结果修改并完成原生后备或独立方案的打包安装和桌面交付。

### 独立接入真实进展与持续目标（2026-10-14）

- 用户追加“设定好目标，持续推进”“所有的待完成，可优化任务，都可以加到目标里面”。活动目标已创建；整体范围及 S1–S8、UX2–UX8 等后续阶段见全项目任务卡“持续目标的范围与推进顺序”，不能把本阶段交付视为整体完成。
- 原生接续尚未成功：项目专属新聊天按钮的后台点击未生效；全局新聊天按钮实际打开页面，但编辑框无 ValuePattern、后台键盘被明确 background_unavailable 拒绝；桌面同时切换到另一运行中项目，已停止操作，不输入其聊天、不使用前台回退、不归档错误聊天。交接保留，当前继续与该入口无关的本机工作。
- Trae 设备导出已实现：公共运行模块按当前 slim/full 选择同一个缓存模块，CommandsRegistry + icubeStore.serviceCollection 调原生命令 icube.common.commands.tooling.getDeviceId；取得真实 16 位设备号，IICubeEventReportService.commonParams 提供另外四个实际请求头。不是读取凭据文件、不是伪造设备、不是 inspector 进程。`trae-full-context-readonly-v5.log` 和 `trae-full-context-worker-inspect.log` 均退出 0，原生/直接状态 HTTP200/code0/checked_in=true/credits100 一致，claim_requests=0；原始失败探针保留。
- 生产 worker 新 capture-session 包含真实设备与四个上下文头，经 Rust 严格解析后只进入本软件加密存储。trae_benefits 根据已保存完整上下文选择 HTTP 直连；旧 JWT-only 上下文仍选原生后备。直连最多一次 claim + 回读，同日/金额确认，任何失败不再调原生后备或重发。新有效回归先红（启动客户端断言失败）、后绿（3 个 Trae 测试 passed/0 failed），证明完整上下文不启动客户端，拒绝/未确认均只一次。尚未完成新全量/打包安装，真实直接发放需下一领取日，今天不再 claim。
- WorkBuddy 新突破：`wb.invoke('auth:getToken')`、`wb.invoke('auth:getAccount')` 已真实返回认证/账号形状，无凭据文件读取；旧 wb.account() 工厂猜测及 __ardotAuthTestApi 均不能替代该正确原生通道（后者只是设计授权）。账号真实域 www.workbuddy.ai。`workbuddy-direct-account-readonly-v4.log` 退出0：独立 GET /v3/config 获取 27 模型，POST /v2/billing/meter/checkin-activity-status 和 get-user-resource 均 HTTP200/code0；推理/领取请求均0。首次错误模型路径404原件保留。独立 OAuth state 接口 www.codebuddy.cn 与 www.workbuddy.ai 均 HTTP200/code0（`workbuddy-independent-oauth-start.json`）；完整扫码登录、保存/刷新及模型 adapter 后续实现，不能据此声称推理已通过。
- 公开源码已按不可变 SHA 下载仅供审阅，未执行第三方项目：`public-client-protocols/repository-metadata.json`、`download-manifest.json`（27文件）。[Trae2api-cn](https://github.com/autumnsentiment/Trae2api-cn) MIT/SHA5283c94290b8503ad8af3658107a64b73eeb032f；[TraeTools](https://github.com/star620/TraeTools) GPL-3.0/SHA4da8bf773dffe1934e4516d67008941e20df7935，仅协议参考，不复制源码；[dsh-connect-qoder](https://github.com/dingminhua/dsh-connect-qoder) MIT/SHA1890fc9c4cc631b3da13593ebfd935ae47e48344；[dsh-connect-workbuddy](https://github.com/dingminhua/dsh-connect-workbuddy) MIT/SHA4b9f9ba0ac454449e5b196b79e5a0917c30a1156；[fengyinxia/qoder2api](https://github.com/fengyinxia/qoder2api) SHA067237288d6c497b16327217c35996ccf1c12008，本次 GitHub 元数据无独立 LICENSE，不照搬。一个 Sliverkiss 入口不可读，采用可读候选继续。
- Qoder 参考实现区分 CN gateway.qoder.com.cn/openapi.qoder.com.cn 与国际 api1.qoder.sh/openapi.qoder.sh，含 PAT/refresh→jobToken、COSY 加密签名和模型/SSE 路径；桌面权益 Bearer 与模型 jobToken 仍需真实转换核验。既有独立 Qoder 权益和定时保留，不能为模型接入读取第三方凭据或伪造指纹。
- 下一项：完成当前 Trae 全上下文代码的全量/Clippy/脚本、重新打包静默安装并以新安装版加密导入；客户端关闭后只读查询证明不启动它，保留旧错误及真实100收据。然后按 R5 完成 WorkBuddy/Trae 独立登录与刷新、模型适配和 Qoder COSY 接入，再依次 S/UX 队列。没有进行新的收费推理。

01:51 门禁接续：`trae-full-context-full-tests.log` 首次全量在 server_stream 的吞吐样本用例失败，退出101，首红保留；单独复现是绿的。源码约束明确过滤0ms样本，原即时回环夹具没有保证非零生成时长，轮询不能使被过滤的样本出现。只给该吞吐专用夹具增加10ms真实处理延迟并检查实测时长，保留起始0/最终1/正吞吐及完整流断言；生产采样算法不变。`throughput-fixture-green.log` 15 passed/0 failed，退出0。Clippy及完整最终重跑会话56033，日志 `trae-full-context-clippy-final.log`、`trae-full-context-full-tests-final.log` 与各自 `-exit.txt`；当前尚未打包，等待真实全量结果后执行 release:prepare/NSIS/MSI/静默安装。

私有追加备份 `D:/UserData/llm-gateway-backups/trae-full-context-20261014-015045`；安全 metadata `trae-full-context-profile-before.json`，14/87/95、master SHA与此前一致；00:05错误原收据SHA `3aec895b4df5f8d70848c01fdd276636cccf6f12b80bb86a8bf509fe70938b77`，最终必须不变。旧 v1 发布物在 `trae-release-before-full-context-20261014`，新构建不得覆盖该归档。新安装后用 `--import-benefit-client trae_cn --data-dir ...` 导入完整上下文，然后关闭客户端并 `--benefits-check-login`；禁止执行 `--benefits-once` 冒充新发放。

另外找到 [Qoder-Daily](https://github.com/L0NE-6/Qoder-Daily)，已审阅 MIT LICENSE 与设备授权源码（未执行）：设备授权网页 /device/selectAccounts 使用PKCE S256/nonce，云端 /api/v1/deviceToken/poll 换令牌，可参考首次登录与refresh。补充下载两次有界超时，已保留两个文件的本机SHA，`research-manifest.json` 明确 revision_pin_verified=false，不称不可变版本已验证。当前两版PAT环境变量均未配置；不能读取其他客户端凭据文件填补。体验参考补充 [CC Switch](https://github.com/farion1231/cc-switch)、[Cherry Studio](https://github.com/CherryHQ/cherry-studio)、[ccusage](https://github.com/ccusage/ccusage)，具体方案仍按 S/UX 任务判据落地。

### R5 拆分实施与完成条件

2026-10-14 06:00后下一批R5.5实施计划：

1. 先实现 `qoder_auth.rs` 的区域固定域名、独立PKCE设备会话、取消/过期/并发消费；保存在既有app_secrets，以qoder/global与qoder/cn命名空间隔离。设备授权的本软件安装标识与COSY真实机器上下文分开，绝不从PAT/UID派生后者，不读取其他客户端凭据文件。
2. OAuth轮询成功后必须通过当前区域 `/api/v1/userinfo` 核验用户，再原子提交自有加密device认证；到期字段只解析已确认单位，未知保留unknown。取消、超时、账号变化、重复消费、反射凭据、数据库等待期间过期均拒绝；原三个权益登录不被自动覆盖。
3. 显式与临期刷新共用单飞及持久化预约；POST `/api/v1/deviceToken/refresh` 使用device refresh，只在账号/区域匹配、新access generation成立时保存；失败保留原pair且不重放。不把device token当PAT或模型jobToken，PAT没有自动refresh。增加合成HTTP对照和首红。
4. 接入真实CLI/IPC与客户端页的登录、状态、取消及刷新，授权网址只在用户明确操作后打开；官方匿名pending/取消免费验收。完整用户网页同意和真实刷新分开记录，不能用匿名HTTP404证明完整授权成功。
5. 再实现PAT/device到模型凭据的已验证转换、纯计算COSY签名和动态目录；公开SDK底层仍启动CLI，不作为脱离CLI的最终路径。Rust固定主机HTTP，已打包Node如需只做内置crypto，不建本机翻译服务、不执行第三方代码。先真实GET目录、后适用单次收费模型验收，保持完整消息/工具/声明媒体/usage/终态，不因简单prompt回声通过就缩小目标。
6. 最后完成与权益/12:05后台任务的明确绑定迁移、完整门禁/实际安装/新日证据与自有清理。旧配置及收据保留，失败不自动重试；平台直连和S/UX全队列均保持active。

联网复验依据：[Qoder官方SDK认证](https://docs.qoder.com/cli/sdk/authentication)明确PAT、服务账号和本机CLI登录三个路径，SDK会启动CLI；[官方Cloud Agents认证](https://docs.qoder.com/cloud-agents/api/conventions/authentication)明确PAT/SAT及同区域端点，仅证明该API权限，不能推出COSY协议已验证。[公开设备协议](https://github.com/Liki4/qodercli2api/blob/native-protocol-migration/docs/oauth.md)是CLI v1.1.34合成基线与历史客户端分析，AGPL代码不复制，只复验事实；MIT候选仍保留原来源。当前本机两版公开程序资源已核实桌面 clientID 为 `732aef47-9cf2-46a2-95fe-4cebb5d0d1fa`，CN 网页域名为 `qoder.cn`；旧 CLI 示例的 clientID 和 `qoder.com.cn` 不直接采用。设备轮询与刷新字段继续保留来源边界；真实网页同意、刷新和模型仍未验，未启动平台客户端或新发模型请求。

沿既有加密账号、Tauri IPC、协议转换和 Provider 扩展，不加数据库表、不新增依赖，不改变路由权重。脱离客户端指日常请求无需平台桌面/CLI进程，平台云服务仍是模型及账户来源。

| 顺序 | 实施项 | 可失败的验收条件 | 当前状态 |
| --- | --- | --- | --- |
| R5.1 | 真实账号/设备导出与直接只读接口 | 身份/区域绑定；仅本软件加密保存；非法域名/跨账号/反射凭据拒绝；不以模块声明替代真实API | Trae完整六请求头、WorkBuddy原生认证+27模型/活动/资源已真实只读通过；Qoder权益已通过 |
| R5.2 | 认证生命周期与自动刷新 | 保存 refresh及真实到期字段；临近到期单飞；旋转结果原子加密保存；失败不覆盖仍有效旧认证；刷新不能重复模型/领取 | 待实现；现有Trae完整请求上下文不等于刷新已实现 |
| R5.3 | WorkBuddy 独立网页授权 | 固定官方区域域名、state与会话/账号绑定；最长300秒客户端等待，服务端TTL未知时单列unknown；只返回授权URL/安全状态，token/poll结果仅后台加密；过期/跨区域/重复完成拒绝 | 国际版后端和生产页面已实现并安装；官方匿名CLI与安装版5项真实IPC通过；共享认证/显式与临期刷新有回归，完整用户网页同意、真实刷新待验证 |
| R5.4 | Trae 独立网页授权 | PKCE/nonce及本机回环回调；只接受当前会话；关闭监听器；真实客户端clientID与刷新路由复验；从服务端确认身份及模型主机，不因第三方示例向未确认主机发送认证 | 待实现；本机新版本刷新路由线索 /trae/api/v3/oauth/ExchangeToken 与部分旧项目不同 |
| R5.5 | Qoder 两版设备授权/PAT及COSY | 两区域隔离，真实机器上下文；区分桌面dt/drt、PAT、模型jobToken与sash认证；签名路径/请求体绑定；RSA/AES临时会话，仅内存密钥；无CLI/桌面仍能读模型 | 两区独立设备登录/五IPC/后台命令/页面已实现并实际安装，19后端/14浏览器/原生IPC7/CLI5通过，两区匿名轮询pending；真实网页授权/刷新/jobToken/api2模型继续实施 |
| R5.6 | 三平台模型直连和协议转换 | 复用现有消息、工具、usage、预算和审计边界；流式唯一终态/异常不伪成功，非流式正确聚合；只标实际支持的模型/能力；不启动客户端或本机HTTP翻译服务；认证不入argv/日志 | WorkBuddy国际版直连及五协议流式/非流式、完整工具/声明媒体/usage/推理与拒绝已实现并安装，本地HTTP回归通过；真实完整登录/模型仍待验证。Trae/Qoder模型路径继续实施 |
| R5.7 | 配置迁移与真实模型验收 | 旧配置不被自动覆盖；候选就绪状态分账号/API/模型；不自动发送收费探针；取得适用的单次费用授权后最多各一次、无自动重试，真实usage/终态和上下文往返独立记录 | 待实现；此前Trae一次授权已耗用，不复用 |
| R5.8 | 安装交付、每日跨日与清理 | 新版实际安装、源/资源SHA、真实UI与状态查询；Trae00:05、Qoder12:05；首红/历史原件不改，下一新日发放真实验证；后台自有客户端/端口收尾 | WorkBuddy增量1320/0/19（另有10条实际跳过消息）及实际NSIS更新、MSI提取、199源文件/21资源通过；安装版5项匿名IPC和3项资料/用量通过。双时间和原收据保留；新日发放、MSI整机及重启仍未验 |
| R5.9 | 传输层重试与登录边界回归 | 权益/refresh除业务层一次预约外禁用HTTP库内置协议重试；解码后反射凭据、旧/新认证、相同access generation、取消及数据库等待期间到期均有回归 | WorkBuddy、Qoder、Trae权益及WorkBuddy模型builder禁用retry；并发刷新、取消、数据库期间到期等回归通过，已进入WorkBuddy增量安装版；真实刷新/模型无重试行为仍待一次真实验收 |

本次系统活动目标保持 active。整体持续范围同时包括全项目队列的未完成设计项、能力 S1–S8 与体验 UX2–UX8；本阶段完成后继续这些阶段，不能提前将总目标标为 complete。

### 02:14 安装交付完成后的下一阶段接续

- 全量1254 pass/0 fail/19 ignored、Clippy/fmt/脚本/文档通过；新NSIS/MSI构建退出0，NSIS真实静默安装退出0、MSI管理提取退出0，各验证21资源。194源清单SHA8a75c11e61831499428134cb6440b6128d9227614336046ffa6206299450f829，规范化EXE SHA04022506ebbe3167bf85cc90256fc75f84a10d865dfad5722c1f321a15bb56d3。失败版本与首红均保留。
- 安装版 `--benefits-verify-native trae_cn` 只读绑定通过，0领取；随后 `--import-benefit-client trae_cn` 加密保存完整上下文。客户端均关闭时 `--benefits-check-login` 实际退出0，前/过程/后观察客户端数均0；三账户有会话、查询错误为空、活动CLAIMED。不再领取今天100，不重写00:05失败原件。
- 新安装版真实UI 3项通过，`trae-full-context-real-account-usage-ui`：28日行→12模型、1个USD参考价、实际曲线截图已查看；Trae显示“完整登录上下文每日直连签到；无需运行客户端”、每日00:05、已领100及历史00:05失败。Qoder仍12:05。原资料14/87/95所有行、配置/master/失败收据SHA不变；Windows双触发器指向新安装版，LastTaskResult1仍对应原00:05失败，下一触发14日12:05。
- 桌面交付 `E:/Users/Admin/Desktop/LLM Gateway 0.2.1 (20261014 签到与体验版)` 更新完成，22个交付文件+SHA清单逐个读回验证。包括新包、手册、验收、独立查询证明、全量汇总、原失败与原生100成功分开的收据、WorkBuddy27模型只读证明及持续队列索引。旧准备版17文件另存 `desktop-prepared-before-full-context`，旧两交付目录不动，无私有资料/凭据复制。
- 所有构建/测试/自有原生GUI已退出，39361/39012/934x/9224/879x/5173无残留；源码写入仍只有当前任务，没有旧代理写入者。全局聊天5次压缩轮转尚被后台text_input/ValuePattern拒绝，未创建/归档新ID；`native-continuation-attempt.json`明确未通过，无独立模型进程。按技能继续不依赖该入口的工作，不前台回退、不伪报完成。未提交/推送。
- **下一具体操作**：先检查实际线程压缩状态与Git脏状态，读上表R5.2–R5.6；将 WorkBuddy 正确 `wb.invoke('auth:getToken'/'auth:getAccount')` 原生入口收敛为生产导入组件，并扩展本软件加密凭据/安全返回，核验独立OAuth和刷新字段；按现有协议层实现 WorkBuddy 流式/非流式、usage、工具等真实支持能力，禁止只有prompt/text的包装器冒充完整上下文。随后Trae网页OAuth/模型目录主机真实性与Remote/IDE协议、Qoder两版设备/PAT/COSY。准备好可审查的模型适配后再取得适用的单次收费验证授权，其余免费实现继续；再进入S1–S8/UX2–UX8。活动目标不标complete。

### 2026-10-14 03:10 WorkBuddy 独立授权后端接续

- 用户追加的所有未完成、可优化项已纳入[统一持续任务索引](持续目标与任务索引.md)，接入AGENTS入口及文档门禁；10份规划校验通过。原卡历史“待执行”须对照当前生产实现，已完成的能力页、成本评分和级联不重复重建。
- 新 `workbuddy_auth.rs` 及headless start/poll/cancel/status/显式refresh已实现，独立复审问题均已修复；17项新授权回归、相关29项通过；全量70汇总/1272 passed/0 failed/19 ignored，exit0，Clippy all-targets、fmt、debug build退出0。`workbuddy-auth-test-summary.json`记录最终数值。无生产页面变化，未追加UI截图。
- 新debug CLI匿名start→pending→cancel→not_logged_in真机退出0，客户端前/中/后0、0模型/领取/刷新、0浏览器或客户端启动；`workbuddy-auth-live-proof.json`。私有资料副本取消后pending/session行0；原真实资料四表全行摘要、config/master/00:05首红收据、已安装EXE不变。没有完成网页同意或真实refresh，不能把匿名流程说成完整登录通过。
- WorkBuddy国际版固定www.workbuddy.ai、官方workbuddy-ai/300秒等待、真实expiry或unknown；登录绑定uid/企业，原认证与pending事务CAS，取消/SQLite等待期间到期不可覆盖。刷新一次预约、失败保留旧pair、无相同access generation重放；新access可合法沿用未旋转refresh。WorkBuddy/Qoder/Trae权益HTTP库retry显式关闭。新源码尚未打包，不覆盖02:09已验收安装版或桌面交付。
- 父目录 `workbuddy-protocol-audit.md/json`、`qoder-protocol-audit.md/json`、`workbuddy-auth-review.md`及 `workbuddy-direct-model-plan.md`为只读协议/复审/下一实施方案。官方refresh后用accounts数组，Qoder不同会话族与双重填充、原AgentAdapter的tools/多模态丢失都已保留具体证据。技术方案不是模型真实验收。
- 最后实查当前线程成功压缩6次、count_known=true；内置原生创建/交接/归档能力未提供、后台文字输入已实际拒绝，沿已有 `native-continuation-attempt.json` 保留阻塞，不前台回退、不用独立模型服务替代。按接续技能继续独立工作，轮转仍未通过。源码写入仅根任务，代理均只读。
- 全量Cargo、rustc/link、自有客户端/Gateway均已退出，9224/934x/879x/5173/39012无监听；五项Run仍不存在，无重启验收。Windows任务LastRun14日00:05:01/Result1仍为原失败，NextRun14日12:05，Trae下一新日直连发放未发生。不重写收据、不再次领取今天100、不提交推送。
- **下一具体操作**：复验线程/工作区，执行父目录WorkBuddy模型计划并补生产登录页面；当前Credential尚无原始uid，需从官方已验证账号响应加密保存，旧会话按hash+enterprise安全回补或重新登录，不能逆推hash。`workbuddy-model`仅候选明确transport方案，需旧runtime及CLI拒绝对照，不自动改桥接配置。模型直连覆盖非流式分派、stream候选及实际发送、provider绑定验证三处，固定主机、完整ChatRequest、唯一DONE/error、reasoning、工具片段、usage及独立credit单位；准备好具体适配再取得单次收费验证授权。随后R5其余平台、S1–S8/UX2–UX8及全部索引队列；目标保持active。

本阶段详细判据和实际边界见[WorkBuddy独立授权后端验收](2026-10-14-WorkBuddy独立授权后端验收.md)。

### 2026-10-14 04:07 WorkBuddy 独立模型生产接入

- 新生产路径使用 `workbuddy-model` 账号运行时：独立网页登录页面、六个Tauri命令、加密UID/认证、临期并发单次刷新、固定官方模型接口与动态目录。普通API及旧CLI/桥接绑定保持独立；不自动迁移已有配置或发送模型测试。
- 注册账号/供应商/新模型使用同一SQLite事务，重复登记仅追加目录新项，保留用户价格、别名、停用、参数、能力账本及旧绑定；任何冲突或插入失败回滚。独立DB回归10项已写，等待下述全量执行结果。
- 完整ChatRequest保留历史/工具/图片及目录明确声明的媒体能力；SSE按字节解析、唯一终态、异常不假成功；非流式聚合reasoning/refusal/usage/工具。credit保留供应商积分单位，不能当成货币或免费。运行时别名后的最终目录再次校验能力、消息+工具schema输入预算和最终输出上限；未知模型/不支持请求零模型POST。
- 独立复审确认的四项工具/目录缺陷均已修复并静态复验，原finding与修复保留父目录 `workbuddy-direct-review.md`。五出口真实本地网关测试另发现Gemini入站原来丢弃工具，已补functionDeclarations与functionCall/Response历史绑定。内置执行工具及多模态工具结果暂明确拒绝，不静默丢内容。
- `npm run build`退出0；完整浏览器UI脚本退出0，新增网页登录、明确打开官方网页、迟到start取消、轮询失败停止、失败取消重试、超时迟到结果拒绝、错误官方域名拒绝、登记后桥接读取失败隔离及窄屏暗色截图均有实际浏览器证据。新截图已查看并复制父目录 `workbuddy-direct-ui/`。脚本29/0/0、19语法、13夹具脱敏及10规划校验通过。
- 专项 `cargo test --lib workbuddy`实际40 passed/0 failed/0 ignored，父目录 `workbuddy-direct-focused-tests.log`。随后新增schema/legacy预算场景与Gemini历史ID回归，最终全量 `cargo test --jobs 1 -- --nocapture`已启动，session88847，日志 `workbuddy-direct-full-tests.log`；**尚未得到最终数值，不能把旧1272或专项40当成最新全量通过**。其后须执行Clippy/fmt、生产debug/native页面验证、完整包与实际安装，更新验收及桌面交付。
- 已安装EXE和桌面交付仍为02:09已验收版本，尚未包含本阶段新功能；原账号资料、00:05首红与原生100独立收据不动。本阶段所有模型HTTP均为本机合成夹具，无真实模型调用/重复领取；真实完整网页同意、真实刷新及真实模型终态仍分别未验。
- 当前线程ID不变，最新实际成功压缩7次、count_known=true；内置创建/交接/归档仍未提供，后台输入限制证据沿原记录。未成功轮转，不前台回退；根任务继续独立推进，所有实现代理已freeze，当前复审代理只读完成。Vite会话40248为本任务测试服务，收尾仅关闭自有实例；不提交推送。
- **下一具体操作**：等待88847全量结果，若失败按首红修复后复跑对应门禁；全绿后依次Clippy/fmt、debug匿名/本地安全IPC验证、新包真实安装与交付，并准备适用单次真实授权验证。继续Trae/Qoder独立OAuth/模型协议、WorkBuddy权益/用量，随后持续索引全部S/UX/N/V任务。总目标active。

#### 04:24 门禁首红及接续更新

- 88847构建后库测试469/0/0；随后 `accounts_http` 的停用运行时回归失败（原合同4xx，新分派误映射5xx），全量因此未通过。旧失败日志保留。已将直连/CLI运行时配置错误恢复为CapabilityUnavailable；定向 `accounts_http`真实2 passed/0 failed，日志 `workbuddy-direct-runtime-contract-tests.log`。
- 最新完整执行采用 `cargo test --jobs 1 --no-fail-fast -- --nocapture`，session29936，日志 `workbuddy-direct-full-tests-final.log`、最终退出文件同名前缀；截至04:24仍构建中，不预报总数或成功。源码fmt已执行，后续Clippy及安装仍未验。
- 内置/Markdown/HTML手册已同步独立账号流程，`docs:manual`真实28章节退出0；前端最新build退出0、完整UI已再次通过，随后账号输入补明确64字符及允许字符帮助，最新UI会话83373正在复跑。Vite40248仍为本任务自有服务，收尾按归属关闭。
- 五出口真实网关工具流通过，同时修复Gemini入站原来丢tools的既有缺陷；functionDeclarations和functionCall/Response绑定保留真实ID，未知内置执行工具/不可转换的多模态工具结果明确拒绝。最新schema和legacy functions零POST以及这些Gemini回归已加入上述完整门禁。
- 下一阶段只读能力审计已复验普通provider保存丢capability_sources_json、目录能力导入及思考声明消费缺口，沿S1–S8原卡推进，具体证据由父目录 `capability-next-audit.md`维护，不重建已有功能。全部待完成与优化项仍属于持续目标。

### 2026-10-14 Qoder 两区独立设备登录阶段

后续优化已纳入同一R5.5队列：切换账号/离开页面/超时时，区分“前端已停止等待”和“后台取消已确认”，清理失败保留可恢复入口；缩小区域间网络等待的互相影响，同时保持数据库CAS、取消、到期和刷新单次边界。两项为本轮只读复审建议，不是已复现的P1/P2缺陷；实施前建立迟到响应、未知取消结果和跨区域并发的可失败验收。

#### 07:24 协议与 WorkBuddy 真实授权增量

- 当前 Qoder 公开程序另证实设备 `expire_time` / `refresh_token_expire_time` 是毫秒；已补解析与到期拒绝回归。首红 `qoder-expiry-red.log` 为0/1/exit101，补齐后专项19/0/0；增量只读复审通过。补字段后的完整 Cargo 1350/0/19/exit0，另10条显式跳过；此数值属于下面 WorkBuddy 首system修复之前，不能复用成最新源码全部通过。
- 当前 Qoder 模型路径进一步定位同区域 `/api/v1/me/jobToken` 签发与 `/api/v1/jobToken/refresh`，不同于旧MIT api1/PAT交换。设备与jobToken的相对到期字段单位不能混用。源包事实及下一实现入口见父目录 `qoder-current-protocol-delta.md`，无 Qoder真实授权/刷新/模型请求。
- 用户已明确允许 WorkBuddy官方网页，并授权一次真实“只回复 OK”。网页登录成功，身份在官方服务核验，原应用加密保存新增一个独立WorkBuddy会话。原3权益认证、14供应商/87模型/95请求、config/master与Trae14日首红原件均保留，证明 `workbuddy-approved-profile-proof.json`。首次GUI程序的PowerShell捕获未得到stdout，只产生一个未完成授权记录；已取消该旧记录，正确捕获后仅打开新官方授权页，没有自动重放模型。
- 已安装S1版在独立测试资料目录进行一次真实模型，先官方目录27项并事务登记，再固定 `deepseek-v4.1-flash`，关闭failover/auth复测/cache/smart/search/领取；用户正文仅“只回复 OK”，max32，stream。实际单次上游HTTP400/code11128/90ms，未返回Done/回复/usage；本应用审计96号只有一个attempt、retryable=false。收据 SHA `747efe9d01e8e6f1ca37e4affc58749c54670d4192d55160be06238df5f1c68d` 不再改写；未扣减证据不等于免费。一次授权已用，不发送第二次。
- 找到本适配器未补国际版首条system的可复现合同缺口，与公开MIT作者记录的11128吻合，但不宣称是本次真实拒绝的唯一原因。仅WorkBuddy路径在缺首system时加中性消息，原系统、用户正文、工具/媒体/历史次序保留，隐式消息计入模型输入预算。首红 `workbuddy-system-head-red.log` 0/1/101；定向41/0/0/exit0，五协议mock加首system断言；独立只读复审通过。修复后平台结果仍未验，不能把这些通过替代真实成功。
- 最新完整 Cargo `qoder-workbuddy-full-tests-final.log`（session24682）仍执行。后续依次最新Clippy/fmt、包/实际安装/Qoder原生匿名IPC及WorkBuddy真实登录读回，完成具体可审查的新版本后才追加申请一次收费验证；总目标active。
- 新发现同时进入R5.6/UX3原队列：`commands::test_provider`目前直接使用普通UpstreamClient，不经过账号运行时解析，WorkBuddy独立供应商的测试按钮不等价于生产账号模型路径。后续复用已存在的账号dispatch并加入禁用/未知运行时/别名/零重试对照，不再建立第二套运行时解析。失败审计的安全业务码/单次attempt/费用未知也加入UX错误处理验收。

- 根任务为唯一源码写入者；原账号、领取配置、原失败收据不变。新 `qoder_auth` 复用既有 app_secrets，region 固定主机、独立本软件设备ID/PKCE、32会话上限、五分钟本机等待、原子消费与 generation CAS。取消/到期/资料变更不能被迟到响应覆盖；必须同区域 userinfo 核验 UID/组织。设备ID不宣称为 COSY 原生机器指纹。
- 刷新只按用户操作执行，本代凭据先持久化预留；失败或未知不重放，旧 pair 保留。新认证须重新核验身份/组织，更新事务拿到 SQLite 写锁后再次检查到期。仅已核实 RFC3339 或相对秒数解析，到期单位未知保留 null。
- 五个 `qoder_login_*` Tauri命令及同名后台CLI命令共用服务；CLI要求现有绝对 data-dir，参数只接受 `global:本机账号` 或 `cn:本机账号`，轮询/取消使用对应会话ID。秘密不入argv或DTO。页面进入只读本机登录状态，官方授权网页需要明确点击，失败即停检查，取消失败可重试取消，双区域独立显示。
- 已通过：17后端专项、cargo check、Clippy all-targets -D warnings、前端build、14浏览器关键交互、29脚本/19语法/13脱敏/10规划/28手册。首次浏览器断言误把含掩码邮箱的同一dd要求完全匹配姓名，失败日志保留，修正姓名定位后专项退出0；不得把首红抹去。新增截图已查看，390暗色无横溢出。
- 正在执行完整 Cargo门禁 `qoder-login-full-tests.log`（session82898）与完整浏览器 `qoder-login-full-ui.log`（session97042），此记录时无最终数值，不能把专项/旧安装证据替代全量或真实登录。后续先完成只读复审、全量结论，再包/实际安装/安装版匿名IPC和账号保全。
- 当前线程实查成功压缩9次，最后持久化记录 `2026-10-13T22:41:26.647Z`。原生创建/交接/归档工具未提供，既有后台输入拒绝证据保留；未轮转，不前台回退。下一具体操作是读取上述全量结果并处理复审，然后建立安装版Qoder匿名会话/取消验证。总目标 active；R5其余/S1–S8/UX2–UX8/N/V及新发现优化全在持续索引。


#### 08:00 安装交付与接续

- 最终源码完整Cargo72组1351 passed/0 failed/19 ignored/exit0，另10条外部实际跳过；Clippy all-targets -D warnings/fmt通过，完整浏览器141新图/exit0，专项19Qoder/41WorkBuddy/14浏览器通过，两个独立复审均完成。
- 新包真实NSIS更新0，201生产源SHA8705da19182eae455bc0e71479aec40c0ee594f283947a2f1846974d2c31f08b；NSIS/MSI提取21资源及规范化EXE5065e05ab8ef0b00ded67c7ef08883631db353a862c375846cf4d618a6801656通过。首次MSI斜杠路径1619保留，原生Windows路径提取0；没有MSI整机安装或重启验证。
- 安装版Qoder真实IPC7/CLI5、两区各一次匿名pending通过；Tauri不可变invoke导致计数监听不支持，v2记录null不记作0。WorkBuddy原账号真登录读回3项、窗口LLM Gateway/RespondingTrue/healthz200/监听归属通过，未再次请求模型/领取/refresh。模型一次400/code11128原件不改，不将修复后mock通过冒真实成功。
- 原3权益认证/14供应商/87模型/95请求、master/config/Trae14日原首红不变，仅新增本次授权WorkBuddy登录一条加密会话。双时间Windows任务Ready仍00:05/12:05，原LastResult1保留，Next14日12:05；五Run仍 absent。自有Vite42112与全部测试窗口/进程已退出，5173/15721/39012/39361/39362无残留监听，用户浏览器不整体关闭。
- 下一具体操作：当前Qoder公开主程序已定位同区域POST /api/v1/me/jobToken（设备Bearer+clientId）和POST /api/v1/jobToken/refresh（模型refresh_token）；先实现自有加密模型认证预约/代次/区域绑定、独立到期解析与离线回归，再核定api2目录/完整请求/权威终态。设备与模型expires_in单位差异不照抄；不从旧PAT/COSY api1、UID或随机值拼原生机器上下文，不启动CLI作为最终替代。Workbench修复后真模型要新一次授权，原收据及首次授权不复用。剩余所有S/UX/N/V及原卡继续active。
- 当前线程真实压缩9次、最后2026-10-13T22:41:26.647Z；原生创建/交接/归档未提供，后台输入拒绝沿原receipt；未完成轮转，未前台回退。源仅根任务写入，review代理已只读完成；无提交/推送。

完整边界见[本阶段验收](2026-10-14-Qoder独立登录与WorkBuddy协议修复验收.md)。

08:11继续核定的区域差异：Global当前推理域api2.qoder.sh；CN同版本公开主程序在authBaseUrl=qoder.cn同一配置指向gateway.qoder.com.cn，不能统一为api2，也不使用test-gateway。事实证据在父目录qoder-current-region-service-proof.json。下一批固定两区各自生产目标，分别核定请求与权威终态。两个新授权问题已经提出：WorkBuddy修复后再一次模型（协议会补中性system）以及打开Qoder两区官方网页；尚未收到答复时均不执行。已用的一次WorkBuddy400授权/原收据保留，不能重放。

#### R5.5 模型认证增量计划（2026-10-14 接续）

1. 在既有 QoderAuth 中区分设备认证和 jobToken；固定区域 API、clientId 与用途，模型有效期按已核定的毫秒合同独立解析，不猜默认有效期。
2. 使用既有加密 app_secrets 保存模型 pair 与单次预约；绑定设备代次、UID/组织/本软件安装ID。签发、刷新失败或结果未知不重放，取消与迟到响应不能恢复已取消认证，账号变化不能被覆盖。
3. 增加只返回安全状态的后台 status/prepare/refresh/cancel 入口，不在 argv、IPC、日志返回秘密。离线验证双区域合同、单飞、跨服务CAS、异常保留旧pair、过期/取消/账号变更及反射凭据拒绝。
4. 认证通过仅代表模型凭据准备完成；继续核定并实现两区推理协议、完整历史/工具/媒体/usage和权威终态后再标模型可用。真实网页登录和收费请求分别沿既有授权边界验收。

当前唯一源码写入者仍为根任务。WorkBuddy既有一次授权已由400收据消费，追加授权问题仍待答复，不重复打开官方页或发送模型请求。既有安装版本与201源/1351测试证据保持为上一阶段，不借用到本批新增源码。

#### 09:00 追加授权、真实失败与字段修复接续

- 用户明确回复允许一次新的WorkBuddy模型请求与Qoder两版官方网页。WorkBuddy新隔离资料目录使用首system修复安装版，实际目录24项，08:42只POST一次deepseek-v4.1-flash“只回复 OK”，网关502、无Done/回复/usage；审计reason为本适配器finish_reason无效，不能归因成已确认vendor502。第二收据SHA bd21f25475004dd4985200f2d74fc5b0498818602b5bc50024a00cb08e6885ce与原400收据不改，不第三次请求；原账号请求记录仍95。
- 两区首轮真实poll取得认证后userinfo身份解析拒绝。公开当前main fetchUser支持id优先的官方字段，新增id/username和扁平/嵌套org兼容，严格冲突/无效类型/反射凭据仍拒绝；首红101，后端32/0/0。首轮失败原件保留、pending已取消，不重放未知poll。
- 空中间finish_reason同样会触发WorkBuddy原解析错误，已按非终态兼容，仍必须真实finish+DONE；缺终态及数字reason即使后接正常终态也拒绝。首红101；42/0/0包含五协议HTTP夹具首片段的空reason回归。未捕获真实原始帧，不宣称这是唯一真因或修复后真实模型已通过。
- 最新library505/0/0、Clippy all-targets -D warnings/exit0、fmt/规划/脱敏通过。Job阶段全量72组1363/0/19，另10真实外部跳过，属于上述两个修复前；新完整全量qoder-workbuddy-auth-followup-full-tests.log（session20143）执行中。
- job后台四命令原生12项通过，均无真实认证/模型/领取。userinfo修复native构建0，debug EXE SHA ce75b9b4cca81d19069fb6f9378cba80c32843f709c7935cc243e849b32928ac。独立不可变副本D:/UserData/llm-gateway-backups/auth-native-20261014-0859/llm-gateway.exe可供用户登录继续，避免全量构建覆盖正在使用的CLI。
- 修复后Qoder两版官方新网页已打开，但五分钟内持续pending，09:00前watch已分别记录expired_without_verified_login并取消pending，未保存已登录或启动客户端。已通过异步问题询问用户页面状态；新登录需要用户实际同意这一外部步骤，不把超时当批准，后端/全量/协议实施继续。
- 新资料保护器精确允许已授权的自有登录记录，原三权益认证、14/87/95、config/master、Trae14日首红及两份WorkBuddy收据原件均通过；未新增原账号模型请求或签到。系统安装版仍是201生产源前阶段，未把debug快照说成安装更新。
- 新优化进入R5.2：认证签发成功后资料验证失败，应设计用途隔离的加密候选检查点，让恢复只重做安全userinfo核验而不重放单次token签发/轮詢；候选不能进入已登录或模型调用。必须先覆盖取消/过期/账号代次/异常的边界，不直接降低现有验证。
- R5.5/R5.6下一实施同时补齐私有模型身份上下文：公开COSY协议的encrypt_user_info需要真正userinfo字段，前端AccountSnapshot的masked_email不能用于签名，也不能从UID/PAT/随机值拼原生机器上下文。仅从本次固定域名/已验证身份的自有HTTP响应获取必要字段，目的隔离地加密保存，不增加公开DTO泄露；旧模型记录缺上下文时只能重新进行安全资料核验，不能自动重新签发或发送模型。再对齐当前两区worker的真实机器/版本/模型目录和请求合同，不把旧CLI/API1格式作为新API2已验证合同。

阶段细节见[Qoder模型认证阶段验收](2026-10-14-Qoder模型认证阶段验收.md)。总目标继续active，全部S/UX/N/V及原卡不删减。下一具体操作：读取session20143全量结果，等用户就新网页状态答复时使用已验证不可变native副本完成Qoder账号核验，再继续区域固定jobToken/api2完整模型协议；额外收费验证须另有未消费授权。仅根任务写源码，未提交推送。


2026-10-14 10:24 两区真实独立登录已完成；每区一次job签发后，新增job自身主体验证与旧pair verification_required兼容保护，真实一次refresh补验均成功。原资料/两份WorkBuddy失败收据保持完整，零Qoder模型/领取/自动重试。最新全量1367/0/19，另10真实外部跳过；新NSIS实际安装/21资源与MSI行政提取通过，202生产源，清单SHA d007dddd602324f6a038e2c255956663808e74d75bbe5e3e6ec865da99d44cbe。新格式旧二进制不可读，私有资料及旧安装备份已保留。证据与全部边界见[模型认证阶段验收](2026-10-14-Qoder模型认证阶段验收.md)，下一项签名目录/区域绑定/完整模型协议，全部目标active。


2026-10-14 11:45 WorkBuddy第三次新增单次授权真实模型已通过：HTTP200/OK/stop/DONE1，输入14输出1总15、reported credit0，一条审计/一次attempt/零重试。正式注册24目录模型后15供应商/111模型/95原请求，旧各行完整保留；一次正式登录刷新及同一身份核验通过。三份模型收据原件保全，额外模型授权已使用，真实工具/媒体未验。Qoder独立COSY签名/codec管道11项离线通过，未打包/接Rust/发真实目录；最小私有签名上下文不含原始姓名邮箱。继续R5.5真实目录和完整模型协议。详见[WorkBuddy直连验收](2026-10-14-WorkBuddy模型直连与安装验收.md)。


2026-10-14 12:05 定时实际验收：Windows任务在12:05:01自行执行，Qoder国际版与CN各新增今日原收据，平台确认各100积分（CREDITS）、granted=true、replayed=false、manual=false。核验仅读收据与任务状态，没有手工补领、重复领取或模型请求。两区发放时间12:05:04/05。任务总退出码1和last-run partial_error来自保留Trae当日00:05失败原收据，程序报告receipt_exists_no_retry；不能据总退出码把两区成功说成失败，也不能覆盖旧失败制造全绿。下一次任务为10月15日00:05，双时间设置保留。原收据SHA及安全汇总见父目录artifacts/Project-Enhancement-20261013/qoder-scheduled-20261014-1205-proof.json。


## 2026-10-14 12:22 独立目录阶段接续

Qoder独立COSY认证已接Rust私有管道、固定区域目录GET、CLI --qoder-model-catalog/--qoder-signing-prepare及Tauri qoder_model_catalog。以自有资料的隔离克隆、真实debug二进制执行，两区分别仅读取一次官方目录：Global17/CN14；并验证本应用的job-signing私有加密上下文各一份，正式供应商/模型/请求/认证各表及config/master不变。无SDK/第三方客户端/本地HTTP桥接/第三方凭据读取，无模型、领取、认证刷新或自动重试。此前模型授权已消费；下一次收费模型请求需新的适用授权。

签名用途绑定region/issuer/device代次/job access代次、账号/组织/自有安装UUID/有效期；只接受principal_verified，JobToken自身userinfo再次核验，组织标签使用该Bearer，隐私默认false且不发同意。Node纯原生crypto，env_clear、私有stdin/stdout/stderr，8MiB入/4MiB出/4KiB错误/10秒限制，超时及取消只清理自有进程。新安装仅包含cosy.cjs/worker.cjs，不含SDK/WASM/合成oracle。当前程序中公开WASM仅用于隔离合成对照。

目录只返回模型ID/显示名/格式、声明的enabled/vision/thinking/free及credit_multiplier；缺失保留None。该倍率是积分倍率，不是现金价格。2MiB响应、固定区域主机、拒绝重定向/重试；签名前后和目录返回后检查认证代次/到期。公有DTO前拒绝明文认证、RSA封装key、encrypt_user_info及本次Authorization/Cosy-Key的反射。独立复审发现的缓存等待跨expiry和反射遗漏均以真实RED exit101暴露，修复后46项原生专项/0失败/0忽略；40脚本/23语法/13夹具，tsc/Vite、Clippy、112 Rust日志扫描+11自测、10规划和28手册章节门禁通过。

完整Cargo session64941当前仍编译执行，不能把专项46视为完整通过；本阶段安装尚未执行，正式安装仍是此前1367回归/202源/21资源版。release:prepare新211源和两签名资源已备齐，安装/原生IPC/MSI提取脚本已准备。真实目录成功不代表模型推理、完整历史、工具/媒体、usage或event:finish权威终态已验证。下一阶段独立模型协议仍需完成；不注册尚不能调用的Qoder供应商。

12:05任务已实际自动领取两区各100积分，平台granted且非replayed；Trae原00:05失败收据仍保留，总任务exit1仅反映该旧失败，无重复领取。正式WorkBuddy第三次请求HTTP200/OK/14输入1输出且授权已用，三原收据不可重放。当前正式15供应商/111模型/95请求；全部R5/S/UX/N/V与原18卡目标active。

聊天持久化成功压缩12次，最后2026-10-14T03:49:53.577Z。仍无原生创建/交接/归档工具，先前后台入口拒绝，未完成原生轮转；沿已有交接继续独立工作，不使用独立app-server模型控制器替代桌面。


12:38 全量验证新跟踪：首轮route_headers两项夹具失败，独立原样两次14/14通过；第二全量在gemini_inbound遇Windows10055（HTTP发送前），未证明全机资源耗尽。单轮串行全量session60378执行中，保留全部断言和专项内部并发。追加夹具改进：端口持有/启动错误传播/健康响应归属核验，当前只是竞态候选，不能据此声称修复。原失败日志与调查见父目录artifacts/Project-Enhancement-20261013/qoder-catalog-full-validation-investigation.md。


## 独立目录真实安装与交付（2026-10-14 12:55）

本阶段实际NSIS静默安装exit0；安装EXE SHA 106ab89fbfcfd4c25420fe4e6fe2f365b5ac705bf5fd9e102955a77fe63493c5，规范化SHA 9e069b596b8e0eade0e48a71615a9d97bcf5e99363e1fe2f8ca2beed7d5ccf9c。原安装完整备份D:/UserData/llm-gateway-backups/installed-before-independent-catalog-20261014-1230。211生产输入SHA 4db54770a12a622d5d3c5c12ef65c3947aaee15c61547308e8000427b64e6a34、23资源逐字节通过；MSI管理提取同规范化EXE/DLL/23资源通过，未作MSI整机安装或重启测试。

真实安装版Tauri IPC在自有隔离资料目录再次读取Global17/CN14，四项原生检查通过：保存登录和model凭据状态、真实固定域目录、真实窗口标题/Responding/healthz200及监听归属、两区登录徽标/页面无pageerror与截图。自有应用和探针退出，测试没有调用模型、领取、刷新认证或自动重试；正式15供应商/111模型/95请求、12认证记录及config/master原件不变，三份WorkBuddy模型收据/Trae失败原件SHA保持。签名上下文两份仅在隔离克隆保存。

安装前完整串行Cargo72组1379 passed/0failed/19ignored，另10外部服务未启动的字面跳过，不能称外部真机通过；新Qoder专项46/0/0、脚本40/0/0和23语法、13夹具、前端构建/Clippy/日志/规划/手册均通过。此前默认并行两次失败原日志保留：一次路由夹具计数/审计污染候选，一次Gemini发送前Windows10055；不伪称它们已全部确定根因。安装后已将route_headers夹具改成既有port0+本实例listener_snapshot、传播serve错误及持有自有任务，16项默认并行通过、两个新增回归验证端口独立/自有关闭不影响另一个、实际绑定错误不能冒充健康成功。该修改只涉及测试，不改变本次211生产输入。完整默认并行session18458执行中，不能提前声明通过。

真实目录和独立认证完成并已安装；Qoder实际推理、完整历史/工具/媒体、usage及event:finish仍在下一阶段实施。模型目录倍率是积分倍率，不是现金单价。当前未注册无法调用的Qoder模型供应商。WorkBuddy第三次一次授权已用，成功文本实测仍为之前已安装的同一模型适配；本阶段未新增WorkBuddy模型请求。总目标仍active。


2026-10-14 13:04 默认并行全量最终通过：72组1381 passed/0failed/19ignored，另10外部服务缺席的字面跳过，不能作外部真机通过；Clippy/fmt0。首次两份默认并行失败日志保留，路由夹具三个确定隔离缺口已修并通过16项默认并行，首事故当时根因仍不追认。新安装版211生产源/23资源、两区目录17/14、真实原生IPC4项、自有应用退出及正式资料/收据/双触发保护均通过；没有新增模型请求。真实目录认证阶段完成，完整模型协议继续实施。源码未提交推送。


13:14 接续记录，完整模型下一阶段已经开始：新独立inference.cjs与3项合成回归复现当前298606-byte模块的两区固定推理URL、22个头、custom编码body和canonical MD5（签encoded body而非raw JSON）；验证原始多轮/工具/媒体JSON字节不被改写，拒绝model/source与目标不一致。仅证明低层签名编码和字节保留，不证明真实高层RemoteChatAsk schema或模型功能。新三文件未接worker/Rust传输、未打包，当前已安装的目录版211源/23资源保持有效；未发Qoder模型请求。继续完整IR/请求体及权威event:finish、模型注册/预算/审计通路，不复用已消费的WorkBuddy授权。


2026-10-14 完整请求/流解析源码阶段已验证：67 Qoder原生专项、44脚本、25语法、13夹具、114 Rust日志扫描+11自测、最终默认并行72组1402/0/19（另10外部跳过），Clippy/fmt/规划退出0。三个独立复审P2均真实首红再修正：跨帧认证反射、正文晚于终态、4MiB raw二次转义信封容量。正式安装仍为已验证的独立目录版，未安装本阶段源码，真实Qoder模型调用未验。接续固定区域模型HTTP、认证代次/取消/到期、非流式聚合、账号运行时/注册/预算/审计；无新增费用授权可复用。详细[源码阶段验收](2026-10-14-Qoder完整请求与流式解析阶段验收.md)，全部目标active。当前聊天成功压缩13次，原生轮转尚未验证。


2026-10-14 17:25 两区首次独立真实文本请求：用户分区各授权一次，Global Qwen3.7-Plus HTTP200/OK/DONE/stop、输入24输出167含推理164、积分0.004942857142857143；CN DeepSeek-Flash HTTP200/DONE/stop但正文“好的”未满足精确OK、输入39输出17含推理15、积分0.00475508。每区POST1/attempt1/fallback0，授权均已消费，不自动重试。Global收据SHA28abd824262c943872b136a933268dd9866e281af148c50e647cb20a6ef11477，CN收据SHA6f6b47ad40ea87a4461df05a8849e2eed9751f78151d69effb9ca252cc01b939，原件保留。正式15供应商/111模型/95请求及旧收据不变；两份私有审计与正式账本分开。两个自有应用退出，端口/自启/双时间守卫通过。正文传输和usage有真实证据，完整历史/工具/媒体及32总输出上限未验；CN字面反馈不通过，不能由HTTP成功宣称任务正确。详见[模型直连验收](2026-10-14-Qoder模型直连与安装验收.md)，全部目标active。


2026-10-14 18:35 Trae独立登录官方合同与准备代码：最新Trae2api-cn公开main仍5283c94290b8503ad8af3658107a64b73eeb032f，官方授权页及精确authorization懒加载chunk匿名GET200、SHA固定，确认PKCE/authCodeInfo/loginTraceID；官方公开main.js SHA9043e042e98bb5ef2e97183097b7ad6f2f7c2d951165b772299aef1a0adff961确认V2 /trae/api/v3/oauth/ExchangeToken及P256设备证明。普通IDE与Solo客户端身份不同；刷新签名材料是POST、固定路径、ClientID、RefreshToken、Unix秒、随机16字节hex nonce，以LF拼接，再ECDSA/SHA256/DER-base64。不能搬用参考项目的随机设备/Mac14字段，不能用新密钥刷新原客户端令牌，也不能读取客户端私钥。

scripts/trae-protocol新增自主实现的PKCE/P256密钥与刷新证明、64KiB单次私有worker，6项纯本地回归通过，固定官方public函数IPe/NPe的隔离oracle再次验证48字节PKCE与LF签名格式；未初始化SDK、未运行客户端、未写私钥或发送登录/模型/领取。该准备组件尚未接Rust/IPC/UI/资源包，不能宣称完整独立登录已实现。下一项实现一次性loopback回调、PKCE V2交换、服务端主体核验、加密设备密钥/账号串行刷新和取消/过期CAS；复用旧真实DeviceID登记新公钥可能影响旧客户端绑定的服务端语义尚不明，真实授权前必须明确并保留原绑定，不能偷偷替换。当前只读审计/官方CDN资料及oracle见父artifacts/Project-Enhancement-20261013/trae-independent-login-audit-20261014.json、trae-independent-login-official-contract.json、trae-native-v2-auth-public-contract.json、trae-native-crypto-oracle-proof.json。整体目标active，所有费用授权已消费。


2026-10-14 18:40 思考映射最终安装：完整73组1432/0/19、另10外部实际跳过，94 Qoder专项、Clippy/fmt/日志/规划通过；前端资产与此前生产Qoder专项一致，无页面改动。实际NSIS静默安装exit0，安装EXE SHAd89450ba85eb09ec1de07880098f3d0f3256fb2b4781fd94d6cc0007525e3f88、规范化SHA49c32900412f2062333614a4f9461335962c81d1855f907f03fd9a93c9bdd359，NSIS及MSI管理提取24资源逐字节通过（未MSI整机安装/重启）。当前220生产输入SHA44c927655754e5ff5f415cc76d749a2435fbbf3f35206a5096d1c3bae7cf6cbe全部匹配。新包免费原生7项、真实目录17/14及登记/发现/绑定/保留配置/页面/自有退出通过；未发送模型/领取/认证刷新。两区已消费的Global200/OK与CN200/“好的”原收据保留，修补的关闭/精确预算字段尚未经真实云端参数验收，32总输出与完整历史/工具/媒体未证明。Trae PKCE/P256准备脚本另有6回归及官方pure oracle通过，但未接Rust/IPC/包，不能称独立登录已实现。正式原资料、旧五自启缺失、双时间与收据守卫通过，下一项按官方V2合同接Trae认证生命周期。整体目标active。

# VibeCoding 任务卡 · 账号权益、签到与领奖

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

# 本机客户端接入 DSH：Qoder CN / WorkBuddy AI / Trae CN

> 建立 2026-10-12 21:05，最后更新 23:45 (Asia/Shanghai)。
> 目标（用户原话）：测试本机的 workbuddy/qoder/trae 接入本项目，提供登录，使其模型在本 DSH 中可调用。
> 追加要求：**后续支持多账号**（不限于每种一个）。
> 本文档不记录任何凭据明文，只记录**位置**与**取用方式**。凭据等同于账号登录态。

---

## 一、结论：三家全部打通

| 客户端 | 端口 | 模型数 | 实现方式 | DSH 全链路判据 |
|---|---|---|---|---|
| **Qoder CN** | 8790 | 14 | qoder2api-hub（读本机 `auth.v1.dat`） | `headless` 输出「收到」exit=0 |
| **WorkBuddy AI** | 8791 | 11 | CDP 借 `wb.http` 主进程代理（**不需要 token**） | 同上 |
| **Trae CN** | 8792 | 13 | CDP 借内部服务注册表 + UI 触发 + `getMessages` | 同上（29.9s） |

**进程隔离有个陷阱，已修**：最初的 `DETACHED_PROCESS` **不足以**脱离 DSH 宿主——Windows 的
**Job Object kill-on-close** 会在宿主退出时把 job 里的**所有**进程一起杀掉。2026-10-12 实测：
宿主重启后三个网关**连同我用 `Start-Process` 起的两个客户端全部消失**，而机器 uptime 29 小时、
并未重启——所以这不是"机器重启导致的"，是 job 连带。

正确做法是**用 WMI 创建进程**，或**由 explorer 启动（注册表 Run）**：

```powershell
Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $cmd }
```

管理入口与恢复：

```
Qoder      : D:\Software\qoder2api-hub\.gateway-ctl.py    start|stop|status
WorkBuddy  : D:\Software\workbuddy2api\bridge-ctl.js      start|stop|status all
Trae       : 同一个 ctl 管两个桥接
一次拉起三家: D:\Software\workbuddy2api\start-gateways-wmi.ps1   （走 WMI，脱离 job）
登录自启配置: D:\Software\workbuddy2api\setup-autostart.ps1  （-Remove 回滚）
```

自启项（`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`）：

| 名称 | 值 |
|---|---|
| `WorkBuddy.WorkBuddyAI` | `"…\WorkBuddyAI.exe" --remote-debugging-port=9224`（**原值不带参数，已加**） |
| `Trae.CN` | `"…\Trae CN.exe" --remote-debugging-port=9226` |
| `DSH-Gateway-Qoder` / `-WorkBuddy` / `-Trae` | 三个网关 |

回滚：`setup-autostart.ps1 -Remove`（会把 WorkBuddy 的 Run 值还原成不带参数）。

---

## 二、统一前置条件（长期可用性的关键）

**WorkBuddy 与 Trae 都必须带远程调试端口启动**，否则桥接起不来：

```powershell
Start-Process "D:\Program Files\WorkBuddyAI\WorkBuddyAI.exe" -ArgumentList "--remote-debugging-port=9224" -WindowStyle Minimized
Start-Process "D:\Program Files\Trae CN\Trae CN.exe"          -ArgumentList "--remote-debugging-port=9226" -WindowStyle Minimized
```

症状：桥接 `/health` 返回
`{"ok":false,"error":"connect ECONNREFUSED 127.0.0.1:9224"}`（或 9226）。

> 建议：给这两个客户端各建一个带参数的快捷方式，避免每次手动传参。

Qoder 侧无此要求（网关独立读凭据文件）。

---

## 二·补、登录跳转取鉴权（不依赖客户端，与本地读取并存）

统一工具：`D:\Software\workbuddy2api\login-jump.js`

```powershell
node login-jump.js qoder cn      # Qoder 中国版设备授权（浏览器完成，token 直接入网关账号池）
node login-jump.js qoder intl    # 国际版
node login-jump.js workbuddy     # WorkBuddy 登录跳转（拿到凭据写进 .env）
node login-jump.js status        # 三家网关与账号状态
```

| 客户端 | 状态 | 协议要点 |
|---|---|---|
| **Qoder** | ✅ **已实现可用** | 网关自带的 PKCE 设备流 |
| **WorkBuddy** | ✅ **已实现可用** | `auth/state` → 浏览器 → `auth/token` 轮询 |
| **Trae** | ⚠️ 协议已完整逆向，实现待做 | 本地回调服务器 + PKCE + ExchangeToken |

### Qoder（走网关现成能力）

```
POST /accounts/login/start {realm}    （面板路由，要 X-Panel-Token 头）
  → {state, authUrl}
浏览器打开 authUrl 授权
GET /accounts/login/poll?state=<state>
  → pending / ok（token 直接入账号池，落 accounts/<uid>.json）
```

注意：`/accounts/*` 是**面板路由**，必须先 `POST /panel/login {password}` 拿 token，再带
`X-Panel-Token` 头（不是 `Authorization`）。这一步踩过。

### WorkBuddy（完整链路，之前漏了中间一步）

```
POST https://www.workbuddy.ai/v2/plugin/auth/state?platform=workbuddy-ai
     Headers: X-No-Authorization: true（另有 X-No-User-Id / X-No-Enterprise-Id / X-No-Department-Info）
  → {state, authUrl}
浏览器打开 authUrl 授权
GET  /v2/plugin/auth/token?state=<state>        ← 取 token 的关键一步，同样用 X-No-Authorization
  → 未授权 {"code":11217,"msg":"11217:login ing..."}（实测确认）
  → 已授权 {data:{accessToken, refreshToken, expiresIn, ...}}
（**这一步是不需要 Bearer 的**；需要 Bearer 的是后面的 /v2/plugin/login/account，属于取账号信息，
  不是取 token。之前误以为"取 token 必须带 Bearer"而判定此路不通，是错的。）
```

取到的凭据写入 `D:\Software\workbuddy2api\.env`（已 gitignore），重启 workbuddy 桥接生效。

### Trae（协议已逆向，实现未做）

```
1. 起本地服务器：http.createServer().listen(0, '127.0.0.1')   → 随机端口
2. 授权 URL（loginUrlBuilder.buildLoginUrl 逆向）：
   {consoleHost}/authorization?login_version=1&auth_from=solo|trae&login_channel=native_ide
     &plugin_version=<tronBuildVersion|local>&auth_type=local&client_id=<...>&redirect=0
     &login_trace_id=<uuid>&auth_callback_url=http://127.0.0.1:<port>/authorize
     &machine_id=<...>&device_id=<...>&code_challenge=<...>&code_challenge_method=S256
     （solo 还要 &hide_saas_login=true）
3. 浏览器授权 → 回调本地 /authorize（携带 code 与 userInfo）
4. 换 token：POST /trae/api/v3/oauth/ExchangeToken
   配套端点：/cloudide/api/v3/trae/GetUserInfo、/cloudide/api/v3/trae/CheckLogin
```

- 本地服务器路径常量：`/login-confirm`、`/login-success`、`/authorize`
- 域名：`www.trae.cn`（`www.trae.com.cn` 会 **301** 过去）；实测直接 GET 这些 API 返回 SPA HTML，
  说明还需正确的 method/header（未完成的部分）
- **待补**：`clientId` 与 `loginHost` 来自 **bootConfig**（主进程持有；渲染进程的
  `sololiteConfiguration` 为 nullish，取不到）
- 已有凭据（备用）：`.trae-token.json` — `Cloud-IDE-JWT`，`source: refresh_token`

---

## 三、Qoder CN

- 网关 `D:\Software\qoder2api-hub`（v1.3.4，纯 stdlib）
- 凭据：只读两步扫描 `%APPDATA%\com.qodercn.app.stable\auth.v1.dat`
  （Chromium os_crypt v10：Local State encrypted_key → DPAPI → AES-256-GCM）
- 面板令牌走 `X-Panel-Token` 头；`POST /accounts/import/desktop`
  （`{}`=只读扫描 / `{path,realm}`=按条导入 / `{all:true}`=全导）
- 账号池落盘 `accounts\01a04335-….json`（**凭据文件，勿提交**）
- 上游凭据自动轮换（`expires_at` 会刷新，当前 ≈2026-11-11）

## 四、WorkBuddy AI（实现方式：借主进程代理）

**为什么不需要 token**：WorkBuddy 是 Electron；preload 暴露 `window.wb.http`（主进程 HTTP 代理，
**自动注入 Authorization**）。模型端点 `https://www.workbuddy.ai/v2/chat/completions`。
本地进程没有 token，但可通过 CDP 让渲染进程用 `wb.http` 代发。

实测约束（全部踩过，别重复）：

1. `responseType: 'text'` —— 回调函数不能跨 IPC（报 `An object could not be cloned`）
2. **首条消息必须是 system**，否则 `11128 first message is not system prompt`
3. **不支持非流式**：`11101 Non-stream chat request is currently not supported`（传 `stream:true`）
4. 渲染进程自己的 `fetch` 打同一端点会 401 —— 只有 `wb.http` 这条主进程通道带授权
5. 模型名用桌面端在用的（如 `deepseek-v4.1-flash`）；`deepseek-v3` 不在本机清单里

实现：`D:\Software\workbuddy2api\workbuddy-bridge.js`（8791，OpenAI 兼容，含流式）

## 五、Trae CN（实现方式：借内部服务注册表）—— 本批次最难的一环

**为什么 CDP 抓不到**：Trae 的生成**不在渲染进程网络栈**，也不走 WebSocket。
它走内部 RPC（`service:"lite"`），最终落到 `trae-api-cn.mchost.guru` 的
`/api/remote/v1/chat_sessions/...`，但调用入口是渲染进程里的服务对象。

**突破口**：`window.__SOLO_LITE_PORT_REGISTRY__`（Trae Solo Lite 自己的服务注册表）

```
ports          : 69 个端口定义（Symbol 作 key）
implementations: 58 个实现
instances      : 30 个活跃实例 ← 关键
  TraeApiPort 实例的 .chat 暴露：
    createSession / sendMessage / getMessages / subscribe / stopSession / ...
  另有 .model.listModels、._apiClientService、._rustTransportAdapter
```

**最终实现路径**（`D:\Software\workbuddy2api\trae-bridge.js`，8792）：

1. CDP 连 Trae Agent 页面（需 `--remote-debugging-port=9226`）
2. 从 `instances` 取 `TraeApiPort`，注入 hook 捕获 `sendMessage` 的 `chat_session_id`
3. **发送**：在 UI 输入框 `.chat-input-v2-input-box-editable` 填文本，
   再点 `.chat-input-v2-send-button`（**参数由 Trae 自己构造**，绕开复杂 body）
4. **读取**：轮询 `await inst.chat.getMessages({chat_session_id})`
5. **提取**：助手回复在
   `item.content`(JSON string) → `.messages[].plan_item.tool_call_info{name:"finish"}.params.summary`
   （退路：任意非空 `params.summary`）
6. **判定新回复**：`created_at` 晚于发送时刻 **或** `message_index` 大于基线

实测：单次生成约 **13 秒**（Trae 的 agent 全流程）；DSH headless 端到端 **29.9 秒**。

已穷尽但**无效**的手段（别重复）：

| 手段 | 结果 |
|---|---|
| HTTP 端点直调（10+ 路径 × 4 头组合） | 全 404 |
| CDP 渲染进程 Network 域 | 生成请求不在栈里 |
| CDP WebSocket 事件监听 | 0 连接、0 帧 |
| browser-level `Target.getTargets` | 只有 2 个 page，无 worker |
| 本地 17788 服务路由（13 路径 × 3 头） | 全 404（它是别的用途） |
| 主进程日志（1.3MB aha log） | 只有框架层日志，无 API 记录 |
| hook `vscode.ipcRenderer.invoke/send` | 0 次调用（RPC 不走它） |
| `@byted-icube` 包静态搜索 | 拿到完整 API 清单，但入口在渲染进程运行时 |
| **`__SOLO_LITE_PORT_REGISTRY__`** | ✅ **成功** |

Trae 凭据（备用，当前实现未用到）：`D:\Software\workbuddy2api\.trae-token.json`
（`Cloud-IDE-JWT`；`GET /api/remote/v1/models` → 12 模型；JWT `source: refresh_token`，exp≈2026-10-19）

### 五·补、两个必须记住的实现陷阱（都实际踩到过）

**1. hook 标志必须挂在「实例」上，不能挂在 `window` 上。**
Trae 重启后 `TraeApiPort` 是**全新实例**，而 `window.__traeHooked` 这类页面级标志在新页面里
可能残留/误导，导致**新实例从未被 hook** → 捕获不到 `chat_session_id` → 每次请求都超时
（实测症状：181.7s 后 502）。现在改成实例级 `inst.__bridgedHooked`，并**遍历 hook `chat` 上
所有方法**（不只 `sendMessage`）从参数里找 `chat_session_id`/`chatSessionId`/`session_id`。

**2. 桥接要能容忍客户端重启。**
客户端重启后桥接**不需要重启**——它每次请求都重新取 CDP target 与实例（`withPage` +
`PICK_INST`）。实测：Trae/WorkBuddy 客户端被重启后，两个桥接 `/health` 自动恢复 `ok`。

---

## 六、DSH 侧配置（三个 provider 已写入）

文件：`C:\Users\Admin\.dsh\profiles\desktop\cordis.patch.yml`。
在**已有的** `- id: llm-pi-ai` 段内、`providers:` 字典里追加
（**不能新建第二段顶格 `- id: llm-pi-ai`**，loader 报 `duplicate loader entry id`）：

| provider | baseURL | 行号 |
|---|---|---|
| `qoder` | `http://127.0.0.1:8790/v1` | 490 |
| `workbuddy` | `http://127.0.0.1:8791/v1` | 555 |
| `trae` | `http://127.0.0.1:8792/v1` | 575 |

三家都用 `headers.Authorization: "Bearer <占位>"`（本地网关无鉴权；插件 README 明确：
无密钥本地服务器需要 `apiKeyEnv` 或 `headers` 里的 Authorization 占位）。

备份：`.bak-qoder-20261012-205838`、`.bak-workbuddy-20261012-220948`、`.bak-trae-20261012-223423`

> ⚠️ **`dsh --dump-config` exit=0 只证明配置能解析，不证明运行时注册成功**。
> 运行时判定：`POST /api/session/modelCatalog`
> （RPC 形状 `{type:"client-request", rpcId, method:"session/modelCatalog", payload:{args:{}}}`），
> 看 `result.value.routableProviders`。
> 另：**headless profile 不继承 desktop 的 providers**，用 `--patch` 验证时要一并带上 provider 配置。

---

## 七、验证证据

| 判据 | 结果 |
|---|---|
| 三家 `/health` | Qoder `ok`+`accounts_ready:1`；WorkBuddy `workbuddy:true`；Trae `trae:true` |
| 三家 `/v1/models` | 14 / 11 / 13 个模型 |
| **Qoder 全链路** | `dsh --profile headless --patch=… "只回答两个字：收到"` → `收到`，exit=0 |
| **WorkBuddy 全链路** | 同上 → `收到`，exit=0 |
| **Trae 全链路** | 同上 → 真实回复，exit=0（29.9s） |
| WorkBuddy 直连 | 非流式 `content:"收到"`；流式 4 帧 SSE 正文正常 |
| `--dump-config` | exit=0，三个 provider 与端口齐全 |
| **desktop profile 运行时注册**（决定性） | 独立实例 `dsh --profile desktop --port 18090` 的 `routableProviders` 含 `qoder`(11 模型) / `workbuddy`(6) / `trae`(8)，另派生 `modlens-*` vision 变体 |
| 客户端重启后直连复验 | Qoder 1.6s「收到」；WorkBuddy 1.2s「收到」；Trae 13.2s「收到」（均 HTTP 200） |

> **纠正一个我先前的错误结论**：我曾据"宿主日志只有 `[llm-ollama] registered provider`"判断
> "宿主的 llm-pi-ai 一个 provider 都没注册"。**这个推断不成立**——llm-pi-ai 很可能**根本不打**
> 这类注册日志（只有 llm-ollama 打）。用同一 profile 的独立实例验证后才确认：**desktop profile
> 下三家都会注册**。判运行时要看 `routableProviders`，不要看日志有没有那一行。

> PowerShell 发中文给桥接会出现「乱码」假象（PS 编码问题）；用 node 直接发 UTF-8 正常。

---

## 八、多账号设计（用户追加要求）

`llm-pi-ai` 的 `providers` 是**字典**，天然支持同一来源多账号：

```yaml
    providers:
      qoder:        # 账号 1
      qoder-2:      # 账号 2（第二个 qoder2api-hub 实例 + 换端口 + 换 accounts 目录）
      workbuddy:    # 账号 1（客户端 9224 + 桥接 8791）
      workbuddy-2:  # 账号 2（客户端 9225 + 桥接 8793）
      trae:
      trae-2:
```

两种落地方式：

- **A. 每账号一实例 + 一 provider 条目（推荐）**：隔离清晰、可独立启停、失败互不影响。
  代价：条目数随账号增长；每个账号需要一个客户端实例（Electron 多开、各自 userData 目录）。
- **B. 桥接层做账号池**：`bridge-ctl` 传多个 `--cdp 9224,9225`，桥接内部轮询 + 失败切换。
  优点：对 DSH 只暴露一个 provider；代价：要改桥接代码，失败语义更复杂。

前提：WorkBuddy / Trae 的多账号需要**多个客户端实例**（各自 userData + 各自调试端口），
因为登录态按 userData 隔离。Qoder 侧简单——qoder2api-hub 已支持多账号池。

---

## 九、未做 / 未验证

1. **GUI 侧最后一格未由我复验**：宿主 API（43120）对非 Electron 会话返回 403，
   我拿不到它的 session token，所以"GUI 模型选择器里能看到三组"这一条**只能由用户肉眼确认**。
   已由我验证的等价判据是：同一 desktop profile 的独立实例 `routableProviders` 含三家（见 §七）。
2. 未做并发、长会话、工具调用压测。**Trae 桥接是串行的**（借用 UI），并发会有竞争。
3. 三家网关**未设 API Key**（仅回环）；qoder 看板仍是默认密码 `admin`。
4. 多账号只做了设计，未实现。
5. 客户端调试端口已写进登录自启（§一），但**若客户端以别的方式启动**（例如手动双击、
   或某个快捷方式覆盖了 Run 键行为），仍可能不带参数 → 桥接 `ECONNREFUSED`。
6. 网关**没有守护**：进程若异常退出，下次登录才会自动回来；期间用
   `start-gateways-wmi.ps1` 手动拉起。

## 十、下一步

1. **在 GUI 里确认**模型选择器出现三组：
   `Qoder CN (local gateway)` / `WorkBuddy (desktop bridge)` / `Trae CN (desktop bridge)`，
   并真发一条消息（这是唯一还需要人眼的一步）。
2. 多账号：按 §八 方案 A 铺第二套（客户端新实例 + 新端口 + 新 provider 条目）。
3. Trae 桥接加队列锁（并发保护）。
4. 可选：给三个网关加轻量守护（定时探活 + 自动拉起），替代"等下次登录"。

## 十一、回滚

1. **DSH**：还原任一份备份（`.bak-qoder-*` / `.bak-workbuddy-*` / `.bak-trae-*`），或只删
   `qoder:` / `workbuddy:` / `trae:` 三段（**别删整个 `- id: llm-pi-ai`**，会连带移除
   commandcode / openrouter / zai 等既有 provider）。
2. **Qoder**：`python .gateway-ctl.py stop`；彻底卸载删 `D:\Software\qoder2api-hub`。
3. **桥接**：`node bridge-ctl.js stop all`。
4. **客户端**：WorkBuddy / Trae 关掉重开（不带参数）即恢复默认。
5. **凭据文件**：删 `D:\Software\qoder2api-hub\accounts\`、`.trae-token.json` 即移除落盘凭据。

## 十二、安全边界

- 三家 token **均未进入本仓库**。凭据落盘：`D:\Software\qoder2api-hub\accounts\`、
  `D:\Software\workbuddy2api\.trae-token.json`（均 gitignore），**不要提交或外发**。
- 本仓既有约定（`docs/VibeCoding任务卡-账号型上游包装.md` 第 87 行）「不许读第三方凭据文件」——
  本轮用户显式授权「直接自抓」；且 WorkBuddy 与 Trae 走的是**借客户端运行时**（CDP），
  并未读取它们的凭据文件。
- `~/.dsh/.credentials.yaml` 未改动，未新增环境变量。

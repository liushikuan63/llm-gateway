# WorkBuddy / Qoder / Trae 本机客户端接入 DSH — 接续文档

> 建立时间：2026-10-12 21:05，最后更新 21:25 (Asia/Shanghai)
> 目标（用户原话）：测试本机的 workbuddy/qoder/trae 接入本项目，提供登录，使其模型在本 DSH 中可调用。
> 本文档不记录任何凭据明文；只记录**凭据所在位置**与**取用方式**。凭据等同于账号登录态。

---

## 一、裁决与结论

用户在三条路里选定 **方案 A**：先落 Qoder CN（自动只读本机凭据，不重启客户端），三家按序推进。

| 客户端 | 接入方式 | 当前状态 |
|---|---|---|
| **Qoder CN** | `qoder2api-hub` 网关 + DSH `llm-pi-ai` provider | **已打通**（DSH 全链路实测出流，见 §3.3） |
| **WorkBuddy AI** | `workbuddy2api`（同类网关） | 未开工；token 是 `$wbEncrypted` 信封，需抓包 |
| **Trae CN** | 无现成反代实现 | 未开工；只有 JWT，缺协议实现 |

---

## 二、Qoder CN 已落地的内容（可复用）

### 2.1 网关

- 位置：`D:\Software\qoder2api-hub`（git clone 自 `shuishuipingan/qoder2api-hub`，v1.3.4，MIT，纯 Python 标准库零依赖）
- 管理脚本（仓库外）：`D:\Software\qoder2api-hub\.gateway-ctl.py`
  - `python .gateway-ctl.py start|stop|status`
- 监听：`http://127.0.0.1:8790/v1`（OpenAI 兼容），看板 `http://127.0.0.1:8790/`（面板密码 `admin`，**上线前应改**）
- 日志：`gateway.log` / `gateway.err.log`（同目录）
- 账号池：`D:\Software\qoder2api-hub\accounts\`（含 `01a04335-….json`，该文件即**凭据落盘位置**）

**为什么必须独立进程**：网关最初挂在 DSH 后台 job 下，宿主一重启就会连带杀掉，模型供应商随即失效。
已用 `subprocess.Popen(..., DETACHED_PROCESS|CREATE_NO_WINDOW)` 拉起，父进程已消失，与 DSH 会话无生命周期耦合。

### 2.2 凭据来源（只读扫描，两步确认）

网关按 Chromium `os_crypt` v10 布局解桌面端 `auth.v1.dat`：
`Local State` 的 `encrypted_key` → DPAPI 解出 32 字节 AES key → AES-256-GCM 解 payload。

本机扫描出两条（**均未静默导入**，走两步确认）：

| realm | 路径 | 处理 |
|---|---|---|
| intl 国际版 | `%APPDATA%\com.qoder.app.stable\auth.v1.dat` | **未导入**（用户只选了 CN） |
| cn 国内版 | `%APPDATA%\com.qodercn.app.stable\auth.v1.dat` | **已导入** |

导入调用（面板令牌走 `X-Panel-Token` 头，不是 `Authorization`）：

```powershell
$login = Invoke-RestMethod -Uri "http://127.0.0.1:8790/panel/login" -Method Post `
  -ContentType "application/json" -Body '{"password":"admin"}'
$hdr = @{ "X-Panel-Token" = $login.token }
# 只读扫描：{} ；按路径导入：{"path":"...auth.v1.dat","realm":"cn"} ；导入全部：{"all":true}
Invoke-RestMethod -Uri "http://127.0.0.1:8790/accounts/import/desktop" -Method Post `
  -Headers $hdr -ContentType "application/json" -Body '{}'
```

### 2.3 DSH 侧 provider 配置

改的是 **`C:\Users\Admin\.dsh\profiles\desktop\cordis.patch.yml`**（当前 profile 是 `desktop`，见环境变量 `DSH_PROFILE`）。
在已有的 `- id: llm-pi-ai` 段内、`providers:` 字典里追加了 `qoder:` 键（**不能新建第二段顶格 `- id: llm-pi-ai`**，loader 会报 `duplicate loader entry id`）。

关键决策与理由：

1. **用 `llm-pi-ai` 而不是 llm-deepseek**：`llm-pi-ai` 支持自定义 OpenAI 兼容 provider；该插件行在 bundle 中**已存在**（`--dump-config` 第 78 行），只是此前无 `qoder` 供应商。
2. **去掉了 `apiKeyEnv`，改用 `headers`**：网关启动为 `api_key_required: false`。插件 README 明确「无密钥本地服务器需要由 `apiKeyEnv` 引用或 `headers` 中的 `Authorization` 条目提供的占位凭据」，且 `GET /v1/models` 模型发现也走同一套头。用 `apiKeyEnv` 会因环境变量解析为空而以 `MISSING_CREDENTIAL` 失败。
3. **模型 id 用官方模型名**（`Qwen3.8-Flash` 等），因为 `id` 是直接发给上游的请求值，网关接受官方名/key/别名三种写法。

配置片段（完整版备份在 `cordis.patch.yml.bak-qoder-20261012-205838`）：

```yaml
      qoder:
        displayName: Qoder CN (local gateway)
        api: openai-completions
        baseURL: http://127.0.0.1:8790/v1
        headers:
          Authorization: "Bearer qoder-local"
        models:
          - id: Qwen3.8-Flash
            name: Qwen3.8-Flash
            contextWindow: 200000
            reasoningEfforts: { off: none, low: low, medium: medium, xhigh: xhigh }
          # …另有 10 个：Max/3.7 系/GLM/Kimi/MiniMax/DeepSeek
```

---

## 三、验证证据（已跑过的，可复现）

### 3.1 配置与凭据层

| 判据 | 命令 | 结果 |
|---|---|---|
| 配置可解析 | `dsh --dump-config` | `exit=0`；resolved config 第 474 行含 `qoder:` 与 `baseURL` |
| 凭据解密 | `POST /accounts/import/desktop` `{}` | 只读列出 2 条（intl + cn），含 uid/昵称 |
| 账号入池 | `GET /health` | `accounts=1, accounts_ready=1, realm=cn` |
| 模型清单 | `GET /v1/models` | 14 个模型，`catalog_source=external-json` |

### 3.2 网关直连（绕过 DSH）

| 判据 | 命令 | 结果 |
|---|---|---|
| 流式出流 | 直连 `/v1/chat/completions` `stream=true` | HTTP 200，13 帧 SSE，正文「收到」 |
| 非流式 + 占位 Bearer | `stream=false`，`Authorization: Bearer qoder-local` | 200，`content="1 + 1 = 2"`，`credits=0.14496328` |

> 上游凭据有效期：`expires_at=1794110956`（≈2026-11-07）。过期后网关按 `refreshToken` 自动轮换。

### 3.3 DSH 运行时（关键判据）

用独立临时实例验证（**不碰正在使用的宿主**），读 `POST /api/session/modelCatalog` 判定。
RPC 形状：`{type:"client-request", rpcId, method:"session/modelCatalog", payload:{args:{}}}`；
面板令牌走 Cookie，不是 `Authorization`。

| 实验 | 配置 | `routableProviders` |
|---|---|---|
| 18081 | 无 `--patch`（走 desktop profile） | **只有** `deepseek-official`、`deepseek-account` |
| 18082 | `--patch` 只放 qoder | `qoder` + 上面两个 |
| 18083 | qoder + `reasoningEfforts` | 同上（证明 `off: none` 等写法没问题） |
| 18084 | **原 8 个 provider**（从 profile 原样提取，去掉 qoder） | 8 个全注册 + 上面两个 |
| 18085 | **9 个 provider**（原 8 个 + qoder） | 9 个全注册 + 上面两个 ✅ |

**结论 1（重要，且与本次 patch 无关）**：desktop profile 的 `llm-pi-ai` 在**独立实例**里能完整注册全部 9 个
provider，但在**正在运行的 Desktop 宿主**里（18081 同配置、以及宿主日志）**一个都没注册**。
宿主日志只有 `[llm-ollama] registered provider "ollama"`，**没有 llm-pi-ai 的注册行**。
即：Desktop 宿主当前只跑 `llm-ollama` + `llm-deepseek`，`llm-pi-ai` 的 provider 一直未生效
（本批次之前就存在的状态，不是本次改动引入的）。因此 **qoder 模型要在宿主 GUI 里可见，必须重启宿主**。

**结论 2（最终判据，全链路已通过）**：用 DSH 真实的 agent 运行路径端到端跑通——

```powershell
# 临时 patch：把 headless profile 的默认模型指到 qoder
dsh --profile headless --patch=<qoder-headless.yml> "只回答两个字：收到"
# → stdout: 收到      exit=0
```

链路：`dsh --profile headless` → `llm-pi-ai` 的 `qoder` provider → `http://127.0.0.1:8790/v1`
→ qoder2api-hub → `gateway.qoder.com.cn` → Qwen3.8-Flash。
（同次运行另有 2 条 `preset-* : pending (waiting for service: agentPresets)` 警告，是 headless
profile 与 preset 无关的既有噪声，不影响本判据。）

### 3.4 可复现的操作要点（踩过的坑）

- `--patch` 只在 `--dump-config` 下能当位置参数用；配 `web` 子命令时必须写 `--patch=<path>`
  且用 `--profile web`，把 `web` 当位置参数会报 `too many arguments`。
- 改 DSH 配置**不碰**正在跑的宿主：用 `dsh --profile web --patch=<file> --port <spare>` 起临时实例当试验台。
- `dsh --profile headless --help` 打印的是 **web app 的 help**（不是 headless 自己的），别被误导。

---

## 四、明确未做 / 未验证

1. **Desktop 宿主 GUI 里尚未看到 qoder 模型** —— 桌面端宿主运行的是旧配置（§3.3 结论 1），
   **必须重启宿主**才能让 `llm-pi-ai` 与 qoder provider 生效。重启会中断当前会话（历史保留，可续）。
   DSH 运行时已证明可用（§3.3 结论 2），缺的只是宿主重载。
2. **WorkBuddy 未开工**。已知：`%LOCALAPPDATA%\CodeBuddyExtension\Data\Public\auth\workbuddy-desktop-ai.info`
   （注意实际文件名是 **`workbuddy-desktop-ai.info`**，开源脚本找的是 `workbuddy-desktop.info`，
   **路径不匹配会报「找不到 auth 文件」**）。实测该文件**可读**，但 `auth.accessToken` / `refreshToken`
   都是 `{"$wbEncrypted":1,"envelope":"…"}` 信封，密钥只在运行中进程内存，**不落盘**。
   账号信息可读：`account.uin`、`account.uid`、`auth.domain=www.workbuddy.ai`、`tokenType=Bearer`；
   `expiresAt=1823344879196`（≈2027-10-11，refresh 同）。抓包端点：`copilot.tencent.com`。
3. **Trae CN 未开工**。仅有 `~/.trae-cn/trae-jwt-token`（明文 RS256 JWT，payload 含
   `data.id/tenant_id/user_id`，`exp=1792408729` ≈2026-10-19，`iss=trae`）。
   本期没找到可用的反代实现；`A-23187/trae-api` 未验证。
4. **网关未设 API Key**（本机回环可接受）；**看板仍是默认密码 `admin`**。
5. 未做跨客户端并发、长会话、工具调用的压测。
6. **未做视觉验证**：宿主 GUI 里的模型选择器没有截图（因为那需要先重启宿主）。

---

## 五、下一步待办

1. **重启用正式宿主**（DSH Desktop，非 Beta），然后确认 GUI 模型选择器出现
   `Qoder CN (local gateway)` 分组与 Qwen3.8-Flash 等模型，并真发一条消息。
2. **WorkBuddy 接入**：需用户 F12 抓一次 `Authorization: Bearer …`（发往 `copilot.tencent.com`）
   与 `refreshToken`；然后按 `syh0119/workbuddy2api` 起第二个网关（建议端口 8791），
   用同样的 `llm-pi-ai` 追加方式挂上。
3. **Trae 接入**：先评估协议可行性；无成熟方案则如实回报为受限项。
4. **用户新提的演进方向（2026-10-12 18:xx 提出，尚未开工）**：
   > 「后续可以增加不下载对应客户端，不读取本地消息（但保留该方式，集成到软件中），
   > 也能通过登录跳转，直接获取鉴权信息」

   即：从「读本机客户端登录态」演进为「**登录跳转 / 设备授权直接取鉴权**」，同时**保留**本地读取作为
   其中一种方式并集成进软件。已知的现成能力：
   - `qoder2api-hub` **原生支持 OAuth 设备授权**（PKCE S256 设备流，看板「+ 添加账号 (OAuth)」，
     双区 URL 构造已按官方差异实现），也支持 PAT (`pt-`) 导入 —— 这条最接近用户要求，可直接用。
   - 三家是否都具备浏览器授权流程待核实（Qoder 确认有；WorkBuddy / Trae 待查）。
   落地位置建议：网关控制台新增「登录跳转」入口 + 状态页区分「凭据来源：本机读取 / 浏览器授权」。

---

## 六、安全边界

- 三家 token **均未写入本仓任何文件**。网关的 `accounts/*.json` 属凭据落盘，位于
  `D:\Software\qoder2api-hub\accounts\`，**不要提交、不要外发**。
- 本仓既有约定（`llm-gateway/docs/VibeCoding任务卡-账号型上游包装.md` 第 87 行）：
  「不许读第三方凭据文件」——本轮**用户显式授权**「直接获取我已登录的凭据」，故按授权执行，
  且仅走各自官方存储 + 只读扫描两步确认（网关设计为「绝不静默采用」）。
- `~/.dsh/.credentials.yaml` 未改动，未新增环境变量。

---

## 七、回滚

1. **DSH 侧**：还原备份 `C:\Users\Admin\.dsh\profiles\desktop\cordis.patch.yml.bak-qoder-20261012-205838`
   （或用脚本只删 `      qoder:` 到其 models 列表结尾的一段）。
   **注意别删掉整个 `- id: llm-pi-ai` 段**，那会连带移除 commandcode/openrouter/zai 等既有 provider。
2. **网关侧**：`python .gateway-ctl.py stop`；彻底卸载则删 `D:\Software\qoder2api-hub`。
3. **账号池**：删 `accounts\01a04335-….json` 即从池中移除（不影响 Qoder 桌面端本身）。

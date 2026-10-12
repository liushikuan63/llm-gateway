# WorkBuddy / Qoder / Trae 本机客户端接入 DSH — 接续文档

> 建立 2026-10-12 21:05，最后更新 22:30 (Asia/Shanghai)
> 目标（用户原话）：测试本机的 workbuddy/qoder/trae 接入本项目，提供登录，使其模型在本 DSH 中可调用。
> 本文档不记录凭据明文，只记录**位置**与**取用方式**。凭据等同于账号登录态。

---

## 一、三家现状

| 客户端 | 状态 | 实现方式 | 判据 |
|---|---|---|---|
| **Qoder CN** | ✅ **已打通** | qoder2api-hub 网关 | `dsh --profile headless --patch=…` 输出「收到」exit=0 |
| **WorkBuddy AI** | ✅ **已打通** | workbuddy-bridge（CDP 借主进程代理） | 同上，输出「收到」exit=0 |
| **Trae CN** | ⚠️ **半通** | token + 模型清单已取到；生成端点未定位 | `GET /api/remote/v1/models` → 200，12 个模型 |

用户追加要求：**后续支持多账号**（不限于每种一个）。设计见 §五。

---

## 二、Qoder CN（已落地）

- 网关：`D:\Software\qoder2api-hub`（v1.3.4，纯 stdlib）
- 管理：`python .gateway-ctl.py start|stop|status`；监听 `http://127.0.0.1:8790/v1`
- 凭据：只读两步扫描 `%APPDATA%\com.qodercn.app.stable\auth.v1.dat`
  （Chromium os_crypt v10：Local State encrypted_key → DPAPI → AES-256-GCM）
- 面板令牌走 `X-Panel-Token` 头；导入 API `POST /accounts/import/desktop`（`{}` 只读扫描）
- 账号池落盘 `accounts\01a04335-….json`（**凭据文件，勿提交**）

## 三、WorkBuddy AI（已落地，实现方式值得复用）

**为什么不需要 token**：WorkBuddy 是 Electron；其 preload 暴露 `window.wb.http`
（主进程 HTTP 代理，**自动注入 Authorization**）。模型端点是
`https://www.workbuddy.ai/v2/chat/completions`，要求 `stream:true` 且**首条消息必须是 system**。
本地进程没有 token，但可通过 CDP 让渲染进程用 `wb.http` 代发。

- 桥接：`D:\Software\workbuddy2api\workbuddy-bridge.js`
  - 管理：`node bridge-ctl.js start|stop|status`；监听 `http://127.0.0.1:8791/v1`
  - `GET /v1/models`、`POST /v1/chat/completions`（含 stream）
- **前置条件**：WorkBuddy 必须以 `--remote-debugging-port=9224` 启动
  （`Start-Process "D:\Program Files\WorkBuddyAI\WorkBuddyAI.exe" -ArgumentList "--remote-debugging-port=9224"`）
- 端点/协议要点（实测）：
  - `wb.http.post(url, body, { responseType:'text' })` 才能跨 IPC 回传（回调函数不可跨 IPC）
  - 首条非 system 会被服务端拒：`11128 first message is not system prompt`
  - 非流式被拒：`11101 Non-stream chat request is currently not supported`
  - 可用模型：`deepseek-v4.1-flash`（桌面端默认）、`hy3`、`hy4-preview`、`gpt-6-astra`、`step-5-preview` 等

## 四、Trae CN（半通，卡在生成端点）

**已拿到的**：

| 项 | 值 |
|---|---|
| 凭据 | `Cloud-IDE-JWT …`（1018 字符），来自渲染进程 fetch 的 `Authorization` 头 |
| 落盘位置 | `D:\Software\workbuddy2api\.trae-token.json`（**凭据文件，勿提交**） |
| JWT payload | `{source:"refresh_token", tenant_id:"7o2d894p7dr0o4", type:"user"}`, exp≈2026-10-19 |
| API 域名 | `https://trae-api-cn.mchost.guru` |
| 模型清单 | `GET /api/remote/v1/models` → 12 个：Doubao-Seed-2.0-Code、**Doubao-Seed-Code(默认)**、minimax-m2.7、glm-5.1、glm-5v-turbo、glm-5、DeepSeek-V4-Pro、DeepSeek-V4-Flash、kimi-k2.6、kimi-k2.5、qwen-3.6-plus、qwen-3.5 |
| 本地服务 | `127.0.0.1:17788`（主进程，CORS 全开、认 `x-jwt-token` 头，但所有试过的路径都 404）；`127.0.0.1:51001`（扩展宿主，非 HTTP） |
| CDP 端口 | 9226（`Trae CN.exe --remote-debugging-port=9226`） |

**为什么卡住（已穷尽的 9 种手段，含失败轨迹）**：

1. HTTP 端点探测（10+ 路径 × 4 种头组合）→ 全 404
2. CDP 渲染进程 `Network` 域抓包 → 生成请求**不在渲染进程网络栈**（只有插件/用量请求）
3. CDP WebSocket 事件监听 → 0 个 WS 连接、0 帧
4. browser-level `Target.getTargets` → 只有 2 个 page，无 worker/iframe
5. 本地 17788 服务路由探测（13 路径 × 3 头）→ 全 404
6. Trae 主进程日志（1.3MB aha log）→ 只有框架层日志，无 API 记录
7. `@byted-icube/ai-modules-chat` 代码搜索 → 大量 webpack bundle 噪音，未见生成端点
8. HTML 输入框 + 发送按钮（`.chat-input-v2-send-button`）已成功触发真实对话（消息确实发出，输入框被清空），但**产生的请求不可见**
9. 官方 CLI/文档路径 → 未验证

**结论**：Trae 的生成链路走**主进程或独立进程**（VS Code fork 常见），CDP 看不到；要打通需要
继续逆向（切入点建议：`byted-icube.solo-lite` 包里找 baseURL，或抓 17788 的真实路由）。

## 五、多账号设计（用户 2026-10-12 追加要求）

`llm-pi-ai` 的 `providers` 是**字典**，天然支持同一来源多账号：

```yaml
    providers:
      qoder:        # 账号 1
      qoder-2:      # 账号 2（另一个 qoder2api-hub 实例 + 另一个端口）
      workbuddy:    # 账号 1
      workbuddy-2:  # 账号 2
```

落地方式（两种，推荐 A）：

- **A. 每账号一个网关实例 + 一个 provider 条目**：qoder2api-hub 支持多账号池（扫描多个
  auth.v1.dat），也能起第二个实例（换端口、换 accounts 目录）。workbuddy-bridge 同理：
  多开 WorkBuddy 客户端会拿到不同 userData，各自 9224/9225 端口 + 8791/8792 桥接。
  优点：隔离清晰、可独立启停；缺点：条目数随账号增长。
- **B. 桥接层做账号池与轮询**：workbuddy-bridge 增加 `--cdp 9224,9225` 轮询、失败切换。
  优点：对 DSH 只暴露一个 provider；缺点：要改桥接代码。

## 六、DSH 侧配置（两个 provider 已写入）

文件：`C:\Users\Admin\.dsh\profiles\desktop\cordis.patch.yml`，在**已有的** `- id: llm-pi-ai`
段内、`providers:` 字典里追加（**不能新建第二段顶格 `- id: llm-pi-ai`**，loader 报 duplicate）。

- `qoder:` → `http://127.0.0.1:8790/v1`
- `workbuddy:` → `http://127.0.0.1:8791/v1`
- 两者都用 `headers.Authorization: "Bearer <占位>"`（本地网关无鉴权；插件 README 明确
  无密钥本地服务器需要 apiKeyEnv 或 headers 里的 Authorization 占位）

备份：`cordis.patch.yml.bak-qoder-20261012-205838`、`cordis.patch.yml.bak-workbuddy-20261012-220948`

> **`--dump-config` exit=0 只证明配置能解析，不证明运行时注册成功**。运行时判定：
> `POST /api/session/modelCatalog`（RPC 形状 `{type:"client-request", rpcId, method:"session/modelCatalog", payload:{args:{}}}`）
> 看 `result.value.routableProviders`。另外 **headless profile 不继承 desktop 的 providers**，
> 用 `--patch` 验证时要一并带上 provider 配置。

## 七、验证证据

| 判据 | 命令 | 结果 |
|---|---|---|
| Qoder 配置解析 | `dsh --dump-config` | exit=0，含 `qoder:` @474、`workbuddy:` @539 |
| Qoder 运行时注册 | 临时实例 modelCatalog | `routableProviders` 含 `qoder` |
| Qoder 全链路 | `dsh --profile headless --patch=… "只回答两个字：收到"` | `收到`，exit=0 |
| Qoder 流式直连 | `/v1/chat/completions` stream=true | HTTP 200，13 帧 SSE |
| WorkBuddy 桥接健康 | `GET /health` @8791 | `{ok:true,cdpPort:9224,workbuddy:true}` |
| WorkBuddy 模型列表 | `GET /v1/models` @8791 | 11 个模型 |
| WorkBuddy 非流式 | node UTF-8 请求 | `content:"收到"`，model=deepseek-v4.1-flash |
| WorkBuddy 流式 | 同上 stream=true | 4 帧 SSE，正文正常 |
| **WorkBuddy 全链路** | `dsh --profile headless --patch=… "只回答两个字：收到"` | `收到`，exit=0 |
| Trae token | 渲染进程 fetch 抓取 | len=1018，JWT payload 已解 |
| Trae 模型清单 | `GET /api/remote/v1/models` | 200，12 个模型 |
| Trae 生成端点 | 9 种手段 | **未找到** |

> PowerShell 发中文给桥接会出现「乱码」假象（PS 编码问题）；用 node 直接发 UTF-8 正常。

## 八、未做 / 未验证

1. **DSH GUI 里尚未看到 qoder/workbuddy 模型**：Desktop 宿主运行时未注册 `llm-pi-ai`
   provider（宿主日志只有 `[llm-ollama] registered provider "ollama"`，**本批次之前就这样**），
   必须**重启宿主**才生效。重启会中断当前会话（历史保留）。
2. **Trae 生成端点**未定位（见 §四）。
3. WorkBuddy 桥接依赖客户端带 `--remote-debugging-port` 运行；否则 `/health` 会报
   `no WorkBuddy page target`。改快捷方式是长期可用性的前提。
4. 未做并发、长会话、工具调用压测。
5. 网关/桥接**未设 API Key**（仅回环）；qoder 看板仍是默认密码 `admin`。

## 九、下一步

1. 重启 DSH Desktop → GUI 模型选择器应出现 `Qoder CN (local gateway)` 与 `WorkBuddy (desktop bridge)` 两组。
2. Trae：继续定位生成端点，或按 §四 的切入点重新逆向。
3. 多账号：按 §五 方案 A 铺第二个实例（换端口 + 换 accounts 目录），加 `qoder-2` / `workbuddy-2` 条目。
4. 给 WorkBuddy 建一个带 `--remote-debugging-port=9224` 的快捷方式，避免每次手动传参。

## 十、回滚

1. **DSH**：还原备份 `cordis.patch.yml.bak-qoder-*` / `.bak-workbuddy-*`，或只删 `qoder:` /
   `workbuddy:` 两段（**别删整个 `- id: llm-pi-ai`**，会连带移除 commandcode/openrouter/zai 等）。
2. **Qoder**：`python .gateway-ctl.py stop`；彻底卸载删 `D:\Software\qoder2api-hub`。
3. **WorkBuddy 桥接**：`node bridge-ctl.js stop`。
4. **Trae**：`node` 进程无需管理（未常驻）；删 `.trae-token.json` 即移除凭据落盘。
5. **客户端**：WorkBuddy / Trae 均为带调试端口重启过，关掉重开即恢复默认。

## 十一、安全边界

- 三家 token **均未进入本仓库**。凭据落盘位置：`D:\Software\qoder2api-hub\accounts\`、
  `D:\Software\workbuddy2api\.trae-token.json`（均已 gitignore），**不要提交或外发**。
- 本仓既有约定（`docs/VibeCoding任务卡-账号型上游包装.md` 第 87 行）「不许读第三方凭据文件」——
  本轮用户显式授权「直接自抓」，且 WorkBuddy 走的是**借主进程代理**而非读取凭据文件。

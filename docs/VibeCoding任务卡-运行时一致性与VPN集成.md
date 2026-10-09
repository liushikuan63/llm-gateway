# VibeCoding 任务卡 · 运行时一致性与 VPN 集成

日期：2026-10-09。本文补充[原 A–D 批任务卡](VibeCoding任务卡-后续完善方案.md)，原任务不被 VPN 方向替换。当前事实与实测结果见[事实源](_FACTS-后续完善方案.md)及[验证记录](验证记录.md)。原任务卡里的 2026-10-05 数量和“尚未实现”属于历史盘点，新增工作以现码复验为准。

## 目标、范围与执行顺序

目标是把配置变更、请求执行、界面反馈及网络出口连成可验证的完整流程。用户已选择“工具内置 VPN 内核与节点管理”，因此增加本应用管理的 Mihomo 内核和节点面板；缓存、故障转移、审计及页面优化继续交付。

| 卡号 | 产出 | 依赖 | 状态 |
| --- | --- | --- | --- |
| R1 | 缓存失效代次、容量热更新、可选有效期及设置入口 | 现有精确缓存 | 已完成，完整门禁通过 |
| R2 | 流式首字节前接入鉴权复测、降级与停用策略 | 现有 FailoverChain | 已完成，完整门禁通过 |
| R3 | 用量/设置页防乱序、轮询单飞、操作提示保留 | 现有 IPC | 已完成，完整门禁通过 |
| R4 | Node 参考实现的 SSE/NDJSON/UTF-8/上下文边界 | 与正式版分开验收 | 已完成，92/92 |
| V1 | 本应用内核启停、节点导入/选择、模式切换和统一出站代理 | R1 的配置发布入口、现有 reqwest/crypto/process | 已完成首阶段，离线实核通过 |
| V2 | 固定版本内核安装/更新、校验、回滚与发行打包 | V1 | 本轮完成，NSIS 实装/卸载及 MSI 提取运行通过 |
| V3 | 系统代理、TUN、管理员服务、崩溃及卸载恢复 | V1/V2 生命周期验证 | 后续 |
| N1 | 不发模型请求的诊断报告与脱敏导出 | R1/R2/V1 | 本轮完成，7 条回归与真实包端验证通过 |
| N2 | 真实请求形状的路由预演 | 现有 explain_routing / classify_preview | 后续 |
| N3 | 并发预算预占、取消结算和未定价策略 | 现有预算/计价/审计 | 后续设计与实现 |
| N4 | 跨协议契约夹具、网络故障与版本升级回归 | R2/R4/V1 | 后续 |

第一批按 R1/R2/R3/R4 与 V1 分开验收，第二批完成 V2/N1 并重新跑完整门禁。NSIS 实际安装/卸载与 MSI 私有目录提取运行分列；MSI 整机安装仍未测试。原 A–D 和 V3/N2/N3/N4 继续保留，后续卡不以“计划已写”标记完成，也不未经验证批量改版本号。

## 给编程助手的共同上下文

```text
正式仓库：llm-gateway，Rust/Tauri 2/React 18，package-lock.json/Cargo.lock锁版本。
独立参考目录：../llm-gateway-node；它不是正式行为基准，也不随Rust仓库必需交付。
先读AGENTS.md、CLAUDE.md和本任务相关实现，检查git status，保护已有未提交修改。
行为权威是src-tauri/tests/。不加依赖、不改路由权重、不改表结构、不做无关重构。
Windows命令用PowerShell，保留文件编码/换行。先执行scripts/cargo-env.ps1；
它会进入src-tauri，后续npm命令先回仓库根。
修复测试先在旧逻辑失败，再在新逻辑通过；默认关闭/不限/严格策略也必须有对照。
UI改动必须真实跑verify:ui并检查新截图，build通过不能替代浏览器验证。
收费上游、真实节点订阅和系统网络改动不作为离线测试前提。
完成后检查git diff --check，记录准确通过/失败/ignored数量及尚未测试的边界。
```

## R1 · 缓存配置和在途请求保持一致

现象：缓存清空只移除已完成条目，未完成请求可能在清空后回填；容量仅构造时读取。配置界面此前没有精确缓存容量/有效期入口。

```text
输入：cache/mod.rs、proxy/server.rs、commands.rs、Settings.tsx、response_cache.rs。
在缓存mutex中管理generation、配置、条目和淘汰顺序。
MISS返回带key/generation的token，store在同一临界区核对generation再写。
配置快照与请求generation一致读取；请求旧snapshot不能借新代次回填。
GatewayState统一发布cfg、router规则、上游代理和cache配置，所有保存/导入入口接入。
五个配置写入口共用无await提交临界区，持AppState.config写锁完成落盘和运行时发布；
普通保存/快照/导入从锁内继承最新统一Key，只有专门轮换在锁内生成新Key。
保存失败不替换当前配置或发布；快照响应须使用实际提交的cfg。
新增ttl_secs，默认0表示不过期；容量默认200，缓存默认关闭。
TTL使用单调时间，从写入开始计时，命中不延长；过期条目不能再HIT。
UI提供开关、容量>=1、TTL>=0，拒绝非法或不安全整数，保留其他配置字段。
输出：缓存实现、运行时接线、设置入口与能失败的回归。
```

验收：旧 MISS 回填返回拒绝且条目为 0；lookup 晚于失效也不能取得新有效 token；容量热改为 1 后最多 1 条；TTL=1 到期变 MISS；TTL=0 保留原行为。缓存关闭、流式/多模态/工具/搜索绕过、HIT 会话续接和零新增计费均保持。

```powershell
& .\scripts\cargo-env.ps1
cargo test --lib cache::tests --jobs 1
cargo test --lib commands::config_commit_tests --jobs 1
cargo test --test response_cache --jobs 1
```

## R2 · 流式鉴权策略与非流式一致

现象：非流式已经支持 auth_failure 策略，但流式启动路径只判断 retryable，401/403 直接结束。

```text
输入：proxy/server.rs、router/failover.rs、server_stream.rs、auth_failover.rs。
把连接上游和读取首个有效事件作为FailoverChain的一次完整尝试。
复用auth_failure.mode、confirm_retries及auth_confirm_handler，不复制策略判断。
Strict首次401/403立即结束；Skip确认后换有Key候选；SkipAndDisable确认后停用，
保留主用/豁免保护。401/403不能把上下文回落到免Key供应商。
初次、复测和候选切换共同消耗总尝试预算；跳过候选不能白占预算。
客户端开始输出后，不再进入任何重试闭包。
保留Ping/Usage/Finish预读、响应协议、审计trace和真实尝试链。
```

验收：严格模式、一次瞬时拒绝后恢复、持续拒绝后降级/停用、豁免、免 Key 候选跳过、预算耗尽及首块输出后中断。所有新用例使用本地 mock 上游；不能把“返回 200”作为完整流结束的证明。

```powershell
& .\scripts\cargo-env.ps1
cargo test --test auth_failover --test server_stream --test server_e2e --jobs 1
```

## R3 · 页面异步更新只接受当前请求

```text
输入：Stats.tsx、Settings.tsx、scripts/ui-smoke.cjs。
查询用生命周期/请求序号守卫，卸载、StrictMode重跑和新筛选使旧结果失效。
用量轮询同一查询保持单飞，上一轮结束后再等5秒，避免慢查询堆积。
加载错误与导出/保存结果分开存放，后台刷新不得抹掉用户操作反馈。
Settings保留初次失败和重试入口；后续刷新不能覆盖更新、更晚的错误或成功状态。
所有设置操作进行中锁定配置控件，通用配置/规则入口也检查缓存草稿；
清空校准使用独立revision，晚到的旧轮询不能恢复已清除样本。
浏览器夹具控制Promise完成顺序并统计实际IPC调用，不只检查页面存在。
```

验收：先发 4xx 查询、后发 5xx 查询，再让前者最后完成，表格仍是 5xx；轮询无重复在途查询；成功/失败导出提示经过一次轮询仍可见；Settings 旧成功/旧错误都不能盖掉当前结果。生成并目视检查截图。

```powershell
# cargo-env.ps1 会进入 src-tauri；前端命令须回仓库根
if ((Split-Path -Leaf (Get-Location).Path) -eq 'src-tauri') { Set-Location .. }
npm run build
# 另一个终端：npm run dev -- --host 127.0.0.1
$env:LLMGW_UI_OUTPUT = Join-Path (Get-Location).Path '.ui-smoke-out'
npm run verify:ui
Remove-Item Env:\LLMGW_UI_OUTPUT
```

## R4 · Node 参考实现协议边界

```text
输入：同级Node项目src/proxy/upstream.js、src/server.js、src/context.js及test/。
先备份，因为该目录不是Git仓库。
SSE按行解析LF/CRLF/裸CR及多行data；Ollama独立解析NDJSON。
OpenAI finish_reason只记录生成停止，保留后续usage-only，终态只发一次done。
EOF缺完成标记、坏JSON及上游error必须报错，首块后不降级、不落成功审计。
JSON请求正文按Buffer累计后一次UTF-8解码，保留体积限制。
上下文按完整消息元数据做最长历史后缀/输入前缀重叠，不能从任意首匹配截断。
匿名会话ID策略另列后续，不在兼容修复里悄悄改身份规则。
```

验收：分块 CRLF、中文 UTF-8 字节拆分、工具元数据、重复短消息、Ollama/usage-only 的实际 token 进审计。Node 的 `npm test` 单独跑；参考实现通过数不混入 Rust 数量。

## V1 · 本工具管理内核与节点

产品流程：安装随附的固定官方 Mihomo 或选择本地内核 → 导入节点 YAML/HTTPS 订阅 → 启动 → 选择代理组节点/模式 → 应用为网关出口。同一应用管理 AI 网关和节点，不需要同时操作另一个客户端。

```text
输入：新增vpn.rs/tests/vpn.rs，lib.rs、commands.rs、api.ts、App.tsx与独立VPN页。
VpnManager持有自己的Child和生命周期锁；Windows隐藏启动，kill_on_drop，退出清理。
默认关闭，不在应用启动时启动VPN，不探测/终止用户FlClash或其他进程。
核验官方Mihomo -d/-f/-t，启动前先验证生成配置，超时或失败杀掉自己的子进程。
使用已有serde_json生成主配置：mixed-port/控制器绑定127.0.0.1，随机鉴权secret，
allow-lan=false，tun.enable=false，Selector VPN组及MATCH规则；空组回退为REJECT。
用户YAML只作为file proxy-provider读取proxies节点，不导入其控制器、脚本、TUN和规则。
节点文件加密保存，运行期临时明文只在独立vpn目录，停止/失败/正常退出清理。
subscription仅HTTPS、禁止URL凭据、禁重定向、有大小限制；不保存URL、不回显敏感错误。
控制器请求no_proxy、固定回环、带鉴权、禁重定向；IPC只返回状态和节点名。
通过/version、/providers/proxies/imported和/proxies确认真实节点已加载，
不能把-t成功当作节点有效。空节点/坏节点必须拒绝启动并清理。
通过PUT /proxies/{group}、PATCH /configs控制核心；切全局前先将GLOBAL选到VPN组。
只允许Selector已有成员切换，名称用URL path segment编码，mode仅rule/global/direct。
配置变更运行时禁止；明确提示先停止。节点导入后再启动，错误不得显示假成功。
V1提供本地文件入口；V2补上随附包安装、固定官方下载、校验和打包。
```

统一出站：正式版 `http_proxy` 必须实际用于普通/流式/非聊天模型请求、搜索/改写、模型目录、额度查询、定价及 CLI 版本查询。显式代理下，本地模型/回环/字面私网地址直连。关闭/未指定时保留原 reqwest 环境代理行为。

应用 VPN 出口前要求受管内核在运行；取消只清除此工具对应的代理地址，不能覆盖后来设置的其他代理。变更端口只迁移仍属于旧 VPN 地址的网关绑定，未绑定不自动启用，其他代理保持；联动保存失败恢复原内核设置。停止内核后不悄悄切成直连；页面说明网关仍保留代理配置，需要取消或重新启动。V1 不负责 CLI 子进程自身的全局网络，V3 才扩展整机接管。

验收：设置端口合法且不同；占用不能被识别成自己的内核；启动验证失败/超时清理；控制器鉴权和 URL 编码、非法组成员/模式拒绝、节点加密与明文清理；代理 mock 接收公网请求而回环请求直接到目标；VPN 页面错误/启动/切换/应用/停止/取消网关绑定均有浏览器断言。应用退出清理由后端生命周期测试验证，不能用浏览器冒烟替代。

```powershell
& .\scripts\cargo-env.ps1
cargo test --lib outbound::tests --jobs 1
cargo test --test vpn --jobs 1
```

已另用官方 Mihomo v1.19.32 对 localhost-only SOCKS 节点、空节点、错误节点形状、GLOBAL 选择和关闭 TUN 做离线实核验证；这些结果不等于外部节点连通性。可用已校验的官方内核运行显式验收（默认 ignored）：

```powershell
# 已执行cargo-env.ps1，当前目录为src-tauri
$env:LLMGW_TEST_MIHOMO_PATH = 'C:\verified-core\mihomo.exe'
cargo test --test vpn real_mihomo_file_provider_offline_acceptance -- --ignored --nocapture
Remove-Item Env:\LLMGW_TEST_MIHOMO_PATH
```

真实内核/节点和整机 TUN 的验证须分别记结果。控制器 mock 通过不能写成“已验证全部节点和真实 VPN”；以上显式验收不下载内核、不请求外部上游、不接管系统网络。

## 新增与后续任务卡

V2/N1 已落实，以下保留实现方案及验收条件，便于复验与后续升级；V3/N2/N3/N4 仍是独立后续方向。

### V2 · 内核安装与发行闭环

使用固定官方 Mihomo v1.19.32 Windows amd64 compatible 发行和 SHA-256 清单。安装包随附官方 ZIP、对应源码 ZIP、原样 GPL-3.0 许可及来源说明；优先离线准备，缺少资源的开发环境才从固定官方地址下载。存在但损坏的资源拒绝安装。内核和安装记录在本工具目录原子发布，失败保留旧选择；同版本同校验值幂等。升级先停止受管核心，回滚恢复之前有效的路径，不替换用户提供的文件。首次新核心启动失败会恢复旧路径，不自动启动旧核心。禁止追踪 latest 后直接执行未验证二进制。

```text
输入：vpn_install.rs、vpn.rs、commands.rs/lib.rs、api.ts/Vpn.tsx、发行资源和verify-release。
固定version/asset URL/ZIP与EXE哈希/入口名，IPC不可传下载URL或校验值。
官方HTTPS下载只允许固定官方资产域和有限重定向，超时/大小限制，有本地fixture验失败。
同卷private staging：校验ZIP→安全解压唯一指定EXE→校验EXE SHA→PE amd64→-v固定版本。
解压子进程只执行ASCII固定脚本，路径作为参数，不拼shell；拒绝路径穿越/链接/重复入口。
持生命周期锁，运行中禁止安装/回滚；取消/失败清理暂存与自有验证子进程。
settings.json单文件同时原子保存选择、当前/之前安装及pending；写失败不发布状态。
预置资源经Tauri Resource路径传入manager，纯new(root)仍支持原测试/手动模式。
kernel_info不返回包路径/URL参数，UI明确支持架构、目标版本、运行锁和旧选择回滚条件。
manifest与运行时常量、binary/source/license的哈希由verify-release一致校验。
NSIS实际安装到独立目录；MSI提取运行与整机安装分别记结果，不把提取写成整机安装。
绝对LLMGW_DATA_DIR隔离本工具状态，另设WebView2数据目录、关闭Agent，不宣称完整portable。
用真实WebView IPC验完整流程，boot splash退场且页面结果加载后才截图。
RunEvent::Exit显式清理用自有窗口线程WM_QUIT复验，不代替托盘按钮或全部Drop的验收。
```

验收覆盖下载断网、校验失败、错误架构、正在运行、更新后启动失败回滚、卸载清理范围。真实核心离线验收与默认 mock 数量分列。本轮 NSIS 实装/卸载、MSI 提取运行各项通过；MSI 整机安装及干净 Windows 的 WebView2 部署未测试。随附资源不意味着已经连接外部节点，也不自动接管整机网络。

### V3 · 系统代理和 TUN 的完整生命周期

在 V1/V2 完成后新增独立开关。系统代理先记录原值和本工具租约；只恢复自己仍拥有的值，不覆盖后来被其他软件修改的设置。TUN 通过独立、最小权限的 Windows 服务管理；本地模型/内网/控制器绕过规则与 DNS 策略明确可见。开始、停止、切换 profile、崩溃恢复、开机启动和卸载必须成套实现。

验收必须用隔离 Windows 环境验证管理员授权失败、端口冲突、FlClash 共存、休眠恢复、应用崩溃、原系统代理恢复和卸载后无残留。未通过之前不自动启用整机网络接管。

### N1 · 一键只读诊断与脱敏导出

现有 `run_gateway_self_check` 会发最小模型请求，因此“本地诊断”使用独立只读实现。收集监听状态、配置是否需重启、候选/禁用数量、缓存统计、预算口径、VPN 是否运行及受管代理是否失联；不发送 HTTP/TCP/模型或公网请求。报告区分 unknown/warning/error，导出只含结构化白名单字段，不含 Key、订阅、请求正文、名称、地址、用户路径或底层错误。

```text
输入：diagnostics.rs/tests/diagnostics.rs，GatewayState/ResponseCache只读快照，IPC/API与Diagnostics页。
serve实际bind后记录监听快照，RAII在退出/取消清除；其他进程占端口不能充当本应用健康。
cache.snapshot_stats仅读物理条目和计数，不像stats那样清理过期项，不新增HIT/MISS。
VpnManager try_lock只读自有Windows process handle，不refresh/reap或访问控制器；忙时None。
summary.vpn_running未知为null，不能将等待安装锁误报为已停止；历史上游健康不冒充在线探测。
VPN未知且显式代理存在时代理归属也未知，UI不能断言为其他代理。
数据库读取限时，启用模型仅计已启用供应商；预算只计启用正限额Key，USD/CNY/unknown分桶。
DTO白名单只含固定文案、枚举和计数；数据库/文件错误使用固定非敏感文案。
页面单飞/请求代次/卸载守卫，导出反馈独立；显式文件对话框选择本机JSON，不向外发送。
先取得旧stats会删过期缓存的红证据，再验证零上游请求、状态不变、失败不泄漏。
```

验收：诊断期间上游调用数为 0、不会改变配置；禁用供应商/空候选/停止内核可定位；注入夹具秘密后导出无泄漏。真实端到端自检仍是单独用户动作。

### N2 · 按真实请求预演路由

已有能力页和 `explain_routing`，新增的是请求形状预演：模型名、工具、图像/音频/视频、token 估算、访问 Key 白名单和策略一起给出过滤原因/候选得分。复用实际解析、硬约束和评分；默认启发式，不联网跑 Jev、不搜索、不消耗配额。必要时提供显式在线试跑，与只读预演分开。

验收：同一离线输入的硬过滤/排序与真实 dispatch 一致；旧六档 golden 不变；预演不写会话、不调用上游、不修改健康分或预算；解释能区分“被排除”与“分数低”。

### N3 · 并发预算预占与实际结算

现有闸门按已落审计消费判定，两次并发请求可能同时获准；不能用界面剩余额度代替原子预算控制。先明确未定价模型、最大输出估算、实际超过预估及客户端断开时的策略，再实现同一临界区判定/预占与每请求一次结算。不同币种仍分开，失败/取消释放预占，真实上游已消耗部分不能凭请求失败抹除。

该卡涉及计费口径，单独设计后实施；若需迁移表结构，先补升级/回退方案，不能挤入本轮 R/V 修复。验收使用同步屏障并发发两请求，证明不能同时越过限额；覆盖缓存 HIT 0 新费用、流中断、重复终态、跨自然月及恢复重启。

### N4 · 跨协议契约与网络故障回归

把完成、错误、工具调用、token usage、CRLF/NDJSON和半流中断制作成脱敏 JSON 夹具，分别验证正式 Rust 与独立 Node。新增 VPN mock 的断连/拒绝和配置热切换，记录差异矩阵；相同夹具不代表两个项目功能完全等价。把已覆盖的协议与手动/真实上游项目分列。

验收：夹具脱敏门禁、错误完成状态、唯一终态、准确 token 和首块后无重放；未实测的协议/内核版本写 unknown。升级框架或内核前先跑矩阵，无理由不改 golden。

## 最终门禁与交付

```powershell
# 在 llm-gateway 仓库根执行
if ((Split-Path -Leaf (Get-Location).Path) -eq 'src-tauri') { Set-Location .. }
& .\scripts\ci-local.ps1 -Step all
# 浏览器验收前，另开终端运行 npm run dev -- --host 127.0.0.1 --port 5173 --strictPort
$env:LLMGW_UI_URL = 'http://127.0.0.1:5173'
$env:LLMGW_UI_OUTPUT = Join-Path (Get-Location).Path '.ui-smoke-out'
npm run verify:ui
Remove-Item Env:\LLMGW_UI_OUTPUT
Remove-Item Env:\LLMGW_UI_URL
# Node参考目录另跑 npm test
git diff --check
```

第一阶段历史结果保留：本机门禁 **CI OK / exit 0**；Rust **1063 passed / 0 failed / 16 ignored**，56 份 suite 输出；UI **81 张截图**、Node **92/92**，显式真实 Mihomo **1 passed / 0 failed / 8 filtered out / exit 0**。当时只完成 R1–R4/V1，V2/V3/N1–N4 仍为后续；20 章手册和 87 个 Rust 文件的门禁数字属于该阶段，不再作为最新数量。

2026-10-09 V2/N1 最终结果：完整本机门禁 **CI OK / exit 0，1078 passed / 0 failed / 17 ignored**；完整浏览器 **UI_SMOKE_OK / exit 0，97 张截图**；Node **92/92**；21 章手册共源及其余本机门禁通过。官方实核 V1 和官方 ZIP 安装显式用例各 **1 passed / 0 failed / exit 0**，共额外 **2 passed**，不重复计入默认 Rust 数量，也不把 17 个默认 ignored 写成全部实测。

两包 **0.2.0 / Windows x64 / 未签名** 构建 exit 0，NSIS 静默安装后真实 WebView/IPC **8 检查 / exit 0**，实际卸载 exit 0，并等子卸载进程完成，程序/resources 删除、测试注册项清理，私有数据保留；原用户配置、主密钥、DB/WAL/SHM 共 **5 文件**大小/哈希不变。MSI `/a /qn TARGETDIR=私有目录` 提取 exit 0，提取 EXE 真实 WebView/IPC 也 **8 检查 / exit 0**，不宣称整机 MSI 安装。资源逐字节核对及 Tauri 的 3 字节 bundle marker 差异已单独说明，包大小/SHA 与历史包区分，详见 [验证记录](验证记录.md)。

可复制真实包端验收（先完成安装或提取；`--exe`、`--data`、`--output` 必须是绝对路径，`--data` 和 `--output` 都必须尚不存在）：

```powershell
# 先只读核对已安装/提取载荷；--bundle 按实际来源选择 nsis 或 msi
node .\scripts\verify-installed-package.cjs --bundle nsis --dir 'C:\acceptance\installed'
# MSI 例：node .\scripts\verify-installed-package.cjs --bundle msi --dir 'C:\acceptance\msi-extracted'
node .\scripts\package-smoke.cjs `
  --exe 'C:\acceptance\installed\llm-gateway.exe' `
  --data 'C:\acceptance\private-data-new' `
  --output 'C:\acceptance\results'
```

脚本使用隔离本地状态与 WebView2 数据目录、不启用 Agent、只导入 localhost-only 测试节点；退出只作用于自己启动的应用窗口线程及受管核心。最后核心 PID 清理通过后才写成功记录。实际最终证据在 `%TEMP%/llmgw-package-acceptance-20261009` 的 `nsis-results-final` / `msi-results-final` 及 `uninstall-and-user-data.json`。`LLMGW_DATA_DIR` 不是完整 portable 模式，不迁移外部 CLI、默认 Agent 产物或系统配置。

本轮 R1–R4/V1/V2/N1 已完成，V3/N2/N3/N4 继续保留。MSI 整机安装、外部节点/真实订阅、收费上游、系统代理/TUN、管理员服务和 GitHub 托管 CI 未作为本轮通过项；WM_QUIT 验证的是 `RunEvent::Exit` 显式清理，不是托盘按钮实测，也不证明所有 Rust Drop 都执行。

推送前另补强 `proc_util.rs` 的测试就绪与清理夹具，覆盖超过旧等待窗口的 2 秒启动延迟；仅改 `#[cfg(test)]`，生产段字节不变，保留原 3 项测试。补查格式、Clippy、全目标编译和 3 项定向测试均通过，不增加全量通过数；具体历史 CI 失败与补测证据见[验证记录](验证记录.md)。

交付记录写明每张卡状态、新增失败证据、最终真实通过数、ignored 和未执行范围。外部 API 依据只查公开项目名及协议问题，不上传源码、日志和订阅凭据。

## 已核对的一手依据

- [React useEffect](https://react.dev/reference/react/useEffect)：cleanup 可忽略过期异步结果；适用于项目锁定的 React 18 useEffect 用法。
- [WHATWG SSE](https://html.spec.whatwg.org/multipage/server-sent-events.html)：UTF-8、LF/CRLF/CR 行结束及多行 data。
- [Ollama streaming](https://docs.ollama.com/api/streaming)：原生流为 NDJSON；[chat](https://docs.ollama.com/api/chat) 的终态含 token 计数。
- [FlClash 官方仓库](https://github.com/chen08209/FlClash)：跨平台客户端及功能参考，不作为本项目实现依赖。
- [Mihomo API](https://wiki.metacubex.one/api/)、[proxy-provider](https://wiki.metacubex.one/config/proxy-providers/)及[官方 CLI 源码](https://github.com/MetaCubeX/mihomo/blob/Meta/main.go)：启动和节点控制接口。
- [Mihomo 上游许可](https://github.com/MetaCubeX/mihomo/blob/Meta/LICENSE)：GPL-3.0；V2 发行需保留其许可与源代码取得资料，不在本轮变更本项目许可证。
- [reqwest 0.12.28 ClientBuilder](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html)：项目直接依赖的锁定版本，显式代理及 no_proxy 设置。

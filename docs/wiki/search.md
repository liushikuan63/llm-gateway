# 搜索模块设计与边界

## r7 已存 npm／HN 真实投影的离线 UI 回放（2026-10-26）

复用上一批两次真实来源收据中保存的白名单响应投影，不重新抓取。12项Windows r7生产前端资产与原构建和独立静态绑定前后均相同；通过仅loopback静态服务和新空Chromium上下文运行，配置／IPC启动壳为明确合成资料。投影保存每条结果的source，离线适配器仅按原顺序、长度和值生成result_sources数组；snippet、score、源完成时间、复用状态、locale等未保存字段不补造。

一次聚焦浏览器回放（session86641，exit0）实际断言并截取3图：npm三条包名／规范链接／来源标签正确；通过正常可用范围控件切到HN后，旧npm结果清空且没有额外搜索；再回放HN三条题目／规范链接／来源标签，未混入npm。截图外层明确OFFLINE REPLAY，生产资产和结果DOM未修改；两次test_search_backend为合成IPC回放，不是两次新外发。独立验收者和集成方均实际查看3图，未发现本片显示缺陷。

页面创建前已限制HTTP为该loopback源、关闭所有WebSocket路由、阻止service worker／弹窗／下载；记录到的非loopback HTTP、WS、弹窗尝试均0。这是浏览器拦截层证据，不冒充系统抓包。浏览器／上下文／本机静态服务均关闭，资产未变；已装应用启动、真实来源、模型、认证、安装和构建均0，模型保持27/100，Global不触碰。最早默认沙箱prepare无法见到Chromium的exit1完整保留；随后正常审批环境使用同一默认Chromium成功，不换浏览器路径或下载。

证据等级仅为“已保存真实响应投影＋显式无损来源适配”的生产资产离线UI呈现。不是同一次线上端到端UI或新原生WebView验收；不补证缺失的quota／原始backoff／费用，也不扩称取消竞态或迟到回包实测。独立报告 `E:/codex/2026-10-14/task/work/m5-merged-candidate-r7-v1/npm-hn-offline-ui-replay-v1/independent-completion-v1.json`，SHA256 `78093be69db9c80dc55eeb0d8007731bb56677ee65df2534ad7b2c1d7d1257ec`。原真实批次一次启动／两GET已消费，不复用。

## r7 npm／Algolia HN 单次真实来源验收（2026-10-26）

已装r7（NSS `77f409a6…`、冻结输入 `a52c5e07…`）经新的空隔离资料，执行一次启动、两次真实注册生产IPC。固定顺序为npm Registry `GET https://registry.npmjs.org/-/v1/search`（text=npm package typescript、size=3），成功并后检后，才切换Algolia HN `GET https://hn.algolia.com/api/v1/search`（query=Rust programming、tags=story、hitsPerPage=3）。HN是Algolia搜索服务，不将其误称Hacker News Firebase API。各自正常返回3条元数据；未带账号／Key／Cookie，不打开结果链接、下载包／正文、运行脚本、翻页、重试或切换出口，不调用模型／认证／安装。

### npm_registry

- [@typescript-eslint/eslint-plugin](https://www.npmjs.com/package/@typescript-eslint/eslint-plugin)
- [@typescript-eslint/parser](https://www.npmjs.com/package/@typescript-eslint/parser)
- [@typescript-eslint/type-utils](https://www.npmjs.com/package/@typescript-eslint/type-utils)

### hacker_news

- [Next Iteration of “The Rust Programming Language” Book](https://news.ycombinator.com/item?id=13663751)
- [RustBelt: securing the foundations of the Rust programming language](https://news.ycombinator.com/item?id=16302530)
- [Microsoft is creating a new Rust-based programming language for secure coding](https://news.ycombinator.com/item?id=21682867)

各GET1／合计2的计数依据正常原生响应和冻结单源、单send、无重试生产合同，不是独立抓包。两源的DTO没有公开共享quota或原始上游backoff，保持未知；实际状态均由not_blocked_locally变为local_interval cooldown，不能将本机等待当成供应商限流／余额／重置。费用无独立收据，不写现金零费用已证实。

执行期间不重叠两源请求；scope更改沿既有生产配置代次／缓存隔离。结束关闭搜索，最后HN保护投影保留；安全配置唯一预期差异为scope从npm变HN，其余安全投影相同。应用正常退出0，端口释放，EXE字节不变。正式资料未访问是新路径／环境和原生安全投影证据，不扩大为正式DB全字节审计。本次是安装版真实来源IPC，不是这批真实结果的UI渲染或完整LLM上下文注入验收；既有合成UI证据单列。

执行会话83959退出0；新增必要离线检查11项和3项语法均通过。独立报告 `E:/codex/2026-10-14/task/work/m5-merged-candidate-r7-v1/npm-hn-live-once-20261026-v1/independent-completion-v1.json`，SHA256 `e762237a00d1f8e19724408cfe6e07f5ea4e3d0c3cd6da6b69765685bf1b0488`。一次启动／两GET已消费并释放窗口，不追加请求；模型27/100，Global及其他原Goal依赖不变。

## r7 单次匿名真实 Stack 来源验收（2026-10-26）

已装r7（NSS `77f409a6…`、冻结输入 `a52c5e07…`）在一份新空隔离资料中，临时启用显式Stack范围，经真实注册生产IPC `test_search_backend` 调用固定受保护消费者。一次启动／一次检索IPC／一次官方GET，固定关键词 `Rust async`、site=stackoverflow、page=1、pagesize=3、sort=relevance、order=desc；不传Key／Cookie／账号凭据，不走代理／重试／翻页／fallback，不打开结果链接。正常返回3条公开题目标题、规范问题链接和stack_overflow来源，详情见收据，不保存原始响应／owner／答案。

返回共享IP／服务配额剩余 `299`，观察时间02:58:55.608 UTC；有效not_before为02:59:55.608 UTC。该有效等待包含本机规则，不是原始上游backoff；DTO不暴露原值，保持未知，未虚构0或重置时间。它不是个人余额／免费模型额度，也不保证未来仍299。关闭search前后原生状态投影相同，未重置ledger。

本批是安装版真实来源IPC，不新称真实结果的UI渲染或LLM自动注入通过；旧UI／生产消费者合成证据分别保留。没有触发限流／收费／未知配额拒绝／恢复，不把这些未观测路径说成实测。正常退出0、端口释放、配置安全投影相同、EXE未变；正式账号资料未访问，未作正式数据全字节审计。

独立收据：`E:/codex/2026-10-14/task/work/m5-merged-candidate-r7-v1/stack-live-once-20261026-v1/independent-completion-v1.json`，SHA256 `e61ec5f459eaf9b7688c2f9492b7c7f09578fbb9be775c7bd3bcab1848855ad7`。GET1计数依据正常响应＋冻结无重试单send合同，非独立抓包。已关闭隔离来源／释放窗口，本票据已用完，不追加采样；模型仍27/100，Global和其他依赖不变。

## Stack Overflow 显式路由、只读状态与界面（2026-10-25 离线源码收口）

FreeAggregate 的新显式范围是 `stack_overflow`，默认 Web、旧 npm/HN 值不变。只有数据库绑定的[资源](../../src-tauri/src/search/resources.rs)提供该能力；[执行器](../../src-tauri/src/search/executor.rs)的预取和手动诊断共用受保护消费者。生产目的地固定为 `https://api.stackexchange.com/2.3/search/advanced`、`site=stackoverflow`，不接模型客户端／凭据，不走其他来源、重试或翻页。旧直接执行／聚合入口、无DB和已知过期配置固定拒绝。配置在最终检查后再改变，不表示已接受的远端请求被取消；旧请求也不能填入新缓存代次。

预取沿用显式查询缓存／合并策略；手动“搜索一次”绕过查询缓存，但继续遵守持久等待。unknown 控制信息可保留当前有效元数据，同时错误标记阻止成功缓存及后续新请求；已知0不自行恢复。保留原最小一分钟与更长用户间隔、backoff、CAS和取消未知语义。只支持问题元数据，不套用网页语言偏好、不读取正文；中性 relevance0 不是质量评分。原生匿名共享IP计数不能当账号余额或免费／费用保证。

[真实注册命令](../../src-tauri/src/commands.rs)只在已选该范围时投影状态，get settings 的状态读取不预留或创建HTTP传输。DB-less仍只声明旧三范围。显式 `recover_stack_overflow_unsent` 必须当前FreeAggregate／Stack范围、资源配置匹配及当前可信账本Reserved证据，随后以原typed CAS决定；保留wait／旧quota，未知、zero和旧版pending都拒绝。十秒未得确认按unknown处理，监督中的数据库工作可能仍完成，需只读重核而不自动重试。这不是任意旧整库回滚的安全授权。

[界面](../../src/components/StackOverflowProtection.tsx)严格区分本机可尝试／等待／未授权预留／结果未知／耗尽／不可读取。schema缺失或坏JSON保持未知并阻止手动检索；计数为十进制u64字符串，真实0不丢。只有Reserved出现一次显式恢复，键盘和消失后焦点回到检索输入；不自动恢复／重发。手动终态后只读回本机状态，按原request代次与卸载守卫发布，保留草稿和已有结果。旧三能力列表不冒充新支持；未保存的后端意图不能悄悄走旧Stack目的地。结果卡后端仅表明免费聚合类别，各条真实来源单独显示，不再固定冒称Bing／DuckDuckGo。

原生本轮132库＋57集成＋3注册IPC共192项、实际TSX的坏JSON3项、7组独立构建前端资产UI，以及types／build／check／Clippy（ipc-tests）／fmt／脚本语法／脱敏／文档门禁通过。此前快照1测试（三表例）按create/apply函数与原测试输入未变证据复用，不计作本轮重新执行。17文件独立复核接受；JSON属性转键Important经0/3红→3/3绿关闭，实际截图的错误来源标签另有生产资产红例→修正。原夹具白名单／按钮定位／首次编译错误均保留，不冒充上游失败。证据 E:/codex/2026-10-14/task/work/stack-exchange-routing-v1/completion.json；截图核对桌面／390px和结果事实。只构建独立前端资产，未打EXE／安装包，未启动原生应用；自有Vite已退出，原测试端口独占绑定复用通过。

本片没有启用用户真实来源、请求官方API、触及真实账号凭据、签发／刷新或改变系统安全。源码、MockRuntime、loopback和生产资产浏览器不代表新包实装／供应商通过；r5及冻结r6不动，模型账本27/100。M3本地切片不标整个Goal完成。下一真实入口／后续候选实装需按原授权边界准备，不为保持active反复造检查卡；Global的20:05资格快照与敏感签发许可、其他区域第二主体条件仍独立保留。


## Stack Overflow 资源与快照边界检查点（2026-10-25 离线源码，尚未启用范围）

现有 create_snapshot/apply_snapshot 的真实注册命令在 Tauri MockRuntime、空内存库和 owned 配置目录中验证：旧快照恢复 config/provider 内容时，保留后来 Reserved 等待、Authorized 未知与已耗尽0的准入 meta。额外旧 meta 字段不会变成整库恢复。一项测试含三个表例；不是原生应用实装，也不证明任意整库／文件回滚安全。新测试兼容仓库既有 Temp/llmgw-personal-test-* 或固定带marker的 owned 目录，写前核对实际 config_path.parent 与 canonical 目录一致；不接触正式资料。

[资源消费者](../../src-tauri/src/search/stack_exchange_scope.rs)只把已知本地事实投影为 ready（本机可尝试）／waiting／reserved／outcome_unknown／exhausted／unavailable；未知不补0，共享IP上次返回的 u64 请求数以十进制字符串保留精度，不能当账号余额／当前可用保证。状态读取仅SELECT，不建HTTP客户端、不预留或重置。原 source_min_interval_ms 的较长值实际作用于预留及响应后等待，至少60秒、最多1小时；后来改短不能缩短旧等待。这是本机策略，不是供应商限额保证。

[Gateway资源](../../src-tauri/src/search/resources.rs)仅数据库型构造持有受保护源缓存，克隆共享一份；DB-less不提供此资源。代理／间隔相同时复用，换配置先校验再退场旧sender；强引用保留至retire ACK。Gateway现有drain_source_writes使用一个总时限覆盖新资源和旧cooldown，超时后不遗失旧owner，下次可继续排空；成功后允许下一显式会话重建，但旧持久wait不清。这不是远端取消、配额退款、全库restore fence或全局永久停发保证。

7项核心与2项Gateway有效红例均保留，最终相关库126、集成57、快照IPC1共184项不同测试通过；IPC1的三个表例不重复计数。check／Clippy（含ipc-tests）／fmt和文档门禁、七文件独立复核及最终输入绑定见 E:/codex/2026-10-14/task/work/stack-exchange-scope-v1/completion.json。未重跑无关全Rust／UI大套件；前端、配置enum及检索executor尚未改，不宣称选择器或实际来源已接通。下一项沿原M3受保护显式路由、只读状态／恢复命令与能力握手、opt-in UI继续；不得让DB-less／旧直连／fallback绕过准入。

此源码未入已装r5或冻结r6，不构建／安装、不发真实搜索／模型／认证。独立的Global到时只读复核已于20:05:33.661Z完成：can_reissue=true、attempt_blocked=true保留，未执行重新签发；不能重复启动该单次检查。真实模型账本仍27/100，完整Goal未完成。


## StackExchange确定未授权预留的恢复（2026-10-25 离线源码）

[准入账本](../../src-tauri/src/search/stack_exchange_admission.rs)新增schema2阶段：reserved、authorized、observed、not_sent。原v1只兼容读取；旧pending不猜未发，旧zero不重置，旧正额度仅在正常新reserve时升级，不批量回填。版本／phase／pending／quota不一致直接InvalidState，不修复或补0。View只多一个can_recover_unsent事实，没有账号或票据ID。

authorize必须CAS把匹配预留持久写成authorized，再复核派发窗口后才给Attempt；取消未拿到返回或提交后窗口过期，已提交的authorized仍是unknown，不能当未发。finish仅结算匹配schema2＋authorized的当前attempt，已知控制完成后变observed；缺／坏控制继续authorized pending。既有stale backoff只延长等待，旧quota不覆盖新事实。

显式recover_unsent只对当前**未回滚**账本的reserved pending执行CAS，改成not_sent并轮换attempt_id撤销旧票据；原ledger、创建、quota观察及全部等待保持。无记录／已完成是无写入NotNeeded；legacy／authorized pending或耗尽拒绝。authorize先赢则恢复不能成功，恢复先赢则旧票据不能授权。[发送器](../../src-tauri/src/search/stack_exchange_sender.rs)已接此本地API并持active守卫；没有自动恢复、重试、翻页或HTTP操作。

旧快照的reserved可能早于后来的实际授权；因此本API不是恢复快照的证明，quiesce／retire也不能让旧事实自动变可信。实际restore/import消费fence仍未接。单文件db4 close/reopen测试仅证明同一当前文件持久性，不证明回滚安全。来源及scope/UI仍未启用；不得以can_recover_unsent显示上游免费／可用。

原10项有效红例和额外授权提交后派发窗口过期负控0过1失败均保留；新增11项全绿，包含真实SQLite UPDATE worker入场后取消、授权／恢复竞争、旧版本／zero／坏阶段、存储失败与零HTTP的sender消费。最终117项搜索库包含这11项，另57 integration，共174项不同测试；check／Clippy（ipc-tests）／fmt和文档门禁全0，285原生输入绑定，扩展旧单文件场景不另加测试数。未重跑无关全Rust/UI长套件，也非安装或真实供应商验证。完成证据 E:/codex/2026-10-14/task/work/stack-exchange-unsent-v1/completion.json。

本源码片无正式库迁移、认证、真实模型／搜索或应用启动；同一轮另有已授权的Global资格只读启动1次，单独回执已写入六区Wiki，不能混称整轮零启动。已装r5、冻结r6、27/100保持；Global can_reissue=true仅资格结果，未签发或清block。


## StackExchange受保护发送消费者（2026-10-25 离线源码，未启用）

[ProtectedSender](../../src-tauri/src/search/stack_exchange_sender.rs)已消费响应检查器和持久准入账本：生产入口固定为`https://api.stackexchange.com/2.3/search/advanced`，GET、`site=stackoverflow`、单页、1–10结果、q转义和有界URL。复用现有无认证搜索出口客户端，不允许生产调用者注入模型client或目标URL；无环境代理、自动重试、重定向、翻页或错误fallback。显式无凭据搜索proxy仍按已有校验，不是系统代理修改。请求在持久reserve＋authorize成功后才execute。

全流程最多10秒，decoded body最多256KiB，UTF-8后复用严格JSON合同。有效quota/backoff在HTTP/API/items失败时也结算；429/503合法Retry-After（秒或HTTP-date）在body await前经受监督CAS只延长wait，不改quota／pending，因此body超时不会丢掉已知等待。缺／非法header不猜延迟。未知控制可能保留本次合法结果，但Completion=ControlsUnresolved且后续查询仍阻止；真实0继续exhausted。绝不把成功结果等同未来可用／免费额度或重置凭据。

错误只返回固定类别和NotStarted／MayHaveStarted／ResponseReceived三态。execute前标MayHaveStarted，收到header后标ResponseReceived；之后失败不能变成0发送，也不能据此证明收费。错误、ledger不保存查询、原始URL/响应错误正文、账号或凭据。取消丢弃本地HTTP future，已有DB元数据任务仍持owner监督直到应答；不退还未知请求。Sender.retire等待本地请求future再await Admission.retire，只是本producer边界，不证明远端停止或其他DB读写者已退出。

原11项有效红例全部失败；独立审阅补6个对照为1过5失败，随后新增17项全绿，其中包括实际gzip、小压缩包解压后超限、缺header压缩体固定失败、活跃HTTP关闭等待、429日期／非法Retry-After及保存失败。首轮相关库106、integration57、check均0，但Clippy因测试MutexGuard作用域exit101；只用词法块修正、无lint抑制或生产代码变化。最终库106复验通过，Clippy（ipc-tests）／fmt补验0；integration57和check0按生产前缀及其余输入逐字节未变证据复用，共163项不同相关测试，285原生输入和独立复核绑定。不是一次全套全绿，未重跑无关完整Rust/UI套件，也不是安装版或供应商验证。完整证据 E:/codex/2026-10-14/task/work/stack-exchange-sender-v1/completion.json，原失败和审阅历史pending快照保留。

[官方API总览](https://api.stackexchange.com/docs)、[方法](https://api.stackexchange.com/docs/advanced-search)、[限流](https://api.stackexchange.com/docs/throttle)及[压缩合同](https://api.stackexchange.com/docs/compression)已仅读取公开说明，没有调用搜索API。现有reqwest支持带Content-Encoding的gzip；代理去掉header的压缩体仍明确不支持，失败保持unknown而不猜数据。尚无scope/config/UI激活或未知／耗尽额度自动恢复；不猜quota reset时刻，不清pending。已装r5、冻结r6及27/100保持。


## StackExchange持久准入组件（2026-10-25 源码）

后续生命周期片新增可等待的`Admission::retire()`：关闭同一owner所有克隆的reserve／authorize／finish；只读view仍可用。私有受监督Tokio任务持owned读锁直到DB应答，外层future取消只分离等待者，不会让已提交SQLite写入失去关闭屏障。retire等待该owner已进入操作后不可逆关闭，不删除meta、不退还quota、不清pending，也不会停止已经独立发出的HTTP或其他DB使用者。实际restore调用者须先停妥这些producer，再await旧owner retire并创建新owner；没有接入此流程之前，不能宣称任意原地恢复安全。取消retire、runtime退出或任务panic不是关闭完成证据。

本片原6项行为红例及独立审阅提出的真实SQLite update-hook worker取消屏障1项红例均保留；先前未声明futures测试crate的编译错误是0行为，改为std poll_fn未增加依赖。最终新增7项包含于相关搜索库89项，另integration57项，共146项不同测试；check／Clippy／fmt全0，原生输入仅准入模块相对上次变更。独立源码复审和最终门禁绑定见 E:/codex/2026-10-14/task/work/stack-exchange-lifecycle-v1/completion.json。仍非完整来源支持，原冻结包／真实请求／认证边界不变。


[准入组件](../../src-tauri/src/search/stack_exchange_admission.rs)使用既有meta表的单一固定方法记录，无schema迁移、历史回填或查询正文／账号身份保存。预留不等于已发送或计费；消费票据前校验私有实例owner、原attempt和当前等待状态。Admission克隆共享owner，新实例不能消费复制数据库记录的旧票据。已知方法backoff只延长等待；过期回执不能改新attempt的quota事实。未知或格式不合法控制保持pending，真实0保持exhausted，不从日期或时间流逝猜恢复。

本组件的一分钟方法级间隔是比相同查询规则更保守的本地策略，不能表述为供应商所有查询的限速保证；尚无发送器，不声称精确HTTP发送间隔。视图只呈现本地等待和曾报告的共享额度，不代表账号余额、免费或费用。

最终新增16项包含于搜索库82项，另57项search integration，共139项不同测试；check、Clippy（含ipc-tests）和fmt均exit0，284项native输入未漂移。独立源码审阅关闭全部Important；原8失败、生命周期2失败、记录边界3失败、owner负控1过2失败及中间Windows夹具清理9过1失败均保留，不伪装一次全绿。最终单独进程后清理只处理新记录的合成夹具，exit0，不递归、不重试。完整绑定见 E:/codex/2026-10-14/task/work/stack-exchange-admission-v1/completion.json；未重跑全部Rust长套件。

本准入组件阶段当时尚无发送器；后续受保护消费者源码已在本页当前章节补齐，仍未接显式scope/UI或启用来源。未知pending／耗尽状态恢复和真实DB恢复消费仍须有证据，不能把owner当任意回滚检测。Web/npm/HN默认行为、已装r5、冻结r6、27/100及Global block保持。


## StackExchange响应合同前置（2026-10-25 源码）

原搜索卡仍有StackExchange显式技术来源待实现。本首片只提供[纯响应检查器](../../src-tauri/src/search/stack_exchange.rs)，没有新增配置scope、发送器、UI、真实请求或默认行为。后续接线必须固定`site=stackoverflow`，只把此站问题ID构造为官方question URL；不能拿其他站响应套用。只有标题及可选问题score／answered元数据参与结果，不遍历body、owner、答案或上游任意link，旧中性relevance0不是质量分数／费用。

[官方限流](https://api.stackexchange.com/docs/throttle)要求响应backoff后等待该方法；无access_token是共享IP额度，一分钟内不应重复语义相同查询。[Wrapper](https://api.stackexchange.com/docs/wrapper)和[advanced search](https://api.stackexchange.com/docs/advanced-search)是当前只读文档来源，不是实测可用或账号免费额度证明，也未证明精确reset时刻。

检查器先限256KiB UTF-8字节／合法时钟及1–10结果，再只接受对象根，保留派生解码的已知wrapper重复字段拒绝；不声称递归拒绝所有item重复字段。可识别有效backoff独立保留，即使API报错或items失效；非法控制显式malformed，缺quota是unknown、真实0是exhausted，不猜重置期限。has_more保留可选事实，不自动翻页。错误仅固定类别，不回传原API错误正文。

初始有效1过5失败后原6项转绿；独立I1证明位置数组可被Serde struct接收，新增单项0过1红后以对象入口关闭。测试写入CRCRLF的编译失败为零行为，另保留，不能算有效红。本次新增7项包含在最终搜索库66项内，旧integration／check／Clippy／fmt精确终态与输入绑定见 E:/codex/2026-10-14/task/work/stack-exchange-contract-v1/completion.json，不重复累加旧6或I1中间执行。

下一必要步骤仍是持久方法等待、重复准入、共享额度事实与生产发送消费者；在这些成立前不能显示来源已支持。源码不在r5安装或冻结r6中，没有新包／安装／认证／模型／真实搜索；原Goal、27/100、Global block和其他真实验收依赖保留。


## 可选公开结果正文：M3 v2 首片（2026-10-21 源码）

按已批准方案新增 `search.page_text_enabled=false`，仅 FreeAggregate 的 web 范围显式开启后读取。旧 TOML 缺省关闭；已启用值随配置重开保留。专用搜索设置命令拥有该开关，旧 IPC 省略字段、无关保存及完整旧快照不改变当前值；在当前界面明确取消勾选才能关闭。其他范围／后端保留原检索，不跟随 npm／HN 的结果链接。

[正文读取器](../../src-tauri/src/search/page_text.rs) 在一次预取起点捕获剩余搜索期限，正文最多4秒，且不超过原免费聚合10秒上限／更短有效设置。每次串行选最多2个合法去重结果，每页最多2次重新校验的重定向（最多6次页面GET）；Gateway共享并发2，繁忙直接跳过。取消Drop调用方future与permit，不启动后台页面HTTP任务；底层系统DNS解析不保证物理中止，但它完成后不会续发该已取消请求。

只接受有界、无userinfo的公网HTTPS:443，拒绝所有非公网／保留／混合DNS答案；规范化末尾点，固定实际连接到已校验地址，保留TLS证书和主机名检查。客户端不继承环境代理、不发Cookie／Authorization／Referer／搜索或模型Key、不自动重试。存在显式搜索代理时正文显示目标无法验证并跳过，原查询继续使用该代理；不会降级直连。owned TLS例外与测试CA仅在cfg(test)，不属于产品开关或发布路径。

每页解压后正文逐块最多256KiB，超限在复制前拒绝；仅text/html／text/plain和UTF-8，拒绝下载、不支持编码、错误页及可识别的挑战／付费墙。尽力抽取可见文字，丢弃脚本／样式／模板／表单等，最多每页6000字符／总12000字符；不执行JS，不声称HTML5布局、通用Readability、完整挑战页识别或进程RSS硬限。

原查询缓存仍只含标题／URL／摘要。正文在缓存外重新访问，只用于本次上下文引用并标注当次读取时间与不可信属性，不是发布时间或实时保证；失败仍保留原检索结果，不重放查询／阻断模型。抓取器不把HTML或摘录写入审计／会话／日志；模型回复仍按原来的会话和响应缓存规则处理，若回复引用网页内容，不作不落盘保证。原上下文窗口准入／裁剪继续在预取之后。手动“搜索一次”保持新查询语义，可返回正文读取状态／时间，但不返回或保存摘录。

每个GET／重定向继续先走现有来源CAS预留，遵守已提交Retry-After、用户间隔以及HN最低360ms保护；四个已知物理origin复用原来源身份。其他目标仅以规范化origin散列建立`result_page`命名空间，沿用原meta JSON字段，没有新表列或正文持久化字段。命名空间与内存最多256个来源约束，容量不足、坏记录或读写失败就拒绝，绝不为腾空间删除未来／未知／待提交限制；本首片没有自动清理策略。429／503只采用合法Retry-After并只延长，缺失／无效头不杜撰配额或等待。

本阶段117项不同Rust测试为有限并集：search库53＋原搜索集成55＋配置提交7＋Tauri命令MockRuntime／自有DB2。新增正文10项包含真实owned HTTPS读取、缓存外消费者、压缩展开／无EOF超限、重定向、取消并发、期限、origin等待及故障对照。初始配置0/1、读取0/1有效首红，以及缺模块／owned meta.updated_at夹具失败均保留。首个Cargo IPC进程在测试main前出现STATUS_ENTRYPOINT_NOT_FOUND；同字节EXE复制到任务artifact并配既有测试manifest后2项真实运行通过，原EXE不变。额外仅--list对照未唯一定位加载原因，不能据此宣称系统DLL损坏或通用加载已修复。Clippy含ipc-tests、check、fmt均0；源码UI新5组与原出口／复用／npm脚本通过，明暗390px及pending截图已看。浏览器替身和MockRuntime不是安装版。

未访问真实网页／搜索源／账号／模型或正式数据库、密钥，不改变系统／代理／安全设置，不构建或安装新包。真实公网DNS／TLS成功、代理正文、非UTF-8、JS渲染、更多源与通用无头／挑战冷却仍未验或未实现；本首片不代表M3整卡／完整M0–M5完成。历次历史段落记录的是各轮当时范围。


[返回维护入口](README.md) · [搜索增强任务卡](../VibeCoding任务卡-搜索能力增强.md)

## 选择与来源

旧五后端及默认配置保持兼容。新 `free_aggregate` 是显式选择的候选后端，聚合 Bing RSS 与 DuckDuckGo；它不是 Tavily/Brave 凭据被拒后的免费回退。任何有 Key 后端的 401/403 保留停止语义，不能借聚合重发同一轮。

内核在 [free_aggregate.rs](../../src-tauri/src/search/free_aggregate.rs)，生产选择与上下文预取在 [executor.rs](../../src-tauri/src/search/executor.rs)，实际设置入口在 [LocalModels.tsx](../../src/pages/LocalModels.tsx)。显式选择已经由独立协调QA在用户Windows安装版验证保存、退出重启及UI读回，绑定见[独立实装验收](../2026-10-18-Search与拒绝回复独立实装验收.md)；非空真实外网检索仍未验。

## 期限、取消与部分成功

两个源共享绝对截止，最多两个在途操作，不启动脱离调用方的后台任务。取消调用方时两条 HTTP future 同时丢弃。某源失败或超时应保留另一条已完成结果；不能由一个更早的外层 timeout 抢先丢弃部分成功。

正文按 chunk 有界累积，不能仅信 Content-Length。结果数、检索词、URL、标题、摘要也各有界。无 Content-Length 的超大流应在等待最终字节或完整源期限前停止。生产免费聚合使用独立直连客户端，禁用重定向和自动 HTTP 重试；不发送供应商 Key，不修改系统代理。其他后端原出站行为保持。

来源顺序用于稳定融合，不代表置信度。URL 去重保留百分号编码和 query 顺序/值，仅做已约定的规范化和 fragment 去除。中立 score 不得当成跨源可比的相关性分数。至少一源成功时可以返回结果及失败来源；所有源失败应明确报失败。

## XML 与不可信数据

RSS reader 限制文档体积和嵌套深度，拒绝 DTD、外部实体、未知实体和错位嵌套；CDATA 作为文字，不重新当作 XML 结构。XML 实体只解码一次，不能把 URL 的百分号再解码后当作同一地址。

TAB/LF/CR 的合法数字实体与同样的原始空白应一致处理；标题可折叠空白，URL 中的空白仍拒绝。拒绝实体与正确解码是两步，不能以有限 HTML entity 表替代 XML 数字实体解码。

搜索结果是资料，不是新的系统指令。候选页面以 JSX 文字显示标题、URL、摘要及来源，部分失败单列；只有没有用户信息的 HTTP(S) 地址成为可点击链接，其他地址仍是文字。浏览器夹具 5 项检查/2 张截图覆盖双主题对比度、选择、来源、部分成功、全源失败清除旧结果及旧 Key 设置。安装版另有真实窄IPC保存/重启UI读回及空查询负向验收。没有真实查询授权时只做本地 HTTP 夹具，不能把挑战页、空页或解析通过说成真实搜索可用。

检索输入框使用当前主题面板色，避免浅色文字配置与固定深色回退背景混用；浏览器以实际计算颜色核验浅色和深色主题的正文对比度均至少 4.5。该检查与生产资产绑定，不能由 TypeScript 编译替代。

## 可选本机查询缓存与单飞（源码增量）

`query_cache_ttl_secs = 0` 和 `coalesce_queries = false` 为旧配置／新安装默认值，保留每次新检索。只在显式免费聚合后端消费这些选项，其他五个后端和凭据拒绝不回落规则不变。「测试后端」始终绕过本机缓存／单飞发起新检索，其结果不能与缓存命中混作实时外网验证。

[资源状态机](../../src-tauri/src/search/resources.rs) 每个 Gateway 独立持有：最多128条内存 LRU，用户 TTL 为0–3600秒，写入时开始计时，读取不续期；只缓存完整非空成功，部分结果／错误／取消不入缓存。键散列当前完整非敏感搜索配置、精确查询／结果数及实际双源端点，保留 URL／查询语义，不含供应商 Key。当前来源与区域仍是既有固定合同，不冒充可配置技术源／语言功能。设置和旧输入可兼容保存，缓存内容与计数不跨重启，状态不写新表。

单飞使用已有 Shared future，由活跃调用方轮询，不生成脱离调用方的后台任务。第一个调用取消不取消其他等待者；最后一个离开才移除在途项并丢弃双源 HTTP。合并者使用已开始查询的剩余源期限，不延长或自动重试。协调中的操作最多32个、单查询最多128个等待者；达到上限在发起新源请求前明确失败，不把失败标作空成功。旧代次操作仍占并发上限，配置发布使旧结果不能回填新缓存；旧捕获配置的迟到调用绕过复用。缓存命中注入标注本机原检索完成时间，不作为网页发布时间、模型费用／权益证明。

设置页提供显式单飞、TTL、只读会话计数和本机返回时间；非法或缺失统计显示不可用，不补零。保存期间禁用重复动作，存储失败保留 TTL 草稿，迟到读取不覆盖新设置，键盘焦点回到同一控件。旧后端 DTO 未提供新字段时保留旧搜索功能，不伪造已关闭／零计数。命令经原有配置提交／运行时发布流程；省略新增输入保留原值，非法 TTL 不改变文件或两份内存配置。

本增量证据只覆盖源码、自有双源 HTTP、内存状态机、Tauri MockRuntime 命令桥接及源码浏览器；未构建安装新包、未访问真实搜索源或正式用户数据。实际双源计数、单等待者与最后等待者取消／响应体关闭、配置代次与迟到、LRU／TTL／资源上限有正反例。逐源限额／持久冷却、技术源、独立可配置出口、网页正文读取与适用实装仍沿原任务卡推进，不因本批收口宣称全部支持。

## 已确认 Retry-After 的持久等待（源码增量）

免费聚合在429／503中收到合法 `Retry-After` 时记录已知期限，下一次源请求前核验；另一个已显式选择且未受限的源仍可返回部分结果，不是第三源或凭据拒绝后的回退。秒数及三种 HTTP-date 按[HTTP 语义](https://www.rfc-editor.org/rfc/rfc9110.html#section-10.2.3)解析，429可携带该头见[RFC6585](https://www.rfc-editor.org/rfc/rfc6585.html#section-4)。不套用插件历史数值、不截短有效厂商期限，也不为缺失／非法头虚构时长、额度或账户资格；这部分和挑战页／操作员自设限额仍待后续合同。

[状态存储](../../src-tauri/src/search/cooldown.rs) 复用现有非敏感 `meta` 表，不新增 schema。版本化命名空间仅含固定源代码及物理端点指纹，value 只存版本、时间及可选等待来源，不存原始URL、查询、Key或头正文；有大小／版本／时间校验。并发CAS更新只延长期限，成功、设置更换或较迟的短期限不能覆盖长限制；坏记录或读写失败不解释为可用。提交成功后才确认已保存，自有文件库重开／Gateway重建有实际请求数和SQL对照，不能称实体应用重启已验。

观察到已知限制后，只为本地数据库提交启动有界、独立监督的写入；不启动后台网络或自动重试。调用方取消不能丢弃已提交的本地写任务，网关停止在原有总期限内同时等待请求审计和源状态，两组故障分开；源写入故障不会误标为模型请求审计故障。写入失败／未确认或关闭超时如实报告，不称干净提交；异常断电前尚未提交的响应仍无持久化保证。

生产自动预取及“搜索一次”诊断都使用同一源保护，后者只绕过查询缓存／单飞，不能绕过已记录等待。只读界面分开已提交、写入待确认、不可读取和“本机未阻止”，不提供重置入口；未阻止／期限届满不证明源在线、厂商额度或可收费调用。普通五后端、价格／权重／权限及请求审计写入器保持原行为。本批仅源码、自有loopback／SQLite／MockRuntime／源码浏览器；真实非空检索、新包／实装、一般源限额和其他队列仍未完成。

## 可选人工源发送预留间隔（源码增量）

`source_min_interval_ms` 默认为0，合法范围0–3600000整数毫秒，仅由显式免费聚合消费。正值在每个物理源HTTP之前，使用原有meta键原子比较／写入下次可准入时间；并发调用者不能共同占用同一时段。它是用户设置的本机准入间隔，不代表供应商额度、计费单位或精确网络发送间隔，不引入自动等候／重试／额外来源。默认0不写新预留，也不能清除已经提交的等待；取消不退回已预留时间，更长的服务端Retry-After优先。写入失败时该次源HTTP不派发，另一个源仍按自身状态独立判断，模型请求维持既有搜索失败不阻断规则。

现有v1记录缺少reason时仍解释为Retry-After；新增的`local_interval`与`retry_after`用于只读来源展示，不保存查询／URL正文或凭据。设置页使用单独草稿和明确保存按钮，边界非法／旧DTO字段缺失不能补零放行；失败保留草稿、保存中禁止重复、键盘焦点返回对应输入。旧客户端省略新字段保留原值，生产命令验证后提交文件并发布两份配置锁；未更改schema、费用、权重或全局代理。

本批实际自有HTTP覆盖默认0两轮4次请求、启用间隔首轮2次后阻止重复、同一私有文件库重开和设回0不洗掉期限、持久化失败0新增请求。核心并发／取消／长短期限等10项、实际选择器6项、受影响MockRuntime命令1项形成有限17项，并非一次全量；源码浏览器8组与既有聚合5组分列。UI组合命令首轮在8组通过后因不支持的focus参数退出1，原失败保留，只通过现有入口补跑未执行的聚合5组。源码截图和MockRuntime不能代替安装版UI或真实供应商检索；技术源、独立出口、正文抓取、一般日额／挑战页策略和其他原卡继续。

## 显式 npm 软件包范围（源码增量）

免费后端新增 `free_search_scope`：`web` 为默认通用网页范围，保留原 Bing RSS＋DuckDuckGo；用户选择 `npm_packages` 后只向公开npm注册表发送，不混发网页源，失败不切回通用网页。其他五后端忽略该范围，旧配置默认web；IPC省略该字段继承当前值，未知枚举在保存前拒绝。范围选择与搜索本身分开，设置读取／保存不执行检索，界面明确显示目的地，后端标题随已保存范围变化。

公开合同依据为[npm Registry API](https://github.com/npm/registry/blob/main/docs/REGISTRY-API.md)：GET `/-/v1/search` 携带原查询text和有界size（本机1–10），只消费objects[].package中的name／description。包名规则参考[npm package.json文档](https://docs.npmjs.com/cli/v11/configuring-npm/package-json/#name)；当前[保守解析器](../../src-tauri/src/search/npm_packages.rs)只接受不超过214字节的安全小写ASCII名称子集，允许合法形状的scope及scoped点／下划线开头名称，拒绝路径遍历、转义、URL分隔符及坏结构；旧大写包名／其他未覆盖名称不冒充受支持。链接由已验证名称生成固定www.npmjs.com/package路径，不跟随上游links／tarball／脚本，也不转发发布者、邮箱、上游评分；结果score保持中立0，描述仍是不可信资料，不能证明包安全性、可用性或推荐质量。

选择npm范围后还需明确包意图：npm package／npm install／npm search／npm 包／npm 软件包／package.json／pnpm add／yarn add或完整安全scoped包名，通过有限词形及边界规则判断；普通Latin、中文新闻、混合非技术文本在任何HTTP前拒绝。它是保守本地策略，不是语义分类器，可能拒绝有效但未明确表达的查询；用户可写明包查询，不以仅出现英文自动混入技术结果。其他技术源尚未接入。

生产自动预取和诊断复用同一[执行器](../../src-tauri/src/search/executor.rs)、直连无重试客户端、有界响应体、总期限、调用方取消、逐源预留／Retry-After及缓存状态。npm在原meta命名空间使用独立物理目标指纹，无新schema；范围及npm物理端点进入查询指纹，切换范围清理旧缓存代次，不能把网页结果当npm结果。401／429／验证页／坏JSON／超大body／超时不重试、不回退、不缓存成功；取消关闭自有响应体并保留已经提交的时间预留，不声明零费用或供应商额度。

本批有限Rust36项＋受影响MockRuntime命令1项，共37个不同顶层用例；源码UI当前4组、受影响资源8组和原聚合5组分列，不叠加初期4个Rust／中间UI绿。生产入口首红为显式npm却返回两个网页源，UI缺入口首红及固定后端标题的真实文案首红保留；另有一次错误来源标签定位造成的测试失败，修正定位而未改生产结果合同。最终浏览器包括键盘、禁用／防重复、焦点、保存失败保留目标、旧DTO、来源标签及390px结果截图。上述均为源码、自有HTTP／SQLite／MockRuntime／浏览器替身，不是安装版或真实npm服务证据；真实账号／非空检索仍待原审批，当前验收包未动。

## 搜索专用独立出口（源码增量）

`free_proxy_url` 默认为None，免费网页／npm检索保持专用直连客户端；用户可显式配置无认证HTTP代理或SOCKS5h端点，仅作用于免费后端，不改模型、其他五搜索后端或系统代理。地址最多2048字符，保存前归一化首尾空白，拒绝userinfo／密码、非根路径、query／fragment、内部控制空白及其他协议。SOCKS代理端点须为IP：锁定reqwest0.12.28创建SOCKS代理时会同步解析代理主机，当前不在请求线程引入这类DNS阻塞；SOCKS5h仍将目标域名交给代理。未接入认证／HTTPS代理／SOCKS代理主机名，不读取现有代理／节点／凭据配置。

[客户端池](../../src-tauri/src/search/egress.rs)复用已有reqwest及socks feature，无新依赖。按[ClientBuilder代理合同](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html#method.proxy)，先no_proxy禁用环境／系统代理，再添加显式Proxy::all；重定向／自动HTTP重试继续禁用，TLS验证不放宽。Gateway资源最多缓存一个显式客户端并逐次核验捕获配置的URL，已有请求持有原客户端克隆；更换出口清除客户端持有者与查询代次，新请求不能借旧出口缓存。物理源等待以源端点为准，切换出口不清除Retry-After或已预留时间。构建／代理拒绝／传输失败不会改用直连，也不改变模型的出口、费用守卫或既有搜索失败不阻断规则。

IPC补丁区分三态：字段缺失保留当前值，显式null清空为直连，字符串设置新出口。只在用户点击“保存搜索专用出口”时提交该字段；保存单飞／TTL等其他选项不能回传页面旧出口覆盖另一个窗口的新选择。设置读取／保存不做网络探测；保存中防重复，失败保留草稿，刷新不覆盖未保存草稿，键盘焦点回到对应输入。旧DTO缺字段显示不可用，不虚构直连／已配置。原文件提交与两份配置发布仍沿既有命令流程，无新schema或路由策略。

本批有效首红为显式代理实际0对预期2，修复后自有代理收到双源2次、直连源0；另有旧页面无关保存回写旧出口的有效UI首红并最小修复。实际自有HTTP覆盖直连／代理切换与缓存隔离、旧捕获配置维持原出口、407无直连回退、429跨出口保留等待及取消关闭自有body。自有SOCKS5h握手验证目标域名和真实元数据返回；HTTPS只验证CONNECT拒绝及0直连，不宣称端到端TLS代理成功。有限Rust18（校验1＋生产消费者13＋查询资源3＋受影响MockRuntime1），源码UI5＋npm4＋资源8＋旧聚合5，初期绿色不重复加数。未变的源body／解析／冷却模块按精确字节关联复用，不重跑原全套。IPv6仅URL校验，实际传输为IPv4自有loopback；没有真实外部代理／检索、正式用户数据或新包实装证据。

## Hacker News 技术讨论元数据范围（源码增量）

显式 `free_search_scope=hacker_news` 只搜索HN story元数据，不自动混发网页／npm，不读取用户、评论或目标文章。默认web和原npm范围不变。有限词形规则只接受编程／数据库／开源软件及programming、compiler、Show HN等明确表达；普通英文、中文新闻和混合非技术文本不因出现拉丁字母自动获得准入。规则是保守本地策略而非语义分类器，可能拒绝有效查询，不提供相关性／包安全性或模型费用证明。

协议依据为[HN官方文档源码](https://github.com/algolia/hn-search/blob/master/app/javascript/views/Api.tsx)：GET `/api/v1/search`，原query、固定tags=story和hitsPerPage（本机1–10），不自动翻页。仓库已于2026-02-10归档，公开源码仅作为协议参考，不证明真实服务仍在线或限制未变；本轮未调用检索端点。[解析器](../../src-tauri/src/search/hacker_news.rs)只提取有界标题／数字objectID及可选点数、评论数、发布时间，链接由数字ID生成固定news.ycombinator.com/item地址；不跟随返回url／highlight／正文，score仍中立0。明确矛盾类型、坏ID、错误包裹或坏JSON不能成为story成功；点数／评论的已知0与未知分开，时间仅是API元数据，检索完成时间另标。

上述归档文档公开单IP每小时10000请求限制，因此当前本机预留至少360毫秒，较长的用户间隔仍优先；`source_min_interval_ms=0`只关闭新增用户间隔，不能关闭该接口策略。`service_policy`与`local_interval`／`retry_after`独立显示，已知更长Retry-After不缩短；其他应用共享IP仍未知，这不是厂商额度或hard-stop证明。HN需要Gateway持久源状态；无存储的旧无状态helper不承诺HN。相同生产消费者复用独立出口、总期限、取消、有界body／缓存／源等待，401／429／503、验证页、超大body、超时不自动重放、不回退网页或npm、不缓存为成功。

`free_search_scopes`由生产枚举生成，客户端不能写入能力列表。旧DTO不提供列表时保留原web／npm选择，不展示未经宣告的HN；错误列表不虚构支持。只有明确选择才提交scope，其他保存省略，避免旧页面恢复另一窗口已更换的目的地。设置读取／保存不探测外网；新范围支持键盘、禁用、防重复、焦点与390px结果显示。源等待快照改为毫秒ISO，页面保留完整UTC值；原整秒序列化和显示均有有效首红，不能展示早于已提交保护的边界。

本批有限Rust46（解析3＋生产选择器16＋源状态12＋查询资源3＋原双源11＋受影响MockRuntime1），不是一次全量；源码UI5＋npm4＋出口5＋资源8＋原聚合5分列，初期绿色和部分组不重复加数。自有HTTP覆盖正常解析、普通查询0HTTP、显式代理、默认不注入HN、缓存范围、取消、不退时间、429／503私有文件库重开／Gateway重建及出口变更不洗掉等待；不称实体应用重启。首红包括生产选项解码拒绝、缺UI入口、无关保存恢复旧scope、后端／UI丢毫秒；没有新schema／依赖／费用／权重变化，当前安装和已验包未动。

StackExchange仍有明确本地依赖：[官方限制](https://api.stackexchange.com/docs/throttle)和[wrapper](https://api.stackexchange.com/docs/wrapper)要求逐方法backoff、语义相同查询一分钟去重、共享IP额度及quota_remaining=0／未知重置处理，再接成功／失败消费者。它们尚未实现，不能全部归为外部缺证。其他技术源、区域／语言、正文读取、一般限额／挑战页和适用实装仍沿原卡继续；真实账号／非空检索仍待原审批。

## 源准入的数据库等待边界（源码修复）

[源状态机](../../src-tauri/src/search/cooldown.rs)在池／读取等待后重新取UTC时间并重查已知内存限制，不把准备阶段等待消耗成新槽位的起点；无状态读取检查也使用等待后的时间／状态。CAS确认返回后再次检查失败／已知限制及槽位是否过期，再合入本次内存预留。新观察的限制即使比本次用户间隔短，也不能被本次较长预留覆盖后继续派发。已过期确认返回准入失败，不伪称有效槽位或简单归类为存储故障；原16次CAS策略、只延长、取消不退时间及已知限制提交监督不变，没有新锁、事务策略、表或依赖。

有效首红初0/3，扩展批次原始1/5：其中1个既有benefits用例被名称过滤带入，不计本批；5个源失败包含真实Gateway资源→自有HN HTTP在过期CAS之后仍发送1次、预期0。夹具使用原内存库单连接、显式poll-pending与FIFO池连接屏障；20／40／400ms仅用于让被控槽位老化，不提高生产或既有3秒超时，不插入生产测试钩子。修复后源状态16＋生产消费者17为33个不同用例，无ignored／环境早退；新4个状态用例与1个真实HTTP用例在其中，不能重复加初期首红或中间绿色。

消费者通过原发送前源保护取得准入，失败不进入HTTP、不自动重放；真实正对照是另一次明确调用成功，仅产生1次自有HTTP。check／全目标Clippy／fmt／适用文档门禁已验；没有前端或IPC变更，旧解析／浏览器证据按未变字节关联，不重复执行。时间预留仍不是精确网络发送调度或共享IP／供应商hard-stop证明。真实服务／账号、新包实装和其余原目标仍未验或未完成；本修复不改变模型费用、权益、排序或原已安装候选。

## 检索结果的完成观察时间与界面输入归属

成功返回（含自建SearXNG有效空结果）在执行器填写既有可选`retrieved_at_utc`，精度沿原秒级UTC；`reuse=fresh`只指本机新执行，不保证源站内容实时、发布时间、收费或可用性。缓存／共享封装保留已产生的完成时间，不因晚到包装或读取缓存刷新观察；旧工厂未提供时间时仍沿既有包装回退。时间描述检索元数据阶段，不含可选结果正文；未改单飞、TTL、来源顺序、配额、费用或评分。

“搜索一次”界面显示本次已提交输入、后端、原完成观察时间与本机复用标记。未知／坏时间或旧接口缺字段保持未知，不用浏览器当前时间补齐；输入只作易失的当前结果归属，长输入最多展示前512个字符串单元并断行，不代表远端规范化检索词，不改变实际提交给原规范化器的输入。正在输入的新草稿不会冒充旧结果的查询。新检索／IPC失败期间保留的上次结果明确未被本次确认；服务器明确全源失败仍沿原规则清掉旧结果，有效空返回则明确本次0条。保存设置清掉旧事实，不自动重试。

有效后端首红0/1先确认自有2个HTTP请求和正确结果后才发现缺时间；最初缺FutureExt的编译失败单列。新增正控同时验证自有传统源正常／空结果、cache包装延迟后原时间保留和缓存读取0额外请求。执行器18、资源3、实际owned TLS缓存预取1、真实Tauri命令MockRuntime→自有HTTP正常／空／4011，有限23个不同Rust用例通过，前后重复不累加；新增界面8＋原聚合5／资源8／出口5／正文5／npm4＝本源35浏览器组通过。长串390px横向溢出实际首红仅补现有breakable类；图已看，TS／artifact前端、check／Clippy含IPC／fmt通过。

浏览器cached／shared标记是DTO渲染兼容，不代表“搜索一次”真的读查询缓存；实际资源缓存和时间保留另由Rust真实消费者验证。命令桥接仍是MockRuntime及自有DB／HTTP，不是实体安装或真实搜索源。未读正式库／密钥、未真实网页／账号／模型、未打包安装；原ignored／环境早退／EFS与完整M0–M5余项保留。

## DDG挑战页的结构签名与普通技术结果（源码修复）

[免费聚合](../../src-tauri/src/search/free_aggregate.rs)不再因整段正文出现captcha、anomaly.js、anomaly-modal或verify you are human而拒绝来源；这些词可以是正常标题／摘要或技术文档。现在在既有256KiB有界响应内识别已知表单action、控件id／class和验证标题，跳过注释及script／style原始文本；转义示例不当成活动控件。只检查已知结构签名，不是通用HTML解析器或所有挑战页的完整识别保证。

已知挑战控件即使带有result-link也失败关闭，其他已完成来源仍沿原部分成功合同可用；普通结果仍需原链接解析、URL／条数和实体守卫。没有改变两源GET／POST、取消／总期限、401／403、无重试、URL去重、排序／模型费用或冷却记录；本片没有新增挑战页冷却期限，缺头／一般限制及真实站点适用性仍属原队列。

第6轮m3-ddg-challenge-classification-v1保留普通CAPTCHA结果被误拒与challenge-form附带链接被误收两条有效0／1首红。最终13项聚合集成＋18项生产选择／执行器相关测试＝31个不同Rust测试通过，0 ignored／明确早退；新增2个顶层测试的内部正常／控件变体不另加数，也不累计格式化前的13项。check／all-target Clippy／fmt及文档通过；早期格式化相对目录失败原件／修正均留存，无编译或夹具失败冒充首红。

全部HTTP为自有loopback，未执行真实DDG／Bing搜索、账号／模型调用、正式Key／DB或新包安装。UI与API字节未改，不重复旧页面／账号／账本套件；此源码未实装。区域／语言首片请求偏好另有未应用提案，不把拟议字段写成已支持功能，原完整M0–M5继续。

## 手动检索的单请求取消（源码／生产命令回归）

“搜索一次”沿原test_search_backend生产入口增加可选UUID／就绪Channel；旧调用省略两者仍兼容。只读设置的manual_search_cancel_supported表示当前命令存在进程内取消能力，不代表来源可用／免费或上游已停止。缺失／false时新UI使用原兼容调用，不分配无法确认闭合的Channel；该能力声明与请求标识均不持久化。

[命令入口](../../src-tauri/src/commands.rs)在任何可失败准备前将JS Channel映射为RAII对象，原有[有界请求控制](../../src-tauri/src/desktop_chat.rs)以独立搜索注册表复用，窗口归属由Tauri原生上下文提供。UUID注册后才能通知可取消；整个配置／Key／DB等待及检索future均在受控运行内。相同UUID的聊天注册表互不取消，不停止网关，不重发搜索或切换来源。取消只结束当前调用／等待，派发后的上游处理及费用仍可能未知，不表示退款或零消耗。

[手动搜索界面](../../src/pages/LocalModels.tsx)就绪前禁用取消；accepted只显示等待确认，不抢先结束busy。实际取消错误才标已取消，已就绪结果仍可胜出并清理取消等待；false或IPC失败继续等待真实结果。草稿保留，旧结果在取消／传输失败后带标签，不把已取消写成等待返回。组件卸载尽力取消本次请求：尚未就绪时记住离开，在就绪通知到达后只取消该UUID；IPC不可用或硬窗口销毁不能当作已取消，仍受原有后端／数据库生命周期约束。

第6轮m3-manual-search-lifecycle-v1保留pending缺取消按钮的有效0!=1首红。最终新增10＋相关兼容40＝50组源码浏览器通过，覆盖请求状态竞态、Channel闭合、旧能力声明、键盘／焦点／草稿和明暗390px；实际生产命令／MockRuntime4项通过，其中准备等待用生产命令future实际首轮poll进入自有单连接DB等待后，经生成的取消命令验证0HTTP，其余覆盖窗口／并发隔离、重复／完成后返回、旧调用与完成元数据。自有并发HTTP两次无重放；SDK通道结束的浏览器夹具证据与MockRuntime消息／guard证据分别保存，不冒充实体WebView2验收。

check／all-target Clippy（含IPC feature）、fmt和适用文档门禁通过。Clippy测试夹具复杂类型首错及别名修正留存；首次独立测试EXE的外层Cargo统计器缺标头误记0，原收据不改，按原始3过汇总补证且不重跑，再仅执行未跑的旧兼容1项。原始UI8／中间10／核验3不重复累计。没有新依赖、配置／表列／持久JSON／费用／权益政策，区域语言提案未应用；全部产品验证使用自有DB／HTTP或合成IPC，无真实搜索／账号／密钥、打包安装或系统修改。旧已安装验收包未更新，整体M3及M0–M5未完成。

## 搜索设置读取失败与只读恢复（源码浏览器）

[搜索面板](../../src/pages/LocalModels.tsx)将元数据读取与保存／检索提示分开。首次读取失败显示明确错误和“重试读取搜索设置”，不再只显示加载中，也不构造默认设置或空来源。重试仅调用get_search_settings，当前读取以代次ref挡重复；待确认时保留错误事实，成功才清除。显式键盘恢复后回到检索输入，若用户已在编辑草稿则不抢焦点。

已有快照刷新失败时保留并标明上次设置、查询结果与未发送草稿；确认前禁用旧设置的保存／检索动作，仍保留只读重试。正常背景刷新没有失败时沿用原编辑合同：新的保存／检索接管发布代次，旧读取晚返回不能恢复旧开关或清掉新读取所有者。被接管的只读RPC不会因此被当作已取消；它仍受原生命周期约束。卸载会失效读取所有者，不发布到新页面，也不混入手动检索UUID的取消注册表。

第6轮m3-search-settings-read-state-v1保留首次读失败错误区域0!=1的有效首红，以及本片一度把健康刷新期间的控件也锁住、阻断既有新保存优先场景的回退失败。后者按原测试／原超时修复，不改断言。最终新7＋资源8＋取消10＋结果事实8＝33组源码浏览器通过；TypeScript／artifact前端／适用文档门禁通过，不重复格式化前或已过中间绿，不重跑上轮Rust／IPC／账号或全套。未变Rust、API、持久结构、来源网络／费用政策，无真实检索／登录／正式Key或DB、包安装／发布／系统修改；整体M3／M0–M5仍未完成。


## 区域／语言请求偏好首片（2026-10-22 源码收口）

按用户已批准的首片，`search.free_search_locale`持久保存`source_default / cn_zh / us_en / gb_en`，只用于免费聚合的web范围。它是来源请求偏好，不保证返回结果严格按地区／语言过滤，也不把来源默认mkt当新增RSS协议。其他后端、npm／HN及原有正文准入合同保持原行为；更多地区、语言和独立组合仍未完成。

| 偏好 | Bing RSS | DuckDuckGo Lite |
| --- | --- | --- |
| 来源默认 | 保留原规范化来源查询和既有参数 | 保留原POST q，不增加kl |
| 中国中文 | 不猜中文language代码，沿用原参数 | 增加kl=cn-zh |
| 美国英文 | q追加language:en loc:US | 增加kl=us-en |
| 英国英文 | q追加language:en loc:GB | 增加kl=uk-en |

附加后的Bing查询仍受原256字符上限约束，超限在两源发送前整体拒绝，不截断。缓存／单飞键带偏好，完成结果保留当次`requested_locale`；切换偏好不清除物理来源的Retry-After或本机发送预留。专用设置提交拥有此值：旧IPC省略字段继承提交时最新值，显式source_default才重置，通用旧配置不能覆盖，非法枚举不提交；配置锁、Gateway快照和TOML保持原子关联。

原核心6、serde1及源码浏览器7保留不重跑；本次用已固定的同字节feature测试EXE，仅执行此前缺失的`locale_production_ipc_narrow_save_and_legacy_contract`，实际1/0、0 ignored／明确早退。该命令使用真实生产宏／MockRuntime及自有DB／配置目录，不调用真实搜索。编译输入到当前源的差异只允许已核实的两份文档，所有代码仍匹配；测试旁manifest只作用任务自有复制EXE，未改系统加载配置。按各自实际执行输入证明相关Rust整文件、共享命令、TS API和UI字节一致后，首片有限Rust并集为8、源码UI为7，不累加重复阶段，也不称一次全量或安装版验收。

[复用与实际IPC证据](../../../artifacts/Project-Enhancement-20261013/execution-schedule-20261018/multi-account-local-v1/native-rotation6-v1/priority-six-regions-v1/m3-locale-reuse-proof-v1.json)记录准确范围；最初复用核对误用较早Rust输入对比后续UI脚本的两次诊断独立保留，未改产品断言或用零测试作绿。当前资格的check／含IPC Clippy／fmt与TS/artifact前端按未变代码复用，文档单独验证。没有真实Bing／DDG请求、账号／模型、正式DB／密钥或新安装；六区仍优先，完整目标保持未完成。


## 原队列 MDN 合同核验（尚未接线，2026-10-26）

原搜索增强卡已列MDN，当前网关未注册该来源；没有新增需求。插件的旧路径和documents字段只作线索，不充当现行公开合同。[官方Yari仓库](https://github.com/mdn/yari)已归档，其README将前端开发指向[Fred](https://github.com/mdn/fred)。公开落地README尚不足以确认部署中的搜索API请求、返回及限制；工具读取其components目录报`Failed to fetch restricted URL`后停止，没有改路读取同一资源。

本片只读公开源码／文档，不调用MDN搜索、不启动应用、不改产品、不构建安装或认证；不由工具限制推断MDN不可用。此原范围项以“现行官方合同缺证”独立挂起，最小下一步是取得正常可读的现行官方协议材料，再按既有架构实施／离线验证。已完成Stack／npm／HN不重复，Global未知失败保留，模型27/100不变。证据`E:/codex/2026-10-14/task/work/mdn-contract-preflight-v1/receipt.json`。


## MDN MCP 固定消息合同源码（未接线，2026-10-26）

官方另有独立的[实验性MCP产品入口](https://developer.mozilla.org/en-US/mcp)，不再把MDN概括为“无公开检索入口”，也不由此宣称旧内部REST受支持或部署版本已验。[公开search实现](https://github.com/mdn/mcp/blob/main/tools/search.js)接收必需字符串query，返回拼接的纯文本，并可能再读取每条文档元数据；一次search不是一条上游HTTP。[公开传输快照](https://github.com/mdn/mcp/blob/main/transport.js)为无会话JSON profile，不能代替部署验证。

隐私退出头固定为`X-Moz-1st-Party-Data-Opt-Out: 1`，只针对第一方analytics；服务声明实验期间可能留存查询，不能写成不记录或不传输。此前Fred目录与Raw按钮读取限制保留；本片采用已可正常读取的独立官方产品／blob资料，没有重取受限Fred目录或探测检索接口。公开文档网页确有读取；零计数只指MCP运行端点／工具／搜索，不是全程零外联。

本地合成反例证明：一条summary与两条文档可以拼成相同文本，不能据此保证条目分段／标题／路径归属。因此只增加[固定消息codec](../../src-tauri/src/search/mdn_mcp_contract.rs)，不引入通用MCP客户端／任意工具，不解析出猜测的SearchResult。三阶段生成的是请求描述而非发送回执；状态／session／JSON响应由未来调用者提供，codec不能自行证明HTTP或远端取消。当前仅JSON消息层，不宣称SSE／运行时接线完成。

本机政策为query单字符串、非空、无控制字符，最多256字符和1024UTF8字节；回复JSON最多256KiB，单text块最多16KiB。它们不是供应商公开上限或成本保证。固定软件身份、不带账号／Key／Cookie；Debug及错误不包含query、原始响应或session。初始化需要匹配协议／tools对象，通知输入仅202空body，search仅固定工具名；任何失败永久封闭该对象。原始响应的已消费字段在丢重前校验，协议对象不接受Serde位置数组。输入Value构造器已经失去的原始重复键不冒称已校验。

输出仅有界不可信文本，document_count始终None；不把语言中的“No results”当权威0，不推导文档／评分／费用。未知能力不授权动作，未来仍不得自动调用get-doc／get-compat或跟随summary链接。来源枚举／配置／UI及原有默认行为未改；此源码不在已装r7，也未打新包。

验证：最初有效原生红例1失败；实现14项通过后独立审阅发现位置数组与重复tools两项Important，再以6个有效红例复现并修正；最终21项原生合同测试通过、0失败／0忽略／1586过滤未执行。单独Node歧义实验1项不计Rust总数。fmt、cargo check --offline --lib、Clippy --offline --lib --tests -D warnings均0；不是一次全Rust全套全绿，保留历史Clippy101和所有红例。没有前端改变，不重跑已过UI或后端真实验收。

独立完成收据：`E:/codex/2026-10-14/task/work/mdn-mcp-contract-v1/independent-completion-v1.json`，SHA256 `ca262363e84018d8655b69856ae1d12eeb8e541f624ee32def425fa91c72ff8a`；源码清单和全部日志在同目录。Global未知失败未动，模型27/100保持。下一集成必须先定义非结构化引用的显示／注入语义，或获得稳定结构化结果合同；当前不能标MDN来源实测／实装通过。



## MDN 非结构化引用消费者（开发源码，未入 r7，2026-10-26）

前一合同片的“显示／注入语义待定”已在本片收口；没有把 opaque 文本解析成假文档。[固定引用 DTO](../../src-tauri/src/search/mdn_reference.rs)仅接受已通过 codec 的 TextReference，字段私有、Serialize-only、Debug 遮蔽；schema/source/format 固定，document_count 始终 null。SearchOutcome 的可选 unstructured_reference 不存在时不序列化，原结构化结果和 hits 语义保持；hits 不代表 MDN 文档数。本片无新增依赖、配置迁移、来源枚举或默认启用。

[引用显示](../../src/components/MdnReference.tsx)按纯文本 pre 呈现，显式标注实验性与“文档数未知”；不解析 Markdown/HTML/URL，不把正文内工具名转成动作。运行时拒绝未知 schema/source/format、非 null 计数、空文本、超过16KiB UTF8、控制字符及孤立代理项，只显示固定错误，不回显非法正文。旧 DTO 继续显示原命中条数和规范化链接，下一结果／设置变更清除旧引用。

既有 prefetch 在结构化空／非空两分支消费引用：独立 User 资料消息位于最后一个用户问题之前，JSON 转义保留原文为一个数据字段；不跟随旧 inject_as 将引用提升为 system，不生成工具调用或执行其中指令。已有结构化注入身份不变；error 非空时不注入引用。这里保证消息角色／编码边界，不保证模型绝对抵抗提示注入。无新增日志／数据库路径；现有 proxy 单独保存原始请求 delta，审计取 backend/error/hits，不持久化该引用正文。未来 transport 仍须保持这些边界。

验证范围：初始 Rust 行为红例4失败／1兼容通过；首次浏览器红例证明无显示。实现后浏览器发现重复状态提示，保留失败并修为单一错误提示；最终5组场景包含纯文本攻击字串、非法元数据、16KiB Unicode边界、390px布局、旧结果与清除行为，截图由实现者和独立审阅者查看。最终 Rust 搜索159通过／0失败／0忽略／1454过滤未执行；search集成 57 通过。fmt、check、Clippy、tsc+Vite build均0。前端日志包装器因GBK打印字符异常曾exit1，但build本身已exit0；未重建，语法检查后续0，分开保留。Vite现有>500kB分包警告未当失败或消除。

独立审阅无Critical／Important／Minor，冻结18项输入和12项变更；清单、全部日志、截图及完成回执在 E:/codex/2026-10-14/task/work/mdn-unstructured-reference-v1。生产prefetch两分支为静态接线核对，原生合成测试执行的是引用消费helper；浏览器为当前Vite源码＋合成IPC，不是已装r7或完整MDN来源运行。未进行真实MCP调用、应用启动、认证、安装包构建或安装，模型仍27/100。下一必要工作为固定官方端点的受限传输与显式来源启用接线及离线模拟测试；部署协议、服务可用性和真实界面端到端仍待独立验收，不重复Stack/npm/HN或Global未知失败动作。


## MDN 固定来源传输与显式选择（源码／本机合成，2026-10-26）

前两片的消息codec与引用消费者现在有[专用来源生产者](../../src-tauri/src/search/mdn_transport.rs)接线，不再将“缺传输／未注册”当当前源码状态。配置序列化值为 mdn_mcp，旧默认仍是关闭搜索／DuckDuckGo；不加入免费聚合的fan-out或任何失败回落。选择与读取设置不自动搜索；真实配置未修改，已装r7和冻结包仍没有此功能。

固定目的地为 https://mcp.mdn.mozilla.net/，客户端独立构造、直连、HTTPS-only，不使用调用方client／key／cookie／代理，禁redirect／retry／referer与压缩协商。生产目的地不提供override；cfg(test)才有严格127.0.0.1/http/非0端口/根路径且无userinfo、query、fragment的合成profile。初始化、通知、search最多三个POST；固定软件身份与既定analytics退出头，不是通用MCP代理，不能自动执行get-doc/get-compat或引用中的工具名／链接。sourceHTTP与模型请求计数分开，三个POST不是服务端内部全部HTTP数。

同一最多10秒总deadline包含本地准备与三阶段；connect最多3秒，每阶段响应≤256KiB，引用text仍≤16KiB。只支持已确认的JSON/stateless profile，拒绝SSE、session、未知Content-Type/encoding、非预期HTTP和codec错误，错误正文不回显；保留固定阶段／类别／状态。超时或本地取消不能证明远端未收到／已取消，不自动重发；不把失败缺字段当0请求或已免计费。

复用现有meta/Cooldowns，但固定MDN服务wait与本地事务间隔使用两个独立非秘密键：先检查持久服务wait，再为本事务预留原配置间隔；每阶段发送前重读服务wait与配置。合法Retry-After在读body前即持久保存；另一已准入事务随后不能越过新wait继续下阶段。本地自己的长间隔不会误拦自己。没有新增表迁移、查询缓存／singleflight或外部遥测。没有数据库／当前配置不匹配时封闭；受限预算上下文在发送前拒绝未核验来源费用。

command/proxy显式MDN分支跳过无关搜索密钥解密和已配置的outbound代理工厂，专用producer不接收这些数据。预取消费真实生产者输出，始终通过既有不可信User JSON引用插入，不伪造标题／URL／文档数；审计search_hits为None、响应不写x-route-search-hits，旧结构化已知0仍保留。UI公开实验性、查询可能留存／退出头不是不记录保证、费用／配额未独立核验；MDN时条数显示空、注入显示User且禁改，但保留旧值，切回旧后端恢复。只应提交公开文档查询，不能传私密内容；未知文档数不显示为0。

验证保留完整失败链：初始有效原生红6失败／1默认兼容通过；首次green因共享Source枚举两处非穷举而编译101，改为独立固定Target，没有塞入旧GET源或加fallback。审阅发现并发wait和禁用控件旧值两项Important及测试端点Minor；并发受控屏障与端点纯函数有效红2失败／1长间隔正控通过，之前E0716夹具借用编译错误不算行为红。UI display-red证实system!=user，修复后3组通过。TestTarget专用枚举的生产Clippy101用cfg(test)关闭，不加allow。以上历史输出均保留。

最终：搜索单元169通过／0失败／0忽略／1455过滤未执行；实际审计／头回归1通过，既有搜索集成57、路由集成16通过（其中一个should_panic绑定失败为预期正反对照）。fmt、check、Clippy、tsc+Vite build及脚本语法均0；既有>500kB分包提示保留。UI为当前源码＋合成IPC三组，不是原生r7；并发／超时／三阶段HTTP使用本机受控服务，source生产者→预取→引用链确有执行。持久wait读回为同一owned SQL数据库上的新resource实例，不冒称文件数据库／系统重启。独立I1/I2/M1均闭合，19输入／12变更与v2冻结源码一致，源码清单／报告／日志／截图在 E:/codex/2026-10-14/task/work/mdn-transport-integration-v1。

剩余真实部署支持、协议版本、实际公开结果、服务端费用／配额仍未核验；本轮未连接真实MCP、启动已装应用、操作认证、构建安装包或安装，模型仍27/100，Global旧未知结果不重试。下一真实验收必须明确公共query（拟Array.prototype.map）、固定三阶段POST最多3次及无自动重试／其他工具／账号／模型／付费边界，并使用最终源码绑定入口；已有Stack/npm/HN及安装票据不能复用。真实MDN和新的候选／实装按相应具体权限另外验收，原完整Goal未完成。


## r8 MDN 限定实装与真实公开查询（2026-10-26）

r8已独立NSIS实装（安装1、installer exit0、NSS及26资源匹配），新空合成启动1／loopback2通过；固定MDN生产IPC一次返回3751 UTF8字节不可信参考，按冻结三阶段协议逻辑3 POST，文档数未知。应用exit0、端口释放、搜索／网关关闭、cleanup为空。不是费用／服务端扇出／真实结果UI或完整LLM注入证明；真实模型新增0，保守27/100，Global远端未知不变。

独立验收者 /root/stream_candidate_review；具体NSIS SHA256 `2a553b499eecb242f337f29a45a7973e618089907753cee69ac99fa4a8a9015d`。实际installed NSS `589f6ab47b740cf8dd3256fca91d0dd46d957dc1c7c9b9a35518ca71ecdf7771`。原r7公开程序28文件备份及旧包保留，没有读取正式账号库；不称正式资料逐字节审计。

MDN唯一公开输入Array.prototype.map，无账号／Key／Cookie／模型或认证；initialize／initialized／search在既定单事务中成功。3是固定生产合同支持的逻辑客户端POST数，不是抓包；服务端扇出、费用、quota／原始backoff保持未知，不从成功推断零费用或不限流。来源可能记录查询／IP／软件元数据；第一方分析opt-out不等于不留存。直接原生IPC真实来源与合成截图分别记录，不称真实结果UI端到端或完整LLM注入已验。

本次安装、启动和匿名来源票据已消费，禁止复用；Global未知认证失败不重试不清标记，模型账本27/100。原始证据 `E:/codex/2026-10-14/task/work/m5-mdn-candidate-r8-v1/qa-entry-r8-v1/install-run-v1/receipt.json` 与 `E:/codex/2026-10-14/task/work/m5-mdn-candidate-r8-v1/qa-entry-r8-v1/native-run-v1/native-proof.json`，交付目录内追加实装回执，构建当时的未安装/待批说明保留历史边界。

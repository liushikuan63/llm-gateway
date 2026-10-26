# 本机请求任务反馈

S3 首个消费者在「用量与审计」的请求明细里保存人工分类／结果反馈。一次反馈对应一次已落盘请求，不等于整个多轮任务完成；HTTP 成功、普通模型回复、用户好评都不是收费、免费资格或可信权益证明。

## 关联与生产入口

- [审计页面](../../src/pages/Stats.tsx) 使用 `query_requests` 的 `AuditRow.id` 归属详情和草稿；同 trace、同秒或同 session 的两个 id 不合并。旧 `recent_requests` 合同未改。缺少或不能被 JS 精确表示的 id 不提供可写反馈入口。
- [反馈组件](../../src/pages/RequestFeedback.tsx) 仅在用户打开详情时读取，通过 [API](../../src/api.ts) 调用 `get_request_feedback`／`set_request_feedback`；[命令层](../../src-tauri/src/commands.rs) 直接委托同一个生产 [repo](../../src-tauri/src/db/repo/request_feedback.rs)。未新增公共 HTTP 写路由，也不调用分类器、模型、账号或元数据 helper。
- 人工分类复用 simple／vision／reasoning；默认未确认。结果为满足需要／未满足需要／不确定，默认未评价。机器分类、实际目标和状态只作当次参考；完整推荐依据没有历史快照时明确未记录，不能重跑当前路由预览冒充当时推荐。

## 持久化与竞态

[旁表 schema](../../src-tauri/src/db/request_feedback_schema.rs) 通过现有幂等初始化入口建立，主键／外键为 requests.id，随请求日志保留期级联清理；不改写机器审计、尝试、用量、费用或排序参数。未操作时不自动产生标注，旧行不猜测回填。

每次保存携带 expected_revision。单个原子写语句检查创建／更新版本，事务内读回，提交后才报告成功；存储失败不自动重放。并发过期写返回 feedback_conflict，页面读取最新版本但保留草稿，用户核对后再次保存才能覆盖该修订。清除仅将人工字段设 NULL，保留递增 revision，拒绝清除前的旧版本写入；重复提交不新增样本行。source=local_user 和 UTC 秒时间由后端写入，版本号而非墙钟用于并发排序。

前后端只编辑正的安全整数 id；后端同时校验版本及有限枚举。错误固定为 feedback_invalid_input／feedback_request_not_found／feedback_conflict／feedback_storage_failed，不原样返回内部数据库错误。写入／日志清理竞争只能产生正常提交后级联删除或明确不存在，不留下孤儿。

页面的 5 秒轮询与行重排不覆盖同 id 的未保存草稿。保存期间禁用本行重复操作；关闭脏详情会提示未保存内容没有写入。失败保留草稿，重新读取不自动保存。原生 label/select/button 与状态提示支持键盘操作；这不是全面屏幕阅读器合规声明。

## 当次候选计划（S3，源码与隔离验收）

用户已另行批准新增 `requests.route_decision_json TEXT` 可空列。沿现有幂等 `ensure_column` 加列，不回填、不重算历史分数；NULL 显示「未记录，原因未知」。这里只在自有 SQLite 验证升级，未执行正式用户库迁移，也不宣称 DDL 无锁。应用回退保留列，不做有损 DROP。

[快照类型与验证](../../src-tauri/src/audit/route_decision.rs) 只接网关内部已经形成的候选 Vec，不再次调用 rank／explain。公共分派在健康／能力、sticky、活动供应商提升与免费选择器处理后、dedup 等待前捕获；配置策略是原配置策略，不冒充虚拟模型临时策略或完整评分解释。级联执行层若适用费用排序，再记录实际顺序及阶段；「排序处理已应用」不等于顺序一定发生变化。免费选择器标记复用当次实际判断，不事后读取新配置补证。

准备 owner 初始携带 `not_reached`，形成计划后在无 await 的有界进度锁内更新；正常／流式 handoff 和响应 body owner 沿用同一快照及已有 AuditTasks 耐久提交，不增加取消审计或 detached writer。[同一 INSERT](../../src-tauri/src/db/repo.rs) 将快照和原审计放入现有 usage_daily 事务；不按 trace 二次 UPDATE，相同 trace 的不同 request id 不互串。准备前取消、计划后去重／数据库等待取消与模型 POST 后取消保留各自原状态、用量未知和审计数量。

版本 1 只保存配置策略、后台个人测试标记、已有分类来源、当时 sticky／活动提升／免费选择器控制事实和候选顺序。最多 64 项、16 KiB JSON；原 ordinal 保留，超长／不安全标识显示该序位信息不可表示，不把下一项冒充首选。总数只指最终可分派链，非所有配置模型；此前过滤原因、评分分解固定未记录。配置上游名并非账号运行时已验证物理模型；计划不证明已派发、交付、免费资格、费用或可信结算。不保存 URL、Key、token、提示词、回复、工具参数或异常正文。

[只读详情](../../src/pages/RequestDecision.tsx) 与同一行的实际尝试／结果及人工反馈并列。非法／未知版本／超限 JSON 仅返回安全的 invalid 投影，不导出原文；旧 DTO 缺字段保持未记录。JSONL 增加版本化 route_decision，固定 CSV 列不变，CSV 暂不包含计划。缓存命中记录本次计划，不复制旧请求计划，不声称发生额外模型 POST。

[公共入口 loopback 对照](../../src-tauri/tests/support/request_decision.rs)、[快照／迁移／事务测试](../../src-tauri/src/audit/route_decision/tests.rs)、[取消 owner](../../src-tauri/src/proxy/server_stream_audit.rs) 与 [源码浏览器](../../scripts/request-decision-ui.cjs) 保留有效首红及夹具／编译／lint 失败；浏览器和 Tauri MockRuntime 均不是安装版验收。独立内部子请求仍待适合自己的决策合同；raw native exact 现记录独立的精确选择观察，见下段。typed passthrough（embeddings／图片／语音）已接其真实类型过滤后的链，使用独立阶段名，见下段；不冒充聊天预处理。正式 issuer 关闭、真实账号探测待原审批；本批不构建发布包或安装。


### 历史评分快照 v2（S8，只限首次实际排序比较）

[排序观察器](../../src-tauri/src/router/ranking_observation.rs)从原比较器的实际计算保留每个唯一候选身份首次出现的分项，不在保存、级联或读回时重跑rank／explain。观察开启与关闭都按原a后b读取并比较同一个total，未缓存分数改变排序。健康和限流可能在后续比较变化，因此这是first_sort_comparison事实，不是所有比较统一分值，更不是最终队首必为最高分的证明。配置策略与当时实际评分策略分列；优先级、sticky、活动、免费选择器和级联后的计划顺序仍单独保存。

新构造为schema2，严格读回同时兼容schema1；旧缺字段未记录，不回填、不重算。单候选可能未发生排序比较，重复身份无法安全关联则不可用；native精确选择仍不适用评分。只保留health／headroom／capability／latency／intent_fit／cost／efficiency七项既有raw、weight、contribution、固定原因以及bonus／total；非有限或非法数据不可用，缺测不补0，因子1不代表测得满分，权重0与真实原始0分开。不保存URL、凭据、正文或原始异常，不生成质量分、不改算法权重和派发。

[有界评分存储](../../src-tauri/src/audit/route_decision/scoring.rs)保持64候选、16KiB UTF8总限额：身份／序号／已有内部观察及状态优先，余量才放完整分项；超限在原序号明确标省略。auxiliary和cascade更新先撤分项再量完整元数据，不能靠旧省略数字判断大小；9→10及99→100的精确边界已保留有效红例并修复。级联新露出的未保存身份不借别人的分数，也不事后再算。仍用既有可空TEXT列与同一INSERT／usage事务及取消owner，无新表、迁移或第二写入者。

[评分及版本／字节测试](../../src-tauri/src/audit/route_decision/v2_tests.rs)、原生审计／真实loopback生产入口与[只读源码UI](../../scripts/request-decision-scoring-ui.cjs)已验；UI显示实际比较时点、未知／真实0／预算省略，v1详情仍兼容。路由关闭对照及金标仅证明固定状态和相同求值顺序，不承诺动态并发状态在相同墙钟时刻被读取。独立审阅预算Important已按实质红绿关闭；没有新真实服务／正式用户库、应用启动、包或安装，r3已装／r4候选均不含此后源码。更广任务／权威版本／质量窗口及独立内部调用仍保留原未验边界。

### 类型旁路的当次计划

现有 `passthrough_dispatch` 在 ModelType 过滤、rank、活动供应商提升及免费选择器之后、原 FailoverChain 等待之前，捕获真实已有链。`embeddings_plan`／`image_generation_plan`／`speech_plan` 明确区分三种入口；它们没有聊天分类／sticky／个人测试证据。Chat 类型不能通过 typed 构造器冒充旁路证据，未知阶段读取为不可用。

仍复用原成功／失败 `RequestLog` 与 usage 事务，未增加列、改变策略／重试／费用／usage 合同或创建新的取消 owner。已收到正常结果、首选失败后回退成功、候选耗尽失败都保存同一实际计划；实际路由目标和尝试链仍独立。路径／输入／URL 不进入快照。图片／语音的缺失用量不因候选计划变成可信用量或权益收据。

16 KiB 预算包含最终的 typed 阶段名；更长阶段名不能让边界快照整条丢失，必要时仅减少已保存候选，保持原 ordinal、总数和省略项。64 项及实际派发链均不放宽。[隔离用例](../../src-tauri/tests/support/request_decision.rs) 逐次核对自有 POST、类型过滤、计划和实际结果；源码浏览器复用相同前端产物，仅补三类阶段展示。原S3片段没有新增取消owner；后续S2的typed处理future取消已接续这一缺口，见下节。它仍不证明TCP断连一定向服务器传播取消，不能套用聊天或安装版证据。

### raw native 的精确选择观察

Trae raw native exact 不经过 ranked 候选链。现从真实 `NativeSelection` 在原 Audit 创建时捕获 `kind=exact_selection`／`phase=native_exact_selected`，`score_state=not_applicable`；仅一个精确目标，不构造 Candidate、重跑 rank 或杜撰评分。配置策略只是当时配置记录，本入口不应用该排序。候选／typed 旧 kind 保持原评分口径；跨 kind、phase、score 或控制事实的非法组合读为不可用。

只保存 provider id、模型别名、已选配置上游和原捕获时刻；不复制账号／主体／selector／token／目录。目标标识不可安全表示时保留该序位的信息缺失，大小限制不放宽。它不替代原 native attempt／usage／代次与权益证据，也不证明已经 POST、交付、收费或可信结算。

同一不可变观察随既有 Audit 的正常提交及 Drop 取消 owner 传递，在原 RequestLog／usage 事务落库；不增加 owner、不提前创建审计、不改关闭／失败／重试／退款或权益策略。初始免费／鉴权／输入拒绝原本没有审计的分支仍不制造观察；之后凭据变化／超时／取消则保留原选择与实际结果的区别。正式 issuer 保持关闭，正向权益消费者仍仅私有 cfg(test) 生产路径证据。

[源码原生路径测试](../../src-tauri/src/proxy/trae/gateway_tests.rs) 以自有 loopback 验证两轮 raw 输入、buffered／stream、准备等待／取消、关闭和审计故障，并仅补受影响的两个 Ledger 成功消费者。只读 UI 明确「精确选择已记录、未进行候选排序、评分不适用」，旧候选计划展示另作有限兼容检查。浏览器、假上游及私有证明均不代表真实账号或安装版；本片未迁移正式库、打包或安装。

### 分类／refine 的有界内部观察

[调用观察](../../src-tauri/src/audit/auxiliary.rs) 只记录实际进入的 Jev decide 批调用与 refine 逻辑调用，最多两条；不是问题数量、全部网络请求或物理 POST 次数证明。白名单包括 purpose／原顺序、可安全表示的配置模型与供应商、开始时刻／本地耗时、已见 HTTP 状态、可解析完整主用量和接口返回／改写采用事实。不复制 URL、Key、token、输入、输出或错误正文；Jev 不从答案／output=0 推断用量，Refine 不把主计数缺失补零。

调用返回标记 returned，future 被丢弃时以 RAII 标记 interrupted_unknown；已见状态／主计数保留，未知不改成零，不表示已交付／未收费或可信终态。正文形状被拒绝时，观察可保留先前解析的完整主计数，但原 RefineOutcome／健康消费合同不改。

同一 preparing owner 持有请求私有 journal；取消先丢弃内部 future，再由原审计冻结观察。形成计划时在 handoff 前冻结，正常／流式／缓存及失败继续消费同一快照。观察与候选链共用16KiB，必要时减少完整候选展示条目，保留原序号／总数／省略项，不限制真实派发。旧字段缺失表示未记录，不能推断零调用。

已接受请求若在内部调用后、尚未交接主 owner 就返回准备错误，原 owner 提交一条带实际状态与观察的失败父审计，主尝试仍为空、费用未知。鉴权／白名单前拒绝仍在 owner 之前；重复抑制原本不新增审计的分支保留原合同，没有父审计的分支不能声称完整观察。[公共入口对照](../../src-tauri/tests/support/auxiliary_observation.rs) 验证正常／Jev错误、分类或refine中取消、另一请求正常完成、网关仍运行，以及完成分类后主模型缺失404。

[只读详情](../../src/pages/RequestDecision.tsx) 区分未知／已知零、返回／中断，不合并主尝试数、费用或预算。未新增schema／writer／重试／退款／供应商权限；普通公开refine和未关联观察的Jev客户端保持原接口。完整财务归属、其他目的及实装继续另验。

## 隐私、验证与未完成

### 当前筛选的人工反馈覆盖

2026-10-25 S3／UX6 的结算结果独立性源码修复：统计页仍等待四个读取全部结算并核当前筛选active，保留原inFlight／5秒轮询保护；覆盖的成功或失败随后独立提交，不再先要求总览、审计查询和校准三个面板都成功。其他面板的错误仍显示，覆盖自身失败会撤下旧成功数值并显示不可用。没有新增查询／重试／写入；不处理永不结算的调用，也不表示所有统计面板均已独立。

六项新合成场景从2过／4失败到6过，原覆盖5项、类型／隔离前端／86单测与131语法／脱敏及独立审阅通过；455项原生输入未变，不重跑长Rust套件。仅本机计数可用性，真实人工标签／数据库未读写，不输出质量评分。v1候选计划已存在且未重做；完整历史评分v2、跨任务与模型版本质量汇总的窗口／偏差口径仍待原具体决策，不能把缺测或HTTP成功补成质量。r3／r4不含本增量，未打包安装、供应商／新认证外发，仍8/100。源码、Goal分类与实际证据（本机未发布证据路径：`../../../artifacts/Project-Enhancement-20261013/execution-schedule-20261018/multi-account-local-v1/native-rotation6-v1/priority-six-regions-v1/s3-coverage-independent-read-source-v1.json`）。

[覆盖消费者](../../src/pages/FeedbackCoverage.tsx) 通过 `get_feedback_coverage` 复用审计 RequestFilter，统计全部匹配请求，不受列表 limit／offset 影响。时间条件作用于请求记录时间，不是反馈编辑时间；没有引入额外默认时间窗或质量阈值。只读现有 requests 与 request_feedback，未新增 schema。

「已标注」是人工分类或结果任一非空；分类确认数单列。满足／未满足／不确定分别计数，未评价保持缺失；清除后的版本行不算已标注，HTTP 成功和机器分类不补人工标签。按审计记录的 routed_provider／routed_model 和人工确认分类分组，未知目标／分类单列；这些历史字段未隔离账号代次或模型版本，不能当作当前能力样本或推荐有效性证明。

总数和分组来自同一只读事务快照；分组最多显示 200 组，超出明确标记，完整总数不按可见分组重算。列表与汇总是各自读取，期间新增请求可能使两者短暂不同。读取失败、非法或自相矛盾的数字显示不可用，不变成零；空筛选和正请求但无人工结果的「数据不足」分开。旧详情表单的写入／版本合同不变。

[覆盖 SQL 测试](../../src-tauri/tests/feedback_coverage.rs) 使用自有 SQLite；[覆盖浏览器用例](../../scripts/feedback-coverage-ui.cjs) 仅使用源码页面与 IPC 替身。汇总不输出质量评分、排名或成功率估计，不训练、不改路由参数，也不生成供应商／费用／权益证明。

只保存有限人工标签，不新增提示词、回复、自由文本或隐藏推理存储。local_user 是本机人工操作来源，不是签名身份；不导入合成标注到正式库，不新增生产 fixture／签发入口。不同记录的费用、未知 usage、reply_unverified 和正式 issuer 关闭状态不因反馈变化。

[SQLite 集成测试](../../src-tauri/tests/request_feedback.rs) 使用自有内存／文件数据库，覆盖初始化无自动标注、相同 trace 的不同 id、版本／清除、重开与幂等初始化、并发写／清理、非法参数和存储故障；[浏览器用例](../../scripts/request-feedback-ui.cjs) 使用实际页面和 IPC 夹具，覆盖用户交互、请求归属、冲突、草稿和错误。真实 Tauri 实装 IPC、正式用户数据库迁移与新包均未执行；浏览器替身不能替代实体安装。

[Rust 命令桥接测试](../../src-tauri/src/commands/feedback_ipc_tests.rs) 另用非默认 `ipc-tests` feature，直接调用同一生产命令宏／参数解析／处理器及真实自有 SQLite，验证前端 requestId／input 合同、版本冲突、来源伪造拒绝和安全错误序列化。它使用 Tauri MockRuntime，不调用 initialize_backend、启动网关或真实 CLI；其他管理器仅指向新建空目录。该证据仍不覆盖真实 WebView2、应用启动／ACL 配置或安装版。普通测试不自动启用此 feature；命令与 Windows 清单条件见[开发说明](development.md)。

批准范围是源码实现和隔离测试。应用回退可保留新增旁表供新版本继续读；不执行有损 DROP，也不宣称 DDL 无锁／零停机。完整 S3 尚余未具备同等发送时身份回执路径的任务关联扩展、其他独立内部调用合同，以及权威版本／质量汇总窗口和偏差口径；当前筛选的事实覆盖已接消费者。不自动调权或训练，不将本批标为 S3／M0–M5 全部完成。

## S2 typed 请求future取消的单审计

embeddings／图片／语音原本只在等待正常或错误结果后提交审计；已实际派发的处理future若被取消会丢记录，写入失败也无从被drain发现。两项有效首红以自有上游实收1POST后取消本次任务验证，不手工构造取消错误。现在[typed派发](../../src-tauri/src/proxy/server.rs)复用原FailoverChain取消journal与[selection owner](../../src-tauri/src/proxy/server_stream_audit.rs)：先销毁内部发送future、收齐现有观察器的取消记录，再由原AuditTasks耐久提交一次499；同一v1计划保留。Ready会解除取消owner，正常／失败结果仍由原写入路径提交，避免重复审计。

typed取消行没有聊天session或聊天prompt估计；专用的非聊天投影仅影响这两个既有可空字段，默认聊天路径保持原值。费用／币种未知保持None，沿原取消格式的计数字段不能解读为已测得零消耗；一次已发生POST及未知结果不产生退款、自动重试或新免费证明。写入失败保留为AuditTasks错误，不能伪称干净排空。

核对关联预算时，实际认证Gateway另复现有限额typed调用绕过公共API原有reference-bound拒绝：HTTP200、1POST的有效首红保留。原守卫已移至[共同带策略入口](../../src-tauri/src/proxy/upstream.rs)，公共API与真实typed生产者都遵守同一409／budget_contract_unavailable／passthrough_reference_bound_unverified／NoReplay合同。未知typed计价单位仍不获准派发；没有新增单位、价格、限额、schema或付费能力。三类有限额请求现均0POST，原无限额正常对照保持。

自有任务取消覆盖三类typed、单次正常返回与写入故障；正常／回退／耗尽计划、完整预算入口及三条既有关闭耐久消费者作有限受影响验收，内部类型变体不另加顶层数量。这里只证明处理future确被取消时的源码生命周期，不证明实际TCP断连必然取消Hyper处理任务、强杀／断电、真实账号或安装版行为；初始输入／鉴权拒绝等未到该owner的分支不新增审计。本段保留当时v1评分未记录的验收历史；后续S8首次比较评分v2源码见上节，未替换旧包或历史回执。


S6／S8历史读回交叉收口：当前本地能力声明的配置修订变化不会回填旧评分；自有文件SQLite经支持→未确认→不支持及关闭重开后，按原审计id直接核对一条v1未知、一条v2分项及原JSON字节，身份／顺序／时间不变。同trace不合并，配置revision和两种JSON文档版本不等于权威模型版本。当前事实页与历史计划分别消费各自同源DTO，不发新查询补历史。见[本地声明合同](model-declarations.md)。

## 完整会话归组与逐请求追溯（S3，源码）

用户选择按完整实际conversation/session归并，同时保留逐请求明细。[只读repo](../../src-tauri/src/db/repo/conversation_feedback.rs)经[两个本机命令](../../src-tauri/src/commands.rs)提供 `get_conversation_feedback`／`get_conversation_requests`；[独立页面消费者](../../src/pages/ConversationFeedback.tsx)在用量与审计手动读取，不加入5秒轮询，不写标签、账号、路由或schema。

筛选先选择会话，再在同一SQLite只读事务读取所选隔离范围当前保留的全部历史，时间边界包含端点。筛选外请求也保留并标明，审计列表limit／offset不切割会话。“完整”仅指当前留存，不证明会话已结束或留存完整。每个request id只计一次；同trace不同id保留，多attempt不变多任务，人工反馈仅取该id当前修订。请求级满足／未满足／不确定／未评价及HTTP／499／错误计数不是会话最终结果或质量分，不从HTTP、reported usage推断费用或免费资格。

隔离依据是已保存session、access-key命名空间、已知原生profile及当次账号槽／主体hash的一致组合；同名session跨区域、主体、槽或访问Key不合并，已证明同一范围更换配置provider／model不拆任务。严格核对现有native_exact attempt与同次精确选择快照、目标和trace；重复、未知版本、非法或缺失回执明确未归组。原生回执是已有生产记录，不是新签名身份认证。首个归组切片仅Trae原生路径已有这些不可变字段；后续四区会话用途回执见下节，所有旧历史仍不借当前账号、模型别名或fixture补证。输出只含不透明group_id、session和公开profile，不返回账号槽、主体hash、认证值、正文或原始异常；session仍是本机已有会话关联，并非权威任务分类／模型版本。

会话页默认50、最多100，与请求页独立；请求明细默认100、最多500，按id递增、exclusive after_id，游标必须属于同一个隔离任务。后续读取是新快照，不承诺跨页租约；无效或消失任务／外来游标返回固定错误，不能伪装空成功。两个扫描各复用既有10万条安全上限，超限明确错误而非截断成功。SQL不取提示词／回复／错误正文，只投影必要字段，对JSON字节另设上限。

筛选变更、卸载或另一读取使旧结果失效，错误独立显示不可用不补0；固定错误和未知原因映射只读取自身属性，`__proto__`／`constructor`不成为React对象内容，敏感错误正文不回显。原审计行、人工反馈写入和原始收据保持。页面只读，不自动采样、重试、归因、调权或训练。

[归组集成测试](../../src-tauri/tests/conversation_feedback.rs)覆盖跨主体／区域／Key、完整留存、取消失败、当前反馈修订、同trace、分页／外来游标、缺记录、超限及无写入；[实际原生生产者测试](../../src-tauri/src/proxy/trae/gateway_tests.rs)用自有loopback产生原生审计再读回，命令宏以MockRuntime自有库验证。离线编译UI九场景和旧覆盖／S8历史兼容通过，独立审阅原型键Important已按红绿修复复核。它们不代表真实用户历史、六区全部可归组、WebView实装或质量汇总完成。r3已装及r4候选未纳入本切片，仍8/100，无新真实服务／认证、安装或新包。

### WorkBuddy／Qoder 发送时不透明回执（源码增量）

前一只读调查已确认本地session／trace／provider／model本来就被保留；缺的是通用逐跳的发送主体合同，不是把临时上游UUID改叫稳定会话。现在[私有回执模块](../../src-tauri/src/proxy/subject_receipt.rs)复用两种transport当前已捕获的稳定主体摘要与内部账号槽，只在现有最后身份currentness fence后同步捕获。credential-generation fingerprint、headers、token、uid／email、授权快照都不进入该对象。它无Serialize，Debug遮蔽；AttemptRecord私有字段也serde skip，不改变认证、刷新、预算、外发body／headers或重试策略。

原审计序列化器拿到本请求已经采用的存储session和trace后，才按带版本的用途域生成scope_digest：当前会话、固定transport／区域profile、原槽、原主体。不同会话／用途／区域／主体／槽不共享一个全局主体标识，同主体凭据代次变化不改该会话摘要；远程Key已有会话命名空间继续保留。另一个binding_digest绑定同一行的session、trace、provider、配置目标和零起attempt。JSON仅version／purpose／profile_id／scope_digest／binding_digest五项；profile版本是网关合同版本，不是权威模型版本。SHA256复用既有结构化摘要约定，是有界用途的单向关联，不承诺抗枚举匿名或供应商签名，也不证明POST交付、终态、权益或费用。

每个Failover尝试独立观察；重复捕获变歧义、无授权捕获为空，spawn不继承上下文。取消先销毁发送future，再从该尝试克隆观察读回，返回的流不去读后来的task-local。新包装层构造时立即装箱，64KiB输入future的旧65576字节红例已保留并修复；不将该边界问题宣称为已发生生产栈溢出。原生Trae的旧回执及取消／usage语义不改，缓存／无派发不凭空复制上次主体。

只读归组对四个固定新profile严格校验版本、用途、摘要及当行绑定；多个attempt、两种回执混杂、非法／未知／缺字段保持未知，不取当前配置、账号或文本补历史。WorkBuddy Global三项wire UUID及Qoder每次新建request／session UUID仍只作原协议关联，不作为稳定主体。用户显式session、本地随机匿名锚点和user字段派生键仍服从原上下文合同，后者不是独立证明整个用户只有一个完整任务。请求级反馈不变成会话质量结果。

自有Gateway／假上游验证WorkBuddy和Qoder两区stream／nonstream双turn归组，Qoder合成认证代次重建仍同主体；取消／重试另以真实FailoverChain加合成sender验证journal，不冒称每种transport断流都已有完整组合验证。两transport实际捕获／解码请求体的负控验证不含回执字段，headers接线另作静态确认。严格读侧拒绝session／trace／目标／attempt重绑、未来合同和多跳歧义；旧8项兼容、MockRuntime IPC、13项源码浏览器与适用门禁已验。完整默认离线Rust及所有skip／首败以源码回执为准，不合成新真实认证／调用次数。未读正式账号／历史，未迁移正式库、打包或安装；六区真实可用性与r3／r4旧包边界不因本源码扩展升级。


<!-- local-evidence-publication-boundary -->
> “本机未发布证据路径”保留原验收资料的定位，但不是源码仓库内链或公开下载；完整收据仍在原工作区，不能因此推断对应验收通过。仓库源码路径、行号、内链及疑似凭据检查均不豁免。

# 账号权益预留：本地账本与适配边界

2026-10-19 会话内 Codex CLI 个人测试是独立的显式测试许可，不是 `VerifiedEntitlement` 签发来源；详见[个人请求生命周期](request-flows.md)。现有本地账号元数据适配器提供观察绑定及发送前重读，不是不可变主体或凭据租约；真实探测仍待原审批，正式 issuer 仍为 None。人工正控只在 `cfg(test)`，通过真实 owned 假 CLI 和原 Gateway 消费者复核单次启动及未知交付；文本、exit0、runtime_id、余额和登录态均不升级为权益收据或 VerifiedTerminal，未知用量／费用保留未知。该源码增量未打包实装，不改变已安装候选或严格免费守卫。

本页接续[目录与严格免费路由](model-catalog-free-routing.md)中尚未支持的账号权益范围。内部 Rust API 的持久整数账本核心及9项历史离线回归保留，尚未接入任何真实账号准入。第6轮已验606输入包含该核心和加法表迁移；当前独立实装EXE为 `6cd220e1…`，这不证明安装后的真实账号账本消费。旧8c854300是本页初始记录时的安装基线。源文件为 `src-tauri/src/entitlement.rs`，加法表定义为 `src-tauri/src/db/entitlement_schema.rs`，由生产 `Db::connect_path` 的迁移链创建。

## 2026-10-19 Trae raw native 权益消费者

Trae沿dispatch_native_exact/current_until→model_stream→TraeAuth.execute_model_request接入现有预留/dispatch/settle；普通ChatRequest的native拒绝合同不变。最终发送绑定捕获的密文来源与proxy epoch、经独立会话校验的稳定主体及实际selected.config_name。Ledger等待后仍由原first-poll认证栅栏重新核对，逐事件与原deadline保持。只有严格native解码器确认clean-EOF的Done可提交收据，直接传递TraeUsageReport，不将Missing/Partial或可选字段填零变成通用Usage；原生与通用解释器分开，正式issuer仍None，登记/解释仅私有cfg(test)请求单位合同。Complete零值与非零值都可按该合同计1次requests，未用tokens/credits/价格换算权益。

8个顶层raw Gateway/自有WAL/loopback消费者覆盖buffered/stream单次结算、同token换密文/代理代次与错主体/model发送前退款、未知HTTP/截断无重放或回退、Missing/Partial/未可信/仅通用解释器保留完整占用、三处存储失败、派发后换代、取消后499/reader释放/完整占用。首红0/2→2/0、边界5/1及取消诊断0/1保留：已有redactor正确保留“owned prefix”的敏感尾缀fix，实测首块只发布owned pre、suffix长度3。保留原输入，精确断言安全输出及未发布尾缀，reader/499/Ledger/3秒断言不变；不修改生产redactor。最终3个受影响命名空间140/0/0（native Gateway41、Trae83、原生认证16），8项包含其中，未加窄批/旧73/54/479，不称全量绿；通用账本9、Qoder/WorkBuddy未重复运行，原通用消费者方法有字节等价记录。

| 交付类别 | 当前证据与缺口 |
| --- | --- |
| 已独立实装 | 原installed6cd候选及原606/340/12绑定；本批没有新包/实装 |
| 仅源码与隔离流程验证 | 后续Qoder、WorkBuddy及本批Trae本地Ledger消费者；不等于安装后真实账号验收 |
| 本地尚未实现 | AgentRequest/AgentReply只有model/prompt/timeout与text/transport；缺可信主体/代次、别名解析后的物理发送归属、权威终态/未知交付接口，不能用runtime_id或CLI字符串替代 |
| 外部合同未验证 | 正式issuer、供应商hard-stop/无付费溢出、真实稳定主体、单位/单次上界与权威结算收据；无新增真实账号/模型/刷新/目录/付费授权 |
| 原队列保留 | 远端压缩权限、N3/V3及其他S/UX/N/V本批未复核，不能从旧台账推成当前未实现 |

首轮lint的await_holding_lock失败保留；只将cfg(test)同步请求断言改为独立词法块，断言和生产代码未变。当前fmt/all-target Clippy/check及受影响成功helper2例实际通过，其余138不重跑，有限140不累加成142。609输入与688路径继续保留，当前业务源码未做release构建或安装；历史19ignored、11明确早退/EFS边界和全部原始失败保留。按用户最新要求，收口后保持原完整Goal active与唯一writer，等待父协调只读交付审计，不启动Agent或其他新阶段。证据在第6轮trae-entitlement-consumers-v1。

## 2026-10-19 WorkBuddy Ledger 生产消费者

沿刚封存的纯本地认证栅栏，现有GatewayState预留/失败处理已接入WorkBuddy的buffered与stream最终生产调用。WorkBuddyModelClient在完成目录与请求体检查后，用捕获的完整source_fingerprint、稳定identity_hash及最终物理model调用Permit.before_send；dispatch提交后再用同一捕获做本地current核验，不再次prepare。stream与buffered共用这条发送路径，只有SSE解码器确认的Done可调用已登记的收据解释，再沿原deadline核对认证后发布终态。

正式issuer解析仍None，人工grant登记与权威收据解释只来自私有cfg(test)合同。该合同明确“可信完整终态计1次requests”，不从WorkBuddy tokens、credits、零价或余额推导权益，不是供应商hard-stop或真实账号收费证明。普通permit=None沿用既有流程；新增NoReplay仅归属于已持有权益预留且可能派发的路径，未把上批未复现的本地401疑点改成所有WorkBuddy错误都不可重试。

8个顶层消费者回归通过真正GatewayState→自有loopback验证：buffered/stream双区各单POST、单预留与单次settled确认；同access token与同UID但保存源已替换、错误稳定主体、错误物理model均在发送前拒绝并仅取消本次预留；503未知派发不转另一合格权益候选，SQL只有1次尝试/0回退并保留完整占用；未可信Done不猜零或结算；reserve/dispatch/settle自有数据库故障均拒绝、无重放；派发后同token认证轮换401仍保留占用；真实TCP断开留下499与dispatched完整占用。取消夹具在POST已收到时先查dispatched，再断开；没有Drop退款，也没有增大3秒上限。

成功首红0/2（409 transport_unverified）→2/0保留。边界首轮5/1的唯一失败是人工wrong-principal使用非64位hex，登记前被InvalidProof拒绝，并非生产绑定放过；仅将测试主体改成合法且不同的64位小写hex，不改proof验证或边界断言。随后直接跑统一WorkBuddy命名空间53/0/0，含全部8项；再跑共享最终发送缺失权限守卫1/0/0。当前同源有限门禁**54/0/0**、两个原始harness summary、54个不同测试名，没有ignored或环境早退。定向2、边界5/1和旧73/479不累加；Qoder旧用例、账本核心9与未变WorkBuddyAuth命名空间未重复执行。这不是一次新全量绿，历史19ignored、11明确早退及EFS条件分支未验继续保留。

源码仍为609输入、688条tracked/untracked路径；新增业务变动只在WorkBuddy transport和Gateway WorkBuddy接线，账本核心/dispatch gate、认证栅栏、Qoder及其他适配器不变。当前源码仅经开发测试编译，未新做release包或实装，installed6cd与既有606/340/12候选不动。Trae/Agent、远端压缩独立权限、N3/V3及其他S/UX/N/V仍留原队列，本批不扩展；真实issuer、稳定主体来源、单位/单次上界、hard-stop/无付费溢出与权威收据仍需真实契约和适用授权。原完整M0–M5保持active，证据在第6轮workbuddy-entitlement-consumers-v1。

## 2026-10-19 WorkBuddy 捕获认证本地栅栏

本批只补下一项认证前提，没有把多平台Ledger接线合并进来。ModelAuthorization捕获已保存的加密源记录、来源指纹和已知到期时间；发生UID补存时捕获补存后的记录。新增model_authorization_current只比较本地区域/槽位、保存源和到期时间，数据库等待后重新检查时间；不重新prepare、不解密记录、不写入、不刷新、不请求账号身份。原prepare_model_authorization_bound在接近到期时刷新、缺UID时查询账号的正常流程保持。到期未知仍沿用原有语义，不以未知到期推断真实有效期限。

WorkBuddyModelClient的目录缓存按完整来源代次隔离；目录锁/网络等待后、最终模型POST前、响应头返回后、读取chunk前后、每个解码事件及EOF终态发布前均核对捕获认证。流期间沿用原请求deadline；已经轮换/移除/到期的捕获不能发布后续旧事件。栅栏没有额外的身份、刷新、目录或模型HTTP请求。

真实自有loopback首红1/2保留：健康通过，目录等待期间轮换仍POST、同一解码帧首事件后轮换仍发布旧Delta。最小生产修订后，WorkBuddy两个命名空间72/0/0；另补纯本地源缺失/过期替换/缺UID替换、跨槽拒绝及数据库等待期间到期的有效对照，均只使用隔离数据。追加网关回归使用真正GatewayState、普通permit=None路径，在POST已收到后轮换；开启Skip鉴权确认、1次确认及3次尝试预算，并用同别名带合成Key的备用候选先证明正常可发送。双区buffered/stream四个内部变体均401、WorkBuddy模型POST1、SQL尝试1/回退0、备用无新增POST、目录1、额外HTTP0。新本地Unauthorized不可重试且不属于is_fatal_auth要求的上游Upstream401/403，疑点未复现，不增加生产NoReplay。此结果只覆盖本批新增代次401，不代表所有旧上游错误合同都已验证。

新增测试只改变cfg(test)网关夹具：保留新例1/0，再仅跑受共享hold/backup分支影响的其余旧9例9/0；当前两个回执输入完全相同。原72中旧网关9用当前10替换，字节相同的其余63复用，有限并集**73/0/0**；原始三批82次通过含重复旧9，不能累加成82项，也不加首红/定向重复或既有Qoder479/核心9。没有执行一次新全量，历史19ignored、11明确早退及EFS条件边界不变。

正式issuer仍None，人工proof仅cfg(test)；该前提批次结束时WorkBuddy/Trae/Agent的Ledger最终发送/权威用量receipt消费者仍未接入；后续WorkBuddy本地接线见上节，Trae/Agent继续待办，真实供应商稳定主体、单位/单次上界、hard-stop和无付费溢出合同另未验。不新增账号操作或供应商请求，不把当前源码当installed6cd；609输入单独封存、原606/340/12实装候选不动。证据在第6轮workbuddy-snapshot-fence-v1。原完整M0–M5保持active，S/UX/N/V其他原卡保留按项复核。

## 2026-10-19 本地 Ledger 生产消费者接线

本批实际接入现有Ledger，不再只有缺证拒绝。CostGate对内部capability作不消耗的预览，GatewayState最终调用按provider、选中alias和purpose取得原子预留；Qoder在实际POST前核对捕获的凭据指纹、由UID/组织/区域派生的主体hash和物理model，提交dispatch意图后再核对认证。buffered及stream都只有在权威Done与受capability认可的用量解释齐备时结算；未知用量、派发后取消/代次改变/503、结算写入失败均保留完整占用，不退款重放或转付费候选。超界保持占用并封锁整个共享池。

人工proof和grant登记map均只存在于cfg(test)，非测试构建的issuer解析明确返回None；DB行、余额、手填零价、槽位和登录状态都不能创建权限。此次私有合同明确以原有requests整数单位计费：自有loopback的可信成功终态计1次请求，未把Qoder tokens/credits或本地价格换算成权益。实际供应商合同仍未验证，也没有开放正式免费/付费账号路径。

同源消费者12项覆盖真实GatewayState→自有loopback的buffered/stream成功、POST可见前dispatch已提交、共享主体池并发仅1POST/1预留、已派发503不尝试另一合格候选、未可信终态/结算数据库失败保留占用、超界封池、summary独立purpose与同池结算、错误主体拒绝及明确派发前cancel退款、TCP取消后499与完整未知占用、凭据轮换后旧占用保留且恢复GatewayState也不能用旧capability再次POST。内部变体不另加顶层数。

首红0/2→成功2/0、并发夹具8/1、错误主体0/1、统一库312/1均保留。并发夹具仅把屏障限为前两次预留，原3秒及全部单POST/单预留/余额断言不变；主体修复只在最终发送加入主体匹配。统一库唯一失败是新夹具的第二请求先被已有401健康冷却挡为503；改用同一隔离数据库上的新GatewayState验证恢复，不改生产冷却策略。其余312项生产/测试正文与当前源严格相同，未重跑proxy；修正项1、核心entitlement::tests9（本源只一次）及此前未启动的4个integration目标随后实际执行。当前有限并集**479/0/1**，原始失败和source差异均有收据，不称一次新全量绿，也不加旧566、22或定向复测。

| 依赖 | 本批实际状态 | 剩余边界 |
| --- | --- | --- |
| 预留/最终发送/结算 | Qoder生产消费者已接，私有人工proof真实loopback通过 | 正式issuer不存在，真实收费契约未验 |
| 唯一性与退款 | 同源核心9例实际通过，消费者SQL核对单预留/单结算；仅明确未派发错误可cancel | 取消或提交不确定不自动退款，不证明断电/强杀提交 |
| 回退与子请求 | 未知派发NoReplay；摘要独立summary权限且同池结算；每个最终调用走同一消费者 | 不把未知503当已知零收费；分类/改写FreeOnly仍走既有本地/不远端策略 |
| 其他账号适配器 | 该Qoder批次时WorkBuddy/Trae/Agent尚未接；后续WorkBuddy本地消费者见上节 | 正式issuer仍失败关闭，Trae/Agent本地适配继续未完成 |
| 外部验真 | 无新增供应商/登录/刷新/领取/付费请求 | 真实issuer、稳定主体来源、hard-stop/无付费溢出、单位/单次上界和权威结算证据需具体合同与后续独立授权 |

609输入源码与原608阶段独立封存，新增消费者源码没有打包/实装；installed6cd及原606/340/12候选、19ignored/11明确早退/EFS与历史失败保持。原完整M0–M5继续active，本地未接部分继续按原队列推进，不能以此批完成替代整体完成。

## 2026-10-19 账号前提生产消费者对照

审计/关闭/cache阶段已单独封存566/0/1，不能与本节新用例累加成另一次全量。沿同一完整Goal增加一个Qoder真实loopback HTTP顶层用例，Global/CN各先证明同一隔离认证可正常发出一个模型POST、完成响应并使用一个目录请求；认证来自明确的cfg(test)种入资料，并经生产model_authorization_current核验，不能冒充真实用户UID或已安装账号适配器验收。

随后同一自有provider设置手填零价，生产free_eligibility仍返回account_hard_stop_and_reservation_unverified。全局免费/继承free请求各有streaming与buffered对照，HTTP均409/free_policy_rejected且无新增目录/模型POST，原认证仍有效，free_entitlement_reservations仍0；两地区共8个内部拒绝变体只计1个顶层用例。该对照证明真实候选过滤消费者拒绝有效认证加本地零价，未绕过候选过滤直接调用Qoder最终物理发送方法，也未签发真实hard-stop证明。

另一个新增顶层单测绕过候选过滤，直接调用生产call_chat_upstream_with_options和call_chat_upstream_stream。全局/继承免费策略均在缺失运行时查询前返回typed FreePolicyRejected、409、不可重试且无账本预约；同一输入的普通策略正控到达运行时解析并返回不同的CapabilityUnavailable。这是自有空库的最终方法拒绝证据，不是已核实Qoder账号或成功分派/结算；4个内部拒绝变体只计1项。本节两项新用例各自1/0/0，不与566或旧库结果累加。

现有Qoder认证、principal来源/领取占有、epoch及本地entitlement引擎等10个实现/测试输入与已验606清单字节相同；从原始库测试日志精确关联8个相关已执行案例，不重新运行或累加。该历史拒绝检查点的静态核对当时没有生产issuer或消费者；后续本页上节已实际接入Qoder本地消费者。真实issuer仍不存在，其余适配器及完整远端子请求仍未完成，不能沿用旧“只有拒绝”描述覆盖新接线，也不能把新局部成功扩大为整体账号准入已完成。真实hard-stop/完整上界合同、第二Trae CN/Qoder CN主体与真实云用量继续未验，无登录/刷新/领取/第三方请求授权。新证据在第6轮account-prerequisite-consumers-v1，新增源码没有打包或实装；原installed6cd及所有原始失败不变。

## Qoder 逐事件代次依赖

Qoder原先只在读取HTTP chunk前后核对捕获认证，同一完整SSE信封解码出的多个事件之间会暂停，但没有重新核对。自有loopback夹具修正嵌套DONE信封首败0/5后，取得有效首红1/4：健康正控通过，首个内容后取消、凭据轮换、到期或持有唯一数据库连接，下一次poll仍能发布旧解码事件。每个事件发布前复用model_authorization_current并沿用原请求deadline后，完整Qoder受影响22/0/0，新增五项已包含其中；两个地区的内部变体不重复计数。首红、夹具错误和原始日志都保留。

这处修订不改解码格式、价格/计费单位、schema或重试策略。取消/轮换/到期后返回不可重放401，授权核验等待数据库超过原deadline时504，不再发布后续事件或Done；健康内容、reasoning、usage和唯一Done保持。均为自有隔离认证与真实loopback，不证明真实供应商权益。它只是本地Ledger消费者接线的前置依赖；本轮后续必须继续实际预留/最终发送/可信结算与并发、回退、子请求和未知占用功能，不以先前缺证拒绝测试作为完成。

## 证明与授权边界

`VerifiedEntitlement`字段私有，不实现Deserialize，没有IPC、配置、DB行或前端“verified”开关能构造它。当前只由cfg(test)的人工离线夹具创建；真实适配器签发路径尚不存在，现有runtime/native exact/Agent免费拒绝保持。未来接入必须核实当前主体、凭据代次、精确模型/用途、单位、有效期、供应商硬停以及每次请求的确定收费上界；不能从余额、手填零价、名称含free或本地RPM余量签发证明。

证明中的capacity是指定权益池/窗口的整数总上限，不能直接填只读接口返回的remaining或浮点余额。request_bound必须是适配器已证明的单次最大消耗，不能用token估算替代。没有可执行的no-paid-overflow合同、全部计费维度/用途或确定上界，仍保持pending。适配器还需在每次最终发送前复查当前身份/配置/证据；账本不能替代生产wire guard。

## 同主体共享与代次隔离

预算键绑定平台、主体hash、权益池、窗口和整数单位，不包含slot、凭据代次或模型别名；同一主体重复绑定slot、不同模型共享同一权益池时不会重复获得余额。不同主体、窗口或已证明独立的单位/池分开计账。平台/池/窗口标识的规范化与真实性是未来适配器责任，用户输入不提供证明。

slot绑定另存credential epoch、凭据指纹、观察revision和快照指纹。旧epoch、旧revision、同revision不同快照以及同epoch切换主体均拒绝；主体切换必须有新凭据epoch。轮换不重置原主体已花费或未知占用；新主体不能借旧主体余额。主体和凭据/快照指纹只接受规范64位小写十六进制hash，不存凭据或明文UID。

同一窗口只允许保持或降低已记账总上限，不从刷新后更高的余额重置用量；增额/新窗口如何证明尚待真实适配合同。另一个slot下调共享上限后，发送前再次核对当前spent+held，不能由旧预留绕过。当前余额读取仅供本地记账/诊断，不授权发送，也不推进分配轮次。

## 持久状态机

SQLite `BEGIN IMMEDIATE`串行完成身份/高水位核验与预算预留，网络不在事务内。取得数据库及完成读取后重新取时钟，并在提交前后检查有效期；时钟低于持久高水位时拒绝。

| 阶段 | 允许操作 | 余额行为 |
| --- | --- | --- |
| reserved | 标记发送意图，或显式取消 | 占用完整上界；取消只退一次 |
| dispatched | 仅凭同单位的确定用量结算 | 无结果、断流、取消或重启仍占完整上界 |
| settled | 相同用量幂等读回 | held减上界、spent加确定用量；不同结算拒绝 |
| cancelled | 重复取消幂等 | 不再退款，attempt不能复用 |

发送意图必须在真正发送前持久化，dispatch不提供可重放的幂等发送许可。若提交等待后证据已到期，返回拒绝并保留未知占用。Reservation没有Drop退款，也没有租约超时自动归还。重启按attempt恢复句柄，只恢复操作身份，不赋予第二次发送权；reserved可显式取消，dispatched不能按“可能没发出”取消。

用量超出预留上界表示硬停/收费上界假设被破坏：保留占用并永久阻断该共享窗口的后续准入，普通刷新不能解除。此时不能记录成零收费成功。只有可信的确定用量可以结算；当前没有真实收据/usage适配器为此提供证据。

## 当前验证

阶段证据沿 `execution-schedule-20261018/m4-entitlement-ledger-v1` 保存。initial-red为编译成功后0通过/2有效失败（尚未支持证明登记）；expanded-regressions-v1为6通过/2失败，其中共享上限下调后仍准入是有效行为首红，另一项是把managed_by写成user_override的验收断言错误。两轮原件均保留。

修正后ledger-regressions-v2实际9通过/0失败/0ignored，cargo check退出0：覆盖同主体双slot与不同模型共享余额、真实旧schema文件经生产迁移/重复迁移/复开、两个独立连接池并发仅一方成功、凭据代次及主体切换、等待DB时过期、提交后到期、预留/发送/取消/确定结算/未知结果重启、超上界阻断及attempt防重。旧模型alias/disabled/price和聊天历史保留。

13:52:52 UTC受影响门禁终态退出0：全目标Clippy `-D warnings` 通过，目录维护15、调度10、费用消费者3、免费生产路径13，共41通过/0失败/0ignored，无环境提前返回；292项源码/测试/依赖输入前后不变，清单SHA `3aa71750575be0d0d847c304bf9a22df42d862656268a12169e4149197284622`。本批连同账本9项共50/0/0，原始日志和输入沿阶段phase-proof保存，没有重跑无变化的旧整批。该受影响门禁不是新的全仓最终门禁，旧候选2136/19/10的边界单独保留。

独立QA于13:54 UTC复核该阶段退出码/日志/哈希，292项输入全匹配，并保存292文件/6540973字节测试输入快照。独立原件位于`independent-candidate-qa/ledger-affected-independent-v1.json`及`ledger-tested-inputs-v1/snapshot.json`；它们绑定该时刻的离线源码阶段，不覆盖后续R5源码变化或新包实装。收口记录已将起始命令标为initial_command；压缩次数与checked_utc只引用实际检查回执，收口脚本不再写入猜定次数或台账更新时间。

这些测试没有访问正式数据库或供应商，也没有证明真实账号硬停、云端并发余额预留、真实收费/收据结算或第二CN主体验收。将来接入真实适配器后，还必须补相应生产消费者的正负向零收费矩阵、最终包绑定与独立实装；此页不把本地引擎记为账号免费权益已完成。

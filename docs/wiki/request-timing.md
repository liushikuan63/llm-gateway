> 2026-10-25T04:47:02.262168+00:00：既有供应商连接测试的模型类型合同已源码修复并独立复核。Embedding／图像／语音首项不再被当作聊天 POST 测试，明确拒绝且不自动挑后续 Chat；原 Trae native 拒绝、首 Chat／空列表 auto 的 hi／max16／非流式请求保持。生产 IPC 有效红例 1过／2失败／0ignored 保留（直接红例为 embedding；其余类型由最终循环绿例覆盖）；最终生产命令 3项及既有模型／供应商集成、fmt／all-target check／Clippy／脱敏与文档门禁通过，准确数量见回执。**仅源码／自有 SQLite 和回环，不是类型专用探测或真实账号通过；当前安装 r3／冻结包不变，无新真实外发、认证、启动或安装，仍 8/100。** 前三项源码修复、历史失败、配置账号及完整 Goal 保留；六区解阻后仍优先。不为此新增矩阵版本，v48 的实装／真实状态不变。[源码及回归证据](../../../artifacts/Project-Enhancement-20261013/execution-schedule-20261018/multi-account-local-v1/native-rotation6-v1/priority-six-regions-v1/provider-connection-model-type-source-v1.json)。

> 2026-10-25T04:19:01.836708+00:00：原 M1 连接检查／S2 可靠观测的源码显示缺陷已修复、独立复核通过。同一供应商新测试开始后，其旧成功耗时不再冒充当前测量；失败和 IPC 拒绝清掉该展示，真实 0 ms 保留，另一供应商失败不抹掉前者有效样本。不改后端健康统计、路由、预算或账号配置。编译前端有效红例已保留；新增 6 项合成 UI、原供应商读回 7 项及类型／构建／脚本／脱敏门禁通过。**仅源码，当前安装 r3／冻结包未改，无真实健康探测、认证、供应商、原生启动或安装，累计仍 8/100。** 搜索地区语言首片已有收口不重做；S6/S8 政策依赖不擅自实施，六区解阻后仍优先。旧失败、Trae 两项源码修复和完整 Goal 保留。[源码证据](../../../artifacts/Project-Enhancement-20261013/execution-schedule-20261018/multi-account-local-v1/native-rotation6-v1/priority-six-regions-v1/provider-health-result-source-v1.json)；[矩阵 v48](../../../artifacts/Project-Enhancement-20261013/execution-schedule-20261018/multi-account-local-v1/native-rotation6-v1/priority-six-regions-v1/six-region-acceptance-matrix-v48.json)。

# 请求逐跳耗时：观测合同与缺口

本页沿[能力数据 S2](../VibeCoding任务卡-能力评价与使用数据.md)记录数值观测。2026-10-19第6轮候选已完成限定范围的同机独立Windows实装验收：606源码/340发布输入/12dist绑定，当前installed EXE为`6cd220e19e34a34b750378eb82dfa733a9fb841145e07c68211a9982fb79c9be`，normalized为`978efaf6e819ae7ca5db7985bbcce03b475e665803d717a94a85a984e709ec6c`。NSIS实际一次exit0；MSI仅管理提取深路径1603后同包短路径0，未做整机MSI安装。原生v3通过1源码账号fixture证据组和9安装产物组，验证本机模拟上游的Responses/override、缓存、去重、504、时序UI与隔离持久化重启，不证明真实供应商或真实账号适配器。

QA明确释放后继续同一完整M0–M5，后续源码不自动继承该安装结论。普通buffered上游/级联选择及选择后的持久化等待取消已在后续源码修订并经过下述回归；鉴权/模型白名单通过后的分派前取消已有下述自有HTTP回归；更早显式native解析仍未覆盖；缓存专有取消/写入失败、审计库自身失败可见性和自有任务正常关闭/JoinError提交屏障已有下节源码回归。进程强杀、断电和新增源码的真实GUI退出没有因此得到证明。原全量exit101与有限并集2310/0/19分别保留，11项明确早退及EFS条件分支不算真机通过；正式数据保护仅元数据比较，不读取密钥，也不证明字节未变。下文各检查点的“未打包/R5安装”等文字均是当时历史状态，当前以本段和[执行索引](../持续目标与任务索引.md)最新记录为准。

## 审计失败、关闭提交与缓存本地边界

异步请求审计沿现有写入任务接入私有完成计数，不保留已完成句柄或响应正文。数据库写入失败、任务panic或runtime取消使既有网关状态last_error保留失败；未提交不再被当作干净关闭。监听器正常结束或异常JoinError之后均先等待已登记审计，两个异常分支保留server错误，审计同时失败时保留两者；等待超过既有5秒窗口明确报告未知提交。退出入口现在先请求本网关停止/排空，再恢复本应用拥有的代理状态，但该新增GUI退出路径尚未实装。没有修改HTTP成功响应、计费/schema、费用准入或全局安全/网络设置。

本批8项有效首红后转绿：审计表故障/正常关闭3项、两种JoinError2项、同步native审计错误1项、cache本地取消/写入失败2项。三项正常关闭使用自有真实HTTP/SQLite，对照审计INSERT失败时交易不产生半条用量、status明确失败，以及取消后唯一连接被持有时stop不提前完成、释放连接后stop返回即能读到499且端口释放。JoinError两项各有提交成功和提交失败对照，不将内部变体另计顶层。同步native是自有WAL/loopback fixture，不是真实Trae账户。cache两项是内部normal_dispatch结合真实loopback MISS与隔离SQL：HIT被取消或会话写入失败后仍单条499/500、显式空attempts、0新tokens/费用，已完成首个MISS只发一次HTTP，不重放、不污染健康。

最终受影响门禁首批在Qoder自有网关用例发生STATUS_STACK_OVERFLOW（exit3221225725），后续targets未执行、没有完整Rust汇总；独立复现和临时标记日志保留。原因定位到async select延迟装箱，使返回future内嵌整段分派future；改为构造时装箱，保留所有断言和默认线程栈限制。新增64KiB捕获future的大小/未poll取消落库对照，最终同批通过。临时诊断标记已移除，没有修改供应商行为或字段类型。

修订后的同一608输入最终受影响门禁为**566/0/1**，包含一次--lib proxy和12个integration targets；按目标与完整测试名核对无重复，核心audit_tasks四项已由这次proxy实际覆盖。8项定向、旧282/286/311与首批崩溃前片段均不额外加总；原始汇总、ignored和环境早退以第6轮s2-audit-durability-v1原日志/收据为准。最终fmt通过后，全目标Clippy首101仅为新增cache测试七字段元组type_complexity；只提取同一元组类型别名，反向展开恢复运行时输入字节完全一致，SQL/断言和生产逻辑不变，全目标Clippy/check随后0。这不是另一批566项执行，当前封存同时绑定该唯一测试文本差异。runtime单测验证未poll任务被销毁会标失败，不能证明进程强杀或断电提交；当前新增源码未打包/实装，installed6cd及原606/340/12 pins继续独立保留，旧2310有限并集/19ignored/11早退/EFS边界不改写。下一沿既有账号前提fixture/loopback与生产消费者队列，不作真实供应商请求。

## QA释放后的普通 buffered 取消

本次源代码增量不属于上面已安装二进制。`normal_dispatch`复用已有选择owner和同次invocation取消journal；先Drop内层HTTP/链路，让数值guard结算后再快照，保证先前失败与当前取消各记一次。正常完成后撤销取消owner，继续原上下文和审计路径；不再重复追加链路本地副本。选择取消不把此前已完成cascade hop改成失败，响应体owner仍保留其既有失败修订语义。没有实际HTTP时不补造timing或费用，且不把取消送入重试、鉴权停用或供应商健康失败策略。

自有真实TCP断开四项有效首红0/4保留；最小接线后4/0，随后同一606输入的受影响不同顶层并集282/0/1（库52、九个integration targets128、server_e2e102），其中1为本地Codex CLI ignored未执行，无明确环境提前返回。额外定向复核包含在282中，不另加数。初次修订E0716临时引用编译错误、所有权回归5/1中的统计预期错误均有独立原日志；后者修正为沿用“没有timing元数据时省略HTTP扩展”的旧合同，并直接验证共享统计中总请求1、已观测0、有效attempt0，未改产品统计行为。

本阶段验证首响应头前、body读取中、prior429及401确认中的普通buffered取消：指定trace单条499审计、实际hop与offset、无backup重放、供应商不误停。另有内部owner零HTTP和已完成hop负面边界；内部owner测试不代表整个dispatch前置流程。已选择成功后仍有上下文/校准持久化await，异步审计仍未绑定进监听器退出提交屏障，故这两处取消/退出耐久与DB失败继续单列，不泛称取消全覆盖。原安装候选2310有限并集、19ignored、11明确早退与EFS未验不因本阶段282而改写。

## 选择成功后的持久化取消与本地写入失败

普通buffered的取消owner现在覆盖整个`normal_dispatch_inner`，包括上游选择之后的上下文/校准await。正常完成只沿原审计路径交接；取消时先丢内层future，再读保留的journal。实际HTTP已完成的hop维持completed/ok，不因客户端之后离开或本地会话写入失败改为上游失败；已完成调用的数值用量通过原CallCharge/UsageEntry保留，各provider/model分别写入已有日聚合。价格计算、费用准入、表结构与权重不变，无可靠价格时请求cost仍未知。

真实TCP对照使用新建的单连接内存SQLite：上游已完整解析并记录成功时间后，持有唯一连接阻塞会话提交。保持连接时先原子提交交换，再返回200和单条成功审计；断开时交换未提交，留下指定trace单条499，HTTP hop仍completed、已知3/2tokens保留、无重放或健康失败。另一负例仅移除自有fixture的session_messages表，审计表保持可用：HTTP返回500，Ready时仍由原owner记录本地unknown分类失败及已完成上游事实，不冒充客户端取消或供应商故障。这不证明审计表自身损坏后仍能落库。

有效首红1/1、会话写入故障0/1、逐调用归属0/1均保留；最初0/2夹具错误（表名及不稳定吞吐完成标记）也保留。最终内部两provider持久化1项和真实loopback3项通过；内部测试不声称实际两次HTTP。最终同一606输入相关门禁286/0/1，为库53、九个integration targets128和server_e2e105，1既有本地Codex CLI ignored、无明确环境早退，不累加中间复测/旧282。原安装2310有限并集及19ignored/11早退/EFS边界不变。

本节源码仍未打包或实装。完整dispatch前置取消、缓存命中专有的写入失败/取消、审计表或连接本身失败、runtime/进程退出提交屏障仍是后续入口。本文的SQL证明来自自有内存库，不代表断电或进程重启耐久；已安装候选的文件库重启证明继续仅绑定其原始pins。

## 鉴权与模型白名单通过后的分派前取消

在会话touch、上下文重建及路由预处理的await期间，前置owner保留合法trace和会话身份，主调用尚未开始时provider/model、attempts和timing保持未知/缺失。进入原normal或stream函数前同步handoff，原执行/响应体owner继续处理后续取消；前置owner不能同时再写第二行。模型白名单拒绝发生在建立owner之前，后续Ready错误仍沿原合同返回，不因为增加取消保护而新增拒绝审计。

真实TCP四项对照使用auto跳过更早的显式native读取，固定主调用尚未分派的SQLite等待边界：streaming/buffered断开均为主HTTP0、单条499、无虚构时序/费用/响应；保持连接后成功执行一次主HTTP并仅一条审计；错误统一Key在DB被占用时仍返回401且不落审计。首红2/2与最终4/0分别保存，未降低断言。最终同一606输入受影响311/0/1（库53、预算/白名单21及其他集成237），既有主调用取消的单行断言、预算与模型白名单合同也实际复跑，明确环境提前返回0。忽略仍为本地Codex CLI；不重复累加定向4、旧282/286或安装候选2310有限并集。

本节不覆盖chat_completions/native_exact::resolve等更早的显式native解析阶段，不把未授权/未解析输入当成已接受请求。主调用前的分类/搜索等辅助活动也不补造成主HTTP时序。原完整M0–M5继续；接续优先验证审计库自身故障、正常关闭/runtime提交、缓存专有取消与写入失败，之后复验账号principal/epoch/结算及最终账本消费者。真实hard-stop、第三方账户和新源码实装未由本节证明。

## 能观测什么

[观测内核](../../src-tauri/src/proxy/timing.rs)按单调时钟记录相对于本次HTTP发送的毫秒偏移：响应头、首个响应体字节、首段正文、首段推理、首段拒绝、首段工具和协议结束；正文/推理内容不进入观测对象。它们是网关看到事件的时间点，不是远端内部计算、排队、工具等待或纯生成耗时。后者没有可信来源时保持未知，不用已有总延迟相减推算。非流式可观测响应头、响应体首字节和完成，不提供首个token。已知小于1ms的观测为0，缺值为null，两者不可混用。

当前产生`TimingSnapshot`版本2，严格兼容版本1；mode为streaming/buffered，terminal为completed/failed/cancelled/incomplete或尚未结束。Ping、Usage、Finish、空字符串及仅tool索引不建立首内容样本；Finish不能截断后续usage，Done是协议完成，EOF缺Done为incomplete，错误为failed，未完成Drop为cancelled。结束后不接受迟到观测；内部时间倒退/溢出时不让不可信值进入样本，也不改变业务请求结果。

## 响应体首字节与版本兼容

首个响应体字节是reqwest交出第一个非空、已解码body chunk时的网关单调时间。它与send返回时的响应头、解析器产生首个正文/推理/拒绝/工具事件分别采样；ping、注释、角色空帧和部分JSON也可能先建立body字节样本，不建立首内容样本。gzip/br解码位于该边界之前，不把它称为TCP/TLS原始wire首字节或远端内部处理时间。

流式在chunk进入parser前记录；非流式及HTTP错误body逐块记录后保持原完整缓冲/UTF-8替换解码，读取失败仍按既有错误路径处理，不把先前累计的合法JSON冒充成功。空body或首字节前取消保持null；实际0ms仍为已知样本。观察句柄的Clone/Drop不拥有调用取消权，结束仍由原HTTP/stream guard结算，费用准入位置与发送数量不变。

逐跳生产数值对象使用schema_version=2、固定10字段，新增first_byte_ms。严格v1的9字段仍可读取；归一化为v2时首字节为null，不从headers/first_text/finished推算。直接Deserialize复用同一严格校验；非法字段/类型/超安全整数/顺序或未来版本拒绝。CSV先验证后保留原v1/v2版本；JSONL沿用既有原存储输出，不回写历史记录。

聚合request_timings仍为schema1，以可选first_byte指标兼容旧DTO。七指标共用原全局10000个含数值attempt预算，最多70000个u64保留值；v1缺首字节不加样本，新v2的0正常计样本，丢样本的指标不给有偏分位数。详情与统计均可读旧版；旧聚合缺first_byte显示未知，存在但畸形的指标拒绝。响应头和语义内容保留原含义。

阶段`execution-schedule-20261018/s2-first-byte-v1`原首红0/7、页面1通过/5失败分别保留。核心7先转绿；最终受影响v2为229/0/1，曾有MSRV及测试夹具lint首错，均保留。MSRV1.77兼容写法修订后，最终只插入一处测试换行；该差异有可逆字节证明、v2完整源归档和生产源逐项复核。最终对应端到端65/0/1重跑、fmt/全目标Clippy0；其余164项为生产/测试输入未变的精确范围复用，不能称新跑了另一套229。ignored均为需要本地Codex CLI的既有项未执行，无环境提前返回。

Windows生产构建、明细13/概览8/原生用量兼容16及附加6项TSX渲染合同通过；两个浏览器focus10组、8张实际查看的图验证v1未知/v2零值、非流式、选择指标、失败保留/刷新/重载及390px双主题，外网请求和控制台错误均0。浏览器为合成IPC，真实HTTP为测试自有loopback/SQLite，未调用真实供应商。独立核心及消费者只读复审未发现阻断；未代跑测试或安装。当前S2仍未打包，不用R5安装或旧全量门禁绑定新源。

## 逐跳归属和并发

[私有作用域](../../src-tauri/src/proxy/timing/observation.rs)由[失败转移链](../../src-tauri/src/router/failover.rs)为每次实际f调用创建，鉴权确认每次独立。HTTP观察在[标准聊天上游](../../src-tauri/src/proxy/upstream.rs)的费用准入和本地client/builder准备成功后、send之前建立，响应头在send返回后记录。私有上下文不属于公共请求字段，不改变Copy wire options。

每个作用域有Empty/One/Ambiguous三态；同f第二次HTTP后永久Ambiguous，不能将最后一个请求冒充整个attempt。嵌套与并发scope独立，普通spawn不隐式继承。返回流持有捕获的Arc和独立生命周期guard，poll/Drop不再读取task-local；丢弃可Clone观测句柄不能取消流。快照短暂加锁、克隆后解锁再序列化，不持锁跨await。

AttemptRecord新增可省略timing，现有attempts_json列无需表迁移；旧记录、无法归属及未覆盖路径保留未知。原latency_ms仍保留旧语义，它在流选路阶段可能早于完整响应结束，不能当作首个token。费用准入拒绝发生在HTTP观察之前，不消费新观测、额外请求或更改预算/健康；缓存重放不复制旧timing。

## 标准HTTP精确候选与推理参数

[私有候选作用域](../../src-tauri/src/proxy/dispatch_target.rs)由失败转移链为每次实际普通调用及鉴权确认绑定不可变provider ID和完整选中ModelRef，与耗时Empty/One/Ambiguous作用域分开。标准HTTP的canonical覆盖、extra body/header、自定义路径和supports_thinking均读取同一选中alias；同physical upstream的兄弟alias不能代供配置，选中overrides=None/空值仍是权威结果。首候选的Responses instructions预检直接使用精确覆盖，与实际发送共用纯冲突校验。无匹配作用域的直接上游调用保留原first-match兼容。

模型声明false包含未知，最终body在合并extra body之后剥除已有thinking/reasoning/reasoning_effort；显式extra body也不能绕过这个声明。声明true则保留现有方言编码已经携带的原生参数，不再无条件删除。当前验证OpenAI原生reasoning_effort和Anthropic原生thinking对象；不新增budget→effort映射，不因模型名或响应含推理事件推测新方言的发送合同，也不把字段已发送解释为供应商已接受。

作用域只供当前调用future内构造请求使用，不继承至tokio::spawn，返回stream不得在poll/Drop时重新读取。未为消除歧义额外裁剪Provider.models；既有FreePolicy过滤、目录投影、费用准入、重试数量和鉴权隔离保持原路径。账号适配器、成功后部分价格/candidate重建仍有单列的历史歧义，本批不宣称全部alias归属修复。

精确候选合同作为后续观测的前置保留；下节新增独立四阶段控制标签。数值TimingSnapshot v1/v2不变，统计扩展维持schema1并兼容旧DTO；当前来源与未验边界以下节为准。

本批`execution-schedule-20261018/s2-wire-modes-v1`保留实际HTTP首红1通过/5失败和instructions预检0/1。修订后check及7项HTTP先通过；受影响v1为263/0/1，fmt/check/全目标Clippy0、600输入不变。独立只读复审指出auth-confirm夹具应保证实际第二alias，随后仅增强该测试辅助文件的数据库顺序守卫和wire.stream断言；最终端到端74/0/1及fmt/Clippy0，其余189项按未变生产/测试范围精确复用，不叠加计数。ignored为既有本地Codex CLI项，未执行。独立复审已确认夹具缺口闭合，未代跑测试或安装。所有HTTP/SQLite均为自有夹具；前端输入/dist未改，本批没有新前端构建、浏览器验收、安装或真实供应商验证。

## M1 Responses 推理控制修复检查点

原始 Responses 的 `reasoning` 曾在入站归一和原生出站两处丢失。当前修订将真实对象保留在普通请求 IR 的公开 `extra.reasoning`，与审计枚举快照分开；原生 Responses 复制对象，Chat Completions 仅将 effort 编码为 `reasoning_effort`，反向也只做字段对应。依据[官方 effort 合同](https://developers.openai.com/api/docs/guides/reasoning#reasoning-effort)及[summary 合同](https://developers.openai.com/api/docs/guides/reasoning#reasoning-summaries)，不猜 token budget、默认 effort 或具体模型可接受的值。标准 HTTP 的 false/未知声明仍在最终 body 剥除控制；summary 等无法在目标方言表达的字段使用本地400明确拒绝，不静默丢弃。compaction 的客户端流输出与内部固定非流式调用分别验证。

账号覆盖在合并前先验证原始 retained reasoning，再从精确分派候选读取配置，选中 overrides=None 不借用同 physical model 的兄弟 alias。不同 canonical 对象或 canonical 参与的 effort 冲突明确拒绝，不能先覆盖 summary/effort 再让验证失效；合并后再次作纯验证。WorkBuddy/Qoder 的新增 canonical 与旧 thinking 冲突均在授权、目录或模型 HTTP 前返回固定脱敏 `RequestControlUnavailable`，HTTP400、不可重试并排除供应商健康失败。旧 flat-only 配置冲突仍保持原 Protocol 行为，不能称全部旧错误已统一。Qoder 原有 minimal→low、独立 budget 规则及 WorkBuddy 目录 supportedEfforts 检查保留，目录依赖的能力拒绝和真实供应商接受程度仍分开。

`execution-schedule-20261018/m1-responses-reasoning-v1` 保留实际 HTTP0/4、compaction夹具3/1、账号0/4及4/2、独立复审新增4项0/4等首红；修订后新4项4/0。同源受影响库139和HTTP142共281个顶层用例通过，另1次隔离子进程执行不重复加总。fmt通过后Clippy曾因新增测试 err().expect 写法exit101，保留原日志；仅11处测试取错写法等价调整后fmt/all-target Clippy0，最终相关窄集12+7=19/0/0。四个文件的受控改写与其他601输入逐字节对照，生产逻辑未变；19是281内的最终复核子集，不称新单批281或额外叠加通过数。全部重批605输入前后绑定，最终仅Wiki/索引文本变化另过文档门禁。

新增实际 owned HTTP 正控已验证 WorkBuddy effort 经过目录规则进入模型请求，以及 Qoder 双区流式/非流式签名后的参数；不是外部供应商或真实账号验收。相关UI源/渲染合同4/5/15/11/16和脚本56通过，未修改前端输入/dist，未重新构建前端或声称新浏览器/安装验证。独立只读复审无新源码阻断，不代替测试或实装。原216/1/1、CLI ignored、账号历史失败均保留；当前仍无M1/S2新包，完整同源终验/独立实装与真实外部边界继续；后续S2负控以本页最新章节为准。

## 超时、缓存与去重负控

`execution-schedule-20261018/s2-failure-cache-negative-v1` 用自有 HTTP/内存 SQLite 验证：headers 前没有 headers/byte/text 样本，headers 后 body 前只有 headers，ping 只建立 byte，正文才建立 text。SSE body 读取错误复用原 reqwest 映射，在返回流前捕获实际 provider/model；不会在后续 polling 读取另一次分派的主体。已有正文后超时，客户端已发送的HTTP状态仍为200，SSE错误及后置审计记录timeout/504，不重放至backup。数值schema2、控制标签七字段及费用准入位置不变，没有推导远端排队或计算时间。

非流式成功HTTP的body deadline同样保留 `GatewayError::Timeout`，不再吞成空JSON/502。已收到的400/401/429/503保持原状态及重试资格，诊断body读取超时不能将它们改写为504。其他读取失败沿用旧坏正文/HTTP错误处理；有效的部分JSON遇到读取错误仍不能被当作完整成功。buffered不记录语义首token，已知首字节和未知首内容分别入样本。

缓存HIT保留独立trace、零消费及空attempts，聚合只计MISS的实际HTTP样本。MISS/HIT审计分别异步写入，夹具有界等待两个指定trace均已提交后才断言聚合。dedup负控关闭缓存：Observe实际发送两次，Enforce第二次409且仅一次HTTP样本；不将拒绝称为coalesce，不借用其他请求的timing。

所有首红保留：初轮1/4含一处将既有可读 `attempt.reason` 当枚举的夹具预期；修正合同并加入直接Rust类型断言后有效首红2/4，最小SSE修订6/0，dedup单独1/0；buffered首红1/2后最小修订3/0。最终同一606输入下全部10项负控通过，后续check0、受影响101/0/0、fmt/all-target Clippy0，合计111个顶层用例，不叠加前述重复用例或旧M1结果。独立只读复审无新增生产阻断，缓存P3等待修订已在最终10项实际复跑通过。

相关UI源/渲染合同4/5/15/11/16、脚本56、文档/手册/diff门禁通过；当前前端输入及dist未修改，没有新浏览器、包或实装结论。正式资料、原失败/ignored及此前源码归档继续保留；非标准HTTP/native/passthrough未因此补造测量，普通buffered客户端取消、上游分派前取消和退出耐久仍属单列边界。下一步完成完整同源门禁、源/dist/NSIS/MSI绑定和独立实体安装验收，完整M0–M5继续，真实外部授权与账户边界仍pending。

## 四阶段控制标签与分组

标准聊天HTTP在同次调用的`wire_modes`附独立schema1固定七字段；数值TimingSnapshot仍是v2固定10字段，旧v1继续可读，两者不能互相补造。固定标签不保存正文、工具名称/参数、预算或effort原文，也不证明参数合法、供应商已收到或执行。

- `caller`在五类标准原始入口、协议解析前提取入站方言和已识别控制标签；调用方自带caller/wire_modes对象不能提供来源。无可信入口保持null。
- `normalized_ingress`在协议归一后、上下文/搜索/工具增强前捕获；Responses compaction在清工具和插摘要提示前捕获，后续不得从改写结果重建。它不是原始JSON。
- `effective_request`来自本跳实际交给编码器的内部请求；`wire`来自合并覆盖/能力处理且费用准入通过后的最终待发送body，随后调用send。连接前失败也可能有此标签，不意味着实网送达。
- 顶层dialect是出站方言，caller.dialect是入站方言，跨方言不要求相等。声明能力、显式关闭、未携带、无法识别和未观测来源分别呈现；工具非空仅表示列表非空，不代表已调用。

同一One/Ambiguous观察边界绑定数值与控制事实，第二个HTTP使整次invocation归属未知，不能借最后一个body。普通、确认复测、fallback各自取对应不可变快照，取消journal也保留同次已结算事实。Direct/internal不重建caller；native_exact、账号适配器、passthrough/非chat与缓存未接线时仍不补同类观测。

CSV新增attempt_N_wire_modes，只导出独立严格投影通过的七字段对象；缺失、未来版本、额外私有字段输出空。JSONL沿原始历史合同。最近24h统计在有效耗时逐跳中单列标签已观测/来源未知/标签无效，完整typed snapshot参与分组；坏标签不丢合法时间样本，不拿输出推理/工具事件当作请求模式。512组、10000个保留attempt和70000数值预算保持不变。

详情提供原始/归一/编码前/body四阶段；概览可按入口和body的推理控制、工具列表筛选，unknown与absent分开。旧DTO缺新字段仍可显示原统计，新字段严格验证，覆盖计数须覆盖每类已入组数量，未入组部分允许更大的全窗口计数。Space/Tab使用原生details与select，窄屏表格局部横向滚动。

已由自有真实HTTP证明当前Responses reasoning会在归一化时丢失；显示caller启用、normalized缺失是准确观测，不等于透传修复。该实际行为缺口进入M1本地修复入口，后续按协议合同和免费mock验证，不追加供应商请求。超时/重试/缓存可靠分类、最终完整门禁及同源新包独立实装继续pending，不以本批源/dist检查点宣布整个S2或M0–M5完成。

阶段`execution-schedule-20261018/s2-control-observation-v1`保留初始HTTP0/4、来源与Gemini0/2+0/1、CSV和分组各0/1及UI1/3、覆盖计数2/3首红；一次遗漏Dialect导入为check失败/零行为，亦保留。受影响v1实际216/1/1、exit101、603输入不变；唯一失败为夹具keyless备选在401后被正确隔离，生产规则未放宽。原136库项+80端到端的生产及未变测试范围有字节对照，随后仅失败用例定向1/0，之前未执行的10套消费者160/0、fmt/全目标Clippy0；不重跑已过批次、不把旧失败改绿或表述为新跑一套完整377。修正fixture仅使用自有合成Key，两个后续执行用进程随机master key和自有DATA_DIR，未保存该随机key。

P3计数下界修订后，详情/统计/原生用量源合同15/11/16与额外四阶段4、覆盖校验5通过；Windows构建及两个headless dist focus13组/14PNG通过、外网和控制台错误0。14图中12图与此前已实际审阅图逐字节一致，另两张统计表右侧已实际查看，确认已知0和截断未知。独立只读审查的两项来源/名称P2与计数P3均在源码层关闭，未代跑测试或安装。浏览器仍为IPC夹具，真实HTTP/SQLite为自有测试，不能据此关闭真实供应商或新包实装。G07新slot1错过启动期限，NOT_STARTED_ACTIVATION_DEADLINE_MISSED，23:36:06 UTC明确释放后主线实际恢复定向用例；不复用旧authority，不将Maven/生成/HTTP15项算已执行。

## 当前覆盖和后续交付

普通标准聊天HTTP的非流式/流式为当前接线入口。CLI、WorkBuddy、Qoder、native_exact及passthrough/Embedding/图片/TTS尚未接入，不能给它们填同样的测量。native_exact的权威用量marker及费用投影保持原合同。

纯内核、scope和stream生命周期已有阶段回归；实际HTTP和持久审计覆盖由父工作区阶段 `execution-schedule-20261018/s2-observation-v1` 的独立源清单、日志及phase-proof核对。首红、测试编译失败、接线编译失败及修正后的结果分别保留；阶段结果不代表完整S2已交付。

CSV已增加每跳`attempt_N_timing`数值JSON列。严格v1/v2投影只接受各自固定字段、已知枚举、非负安全整数与一致的时间先后；未知版本、无效类型、额外内容字段或旧记录导出为空，已知0保留。CSV保留已验证对象原版本；JSONL沿用原始审计输出，不回写历史。工具首事件只接受非空函数名或参数，单独的id/index属于元数据，不能提前建立首段工具时间。

[审计明细](../../src/pages/RequestTiming.tsx)以同一数值合同解释已知/未知与非流式限制，支持原生details键盘操作。终态来自实际流观察，不能把首事件选路成功当作整条流成功；HTTP本身完成也不能覆盖外层attempt失败，仍显示其失败状态码；无观测的旧记录明确显示阶段耗时未知。定向构建资产验收沿阶段`timing-ui-v3.json`：10项TSX渲染合同、5组真实浏览器交互、4张实际查看的图覆盖Enter/Space/Tab、读取失败/重载和390px双主题；此浏览器使用IPC夹具，不证明真实后端落盘或安装版通过。

## 逐跳统计与覆盖率

[共享只读投影](../../src-tauri/src/db/repo/request_timing.rs)提供带版本的`request_timings`扩展。IPC始终可返回独立最近24h统计，HTTP `/gw/stats`只在窗口存在可解析普通hop的非空timing字段时增添扩展，保留旧无marker对象形状；认证不变。统计窗口包含两端，排除未来行，使用自己的请求分母，不借旧概要的since-only或累计请求数。

请求互斥分类为已观测、未观测、无逐跳明细、无效；损坏与合法hop混合的整行归无效，其中合法hop仍按自身provider/model计样本，不借最终row winner。原始NULL、空数组、无timing、native/runtime、无效metadata分别保持语义；空数组可能来自缓存或旧记录，不能据它报告缓存命中。损坏/超限整行没有可枚举明细时不猜逐跳数量。前端明确覆盖只针对已落审计的请求，不等于所有发起/取消请求。

分组维度为逐跳供应商/模型、流式/非流式、尝试终态、该请求网关消息输入估算分档，以及实际观察到的推理/工具输出事件。消息估算不是供应商实际token；没有事件包含未知，不能声称该请求关闭思考或工具。可靠请求模式/精确发送输入证据仍待生产侧接入。completed HTTP且外层ok=false单独为attempt_failed，缺失ok不猜成功。

SQL在SQLx分配String之前按TEXT及65536字节限制每行，最多解析64跳；最多512组，全局最多保留10000个有数值时间点的attempt，七指标共享预算。每个指标分别提供samples/retained_samples和nearest-rank P50/P95；未知不计零，实际0保留。超限仍计有效样本和未入组数，但任何丢样本指标的分位数保持null，不能用有偏前缀冒充完整分布。此限制仅保证新projector的观测存储有界，不代表原有整个Stats端点的其他查询已资源有界。

[统计页面](../../src/pages/RequestTimingSummary.tsx)支持七指标、方式/终态/输入估算/输出事件/模型筛选、20组分页和键盘横向滚动；该筛选及下方审计筛选都不改变统计覆盖分母。旧DTO不出现新面板，未知版本或损坏合同只给不可用提示；刷新失败保留最后可读结果。v1严格数字/枚举/重复分组检查不展示原始额外数据。

本批阶段`execution-schedule-20261018/s2-timing-stats-v1`实际22/0/0（仓库7、IPC6、真实loopback HTTP消费者9）、全目标Clippy0；Windows构建及6项新统计TSX/10项明细合同通过，统计浏览器5组/4张v2实际复核图（无外网/控制台错误）。首红、v1两张长表截图裁剪/黑区与v2短表修正分别保留。独立实现审查未发现阻断，未代跑测试/安装。这里的浏览器为合成IPC夹具，HTTP为测试自有SQLite/listener，不代表真实供应商、安装包或完整S2交付。

## 选中上游后的响应体取消审计

[响应体唯一收尾器](../../src-tauri/src/proxy/server_stream_audit.rs)在async-stream生成器外构造，未poll的响应体也持有审计所有权。EOF和Drop用Option::take只调度一次；先同步Drop生成器/上游，让数值timing完成，再序列化快照。锁内只有小型数字/状态更新，不跨await/yield，不复制正文或工具参数。

客户端在首段后断开时，审计状态499/error=client_cancelled，逐跳保留实际cancelled、已知首段和结束时间；取消不追加供应商请求、重放、降级或health惩罚。取消cost保持未知而不是免费，旧token标量占位也不能当作供应商实际账单。请求响应头已发送时不会把其HTTP状态改成499，499描述审计结果。

已确定上游Err/EOF在持久化await前记录失败，取消不能覆盖它；已正常完成的状态在所有终帧yield前记录，取到终帧后直接释放而不读EOF不会丢审计。正常成功仍使用原latency取样点、final usage和原候选计价函数；后置失败改为保留真实header trace、已知winner/route/输入估算，旧helper的随机trace/空winner不再冒充这一路径。

上述响应体检查点的范围限于selected成功后的响应体；首有效事件前的接线见下方后续检查点。现有async数据库写入是活runtime内best-effort，进程退出/runtime关闭/数据库故障不保证耐久，只记录错误，不添加无界重试。客户端取消的部分对话上下文保存仍是单独缺口，本批不宣称完整S2交付。

本批`execution-schedule-20261018/s2-stream-cancel-v1`先有真实断开0/1首红，v1的3/1为测试错误读取recent_requests未提供的trace字段，修正为SQL权威读回。实际受影响批110/0/1与Clippy0（1项为需要本地Codex CLI，未执行），含收尾器4、端到端52、流式15、追踪14、审计16、HTTP统计消费者9。原测试辅助文件CRLF恢复后，格式检查0、统计7+IPC6+真实HTTP4共17/0/0、Clippy0，前后597输入不变；与110项有重叠，不相加为新增测试数。v1精确源归档和纯换行对照分别保留。只读独立生命周期审查无阻断；它未代跑测试或安装。

## 流式选择阶段取消

后续[选择阶段回归](../../src-tauri/tests/support/timing_observations.rs)已补上stream_dispatch尚未拿到首有效事件时的审计交接。失败转移链的`cancellation_records`默认关闭，只有流式生产入口显式启用；四处正常成功/失败追加同步进入本请求journal，journal不参与预算、准入、重试、鉴权隔离或健康决策。

取消调用的future不会正常返回其timing。[取消感知作用域](../../src-tauri/src/proxy/timing/observation.rs)先Drop所属future/HTTP guard，再回调提交这一跳的真实目标和已结算观测；Empty和永久Ambiguous仍为未知，不借另一请求或最后一个HTTP。确认复测取消保留确认前缀，不触发“鉴权已确认不可用”，不继续fallback。

[选择收尾器](../../src-tauri/src/proxy/server_stream_audit.rs)先释放嵌套调用，再取出先前已完成及当前取消的journal并调度单行审计。正常Ready会撤销选择阶段writer，随后由旧整体失败路径或响应体唯一owner接手；该交接段没有await。有实际调用时使用真实最后尝试目标；没有调用时provider/model保持None，不造winner。首内容偏移未知不填0；尚无响应头时响应头偏移也保持null。

本批`execution-schedule-20261018/s2-selection-cancel-v1`真实首红0/2；一次既有夹具model类型误改导致check失败/零行为，已修正且原日志保留。其后耗时核心/作用域20/0/0、真实HTTP8/0/0及Clippy0；最终受影响198/0/1及fmt/Clippy0，597输入前后不变，重叠数量不累加。198包括收尾器4、鉴权11、路由39、免费策略13、预算21、端到端56、流式15、追踪14、审计16、HTTP统计9；ignored为需要本地Codex CLI的既有项，未执行。预算测试的自有占用端口负控按预期触发10048，不算环境跳过。

实际loopback覆盖无headers、headers+ping无内容、prior429后下一跳取消及401确认复测取消：两跳各自failed/cancelled、确认前缀、SQL trace、单行审计、单次实际调用/确认两次调用、backup0、上游释放、供应商不被误停均有断言；同时读取共享stats确认自己的单请求分母、两跳分组、未知首内容零样本和已知headers/finished样本。全部是测试自有SQLite/listener，不证明真实供应商流量。独立只读审查发现并报告过上述夹具编译错误，修正后check0且行为通过；生产链未发现其他阻断，审查者未代跑测试或安装。

当前覆盖stream_dispatch的上游选择和后续响应体；它不等于所有网关取消均已落库。进入上游分派前的路由/鉴权准备、普通非流式取消，以及客户端取消时部分对话上下文仍是分开的生命周期边界。运行时/进程退出或数据库故障仍无耐久保证。不因未知时延/账单数据而补零或追加实网探测。

首有效事件前的流式选择、后置响应体取消及四阶段控制标签的导出/概览/详情已有本地证据；Raw Responses归一丢失推理控制的实际修复、失败/超时/重试/缓存的可靠分类及timing负控、最终完整门禁及同源新包独立实装仍待完成。当前本地接线不代表完整S2已交付。

不改变路由评分、权重、原有端到端P50/P95及数据库表；无真实供应商请求或额外评测，真实时延分布仍待适用授权下的被动样本。


## 2026-10-19 当前源码门禁补验边界

第6轮原生主线在相同606输入下完成默认Rust目标的有限并集：2310框架通过、当前失败0、默认ignored19；不是重新运行一次完整全量。首轮全量实际2100/1/15、exit101已保留：D盘工作区自有数据目录继承Users/Authenticated Users，而Windows密钥创建沿父目录继承ACL，既有权限测试假设正常用户数据目录的私有权限。保持源码与安全断言不变，原fixture再次4/1为真实负例；新自有用户Temp目录5/0，crypto5/0为重复复核不累加，仅补此前未执行的12 targets，209/0/4。没有对原失败文件、真实用户数据或系统安全做ACL修改。该结论修复runner前提，不保证任意共享portable目录都有独立DACL保护。

框架通过包含11次明确环境提前返回（本机模型后端缺失及空测试库的数据库ACL项），另有2个EFS条件测试分支未被独立确认；不把这些当作真机或真实外部通过。doc-test harness退出0、实际0项。fmt/all-target Clippy/check、UI源合同51、脚本56与前端构建、文档门禁均0。新构建dist12及前端输入167与此前审阅字节一致，复用范围有界，不声称新浏览器验收。原始日志、before/after、精确target清单与恢复证据沿`execution-schedule-20261018/multi-account-local-v1/native-rotation6-v1/current-gate-summary-v1.json`及其`full-gate-acl-recovery-v1`子目录。下一仍为同源新包、独立Windows实装和完整M0–M5剩余队列；旧R5安装不能替代。

## S4：Ollama 原生生成报告与本机计时分开

既有avg_tps／Pareto速度由输出token除以调用窗口耗时得到，可能包含等待、认证或本地开销，不是纯生成时间；其采样门槛、EWMA和路由效率参数在本片未改。能力页已标明旧调用窗速率，新指标没有暗中替换前沿或评分。

[Ollama官方使用统计](https://docs.ollama.com/api/usage)为最终done=true响应提供eval_count和eval_duration，时间值为纳秒。本片[数值报告](../../src-tauri/src/proxy/generation.rs)只读取这些上游字段，以eval_count×1000000000÷eval_duration计算报告生成tok/s；不借用本机阶段时间，不从订阅积分、字符数、模型名或退出0推断token。它是协议报告，不是独立实测、供应商扣费凭据或任务质量证明。

[同一HTTP观察器](../../src-tauri/src/proxy/timing/observation.rs)仅对实际Ollama方言创建私有共享报告；buffered成功JSON或stream最终NDJSON行捕获，原AttemptRecord.generation在已有attempts_json中随同一owner／事务序列化。没有新增表、额外请求或正文／隐藏思考内容；未有原生报告的其他协议与旧记录保持未知。多次实际调用导致观测ambiguous时不借前次；同一观测看见多个final报告也不能拼凑样本。报告只用于该次调用的描述。

明确eval_count=0与正eval_duration可形成已知零；缺失／负数／小数／越界计数、缺失／无效／零耗时或eval大于已报告total时，速率未知，不补0。u64纳秒以十进制字符串保留精度。失败、取消、incomplete不成为有效生成样本，即使已经看见数字；UI也核验本跳外层成功、完成终态和mode一致，损坏／矛盾旧对象不显示成已知速率。很小正速率用科学记数，不舍入成0。

[审计详情](../../src/pages/RequestGeneration.tsx)另展示报告总耗时、模型load和prompt评估，未知项不假零。没有从这些值相减推断工具等待或网关开销；推理token、外部工具和缓存收益的拆分仍未知。缓存命中没有新模型调用，就没有新generation报告，不能继承首次HTTP采样。公开协议usage的既有缺省兼容及原参考计价均保留。

[实际自有Gateway回归](../../src-tauri/tests/ollama_generation_observation.rs)验证buffered／stream、报告9tok/s与本机耗时独立、缺失／无效／零、原wire默认值、缺终态／内联错与cache HIT；严格计数自有POST／同一trace SQL审计。模块单元另覆盖大纳秒、重复报告、取消和并发独立状态。首次缺generation有效0/1及源码UI缺详情首红保留；前端Safari13 BigInt字面量构建失败也保留，已用规范十进制长度／字典序代替，未升级浏览器目标。输入／日志／截图与去重结果沿第6轮s4-ollama-generation-v1；仅源码／合成上游，不是已安装版或真实Ollama性能通过。

其他协议原生生成报告、按思考／工具／输入长度的统计窗口、有效任务质量与时间／成本联动、重启后统计和版本隔离仍须按原S4/S7/S8合同继续；本片不宣称完整S4。原N3留存隐私选择尚未答复，两项有效红仍在，不能用本片有限绿掩盖；无正式库／真实账号／发布或安装动作。

### S4 被动原生报告覆盖（源码阶段）

现有24小时审计扫描一次读取有界attempts_json，同时返回可选generation_reports；未新增表、请求或评分消费者。SQL仍先限制每行64KiB及64个逐跳槽位，分组512、保留样本10000沿用现有资源上限，两个指标窗口独立保留。请求按可检查／未保存／空明细／整行损坏互斥计数；已解析逐跳槽位再分可用、未报告、未知和无效，非对象也属无效槽位，不猜损坏整行的逐跳数。含可用报告请求数是至少一跳可用的请求，不是每跳完成率；缓存空明细没有新报告。

仅成功完成、外层ok且模式匹配的合法Ollama原生报告进入速率分位数，报告输出0与未知分开。沿实际逐跳供应商／模型、传输、消息输入估算、事件和调用参数标签分组，不借请求最终赢家；标签缺失／无效独立标明。P50／P95是逐跳报告速率的等权分位数，不是总token除总耗时或供应商性能保证；超保留上限的相关分位数未知，超分组上限仍保留覆盖计数。报告保留期受现有审计清理约束，不能据此制造跨月财务保全；N3留存隐私选择仍待明确答复。统计页旧DTO缺少新字段时保持兼容，损坏新字段仅使新面板不可用。

源码浏览器／隔离SQLite与已安装版严格分开；没有实际Ollama模型、真实供应商、正式用户库或额外评测调用。未改现有health／EWMA／router参数或费用与权益政策。

读模型精度：已用锁定serde_json复现u64最大纳秒派生速率在JSON读回时相差1ULP。聚合器仅接受原整数计算值或同一锁定编解码器的精确往返值，并由整数输出量／纳秒重新计算汇总速率；无宽泛epsilon，不把其他不一致数字视作可用报告。此规范化不改原审计JSON、旧详情投影或原上游采集器；旧详情的严格数值校验仍可能对极小速率保守显示未知，不能借新汇总宣称全部详情已覆盖。

### S4 原生报告详情读回精度修复（后续源码）

上一段“旧详情对极小速率保守未知”保留当时边界；本次同一生产Gateway→自有loopback→SQLite→实际query/recent DTO→未修改前端已建立有效反例。原因是锁定JSON解析对冗余f64派生速率产生1ULP往返差异，而整数输出／纳秒仍精确。现query/export共用审计映射及recent只读DTO复用同一个严格原生报告投影，仅规范化已验证派生速率；审计原JSON不改，不增加字段／schema／请求，不更改未知usage、原标量或财务权益语义。

前端仍按精确整数合同校验，未增加epsilon。非法source／版本／多余字段／错误速率不被规范化，合法0与unknown报告不变；非法整行与legacy原有处理保留。数字JSONL视图会带规范化的冗余派生值，不再以解码器舍入后的位模式伪造未知；CSV列和SQL审计原文不变。源码实际生产查询可达与浏览器证据不冒充实体安装或真实模型性能；buffered／stream各自实际POST与审计计数，内部变体不额外累加顶层测试。

### S4 文件库关闭／重开证据（源码）

自有文件SQLite经生产Db::connect_path初始化，写入11条纯夹具记录，主动关闭整个连接池并drop，再以同一文件重新初始化。原attempts_json逐字节、窗口内查询／导出及完整统计前后相等；窗口内9条请求／6个已解析槽位中2个可用报告按streaming／buffered独立，未知报告schema、未知timing schema、缺失生成时间／报告、NULL／空数组／损坏整行和窗口外对照保持原分类，合法极小速率不变0。WAL／外键／quick_check有实际输出，两次池均确认closed，未操作正式数据库。

这是已有机制的文件库持久化补证，生产实现没有为该测试改动；不是强制终止／断电恢复或安装应用重启验收。观测schema版本与模型权重版本不同：前者未知则不入可用报告，后者当前原生报告没有权威身份，不能据相同模型名宣称已完成模型版本隔离。S7增量评分、最小样本／窗口／衰减等仍须独立设计，未借本证据启用新评分或发评测请求。

### S7 最终发送配置投影（被动观察前提）

[投影器](../../src-tauri/src/proxy/observation_context.rs)只为普通Ollama HTTP生成可选的版本1非正文投影。输入来自同一不可变Provider、最终实际URL／body物理模型和白名单数字／布尔options与think固定标签，按固定域和排序序列SHA256；不序列化整个Provider／Model／请求，不读授权头、密钥、正文、工具定义或stop字符串。含userinfo／query／fragment的URL、未知／错误类型选项、未识别think或账号runtime使投影保持未知，调用行为不变。投影是明确白名单子集，不是完整请求等价或完整配置版本；服务端权重、外部环境和未纳入内容仍未知。数字/布尔选项具体名单与资源边界以该模块为准。

绑定RequestBuilder的输入在预算await前已经不可变，派发准入完成后才将投影放入同一个HttpObservationGuard；后续body等待、Drop取消和延迟审计序列化不读取最新配置。原SharedTiming的单／多观察上下文规则保留，多HTTP调用歧义不挑一个投影。AttemptRecord.observation_context可选字段沿原attempts_json及原审计owner落库，旧记录不据当前配置补造；无新表／列或配置开关，不改变timing v1/v2或原生generation的11字段合同。账号native路径仅None构造，不承诺新覆盖。

投影对象6字段为schema_version、scope=ollama_wire_projection_v1、provider_id、physical_model、fingerprint_sha256、model_revision=null。SHA固定64位小写hex；权重版本当前始终未知，不能把哈希当账号主体、VerifiedEntitlement或免费／费用证据。实际body模型可能与较早候选标签不同，详情明确显示body字段；provider_id必须属于本条逐跳。

[耗时读模型](../../src-tauri/src/db/repo/request_timing.rs)及原生生成速率分组将投影作为额外键，分别标observed／unknown／invalid，不混合不同投影；无效元数据不丢弃合法数值样本，不反射原坏字段。512分组／10000保留样本资源上限沿用，投影增加分组时仍保留全窗口分母，超限统计按原规则未知。UI旧DTO无字段兼容，新版详情及两类统计真实消费投影并做父provider绑定，未知模型权重版本始终明示。

同模型不同投影原本合成1组的SQLite有效首红、实际Gateway缺投影首红、源码浏览器重复组首红及合法格式但错provider详情首红均保留。自有上游在首个HTTP等待中切换配置，实际收到num_ctx32768／4096，首请求审计仍保留旧投影；buffered／stream、未知／非法、并发／歧义及原计时／生成回归有有限证据。锁类型夹具编译失败单列，非有效首红；旧controls入口改test-only清除dead-code警告，不改实际发送。

这只是S7被动来源分组前提：未选择新最小样本／衰减／窗口／权重，不把样本写入路由分数或health EWMA，不证明真实模型版本隔离与任务质量。源码浏览器／loopback不算已安装版或真实供应商实测；原留存隐私选择已获本次明确批准，2项历史红测及后续实现另沿N3留存阶段，不借本片宣称已修。

## S7：既有调用窗 TPS 的已知目标配置归属

[内存来源登记](../../src-tauri/src/proxy/health/tps_source.rs)为现有调用窗 TPS 加发布代次，不用上面的原生 generation 报告或 observation_context 替换指标。已知绑定仅含供应商ID、方言、配置base_url、已存配置密文标识、runtime绑定ID和配置上游名；空模型列表登记与Router相同的隐式default。配置密文仅在已加载内存中比较，不解密，不输出摘要，不读取master.key；无新表、列、配置JSON或持久化统计。

发布新路由时同步失效改变／移除目标的旧TPS；同来源的备注／价格保存、同物理上游的多个别名保留数据。每个已有生产TPS提交点在请求等待前捕获私有owner，完成后在同一来源读锁下校验并提交；发布／清旧值与提交序列化，Arc代次不因A→B→A或删除重建复活。旧请求仍按原规则完成，但不能回写新目标速率。旧候选读侧也不能借用不同当前配置的TPS；健康、冷却、延迟和限额其他字段保持原语义。

原completion_tokens／调用窗耗时口径、零／非有限／2000tok/s守卫、0.8/0.2 EWMA、配置最小样本（默认5）和路由权重均未改；少样本仍未知，不能把零样本当零性能。独立HealthRegistry的旧公开记账接口保留兼容，网关生产路径用来源owner版；这不是给旧接口签发来源证明。数据收集不主动发模型、身份、目录或刷新请求，也不退款或重放。

[自有Gateway对照](../../src-tauri/tests/tps_source_binding.rs)保留旧5样本跨URL、迟到完成累计7的两项有效首红，以及空模型列表5次成功但0样本的兼容首红；现验证同源备注／价格保留、换源清旧值与新源正常采样、迟到完成不回写、删除重建、假凭据配置及既有default正常入口。私有单元另验证旧候选、新Registry、A→B→A、物理别名与旧字符串键歧义；关联普通／流式、原生用量／Audit和旧接口只作源码／隔离证据，不是真实模型或已安装版验收。

这仍不是完整S7：绑定不证明实际权重修订、运行时内部／认证状态或完整请求参数、代理与硬件条件；不把配置哈希当principal、VerifiedEntitlement或收费收据。已批准的24小时空闲失效见下节；其他窗口／衰减、最小样本新政策和完整版本隔离继续按原卡先设计后实施，没有启用新的增量质量评分。


后续消费者复核另补Pareto：原对比视图未传入捕获的Provider配置，旧A快照可借用当前B的TPS。真实自有Gateway先为A／B各采样5次，新旧视图同得B速率的0/1有效首红保留；现该唯一读点复用同一来源校验，当前B可用、旧A未知。原5个币种／质量／少样本与前沿取值回归也通过，价格／质量／支配算法未改。原156项输入有单行逆向和共享夹具字节证明，不重跑；新增6项形成162个不同测试的有限并集，不是一次全库验收。


### M1 原生成功记账的首次样本前提

原生exact入口绕过普通HTTP成功记账；旧完成回调直接提交TPS，而既有TPS守卫按设计不创建健康条目，导致无手工预热时完整响应和后续速率都可能一直没有成功观察。buffered／stream自有回归保留0/2有效首红：一次实际POST成功，首样本及检查时间仍0。

现完成终态且审计提交成功后，[原生完成回调](../../src-tauri/src/proxy/trae/gateway.rs)使用[已知来源归属](../../src-tauri/src/proxy/health/tps_source.rs)提交成功观察与可选TPS；当前代次校验和两次更新共享来源读锁，旧代次不能创建新来源健康。缺usage仍是真实传输成功，但不补completion_tokens=0，也不生成零速率样本；已知0仍沿原参考速率守卫处理。错误状态、未完成流或审计失败不走此成功回调。

新增正常对照复用未修改的原生fixture，各模式严格1POST；既有usage未知／部分／零／完整、晚到usage、错误／EOF、审计失败及单次请求拒绝消费者均保留。原始wire、原生报告JSON、令牌限额、费用／权益与路由政策未改；健康成功不等于任务做对、VerifiedTerminal、扣费收据或真实账号就绪。仅源码／自有loopback，未实装或实际供应商验证。

## S7 TPS 参考的24小时空闲失效

用户已明确批准既有调用窗TPS参考在连续86400秒没有接受有效样本后失效。[健康登记](../../src-tauri/src/proxy/health.rs)单独保存进程内单调Instant：年龄小于86400秒仍当前，达到边界或时钟不可比较则参考未知。健康成功时间、无usage、错误／取消、缓存返回、读取、同源备注／价格保存以及S1导入的来源时间都不续期；仍须通过原正token／正耗时／有限且不超过2000tok/s守卫及当前来源owner。没有新表列、配置字段或持久化JSON，重启仍从无样本开始。

这不是滚动24小时均值或计数。保持原0.8/0.2按样本EWMA；首个失效后的有效样本在同一条目写锁内清除旧基底和计数，再累计为1，继续遵守已有可配置最小样本（默认5）。来源发布锁仍先于条目锁，两个并发新提交只重置一次；来源切换清时间，退役owner不能刷新。路由与Pareto收到None而非测得零速率；评分上下文同时捕获速率与时效元数据，排序期间跨边界只影响下一次决策。

health／解释新增可选只读tps_reference，Pareto增加speed_reference，schema_version为1；状态区分no_sample、current、expired、source_mismatch、clock_unknown，说明max_idle_seconds、可用年龄及当前配置代次样本数。来源不符不输出其他代次的年龄或样本数，也不输出指纹、凭据或内容。旧DTO缺字段时UI显示“时效信息未知”，不推断当前。能力页只展示本次读取快照，可显式重新读取；速率仍是含等待／本地开销的调用窗指标，不是原生生成速率、任务质量、权威权重版本或账单证明。

[可控时钟及自有Gateway回归](../../src-tauri/src/proxy/health/idle_age_tests.rs)保留86400秒后仍累计5的有效0/2首红，并覆盖健康／无效样本不续期、有效续期非滚动、来源与并发、未知时钟、重启和旧DTO。实际自有loopback经网关5次POST采样、缓存命中不增加POST也不刷新TPS时间、过期读侧未知、第6次POST开启新代次；配置门槛1／3／5仍有效。源码浏览器验证新状态、旧DTO、键盘与960px布局，独立于已安装版和真实供应商证据。完整S7的权威模型／运行时版本、完整条件和任务质量等余项仍保留；正式issuer与费用／权益政策未改。

## S8 排序与解释复用当次TPS参考

旧解释接口及离线请求预览原先先调用排序，再为说明重新计算cost_context。有效样本恰好在两次读取间到达／过期，可能让同一响应的序号使用有效TPS，而效率分项却显示未知。[可控时钟正反对照](../../src-tauri/src/router/cost_context_tests.rs)直接调用实际explain_candidates及GatewayState::preview_routing_request，保留两项0/2首红；稳定时钟正常对照同时通过。

[排序器](../../src-tauri/src/router/mod.rs)现在内部返回当次候选与已捕获权重、成本／TPS区间、参考及任务上下文；普通rank_with_intent保留原Vec接口，过滤、排序、priority与sticky正文沿用。旧解释与[预览](../../src-tauri/src/proxy/server_request_preview.rs)复用该上下文，活动目标调序后仍按同一快照解释；没有新公开DTO、持久化格式、评分参数、费用或准入政策。后续调用重新捕获，仍会看到真实过期／新增样本。

这只固定当次TPS／价格分钟和权重解释，不把健康／额度读取或离线预览变成资源预留、原子分派或后续请求保证。FreeOnly预览仍不推进免费选择器，也不借普通评分说明权益；正式issuer关闭。回归仅自有时钟／SQLite／loopback；不是推荐质量、权威模型版本或已安装版验收。

## N4 typed成功正文的超时分类

[typed正文读取](../../src-tauri/src/proxy/upstream.rs)原来把成功HTTP头后的JSON／二进制body读取错误统一包装为Other500，丢失reqwest超时分类。自有原始HTTP上游先给出200和一字节正文，再保持未完成；实际Gateway返回500而不是504的有效首红保留，完整200正控及400／401／429／503错误头优先对照同时通过。

现在仅两处成功正文读取调用私有map_typed_body_error：超时复用原map_reqwest_err得到Timeout504，非超时错误仍按原表达式构造Other；已知非成功头分支、JSON解析／诊断和旧重试资格未改。Timeout与原Other均沿原可重试合同，未知费用保持未知，没有新增统一NoReplay或免费／预算策略。有限额typed仍在共同wire入口按原reference-bound合同拒绝，不能借此获得新计价单位或派发权限。

[实际Gateway网络边界](../../src-tauri/tests/proxy_network_boundaries.rs)覆盖embeddings／图片／语音各自完整响应与正文期限，逐次1POST／1审计；known400／401／429／503即使诊断body超时仍保留原HTTP状态。typed取消／正常单次提交、写入故障与正常／回退／耗尽计划只作必要受影响兼容。最终7个不同测试通过，无ignored或明确早退，内部分支不额外累计；check／all-target Clippy／fmt通过。原代理热切换已有独立源码正向范围，本片不因旧台账“后续”字样重复运行，也不把CONNECT拒绝冒充TLS正向。

本片仅源码及自有回环／SQLite，不是实际公网代理、真实账号或安装版验证；普通buffered坏响应政策、深层输出语义与版本升级等原N4余项继续。无新schema／费用单位／权重／JSON合同、正式资料或系统网络修改；前端未变，不重复构建／浏览器／安装。

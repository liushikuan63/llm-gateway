# Responses 指令与 Trae 设备兼容验收

本批已完成受限 Responses instructions 行为和 Trae 官方设备 OS 有限枚举兼容的源码、完整回归、包资产、静默安装及隔离合成 IPC 验收。全部原目标 R5 和其他任务继续 active。

非空 instructions（含空白）在历史重建后只注入一次，持久化保留调用者原始增量；省略、null、空串保留旧语义。严格模式保留当前输入，超出既有校准容量时拒绝，不隐式压缩、精炼、搜索或调用辅助模型。缓存/去重私有身份隔离，同时保留旧请求字节、tools 绕过及既有计费/流错误边界。HTTP Responses 方言、流式/重试/显式压缩路径随选项传递；此前压缩丢失的历史不能恢复，previous_response_id、opaque reasoning、原生 Responses 及未映射控制仍是限制。

R3 修复严格 HTTP Responses 与实际生效模型 extra_body.instructions 的精确键存在冲突：null、空串、同值及非字符串值均在实际首选路由缓存前拒绝，后续实际 HTTP 传输亦在发送前防御；不删除或修改保存的配置。局部冲突使用固定安全字段错误400、不可重试，不回显配置值，不造成健康降级；此前已发生的真实失败尝试和其审计/健康记录保留。未使用的冲突后备路由不阻止有效首选路由，默认省略/null/空指令及其他方言保留原行为。

设备仅增加官方 windows/mac/linux 三个精确小写枚举；已有枚举、敏感头原始编码、会话来源和账号隔离保持。此修复不证明真实账号 OS、平台目录或模型调用成功，也未建立 Global 首次设备 bootstrap。

原整批复审、后续 WF1 changes-required 发现及 R3 范围修复复审均保留；R3 范围复审确认 WF1 和局部健康副作用已修复，0 未解决发现，不冒充重做无关源码语义扫描。最终冻结 365 文件与快照一致；当前 244 个 Rust 输入完整新 Cargo-v2 验证实际 1957 passed / 0 failed / 19 ignored，共 79 个顶层测试组；76 个根集成目标逐项覆盖。另有 10 条显式外部提前返回。ignored 与外部提前返回均不计真实外部流程通过；不与 R1/R2/R3/枚举或历史完整回归的重叠计数相加。严格 Clippy 使用 R3 after-health 当前输入绑定的退出0原始收据。fmt-v3/check-v2/scripts-v3/redaction-v3/plan-v3 均有当前输入绑定的退出0收据；manual-v3 是生成，manual-v4 是独立 verify:manual 检查。

Tauri package 实际 beforeBuildCommand 完成 docs:manual、release:prepare、tsc/Vite build；288 个发布输入与当前源码/冻结清单一致。最终 dist 全焦点界面夹具 226 张 PNG，退出0、stderr为空、source/dist不变。此处浏览器夹具与下述安装版 IPC 分别计数。

NSIS 静默实装退出0，MSI 管理提取退出0，各26资源和规范化 EXE一致；没有 MSI 整机安装或重启验收。安装 EXE SHA `d6047d76fc26815b0cf80df745940217a653c54ed21d8abe41e4b2cdb7ed965f`；发布输入清单 SHA `1a8425a443d3e4bf7dd347e898ca098b680ee093d94b85c6415a4c1cc4fc9948`。

安装版使用精确小写 windows 合成会话与独立数据库，真实 Tauri IPC 5项/5张 PNG；本地拒绝代理记录一个无凭据头 CONNECT，平台转发0。两次隐藏进程正常退出、端口释放、WinINet不变；private provider/model/request/runtime0、secret1。没有真实账号目录或模型成功证据。

正式资料16条密文、10份收据、密封主密钥、配置、请求记录、签到时间及物理计划逐项保持；Trae CN00:05、Qoder两区12:05，旧5自启缺失、客户端/旧桥/遗留端口/网关进程0。任何自然调度变化都会阻止本交付器，须根任务裁决，不能静默忽略。

历史原目录单次读取与后续诊断单次读取的脚本、授权、attempt、proof 均保持原字节。后续诊断仅记录1次目录 IPC、catalog_success=false、native_device_profile_unverified；没有测量实际HTTP次数，授权已消耗，禁止重试。之后官方枚举修复完成，但本阶段未再作真实目录读取。收费模型请求0，真实平台目录请求0；登录、刷新、设备、领取及真实模型能力未在本阶段验证。

全部原目标继续见[持续目标](持续目标与任务索引.md)与[原任务卡](VibeCoding任务卡-开源能力吸收.md)。

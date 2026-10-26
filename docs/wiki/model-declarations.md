# 本地能力声明：三态与配置修订

这里只是既有六项本地配置：工具、思考／推理、图像输入、音频输入、视频输入、流式。它们不是实测、权益、免费资格或质量评分。并行工具、结构化输出、音频输出及类型专用向量不由本合同新增；不改变评分权重、硬准入或 stream 策略。

## 保存与来源

[生产声明模块](../../src-tauri/src/capability_declarations.rs)与[编辑表单](../../src/pages/ModelDeclarationFields.tsx)使用 supported／unsupported／unknown。只有 supported 投影为原布尔 true；另两者都是 false，但声明元数据分别保存，不能再从 false 推导否定来源。旧 true 是旧／默认配置未核验；旧 false 仍是未确认。

显式记录三个状态都保留本地目录字段覆盖，包括同值 false；“未确认”不等于恢复自动目录管理。旧客户端不带补丁时，原值未变的声明保留，改变某个布尔只使该字段的旧声明失效。未来或损坏声明不能被伪装成新本地声明。

已有可空 `models.capability_declarations_json` 只保存版本 1 的 local_configuration 文档、目标绑定、服务端代次和每字段状态／记录时间。旧 NULL 不回填；正常配置保存可建立空信封供删除重建 ABA 防护，它不制造字段来源。布尔、元数据与目录覆盖在原事务内提交或回滚，没有额外保存者。

## 修订和适用范围

`binding` 和 `revision` 是配置保存的并发校验，不是权威模型版本，也不证明账号身份或物理后端一致。绑定包括当前供应商／端点／运行时、别名、配置上游、路径和模型类型；编辑依赖同次读取的模型及声明，过期修订、重复别名／字段、目标变化和布尔投影不符会拒绝保存。读取不生成或保存新修订。

账号型运行时或非聊天模型的本地声明标为 not_applicable／只读，不能把不适用维度计为不支持。legacy、declared、invalid 和 not_applicable 保持区分。格式、版本或绑定不符时声明只读，不能借另一模型的当前记录补证。

## 当前读回和历史

[能力事实页](../../src/pages/Capabilities.tsx)把同一 ProviderView、同一别名的声明传给[只读消费者](../../src/components/ModelCapabilityDeclaration.tsx)。有效本地声明分别显示支持、不支持、未确认；尚无声明的字段仍按旧布尔参考。非法／只读异常或与当前布尔不一致的 declared 记录显示资料不可确认，不生成来源；非聊天保留旧适用范围说明。此接线不新增请求、写操作、排序或费用推断。

[S8 计划历史](request-feedback.md)另有版本 1／2 的审计快照；其 schema_version 与上述本地声明文档版本、配置 revision、权威模型版本是不同概念。历史只展示保存时事实，不读取当前声明重算分项。旧快照缺评分保持未记录，不从当前配置回填。

[自有 SQLite 交叉测试](../../src-tauri/src/audit/route_decision/compatibility_tests.rs)通过生产配置写入、实际排序观察及原审计 INSERT，改变 supported→unknown→unsupported，再关闭／重开新建数据库，核验当前修订确实变化而同 trace 的不同请求仍保留原 v1／v2 身份、顺序、时间和分项。unknown 与 unsupported 的相同旧 false 投影不成为新的质量政策。此测试不涉及正式用户库。

[现有声明契约](../../src-tauri/tests/capability_declarations.rs)与[读回 UI 合成测试](../../scripts/s6-declaration-readback-ui.cjs)分别验证后端、源码页面；本轮复用 S8 已验的未知、字节预算及顺序合同。历史 S6 批准／收据是本地保存证据，不冒充本轮原会话独立读回；旧输入哈希未全部匹配当前树，因此当前 S6 契约已重新有限运行。浏览器夹具不等于真实账号、原生 Tauri 或安装版通过；r3 已装、r4 冻结候选不包含这次后续源码。


[本轮源码、统一清单与独立证据](../../../artifacts/Project-Enhancement-20261013/execution-schedule-20261018/multi-account-local-v1/native-rotation6-v1/priority-six-regions-v1/s6-s8-history-readback-source-v1.json)。

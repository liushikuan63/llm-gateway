# VibeCoding：Qoder 双区会话用量与平台积分

2026-10-15，沿原 S5/B3/R5 队列，先核验再实现。当前249输入完整历史包已交付；本阶段不启动Qoder/Trae/WorkBuddy、不发送模型请求、不读取第三方凭据文件。

## 已核验依据

本机 Qoder CLI 312诊断日志内8770完成事件，四项Token字段全0；两区项目历史115/35文件内31063/3299个assistant usage也全0，不能据此补真实Token。项目记录包含逐请求credits、billable与request_id；国际区22326独立请求ID、有8737重复记录，CN3299独立ID。只读字段类型及数字统计已留证据，未保留正文或原ID。

官方 [SDK cost/usage](https://docs.qoder.cn/en/cli/sdk/cost-usage) 区分逐请求、会话累计和账号余额；billable表示是否计入额度，Tokens不可直接换Credits，不能把不同口径相加。国际区实际有正credits但billable=false，故不能将所有credits当扣款。公开 MIT [qoder-token-usage](https://github.com/dgjin/qoder-token-usage) 仅静态参考：默认假定custom_model=DeepSeek-Flash、缺字段COALESCE0等不采纳。固定HEAD1a500600c3b3da374a02ea86723fbfdee44e3ce1。两区独立shared_client聊天库存在但chat_message当前0行，只读未触凭据/消息正文。

## 生产合同

1. 新增两区已知项目日志 `.qoder/projects/**/*.jsonl`、`.qoder-cn/projects/**/*.jsonl`，source=`qoder_history`、profile=`global`/`cn`。现有诊断日志来源保留并明确可能同一请求，跨来源不能直接加总。不开任意目录/配置路径，不改用户gateway.db。
2. 只处理assistant.message.usage逐请求记录，优先区域内全局request_id，缺失时仅有可证明message.id才作为单独命名空间fallback；全部原ID仅用于内存或哈希，不持久化任意文本。跨文件、fork复制、stream副本不可重算。相同ID模型/日期/金额/billable冲突保持部分说明并排除无法确认的金额；缺标识不猜计费。result累计值不计。
3. 原input/output/cache全零记录继续未知，不能当免费/未命中。未确认输入总量是否已包含缓存前，不猜输入/不产生伪精确API费；明确非零输出/缓存可展示原字段。Credits不推Token，不转换USD/CNY，不默认qfmodel/auto/custom_model对应某现金模型。
4. ClientUsageRow增加可选platform_credits，旧report默认None保持可读；字段固定：billed、not_billed、unclassified（均已知数值小计f64），billed_records、not_billed_records、unclassified_records、missing_records、conflicting_records（u64）。billable=true/false/缺失分别进对应桶；缺金额不当0，显示缺失条数；冲突排除金额并说明。逐日/范围合计同时合并，不修改原estimated_cost/currency含义。UI按来源/区域展示日志标记计费、未计费和状态未知积分，并保留“非账号余额/非最终扣款/非API现金费”的说明。
5. 快速与完整模式同时接入；完整任务白名单投影增加message.id/model/usage、timestamp等必要字段，任何content/tool/思考/凭据都跳过；原始JSON有效性、文件快照与跨进程lease继续原逻辑。旧249任务恢复兼容，不以加字段破坏旧JSON；已启动旧快照不偷偷加入新来源。
6. 安装版实测发现国际版固定日志根是用户迁往D盘的junction。允许受控解析这一已知来源根：最终目录必须仍以匹配本区域的 `.qoder/projects` 或 `.qoder-cn/projects` 结尾；禁止无关目标、错区目标、非目录及内部链接。不开任意路径参数，不改连接或搬数据。快速与完整共享判定，持久文件保持逻辑路径并由文件ID/长度/原始hash约束恢复；改指向或换文件应拒绝继续，旧快照不扩域。

## 顺序及可失败验收

- [x] 两区公开/本机数字合同与重复/冲突审计完成，先真实RED；缺金额、nonfinite/negative、缺billable、0占位、缺ID/ID命名空间、chunk/fork跨文件重复、会话result不加、区域不混及来源不相加正反例。
- [x] 核心读法、Credits聚合与完整投影；旧47+16/full lease/rawvalidity/计价对照保持，小样quick/full相同，旧serialized report兼容。
- [x] 生产页面来源/区域标签、积分可解释、未知条数、USD/CNY不混、日期筛选/曲线原逻辑、取消继续与窄屏；fixture与真实原生分开。
- [x] 独立复审、全门禁、实际安装、安装版本机两区真实数字扫描及重复不计验证，原资料/收据/两代理/定时任务/自启保护。不为验收补发收费请求或制造真实平台聊天。
- [x] 门禁附带缺陷：旧自启单元测试有两条写真实Run后未恢复。首轮误执行全lib630包含这些旧测试，不能声称没有真实注册表副作用。先用不写Run的断言建立RED，再仅在cfg(test)采用线程局部UUID测试键和自有清理；生产键不变，专项/全量后只读核对实际Run。阶段前LLMGateway基线缺失，目前物理/打包视图均未开启，不猜原值、不擅自恢复，单列证据边界。
- [x] 安装版v1真实失败保留：441文件、617595541字节，国际区根因`links_skipped`，CN35文件可读；两个自有实例正常退出、代理未变。新增迁移根受控解析的RED/正确目标GREEN、错误目标/错区/内部链接仍拒绝，改指向后原快照恢复拒绝；独立复审后新包fresh v2原生对照，不覆盖v1失败证明。

全部持续目标保持active；本阶段不承诺从0占位重建Token，也不代替未授权的CN官方网页/模型实测。更多桌面缓存、Claude等来源及价格覆盖沿原S5继续。

本阶段实际交付详见 [2026-10-15-Qoder双区日志积分验收.md](2026-10-15-Qoder双区日志积分验收.md)。其他原队列继续active。

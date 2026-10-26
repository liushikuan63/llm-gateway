# WorkBuddy 双区与 Codex 继承阶段验收

2026-10-15，新包已实际静默安装至 `D:/Program Files/LLM Gateway`，继续原持续目标；本阶段完成不代表全部目标完成。

WorkBuddy 国际版与 CN 的独立授权、加密命名空间、身份、刷新、取消、模型目录、运行时登记和模型 dispatch 按区域隔离。旧 Global key、身份和缺 region 运行时保留兼容；未知明确区域拒绝，供应商串区在模型 POST 前失败。两张区域卡同名账号可独立操作，离开页面后的取消失败保留本区显式重试入口，不自动重试，另一地区继续使用。

Codex 完整头和可靠归属的首条明确 last 用量现在可计入；已确认的父子继承前缀排除重复。父源缺失、冲突或循环不会丢弃可靠 cutoff 之后的子请求；子自身身份/时间冲突继续保持未知。无法证明的历史不估成零、不按首次累计总值收费，现有镜像去重、费用口径和读取上限保留。

| 检查 | 实际结果 |
| --- | --- |
| Codex 首条/继承专项 | 47 passed / 0 failed / 0 ignored，实际 RED→GREEN，独立限定复审通过 |
| WorkBuddy auth/transport/gateway | 53 passed / 0 failed / 0 ignored，含 CN 流式/非流式、历史/usage 与串区零 POST |
| 运行时 / 原子登记 | 21 / 11 passed，0 failed / ignored |
| 完整 Rust | 74 组，Cargo 报告 1548 passed / 0 failed / 19 ignored；10 条外部服务缺失提前返回另行列出，不算外部验收 |
| 最后测试夹具修正 | 完整回归后仅调整测试初始化及 Global test constructor 覆盖；受影响53项复验通过，生产行为未改 |
| fmt / Clippy all-targets -D warnings | 最后退出0；首轮两条测试夹具告警及失败日志保留 |
| 脚本 / 语法 / 脱敏 | 50 tests / 0 fail / 0 skipped，最终37脚本语法；129 Rust文件、11自测样本及13夹具通过 |
| 打包资产完整界面 | 退出0，158新PNG；双区8组/5图取消恢复 RED→GREEN及复审 |
| NSIS | 实际静默安装退出0，26资源匹配，原安装目录已备份 |
| MSI | 管理提取退出0，26资源匹配；未做整机 MSI 安装 |
| 安装版双区 / 代理 / 真实日志 | 实际 IPC 与生产 UI，4+5+3组/2+4+3图；四个自有实例均退出、3个测得正常退出码0，4个验收端口已释放，私有库四表均0 |

生产输入244项，清单SHA `05a2a4c8de52177dac6fb7e8dcb8ad62d216dc6aa85b909c6d2396a209d037f9`；安装EXE SHA `03b06c9057729b491e973f7c863dfc9ace5f226326d24e069e09833cfbe89b8d`，归一化EXE SHA `b9ed825a6895b25ba7db00b408da3397eb8752aee3513fc2746f1e02fef97854`。源HEAD仍为 `b2fc256f95f4e11131d0b43882b1384f4bd01e15`，保留原脏改动，不提交推送。

真实 Codex 后端读取24文件、128 MiB达到上限，状态 partial，tail_only/large_line/scan_limit/父源不完整与累计基线说明均保留；真实总量、API参考费用及费用曲线按日键盘筛选通过。仅代表已读范围，不是完整历史或账号实际扣费。完整历史分批游标仍沿 S5 继续。

官方 CN 匿名 `/v3/config` HTTP200/code0但 models:null；个人目录 HTTP401，不能确认登录后模型形状。CN 真实登录/目录/模型均未验收；官方页面打开申请已提出，未取得新许可前不打开。之前单次费用授权已消耗，不复用。全机 Windows 系统代理 setter、原生托盘点击、清理后整机重启仍未实测。

正式资料及所有原始收据保护通过，本轮正式模型请求/手动领取增加0。物理HKU五项Run与Windows启动清单均已清除，WorkBuddy/Trae及旧桥接进程/监听0，保留DPAPI备份；Trae00:05、Qoder两版12:05触发未改。WinINet完整快照旗标3及SHA `62dc1ab8e96bab8845063fe39f1408edaaa851fd57fe52575dfb60809b61090f` 前后一致。

原始证据位于父目录 `artifacts/Project-Enhancement-20261013/`：`workbuddy-regions-final-delivery-proof.json`、`workbuddy-regions-full-test-summary.json`、`workbuddy-regions-source-verification.json`、三组 `workbuddy-regions-installed-*` 及匿名目录证明。失败、原有费用收据与历史验证不覆盖。安装备份 `D:/UserData/llm-gateway-backups/installed-before-workbuddy-regions-20261015-0615`。

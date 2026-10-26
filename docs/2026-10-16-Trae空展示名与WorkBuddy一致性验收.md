# Trae 空展示名与 WorkBuddy effort 一致性验收

本阶段将明确空字符串展示名回退为已通过校验的 config_name，保留 null、非字符串、控制字符、超长等拒绝边界；与已审查的 WorkBuddy effort 一致性改动组合交付。selector、身份、限额、能力、来源筛选和代际保护保持。原目标、R5 保持 active，S6 仅引用本次验收不新建重复卡。

任务及整批源码复审 findings 均为 0。Trae 专项 77+83=160 项、WorkBuddy 隔离 full1997 均为各自证据，不相加当组合 full。新组合完整 Rust 实跑 2000/0/19，79 个根组，覆盖所有根 integration targets；嵌套组不重复计数。ignored 及 10 条外部提前返回不是实际平台成功。

新包构建退出 0，发布 292 输入与冻结生产集合一致。NSIS 静默安装 0、MSI 管理提取 0，各 26 资源与规范化 EXE 一致；未做 MSI 整机安装或重启。前端 58 文件、dist12 文件一致，226 旧 responses UI PNG 复用，本阶段未跑新浏览器。安装版合成 IPC5 项/5PNG、2 隐藏进程正常退出、端口清理、拒绝代理 0 转发；不是实际账号目录或模型成功。

正式观察 JSON 保持 18 条密文、12 份每日收据、config/master/请求/代理/自启/时区/00:05 与12:05 调度；只读 JSON，不读取正式 DB。时间序严格满足 before≤install≤probe.start≤probe.finish≤after≤pre，probe.finish 在子进程 finally 清理之后。四份旧源码已按审查升级，旧哈希由精确 before 备份与旧冻结副本保全，并映射至本轮新冻结；其他历史 immutable 与已消费授权不变。

引用的兄弟阶段14:21单次真实目录读取已经实际发起1次目录IPC，首先在 display_name 空字符串处拒绝；proof e8c624… 与执行复核仅证明局部 shape_invalid，授权已耗尽，实际wire请求总数未测量。代码修复不能当作真实目录成功。本包免费修复交付阶段新增真实目录IPC为0，不将此前实际读取抹除；免费交付全部完成后，新的真实元数据读取仍须另外明确单次授权。真实模型权限与推理未验证。沿[持续目标](持续目标与任务索引.md)和[R5主卡](VibeCoding任务卡-开源能力吸收.md)继续。

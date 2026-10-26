# 2026-10-18 WorkBuddy 双区额度交付验收

本记录描述已经完成的 M0 额度批次，不覆盖后续 Search/refusal 候选。维护说明见 [Wiki](wiki/README.md)，持续队列见[执行调度](持续目标与任务索引.md)。Wiki 是后续新增维护要求的补充，不改写原安装收据或原源码清单。

## 实际安装与输入

- HEAD：`b2fc256f95f4e11131d0b43882b1384f4bd01e15`，dirty 工作区；302 项生产源码与 12 个最终 Windows 资产逐项匹配。
- 源清单：`51a6514d1ceada99dbc9eea29bc93eecd0de08e720bf3801926f8cc104090511`。
- 已安装 EXE：`7faeb08c85cb56cf8bcad1c9018cffd1bd9b2a43606bb0783ac63f4beefa8ec8`。
- NSIS：`53c135147daa48468ae42272e1506286dd18472a014c49d6cb92a5211b58ad79`；MSI：`b07024a8bf015f4b6a223ebbd53fb33d10e643bf99e9eba27b9bf66e572ec28a`。
- 实际 NSIS 静默安装退出 0，MSI 管理提取退出 0，两个 payload 各 26 资源与规范化 EXE 匹配。未做 MSI APP 运行或 MSI 整机安装。
- 本批运行机器为用户当前 Windows x64，安装/启动由本批主线实现者执行，独立代理只读复核执行证据。准确称用户 Windows 实测；后续新增的独立实装 DoD 不能追认成这一批已经由独立执行者安装。

## 验证范围

库根结果 995/0/0，child 1 不重复加算；四个相关 target 合计 67/0/0。全目标 Clippy 0；Node 56/0/0、65 个脚本语法及 13 份夹具脱敏通过；手册 28 章节；Windows 资产浏览器夹具 237 图。

安装版新额度 IPC 门禁 2 项/2 图，一次自有空资料进程正常退出。两区 quota IPC 在总开关关闭时本地拒绝；私有 DB 的 providers/models/requests/app_secrets 均为 0，端口释放，WinInet 摘要保持。这不证明云端额度成功。

正式 18 供应商、163 模型、106 请求、27 加密行及 18 份收据、配置和主密钥摘要与安装前一致。Windows 任务定义保持，Trae 00:05、Qoder 两区 12:05。清理改用直接 Run/Startup/任务读取核验已知渠道；未做整机重启。

本批没有执行 whole Cargo suite，不能把专项或历史 2057 写成本候选全量通过。真实个人/企业额度、过期刷新、领取和 WorkBuddy HTTP400 恢复均未本批验证，没有新增外部模型、额度或领取调用。

## 证据与耗时

本机阶段证据目录为父工作区的 `artifacts/Project-Enhancement-20261013/workbuddy-native-quota-v1/`；目录本身不是仓库源码交付的前置依赖。

最终 proof SHA256：`029db2de734ad87e670f48d6e4c8fd0dd77c4358e3ec7195fb25247285d39905`；执行复审 SHA256：`b374be79ef97ccc21b00b635640968c8e26bf09726b9a9d8f9b08a665dbc695c`。交付目录 11 文件/10 checksum。

实测 release 编译 238 秒，前端构建 0.661 秒，安装及 MSI 提取 8.484 秒，新原生门禁 18.048 秒；未记录的分析或等待段不补算，也不由本批推断剩余项目工期。

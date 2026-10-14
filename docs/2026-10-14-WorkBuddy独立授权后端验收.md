# WorkBuddy 独立授权后端阶段验收

本阶段继续账号权益任务卡 R5.2/R5.3，生产调用路径为 Rust 后端和 headless CLI。只使用本软件的加密资料及固定 `https://www.workbuddy.ai` 官方发行方，日常协议请求不启动 WorkBuddy、CLI 或本机翻译服务。当前阶段尚未接入生产登录页面、自动临期刷新、Provider 模型调用或 WorkBuddy 领取；不能声称已完成全部独立接入。

## 本阶段改动

- 新增 `src-tauri/src/workbuddy_auth.rs`，使用既有 `app_secrets` 和 AES/DPAPI 体系，不加依赖或数据库表。内部凭据没有 Debug；访问/刷新令牌和服务端state不通过状态DTO返回。
- 固定官方 `platform=workbuddy-ai` 和 state/token/initial account 路径。只对11217和12151返回等待授权；不把其他业务错误当待登录。服务端没有TTL时保留unknown，客户端最大等待300秒；绝对expiry按毫秒、相对expiry按秒，缺失保持未知。
- 首次完成要求当前会话有效、账号和企业绑定一致；pending消费与原认证baseline比较/保存同一事务。取消、并发变更、等待SQLite写锁时过期以及提交前过期均不能覆盖原认证。最多32个待授权会话，由原子SQL插入限定。
- 显式refresh在发请求前持久化一次预约，失败或中断保留原pair且不重放；成功要求新access generation，通过官方 `/v2/plugin/accounts` 列表核验原uid与企业后原子保存。合法的新access可以沿用未旋转的refresh，真实到期字段照实保存。
- 反射检查在解码后的JSON字符串上执行，包括引号/反斜线、短state、重新登录的旧pair及刷新的旧/新pair。WorkBuddy、Qoder权益和Trae权益传输均禁用HTTP库内置retry；这项加固尚未进入当前已安装的Trae完整上下文版。

CLI入口为 `--workbuddy-login-start <account-id>`、`--workbuddy-login-poll <login-id>`、`--workbuddy-login-cancel <login-id>`、`--workbuddy-login-status <account-id>`、`--workbuddy-login-refresh <account-id>`，均必须指定 `--data-dir <已有完整资料目录>`。登录ID是本机生成的UUID，服务端state保留在加密pending中，不通过argv传递令牌。

## 已实际执行

父目录证据：`D:/Java/GitHub/llm-auto/artifacts/Project-Enhancement-20261013`。

| 验证 | 结果与证据 |
| --- | --- |
| 授权回归 | 17 passed / 0 failed / 0 ignored；`workbuddy-auth-final-tests.log`，退出0。包括并发刷新只发一次、账号/企业绑定、取消不恢复、有效期未知、错误/反射响应、SQLite等待期间过期、原子会话上限和原认证保护。 |
| 全量 Rust | 70组汇总，1272 passed / 0 failed / 19 ignored，退出0；`workbuddy-auth-full-tests.log`及`workbuddy-auth-test-summary.json`。ignored保留原外部环境/真实流程要求，不能算通过。 |
| 相关集成及Trae回归 | 权益19、登录上下文6、官方域名契约1、Trae直接/原生领取3，共29 passed / 0 failed / 0 ignored；两个日志 `workbuddy-benefits-regression.log`、`workbuddy-trae-regression.log`，退出0。全部是本机测试，不是再次真实领取。 |
| 静态及构建 | check、Clippy all-targets `-D warnings`、debug binary build退出0；`workbuddy-auth-final-clippy.log`、`workbuddy-auth-debug-build.log`。debug EXE SHA-256为de9b82285a3253dcb363419544a61ac5f249373f3cff860adc60aa4f45dd1d3a。 |
| 官方匿名流程 | 新后端CLI在私有资料副本上start→pending→cancel→not_logged_in退出0；`workbuddy-auth-live-proof.json`。授权网址官方域名通过、serverTTL未知、300秒等待上限、pending密文均实证；取消后自有pending/session行0。 |
| 独立复审 | `workbuddy-auth-review.md`；6条首轮/残留问题已核实修复，最终定向复查未发现新增P1/P2；审查只读，未冒充测试。源码SHA CCC289E35AF65394BEE4D1BE1F9DEC1C5B509E36F494216C2F4EDF3F686F584E。 |
| 脚本与规划 | 29脚本测试、0跳过；19语法文件、13脱敏夹具通过；10份规划文档校验通过。 |

有效失败回归保留：相同access generation接受的首红、官方account路径及转义凭据的复审首红、同账号重新登录反射旧pair首红。没有覆盖或删除失败记录。

真实匿名探针于北京时间2026-10-14 02:54执行：客户端前/过程/后均0，没有打开浏览器/客户端，没有刷新/模型/领取请求。原真实profile的providers/models/requests/app_secrets全部行的摘要，以及config、master和Trae首个00:05失败收据均未改变；安装版EXE未改变。只向WorkBuddy发送匿名授权state及只读poll，取消后私有副本授权状态清空。

## 未完成与下一步

完整用户网页同意、真实token/expiry字段、真实刷新与模型流式/非流式、工具和usage仍未实测。当前真实探针仅证明匿名授权服务可达及pending/cancel流程正确。GUI未变，本阶段没有新增浏览器截图；已安装的Trae修复版与桌面交付仍对应上一阶段，不把此debug验证称新包安装。

下一步完成WorkBuddy生产登录页面及共享认证服务，复用同一安全存储接入Provider，保持完整messages/tools/多模态/usage及预算审计边界。再推进Trae独立网页/刷新、Qoder两版设备/PAT/COSY；收费模型仅在具体适配就绪且有适用单次授权时验收。总体目标及[全部任务索引](持续目标与任务索引.md)保持active。

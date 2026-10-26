# 配置占位示例

[返回维护入口](README.md) · [完整使用手册](../使用手册.md)

下面只说明配置语义，供隔离测试使用。2026-10-18 起点的 M0 安装版 `7faeb08c…` 当时尚未包含新搜索后端；后续显式后端选择已有独立实装证据，见[搜索边界](search.md)。本轮查询复用仍只是源码增量，不向当前正式配置写入新选项或期待旧包具备该功能。正式配置保留，由用户通过对应版本界面选择。

```toml
# 不包含任何凭据；不要覆盖现有正式配置。
[proxy_control]
start_gateway_on_launch = false
start_system_proxy_on_launch = false

[search]
enabled = false
backend = "free_aggregate"
max_results = 5
timeout_ms = 3000
inject_as = "user"
# 以下是本轮源码新增项；旧配置缺省时关闭，不包含凭据或数据表迁移。
query_cache_ttl_secs = 0
coalesce_queries = false
# M3 v2: explicit persisted opt-in; excerpts remain request-local.
page_text_enabled = false
```

旧后端值为 `tavily`、`brave`、`sear_xng`、`bing_cn`、`duck_duck_go`。展示标签不等于 serde 值；例如不能用 `duckduckgo` 替代 `duck_duck_go`。新候选不得改变旧配置默认。

查询复用只作用于显式 `free_aggregate`，TTL0关闭缓存、最大3600秒，单飞独立默认关闭；内存结果／在途项／计数不跨重启，设置本身可保存。旧搜索设置输入没有新字段时继承当前值，而不是暗中重置。缓存不是实时查询或收费／免费资格证明；“搜索一次”诊断保持新检索语义。新旧包实体升级／回退尚未做本增量验收。

API Key 通过已有设置接口加密存库，示例及 Wiki 不放访问令牌、刷新令牌、主密钥、Cookie 或真实账号资料。配置命令和截图也不能回显它们。

双区独立登录各自绑定本机账号名称和主体；地区不是可随意改写的普通标签。WorkBuddy 国际版和 CN 可以使用同样的本机名称，但权益账号 ID 必须独立。操作步骤见[客户端独立聊天指引](../客户端独立聊天配置与当前限制.md)。

Trae 00:05、Qoder 两区 12:05 仍以当前已保存设置与 Windows 任务为准；示例不新增、不覆盖领取账号或定时计划。

## 恢复快照的原子接收（V3）

启动加载现有恢复记录时，restore／before／可选owned先按既有大小、NUL及flags规则全部校验，再在同一内存锁内发布；任一非法快照不改变已知恢复／归属。旧仅restore格式仍兼容，owned缺省保持原语义；记录格式、系统代理写入与安全权限不变。本轮3个恢复用例、30个内存管理用例和4个持久化用例为37项有限源码回归，原1/2首红与错误过滤的0测试收据均保留，后者不算通过。未操作真实WinInet／正式恢复文件，不是异常断电或实体安装验收。

公开结果正文的当前源码合同见[正文首片](search.md#可选公开结果正文：m3-v2-首片（2026-10-21-源码）)。新开关由专用搜索设置命令拥有；旧输入或完整快照省略／携带旧值均不覆盖当前开关，避免无意外发。显式开启、关闭用当前搜索设置页；正文不走现有搜索代理的隐式直连回退。本轮不向正式配置写入。

M5后续已补普通`update_config`生产命令的MockRuntime／自有DB正控，不只直接调用提交helper：旧false与缺字段保留最新已启用正文开关，专用命令关闭后旧true也不能复活；回复、两份内存配置与独立TOML读回一致。fixture权益前后均APP／关闭／空账号，task_change=false直接本地提交，不注册系统任务；监听始终未启动。此1项及Clippy／fmt为源码限定验收，未执行正式库／真实领取／新包；原加载首错与全部历史边界仍保留。

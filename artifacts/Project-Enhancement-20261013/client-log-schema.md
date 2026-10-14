# 客户端用量日志：最小脱敏 schema 笔记

核对日期：2026-10-13（Asia/Shanghai）。本文件只保存字段形状、有限样本计数、实现判据和来源索引，不保存真实日志、提示词、回复、会话/响应 ID、账户、凭据或私人项目名。调查不发起模型请求，不修改客户端文件或进程。

## 支持范围与统一口径

首批支持 Codex 新用量事件和旧累计事件回退、DSH stable/beta 的每日用量账本、Qoder CLI 的完成事件。三家来源分别报告，DSH stable 与 beta 不自动合并；客户端侧与网关侧可能观察到同一次调用，也不得直接相加称为全局唯一消费。

统一 `input_tokens` 表示含缓存读取/写入的总输入；`cache_read_tokens` / `cache_write_tokens` 为其中的子项，不能再次与总输入相加算总 tokens。`output_tokens` 已含推理输出的客户端不再加 reasoning。缺失或未提供的字段用 null；混合已知/未知时保留已知小计并列入 `partial_fields`，界面显示“≥N，部分未报告”。日志 token 不等于供应商账单，不从全零或缺失值推导 0 成本。

最小 DTO 建议：

```text
report {
  generated_at,
  sources: [{ source, profile, status, files_scanned, records_scanned,
              skipped_records, notes /* 固定枚举，不含原始路径/异常 */ }],
  rows: [{ source, profile, model,
           granularity: request | daily_aggregate | cumulative_delta,
           day,
           requests: number | null,
           input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
           partial_fields: /* 上述四项字段名的白名单 */,
           known_usage_records, unknown_usage_records,
           cache_hit_requests, cache_miss_requests, cache_unknown_requests }]
}
```

`known_usage_records` 是可观察记录数，不保证等于请求数。DSH 一个日账本桶不是一次请求。旧累计增量也不能假定等于一请求。请求命中/未命中/未知计数无足够细节时一律 null，不能从累计 cache tokens 推算请求命中率。

## Codex

有限目录：`%USERPROFILE%/.codex/sessions/2026/10/13/*.jsonl`。只抽查最近三份文件；单文件最多读取末尾 2 MiB。按另一轮字段核对时，三份文件分别含 47、25、22 条新用量记录及 48、25、22 条旧用量事件；日志仍可能追加，以上是当次快照计数。

新事件形状：

```text
{
  timestamp: RFC3339,
  ordinal: integer,
  type: "token_usage_record",
  payload: {
    thread_id, turn_id, session_id, root_turn_id, response_id,
    usage: {
      input_tokens, cached_input_tokens, cache_write_input_tokens,
      output_tokens, reasoning_output_tokens, total_tokens
    },
    turn_token_usage: /* 累计 */,
    thread_token_usage: /* 累计 */
  }
}
```

这三份样本中，response_id 全部非空且文件内唯一；`sum(usage)` 与最后的 `thread_token_usage` 各字段完全一致。94 条新 usage 都能匹配旧 `token_count.info.last_token_usage`；全部满足 `total_tokens = input_tokens + output_tokens`、`cached_input_tokens <= input_tokens`。因此优先只统计新事件 `payload.usage`，同一 response_id 去重，不能再加入累计字段或旧镜像事件。

新事件本身没有 model；`turn_context.payload` 含 `turn_id`、`model`。样本中新事件 turn_id 都能匹配对应 context。用相同 turn_id 的上下文归属模型，不读取/保留其中 cwd、workspace_roots、提示信息；缺上下文时模型为 unknown，不能猜当前配置的模型。`timestamp` 本条自带；ordinal 可作为同文件稳定顺序信息。

旧事件：`type=event_msg`、`payload.type=token_count`，字段为 `payload.info.total_token_usage` / `last_token_usage`，另有 model_context_window。rate_limits 与账号额度不用于 token 聚合。新事件存在的会话范围内不叠加旧事件；只有旧格式时按相邻累计快照求非负增量，重复累计值不计数，累计重置/缺基线应明确 partial，不将累计总量全压到一个请求。旧格式 `requests=null`、`granularity=cumulative_delta`。

官方资料仅确认 token 类型有 total/input/cached_input/output/reasoning_output；未在检索到的官方页面中找到本机新增 `token_usage_record` 的持久化协议承诺。上述新旧对应、模型关联和累计性质由本机字段与数值一致性核对支持，不冒充稳定官方接口：
[OpenAI 官方 turn/token usage 说明](https://learn.chatgpt.com/docs/config-file/config-advanced#turn-and-tool-activity)。

## DSH stable / beta

首批读取两个固定、各自独立的文件：

- `%USERPROFILE%/.dsh/dsh-usage/usage-ledger.json`。
- `%USERPROFILE%/.dsh-beta/dsh-usage/usage-ledger.json`。

不要读取同目录 `provider-snapshots.json`：它是账户/额度状态而不是本功能需要的模型 token 账本，包含敏感配置字段。

```text
{
  version: 1,
  days: {
    "YYYY-MM-DD": {
      "<provider>": {
        "<model>": {
          inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens,
          reasoningTokens, calls, cost
        }
      }
    }
  }
}
```

stable 样本为 4 个日期 / 5 个 provider-model-date 桶；beta 为 2 个日期 / 2 个桶。所有样本桶有 calls 和四项 token 字段。该文件是快照，重复刷新必须替换读取结果，不能 append/add；同一源的键为 `(day,provider,model)`，跨 profile 保留来源。calls 才是账本报告的调用次数。cache 命中/未命中/未知请求数三项为 null，原因是日账本没有逐请求粒度。

输入口径证据：本机安装的官方 DSH Desktop 0.2.0-rc.2 中，`app.asar!/dsh/node_modules/@deepseek-ai/dsh-token-meter/lib/types/usage-projection.js:14-18` 明确把 usage.inputTokens 映射为 uncachedInputTokens；该文件 :57-58 将 prompt-side 数量定义为 inputTokens + cacheReadTokens + cacheWriteTokens。`turn-usage.js:21-35` 采用相同求和，:94 将汇总 inputTokens 命名为 uncachedInputTokens。故统一总输入需相加三项，不能把 ledger.inputTokens 当已含 cache 的数。

边界：已确认核心用量字段口径，但在限定检查的当前安装包入口、相关 token-meter 包及个人插件入口中未定位到 usage-ledger 的直接写入函数。账本只有 version/days，没有 timezone，也没有事件 ID；**写入时区、账本生成器是否曾重放/重复累计尚未证实**。首批按账本日期原样查询/显示“源账本日期”，不进行 UTC 转日或拆分部分日；若 UI 统一用本机自然日，应标注日期边界来自源账本、时区尚未确认，不能声称已证实 Asia/Shanghai 写入。按当前本机时区解释应作为明确假设，不能作为已验证事实。

额外格式观察（不纳入首批实现）：会话文件为 `sessions/<workspace>/<session>/session.v4.jsonl.zstd`，由串接的 zstd frames 组成，不是明文 JSONL。普通一次性 zstd 解压只取得首 frame。本次只有限验证少量样本格式；首批不继续读取会话正文、不加 zstd 依赖。内部 `assistant/message.data.usage` 也是 input 不含 cache 的字段形状；相同 turn/step 的替换与 retry 有专门 fold，不能未来简单逐行加总。相关官方本地实现 `usage-projection.js:81-110` 对替换槽作 subtract previous + add next，retry-started 关闭替换槽。

## Qoder CLI

固定目录形状：`%USERPROFILE%/.qoder/logs/sessions/<encoded-project>/<session>/segments/*.jsonl`。不把 encoded-project 或完整路径放入 UI/报告。

最近三份很小的 segment 只有启动/阶段事件，没有 token；另只抽查三份最近且文件体积超过 50 KiB 的 segment（每份最多末尾 2 MiB），分别观察到 20、88、37 个 `model.response.completed`。

```text
{
  ts: RFC3339,
  seq: integer,
  level,
  type: "model.response.completed",
  turn_id, loop_id, request_id,
  data: {
    request_index, provider, model, stop_reason, content_block_count,
    input_tokens, output_tokens,
    cache_read_input_tokens, cache_creation_input_tokens
  }
}
```

145 个完成事件都有 `content_block_count > 0`，四项 token 却全部为 0。本机此数据只能说明模型请求已完成、日志未提供有效用量；四项 tokens=null、unknown_usage_records 增加，不算真实 0 tokens、0 成本或 cache miss。model 是客户端记录的别名 fast/auto，不宣称是真实上游模型身份。

三个样本内 request_id 和 seq 均分别唯一；request_index 会在 turn 间重复（20/88/37 完成记录分别仅 19/86/34 个不同 index），不能仅靠 request_index 去重。优先 `(profile,session,request_id)`；无 request_id 时用 `(session,segment,seq)` 并标注降级。`turn.finished` 同时带 token 字段，是另一个汇总层，不能再加一遍。

官方 Qoder cost-usage 页面本次取回失败；没有证实非零情况下 input 是否已含 cache，首批全零只能按 unknown。未来有真实非零样本或官方协议依据后再开放输入归一化。不得为获取证据额外发送模型请求。

## 首批回归判据

1. Codex 新记录与旧镜像同时存在只计一次；response_id 重复不加；不同 response_id 但 token 数值相同仍计两次；跨 turn 模型归属正确；缺 context => unknown；旧累计相同/增长/重置分别覆盖。
2. DSH 读取同快照两次结果相同；两个 profile 同 provider/model/day 不互相覆盖；总输入加 cache 各一次；calls 保留；三个请求命中计数为 null；日范围按源日期处理，拒绝把部分日切成精确小时。
3. Qoder 完成事件且四项全零 => unknown，不能 0 命中率；同 request_index 不同 turn/request_id 不去错重；turn.finished 不重复计量。
4. 不完整末行、非法数字、负数、溢出、超大文件/行、不可读目录、空目录独立报告，不能崩溃/伪造 0；限制单文件和总扫描预算，源文件只读；输出只含固定来源标签/模型/计数，不含消息/账户/文件路径/原始异常。

## 与本次其他卡的边界

诊断卡：Xray 必须新增无副作用快照，不可复用会 reap 子进程/删除临时配置/解密节点的 `status()`。OwnedProcess 可用持有 handle 的零等待只读活性检查；Xray gate/settings/child 任一 try_lock 忙则 None。匹配代理端点和运行态必须来自同一内核；同端口多重匹配视为归属未知，不能用任一内核存活为另一端口背书。保留 `collect(cfg,gateway,vpn)` 为旧入口 wrapper，新增多运行时入口，避免破坏现有固定 report schema。

C1 公共 CLI runner 的非零失败仅返回固定类别和退出码。参考 `src-tauri/src/agent_upstream/mod.rs:412-429`：stdout/stderr 可包含凭据或完整提示词，只用于内存中分类，不直接进入 IPC/日志；截断不等于脱敏。命令显示值也应用固定工具名，避免把用户可执行路径当作错误前缀。成功输出仍维持现有业务契约，但新增失败回归必须含 stdout/stderr 假密钥并断言均不外泄。

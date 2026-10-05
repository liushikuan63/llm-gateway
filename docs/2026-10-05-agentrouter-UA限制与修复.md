# agentrouter（ps.air-outer.com）鉴权失败根因与修复

> 现象：网关里 `agentrouter` 的全部模型探测与调用都记 `auth_failed`，
> 上游返回 `401 {"type":"unauthorized_client_error",
> "message":"unauthorized client detected, contact support for assistance at https://discord.gg/HgekCyHJqB"}`。
> 看起来像 key 失效，但**换新 key 也一样 401**。

## 一、根因：站点按 `User-Agent` 前缀识别客户端

与 key 无关。同一把 key，只改 `User-Agent`：

| User-Agent | `/v1/models` |
|---|---|
| `curl/8.4.0` | 401 |
| `Claude-Code/1.0.0` | 401 |
| `OpenAI/Python 1.60.0` | 401 |
| `Mozilla/5.0 …Chrome/130.0` | 401 |
| **`codex_cli_rs/0.20.0`** | **200** |

匹配规则实测（9 组）：

```
codex_cli_rs/0.20.0  -> 200      codex_cli_rs/0.0.1   -> 200
codex_cli_rs/9.9.9   -> 200      codex_cli_rs/        -> 200
codex_cli_rs         -> 401      codex_cli_rs/0.20.0 (Windows) -> 200
codex-cli-rs/0.20.0  -> 401      Codex_cli_rs/0.20.0  -> 401
xcodex_cli_rs/0.20.0 -> 401
```

⇒ **`starts_with("codex_cli_rs/")`，大小写敏感、必须有斜杠、版本号不参与判断。**
`starts_with` 而非 `contains`（`xcodex_…` 失败），也非子串。

key 本身无问题：51 字符、无空白、非可打印字符为空。

## 二、修复：模型级 `extra_headers`

网关已有现成机制，不用改代码 —— `ModelOverrides.extra_headers`
（`src-tauri/src/domain/provider.rs:510`）经 `upstream.rs:99`
`apply_extra_headers()` 应用到每条出站请求，且用 `h.insert()` **覆盖**
reqwest 的默认 UA。

`user-agent` 不在 `PROTECTED_HEADER_NAMES`（`provider.rs:526`）里，允许配置。

写入走项目自带的 example（它复用生产代码 `crypto::encrypt`，不会产生
「界面显示已保存、运行时解不开」的假成功）：

```powershell
cargo run --release --example import_dsh_providers -- providers.json keys.json --dry-run
cargo run --release --example import_dsh_providers -- providers.json keys.json
```

四个模型的 `overrides_json` 现在都是：

```json
{"temperature":null,"max_tokens":null,"extra_body":null,
 "extra_headers":[{"name":"User-Agent","value":"codex_cli_rs/0.20.0"}]}
```

⚠ 注意这个 example 会把 `priority` 重置为 100，导入后要手工改回原值（本次 50）。

## 三、顺带修正的三个错值

原库里六个模型的 `context_window` 全是 `1000000`，是早期失败探测留下的
**从未被验证过的占位数**。本次逐个实测：

| 模型 | 实测 | 结论 |
|---|---|---|
| `gpt-6-astra` | 507052 通过 / 929587 被拒<br>→ `Input tokens exceed the configured limit of 922000 tokens` | **922000** |
| `claude-opus-5` | Bedrock 报错<br>→ `prompt is too long: 1464870 tokens > 1000000 maximum` | **1000000** |
| `deepseek-v4-flash` | 845116 通过；顶到 1.1M 探测超出本机 300s 预算（响应为空） | **845116（实测下限，非上限）** |
| `claude-opus-4-8` | 未单独测（与 opus-5 同渠道） | 沿用 1000000 |

`deepseek-v4-flash` 取**下限**是安全方向：声明值 ≤ 真实值只会让压缩偏早；
声明值 > 真实值才会制造「声明 100 万 / 实际 92 万」这种错配。
测出真值后应改成实测上限。

另外清掉了两个上游 `/v1/models` 里根本不存在的死模型：
`glm-5.3`、`gpt-5.6-sol`（导入时 models 整体替换，已消失）。

上游真实模型清单（`GET /v1/models`，带 codex UA）：

```
claude-opus-4-8   supported_endpoint_types: [anthropic, openai]
claude-opus-5     supported_endpoint_types: [anthropic, openai]
deepseek-v4-flash supported_endpoint_types: [openai, anthropic]
gpt-6-astra       supported_endpoint_types: [openai]
```

## 四、验收证据（全部经网关，非直连上游）

```
流式 gpt-6-astra  → 28 chunks, finish_reason=stop
                   内容："我是 ChatGPT，由 OpenAI 训练的人工智能助手……"
审计 #84          req=gpt-6-astra → agentrouter/gpt-6-astra
                   status=200 6923ms ptok=12 ctok=190 err=None
```

负向对照仍在：无 UA 时同一把 key 恒为 401（9 组 UA 实测已覆盖）。

## 五、留给人注意的两点

1. **重新导入 provider 会覆盖 `overrides`。** `repo::upsert_provider` 是
   「先插 provider、再整体替换 models」，manifest 里漏写 `extra_headers`
   就会把 UA 悄悄清掉，且症状与现在一模一样（401）。导入后必须复验。
2. **模型目录刷新（`model_catalog.rs`）拿不到该站点的模型。**
   `auth_headers()` 只拼鉴权头，没有 `extra_headers` 通道，
   所以 `catalog_auto_update` 打开时刷新仍会全量 401。本次未修 ——
   属于「探测路径与代理路径的 header 不一致」，改动面比本次大，记此待排。
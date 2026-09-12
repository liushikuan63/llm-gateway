// Isolated browser regression: the IPC fixture never reaches the user's gateway or providers.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { pathToFileURL } = require("node:url");
const { chromium } = require(process.env.LLMGW_PLAYWRIGHT_PATH || "playwright");
const output = process.env.LLMGW_UI_OUTPUT || path.join(os.tmpdir(), "llm-gateway-ui");
const baseUrl = process.env.LLMGW_UI_URL || "http://127.0.0.1:5173";

async function fixture({ empty = false, configFailure = false, providerFailure = false } = {}) {
  window.isTauri = true;
  const model = (id, context = 32768, price = null, extra = {}) => ({ alias: id, upstream: id, context_window: context, supports_tools: true, supports_vision: false, supports_audio: false, supports_video: false, supports_stream: true, price, overrides: null, ...extra });
  // 带峰谷价的模型：谷时（UTC 16:30–00:30）打五折，用于验证时段规则的往返保存。
  const peakValleyPrice = { prompt: 2, completion: 8, currency: "cny", tiers: [], source: "manual", rules: [
    { label: "谷时", start_minute: 990, end_minute: 30, prompt_multiplier: 0.5, completion_multiplier: 0.25 },
  ] };
  const provider = (id, name, dialect, url, models) => ({ id, name, dialect, base_url: url, api_key_masked: "已保存", models, enabled: true, priority: 10, rpm_limit: 0, intelligence: 70, note: null, is_active: false, health: { health: "healthy", success_rate: 1, avg_latency_ms: 100 } });
  window.__fixtureProviders = [
    { ...provider("openrouter", "OpenRouter", "openai", "https://openrouter.ai/api/v1", [model("openrouter/free", 131072), model("my-chat")]), is_active: true },
    provider("anthropic", "Anthropic", "anthropic", "https://api.anthropic.com/v1", [model("claude-sonnet", 200000)]),
    provider("ollama", "本地 Ollama", "ollama", "http://localhost:11434", [model("qwen-local")]),
    provider("multimodal", "多模态服务", "openai", "https://example.test/v1", [model("vision-model", 65536, peakValleyPrice, { supports_vision: true, supports_audio: true, supports_video: true })]),
    { ...provider("disabled", "备用服务", "openai", "https://example.test/v1", [model("backup-chat")]), enabled: false },
  ];
  window.__fixtureConfig = { bind: "127.0.0.1", port: 15721, allow_lan: false, unified_key: "fixture-only", routing_strategy: "balanced", custom_rules: [], max_fallback_attempts: 3, upstream_timeout_secs: 90, sticky_ttl_secs: 1800, compact_threshold_tokens: 60000, compact_keep_recent: 12, analytics_retention_days: 30, log_request_body: false, http_proxy: null, failover_enabled: true, catalog_auto_update: false, catalog_feed_url: null, remote_mode: { enabled: false, public_url: null }, takeover: { claude_code: false, codex: false, gemini_cli: false } };
  window.__fixtureSaved = [];
  if (empty) window.__fixtureProviders = [];
  window.__fixtureCalls = [];
  // CLI 检测夹具：一个已安装且可更新、一个已安装且最新、一个未安装。
  const cliTool = (id, label, npmPackage, installed, version, latest) => ({
    id, label, npm_package: npmPackage, installed,
    path: installed ? `C:/Users/fixture/AppData/Roaming/npm/${id}.cmd` : null,
    version, install_command: `npm install -g ${npmPackage}@latest`,
    latest_version: latest, update_available: installed && version !== null && latest !== null && version !== latest,
    check_error: null,
  });
  window.__fixtureCliTools = [
    cliTool("claude_code", "Claude Code", "@anthropic-ai/claude-code", true, "2.0.0", null),
    cliTool("codex", "Codex CLI", "@openai/codex", true, "0.44.0", null),
    cliTool("gemini_cli", "Gemini CLI", "@google/gemini-cli", false, null, null),
  ];
  window.__fixtureCliToolsWithUpdates = [
    { ...cliTool("claude_code", "Claude Code", "@anthropic-ai/claude-code", true, "2.0.0", "2.1.97"), update_available: true },
    { ...cliTool("codex", "Codex CLI", "@openai/codex", true, "0.44.0", "0.44.0"), update_available: false },
    { ...cliTool("gemini_cli", "Gemini CLI", "@google/gemini-cli", false, null, null), check_error: null },
  ];
  // 用量页夹具：一条含两次降级尝试的成功请求，一条 429 全败请求，一条未计价请求。
  // 金额刻意不同币种，验证界面分币种展示而不是相加。
  window.__fixtureStats = {
    window: "24h", total_requests: 3, today_requests: 3, success_rate: 2 / 3,
    avg_latency_ms: 320, total_fallbacks: 2, fallback_request_count: 2, fallback_rate: 2 / 3,
    total_prompt_tokens: 1234, total_completion_tokens: 567,
    provider_distribution: [
      { provider_id: "openrouter", provider: "OpenRouter", requests: 2, successful_requests: 2, prompt_tokens: 1200, completion_tokens: 500, fallback_attempts: 1 },
    ],
    spend: {
      today: [{ currency: "usd", cost: 0.0123, requests: 1 }],
      days7: [{ currency: "usd", cost: 0.0123, requests: 1 }, { currency: "cny", cost: 1.5, requests: 1 }],
      days30: [{ currency: "usd", cost: 0.0123, requests: 1 }, { currency: "cny", cost: 1.5, requests: 1 }],
      unpriced_requests_30d: 1,
      daily: [{ day: "2026-09-12", currency: "usd", cost: 0.0123, requests: 1 }],
      by_provider: [{ provider_id: "openrouter", provider: "OpenRouter", model: null, currency: "usd", cost: 0.0123, requests: 1, prompt_tokens: 1000, completion_tokens: 200 }],
      by_model: [{ provider_id: "openrouter", provider: "OpenRouter", model: "vendor/chat:free", currency: "usd", cost: 0.0123, requests: 1, prompt_tokens: 1000, completion_tokens: 200 }],
      note: "按模型配置的价格 × 实际 token 本地估算，按 UTC 自然日聚合；不是上游账单，请以厂商账单为准。",
    },
  };
  window.__fixtureRequests = [
    { ts: 1789000000, client: "local-unified-key", requested_model: "auto", routed_provider: "openrouter", routed_model: "vendor/chat:free",
      status: 200, latency_ms: 410, prompt_tokens: 1000, completion_tokens: 200, fallback_attempts: 1, error: null,
      cost: 0.0123, currency: "usd", rate_label: "谷时 · 输入≥272K 档", estimated_prompt_tokens: 1450,
      attempts: [
        { provider_id: "bad", provider: "限流服务", model: "vendor/chat:free", status: 429, reason: "上游返回 HTTP 429: rate limit exceeded for this key", latency_ms: 130, ok: false, retryable: true },
        { provider_id: "openrouter", provider: "OpenRouter", model: "vendor/chat:free", status: null, reason: null, latency_ms: 280, ok: true, retryable: false },
      ] },
    { ts: 1788999000, client: "local-unified-key", requested_model: "auto", routed_provider: null, routed_model: null,
      status: 401, latency_ms: 90, prompt_tokens: 0, completion_tokens: 0, fallback_attempts: 0, error: "auth_failed",
      cost: null, currency: null, rate_label: null, estimated_prompt_tokens: null,
      attempts: [
        { provider_id: "expired", provider: "失效密钥服务", model: "vendor/chat:free", status: 401, reason: "上游返回 HTTP 401: invalid api key", latency_ms: 90, ok: false, retryable: false },
      ] },
    { ts: 1788998000, client: "local-unified-key", requested_model: "auto", routed_provider: "ollama", routed_model: "qwen-local",
      status: 200, latency_ms: 220, prompt_tokens: 234, completion_tokens: 67, fallback_attempts: 0, error: null,
      cost: null, currency: null, rate_label: null, estimated_prompt_tokens: 210, attempts: null },
  ];
  const session = (id, title, compactCount = 0) => ({ id, title, snapshot_id: compactCount ? "snapshot-fixture" : null, sticky_provider_id: "openrouter", sticky_model: "vendor/chat:free", sticky_expires_at: null, total_tokens: 24680, compact_count: compactCount, summary: compactCount ? "任务目标：完善通用网关的会话界面。\n已完成：保留上下文、工具交换与降级处理。\n下一步：验证长文本排版和快速切换。" : null, created_at: "2026-09-10T08:00:00Z", updated_at: "2026-09-10T08:30:00Z", message_count: 4 });
  window.__fixtureSessions = [session("session-slow", "通用网关长上下文与工具调用验收", 2), session("session-fast", "快速切换验证会话"), session("session-empty", "空会话")];
  const message = (id, role, content, compacted = false) => ({ id, session_id: "session-slow", role, content, tool_calls: null, tool_call_id: null, name: null, routed_provider: role === "assistant" ? "OpenRouter" : null, routed_model: role === "assistant" ? "vendor/chat:free" : null, compacted, prompt_tokens: 2000, completion_tokens: 500, created_at: "2026-09-10T08:10:00Z" });
  window.__fixtureMessages = [
    message(1, "user", "请保留任务目标和已确认的配置，并优化这个较长的上下文页面。".repeat(20), true),
    { ...message(2, "assistant", "开始检查配置文件。", true), tool_calls: JSON.stringify([{ id: "call-fixture", type: "function", function: { name: "read_config", arguments: '{"path":"example/config.toml"}' } }]) },
    { ...message(3, "tool", "读取结果：配置有效。\n" + "路径/".repeat(140), true), tool_call_id: "call-fixture", name: "read_config" },
    message(4, "assistant", "这里是最新的完整答复。\n\n" + "长文本应保留换行，在有限宽度内自然换行；工具调用记录可单独展开。\n".repeat(24)),
  ];
  window.__TAURI_INTERNALS__ = { invoke: async (cmd, args) => {
    window.__fixtureCalls.push(cmd);
    switch (cmd) {
      case "list_providers": if (providerFailure) throw new Error("模拟供应商读取失败"); return structuredClone(window.__fixtureProviders);
      case "get_config": if (configFailure) throw new Error("模拟配置读取失败"); return structuredClone(window.__fixtureConfig);
      case "discover_provider_models":
        if (args.input.base_url.includes("broken")) throw new Error("上游返回 HTTP 401，请检查密钥权限");
        return { base_url: "https://example.test/v1", warnings: [], models: [
          { id: "sample/chat", name: "Sample Chat", context_window: 131072, context_source: "provider", supports_tools: true, supports_vision: true, supports_audio: false, supports_video: true, supports_stream: true, is_free: true,
            price: { prompt: 1.5, completion: 6, currency: "usd", source: "catalog", rules: [], tiers: [{ min_prompt_tokens: 272000, prompt: 3, completion: 12 }] } },
          { id: "unknown/chat", name: "Unknown Chat", context_window: 32768, context_source: "default", supports_tools: null, supports_vision: null, supports_audio: null, supports_video: null, supports_stream: null, is_free: false, price: null },
          { id: "sample/reasoner", name: "Sample Reasoner", context_window: 65536, context_source: "provider", supports_tools: true, supports_vision: false, supports_audio: true, supports_video: false, supports_stream: true, is_free: true,
            price: { prompt: 0.5, completion: 1.5, currency: "usd", source: "catalog", rules: [], tiers: [] } },
        ] };
      case "upsert_provider": {
        const input = structuredClone(args.input);
        window.__fixtureSaved.push(input);
        const next = { ...input, id: input.id || "fixture-new", api_key: undefined, api_key_masked: "已保存", is_active: false };
        window.__fixtureProviders = [...window.__fixtureProviders.filter(p => p.id !== next.id), next]; return next.id;
      }
      case "update_config": window.__fixtureConfig = structuredClone(args.cfg); return { config: args.cfg, restart_required: false, restart_reasons: [] };
      case "test_provider": return { ok: true, latency_ms: 42, model: "sample/chat" };
      case "list_snapshots": case "list_remote_access_keys": return [];
      case "recent_requests": return structuredClone(window.__fixtureRequests);
      case "stats_overview": return structuredClone(window.__fixtureStats);
      case "import_bundle": {
        if (args.src.includes("broken")) throw new Error("该目录里没有 config.toml 或 gateway.db，不是导出包");
        return { result: { providers_imported: 2, models_imported: 5, providers_missing_key: ["异地服务"], config_imported: true, preserved_security_fields: ["统一访问 Key", "远程 HTTPS 模式"] }, backup_dir: "C:/fixture/backup-20260912-010203" };
      }
      case "export_bundle": return null;
      case "plugin:dialog|open": return "C:/fixture/bundle";
      case "pricing_status": return window.__fixturePricingStatus;
      case "refresh_pricing":
        window.__fixturePricingStatus = { at: "2026-09-12T02:00:00Z", manual: true, feed_url: "https://openrouter.ai/api/v1/models", feed_models: 443, updated: 2, skipped_manual: 1, unmatched: 1 };
        window.__fixtureProviders = window.__fixtureProviders.map(p => p.id === "anthropic"
          ? { ...p, models: p.models.map(m => ({ ...m, price: { prompt: 3, completion: 15, currency: "usd", tiers: [], rules: [], source: "catalog" } })) }
          : p);
        return { feed_models: 443, updated: [{ provider_id: "anthropic", provider: "Anthropic", alias: "claude-sonnet", prompt: 3, completion: 15, currency: "usd", tiers: 0 }], skipped_manual: 1, unmatched: [{ provider_id: "ollama", provider: "本地 Ollama", alias: "qwen-local" }], feed_url: "https://openrouter.ai/api/v1/models" };
      case "list_token_calibrations": return [
        { provider_id: "openrouter", model: "vendor/chat:free", samples: 12, ratio: 1.35, updated_at: "2026-09-12T01:30:00Z" },
        { provider_id: "ollama", model: "qwen-local", samples: 3, ratio: 0.92, updated_at: "2026-09-11T22:10:00Z" },
      ];
      case "clear_token_calibrations": window.__fixtureCalibrationsCleared = true; return 2;
      case "detect_cli_tools":
        if (window.__fixtureUpdatedTool === "claude_code") {
          // 更新后重新检测必须反映新版本，否则界面会显示"更新成功但仍提示可更新"。
          return structuredClone(window.__fixtureCliTools).map(item => item.id === "claude_code"
            ? { ...item, version: "2.1.97", latest_version: "2.1.97", update_available: false }
            : item);
        }
        return structuredClone(window.__fixtureCliTools);
      case "detect_cli_tools_with_updates": return structuredClone(window.__fixtureCliToolsWithUpdates);
      case "update_cli_tool": window.__fixtureUpdatedTool = args.id; return "added 1 package in 2s";
      case "run_gateway_self_check": if (window.__fixtureSelfCheckFails) return { healthy: false, base_url: "http://127.0.0.1:15721", routed_via: null, latency_ms: 12, error: "网关返回 HTTP 503：所有候选 Provider 均不可用（尝试 2 次）" };
        return { healthy: true, base_url: "http://127.0.0.1:15721", routed_via: "openrouter/vendor/chat:free", latency_ms: 321, error: null };
      case "list_sessions": return structuredClone(window.__fixtureSessions);
      case "get_session_messages":
        await new Promise(resolve => setTimeout(resolve, args.id === "session-slow" ? 180 : 5));
        return args.id === "session-empty" ? [] : args.id === "session-fast" ? [message(10, "assistant", "这是快速切换后的正确会话内容。")] : structuredClone(window.__fixtureMessages);
      case "compact_session": {
        const current = window.__fixtureSessions.find(item => item.id === args.id);
        current.compact_count++; current.summary = "手动压缩已完成，重要任务信息已保留。"; return "压缩完成";
      }
      case "delete_session": window.__fixtureSessions = window.__fixtureSessions.filter(item => item.id !== args.id); return null;
      case "apply_takeover": return [{ client: "Codex", path: "C:/fixture/.codex/config.toml", backup_path: "C:/fixture/.codex/config.toml.backup-test", status: "updated" }];
      case "get_unified_key": return { unified_key: "fixture-only", openai_endpoint: "http://127.0.0.1:15721/v1" };
      case "get_provider_quota": {
        await new Promise(resolve => setTimeout(resolve, args.adapter === "auto" ? 180 : 5));
        if (args.adapter === "deepseek") throw new Error("额度接口返回 HTTP 401");
        return { provider_id: args.providerId, source: args.adapter === "auto" ? "旧请求来源" : "测试额度来源", checked_at: "2026-09-10T08:00:00Z", status: args.adapter === "newapi" ? "unsupported" : "ok", metrics: args.adapter === "newapi" ? [] : [
          { label: "Key 消费限额", scope: "key", unit: "USD", used: 12, total: 100, remaining: 88, unlimited: false, model: null, resets_at: null },
          { label: "订阅每日额度", scope: "subscription", unit: "USD", used: 2, total: null, remaining: null, unlimited: false, model: null, resets_at: null },
        ], expirations: args.adapter === "newapi" ? [] : [
          { label: "Key 有效期", scope: "key", expires_at: null, unlimited: true },
          { label: "订阅到期时间", scope: "subscription", expires_at: "2027-01-01T00:00:00Z", unlimited: false },
        ], warnings: ["当前 Key 限额不是账户余额，未提供逐模型剩余调用次数。"] };
      }
      default: throw new Error(`Unexpected fixture IPC: ${cmd}`);
    }
  } };
}

(async () => {
  fs.mkdirSync(output, { recursive: true });
  const browser = await chromium.launch({ headless: true, ...(process.env.LLMGW_BROWSER_CHANNEL ? { channel: process.env.LLMGW_BROWSER_CHANNEL } : {}) });
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, reducedMotion: "reduce" });
    await context.addInitScript(fixture);
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    page.on("dialog", dialog => dialog.accept());
    await page.goto(baseUrl);
    await page.getByRole("heading", { name: "OpenRouter", exact: true }).waitFor();
    await page.screenshot({ path: path.join(output, "providers-desktop.png"), fullPage: true });
    await page.getByRole("textbox", { name: "搜索供应商" }).fill("qwen-local");
    assert.equal(await page.locator(".provider-card").count(), 1);
    await page.getByRole("textbox", { name: "搜索供应商" }).fill("");
    await page.getByLabel("供应商状态筛选").selectOption("disabled");
    assert.equal(await page.locator(".provider-card").count(), 1);
    await page.getByLabel("供应商状态筛选").selectOption("all");
    await page.getByRole("button", { name: "＋ 添加供应商", exact: true }).click();
    await page.getByLabel("供应商名称", { exact: true }).fill("模型发现测试");
    await page.getByLabel("API 地址", { exact: true }).fill("https://example.test/v1/chat/completions");
    await page.getByLabel("API Key", { exact: true }).fill("fixture-only");
    await page.getByRole("button", { name: "获取支持模型", exact: true }).click();
    await page.locator(".catalog-item").first().waitFor();
    assert.equal(await page.getByLabel("API 地址", { exact: true }).inputValue(), "https://example.test/v1");
    await page.getByLabel("筛选模型能力").selectOption("free");
    assert.equal(await page.locator(".catalog-item").count(), 2);
    // 目录的模态能力以标签呈现，并可据此筛选。
    await page.getByLabel("筛选模型能力").selectOption("video");
    assert.equal(await page.locator(".catalog-item").count(), 1);
    assert((await page.locator(".catalog-item").first().innerText()).includes("视频"));
    await page.getByLabel("筛选模型能力").selectOption("all");
    await page.locator(".catalog-item").filter({ hasText: "Sample Chat" }).getByRole("checkbox").check();
    await page.locator(".catalog-item").filter({ hasText: "Unknown Chat" }).getByRole("checkbox").check();
    await page.getByRole("button", { name: "添加所选模型" }).click();
    const configured = page.locator(".configured-model");
    assert.equal(await configured.count(), 2);
    assert.equal(await configured.nth(0).getByLabel("上下文长度（tokens）").inputValue(), "131072");
    assert.equal(await configured.nth(1).getByLabel("上下文长度（tokens）").inputValue(), "32768");
    // 目录价格随模型带入，未提供的模型保持「未配置价格」。
    assert.equal(await configured.nth(0).getByLabel("输入价格").inputValue(), "1.5");
    assert.equal(await configured.nth(0).getByLabel("输出价格").inputValue(), "6");
    assert.equal(await configured.nth(1).getByLabel("输入价格").inputValue(), "");
    assert((await configured.nth(1).locator(".price-state").innerText()).includes("未配置价格"));
    // 只填一半价格必须被拒绝，且指明具体模型。
    await configured.nth(1).getByLabel("输入价格").fill("0.8");
    await page.getByRole("button", { name: "保存供应商", exact: true }).click();
    await page.getByRole("alert").filter({ hasText: "必须同时填写" }).waitFor();
    await configured.nth(1).getByLabel("输入价格").fill("");
    // 参数覆盖：受保护字段被拒绝，合法配置可保存。
    await configured.nth(0).locator(".overrides-block > summary").click();
    await configured.nth(0).getByLabel("温度覆盖").fill("0.25");
    await configured.nth(0).getByLabel("max_tokens 覆盖").fill("512");
    await configured.nth(0).getByLabel("额外请求体（JSON 对象）").fill('{"messages":[]}');
    await page.getByRole("button", { name: "保存供应商", exact: true }).click();
    await page.getByRole("alert").filter({ hasText: "受保护字段" }).waitFor();
    await configured.nth(0).getByLabel("额外请求体（JSON 对象）").fill('{"top_k": 12}');
    await configured.nth(0).getByRole("button", { name: "＋ 添加请求头" }).click();
    await configured.nth(0).getByLabel("请求头名称 1").fill("X-Tenant");
    await configured.nth(0).getByLabel("请求头值 1").fill("tenant-42");
    // 峰谷价：起止时间往返（16:30 → 00:30 = 跨午夜），倍率按百分比填。
    await configured.nth(0).locator(".price-rules > summary").click();
    await configured.nth(0).getByRole("button", { name: "＋ 添加时段" }).click();
    await configured.nth(0).getByLabel("时段名称 1").fill("谷时");
    await configured.nth(0).getByLabel("时段开始 1").fill("16:30");
    await configured.nth(0).getByLabel("时段结束 1").fill("00:30");
    await configured.nth(0).getByLabel("输入折扣 1").fill("50");
    await configured.nth(0).getByLabel("输出折扣 1").fill("25");
    // 非法时间必须被拒绝并指明模型与时段。
    await configured.nth(0).getByLabel("时段开始 1").fill("25:99");
    await page.getByRole("button", { name: "保存供应商", exact: true }).click();
    await page.getByRole("alert").filter({ hasText: "HH:MM" }).waitFor();
    await configured.nth(0).getByLabel("时段开始 1").fill("16:30");
    await configured.nth(0).getByLabel("上下文长度（tokens）").fill("65536");
    await page.getByRole("button", { name: "重新获取模型" }).click();
    await page.getByRole("status").filter({ hasText: "获取到" }).waitFor();
    assert.equal(await configured.nth(0).getByLabel("上下文长度（tokens）").inputValue(), "65536");
    assert(await page.locator(".catalog-item").filter({ hasText: "Sample Chat" }).getByRole("checkbox").isDisabled());
    await page.screenshot({ path: path.join(output, "model-discovery-desktop.png"), fullPage: true });
    await page.getByRole("button", { name: "保存供应商", exact: true }).click();
    await page.getByRole("dialog").waitFor({ state: "hidden" });
    const saved = await page.evaluate(() => window.__fixtureSaved.at(-1));
    assert.equal(saved.models.length, 2);
    assert.equal(saved.models[0].context_window, 65536);
    assert.equal(saved.models[1].supports_tools, false);
    assert(!("source" in saved.models[0]));
    // 价格与参数覆盖必须整体提交，且未配置的模型保持 null 而不是 0。
    assert.equal(saved.models[0].price.prompt, 1.5);
    assert.equal(saved.models[0].price.completion, 6);
    assert.equal(saved.models[0].price.currency, "usd");
    // 目录提供的输入长度分档必须随模型保留，长上下文估算才不会被低估。
    assert.deepEqual(saved.models[0].price.tiers, [{ min_prompt_tokens: 272000, prompt: 3, completion: 12 }]);
    assert.equal(saved.models[1].price, null);
    assert.equal(saved.models[0].overrides.temperature, 0.25);
    assert.equal(saved.models[0].overrides.max_tokens, 512);
    assert.deepEqual(saved.models[0].overrides.extra_body, { top_k: 12 });
    assert.deepEqual(saved.models[0].overrides.extra_headers, [{ name: "X-Tenant", value: "tenant-42" }]);
    assert.equal(saved.models[1].overrides, null);
    // 峰谷价必须整体提交：跨午夜时间转成分钟数，百分比转成倍率。
    assert.equal(saved.models[0].price.rules.length, 1);
    assert.deepEqual(saved.models[0].price.rules[0], {
      label: "谷时", start_minute: 990, end_minute: 30, prompt_multiplier: 0.5, completion_multiplier: 0.25,
    });
    // 目录带出的模态能力随模型保存。
    assert.equal(saved.models[0].supports_video, true);
    assert.equal(saved.models[1].supports_tools, false);
    await page.getByRole("button", { name: "＋ 添加供应商", exact: true }).click();
    await page.getByLabel("API 地址", { exact: true }).fill("https://broken.test/v1");
    await page.getByRole("button", { name: "获取支持模型", exact: true }).click();
    await page.getByRole("alert").filter({ hasText: "401" }).waitFor();
    await page.getByRole("button", { name: "＋ 手动添加" }).click();
    assert.equal(await page.locator(".configured-model").count(), 1);
    await page.getByRole("button", { name: "取消", exact: true }).click();
    for (const viewport of [{ width: 900, height: 650 }, { width: 390, height: 844 }]) {
      await page.setViewportSize(viewport);
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), `page overflow at ${viewport.width}`);
      await page.screenshot({ path: path.join(output, `providers-${viewport.width}.png`), fullPage: true });
      await page.getByRole("button", { name: "＋ 添加供应商", exact: true }).click();
      assert(await page.evaluate(() => document.querySelector('.provider-editor').scrollWidth <= document.querySelector('.provider-editor').clientWidth), `dialog overflow at ${viewport.width}`);
      await page.screenshot({ path: path.join(output, `model-editor-${viewport.width}.png`), fullPage: true });
      await page.keyboard.press("Escape");
    }
    await page.setViewportSize({ width: 1440, height: 1000 });
    // 刷新定价：说明更新/跳过/未匹配的数量，且手工定价不被覆盖。
    await page.getByRole("button", { name: "刷新定价", exact: true }).click();
    const pricingMsg = page.getByRole("alert").filter({ hasText: "定价源返回" });
    await pricingMsg.waitFor();
    const pricingText = await pricingMsg.innerText();
    assert(pricingText.includes("更新 1 个"), pricingText);
    assert(pricingText.includes("跳过 1 个手工定价"), pricingText);
    assert(pricingText.includes("未在定价源中找到"), pricingText);
    // 手工定价的模型仍显示手工标记，说明未被刷新覆盖。
    assert((await page.locator(".provider-card").filter({ hasText: "多模态服务" }).innerText()).includes("vision-model"));
    await page.screenshot({ path: path.join(output, "pricing-refresh-desktop.png"), fullPage: true });
    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "用量与审计", exact: true }).click();
    // 分币种展示：两种币种必须分别出现，且不能相加成一个数。
    await page.locator(".spend-cards").waitFor();
    const spendText = await page.locator(".spend-cards").innerText();
    assert(spendText.includes("$0.0123"), `小额美元金额应保留 4 位有效精度：${spendText}`);
    assert(spendText.includes("¥1.50"), `人民币金额应独立展示：${spendText}`);
    assert((await page.locator(".unpriced-hint").innerText()).includes("未配置价格"));
    // 花表明细与未计价请求
    assert((await page.locator(".card").filter({ hasText: "近 30 天花费 · 按模型" }).innerText()).includes("vendor/chat:free"));
    assert((await page.locator(".requests-table").innerText()).includes("未计价"));
    // 计价档位与估算值都要能看到，便于对账。
    const requestsText = await page.locator(".requests-table").innerText();
    assert(requestsText.includes("谷时 · 输入≥272K 档"), requestsText);
    assert(requestsText.includes("估 1,450"), requestsText);
    // Token 计数校准表：样本与比值来自真实日志，重置需要确认。
    const calibrationCard = page.locator(".card").filter({ hasText: "Token 计数校准" });
    const calibrationText = await calibrationCard.innerText();
    assert(calibrationText.includes("1.35×"), calibrationText);
    assert(calibrationText.includes("12"), calibrationText);
    await calibrationCard.getByRole("button", { name: "重置校准" }).click();
    assert.equal(await page.evaluate(() => window.__fixtureCalibrationsCleared === true), true);
    // 降级链展开：第一跳 429 失败、第二跳成功。
    await page.locator(".attempt-toggle").first().click();
    const chain = await page.locator(".attempt-row").first().innerText();
    assert(chain.includes("限流服务"), `降级链应显示第一跳：${chain}`);
    assert(chain.includes("429"));
    assert(chain.includes("OpenRouter"));
    assert(chain.includes("上游返回 HTTP 429"));
    await page.locator(".attempt-toggle").first().click();
    // 401 那一行：失败且不可重试时必须明确显示「已停止降级」。
    await page.locator(".attempt-toggle").nth(1).click();
    const blocked = await page.locator(".attempt-row").last().innerText();
    assert(blocked.includes("已停止降级"), `401 后必须显示停止降级：${blocked}`);
    assert(blocked.includes("invalid api key"));
    await page.screenshot({ path: path.join(output, "stats-spend-desktop.png"), fullPage: true });
    for (const width of [900, 390]) {
      await page.setViewportSize({ width, height: 844 });
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), `stats overflow at ${width}`);
      await page.screenshot({ path: path.join(output, `stats-${width}.png`), fullPage: true });
    }
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "会话上下文", exact: true }).click();
    await page.locator(".session-item").first().waitFor();
    await page.getByLabel("搜索会话").fill("空会话");
    assert.equal(await page.locator(".session-item").count(), 1);
    await page.getByLabel("搜索会话").fill("");
    await page.locator(".session-item").filter({ hasText: "通用网关长上下文" }).click();
    await page.locator(".session-item").filter({ hasText: "快速切换验证" }).click();
    await page.getByText("这是快速切换后的正确会话内容。", { exact: true }).waitFor();
    await page.waitForTimeout(230);
    assert.equal(await page.locator(".session-message").count(), 1);
    assert((await page.locator(".session-message").innerText()).includes("快速切换后的正确会话内容"));
    await page.locator(".session-item").filter({ hasText: "通用网关长上下文" }).click();
    await page.locator(".session-message").nth(3).waitFor();
    await page.screenshot({ path: path.join(output, "sessions-desktop.png"), fullPage: true });
    await page.getByText("工具调用记录", { exact: true }).click();
    assert((await page.locator(".tool-calls[open]").innerText()).includes("read_config"));
    for (const viewport of [{ width: 900, height: 650 }, { width: 390, height: 844 }]) {
      await page.setViewportSize(viewport);
      assert(await page.evaluate(() => document.querySelector('.session-detail-card').scrollWidth <= document.querySelector('.session-detail-card').clientWidth), `session overflow at ${viewport.width}`);
      await page.locator(".session-detail-heading").scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(output, `sessions-${viewport.width}.png`), fullPage: true });
    }
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.getByRole("button", { name: "压缩上下文", exact: true }).click();
    await page.getByText("手动压缩已完成，重要任务信息已保留。", { exact: true }).waitFor();
    await page.locator(".session-item").filter({ hasText: "空会话" }).click();
    await page.getByText("该会话暂无可显示的消息", { exact: true }).waitFor();
    await page.getByRole("button", { name: "删除会话", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "会话已删除" }).waitFor();
    assert.equal(await page.locator(".session-item").count(), 2);
    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "供应商", exact: true }).click();
    await page.getByLabel("OpenRouter 更多操作").click();
    await page.locator(".provider-more[open]").getByRole("button", { name: "查询额度 / 有效期", exact: true }).click();
    await page.getByLabel("额度查询适配器").selectOption("openrouter");
    await page.locator(".quota-result-header").filter({ hasText: "测试额度来源" }).waitFor();
    await page.waitForTimeout(230);
    assert(!(await page.locator(".provider-quota-dialog").innerText()).includes("旧请求来源"));
    assert((await page.locator(".quota-metric").nth(1).innerText()).includes("未提供"));
    assert.equal(await page.locator(".quota-expiration").count(), 2);
    await page.screenshot({ path: path.join(output, "quota-desktop.png"), fullPage: true });
    await page.getByLabel("额度查询适配器").selectOption("deepseek");
    await page.locator(".quota-error").filter({ hasText: "401" }).waitFor();
    assert.equal(await page.locator(".quota-metric").count(), 0);
    await page.getByLabel("额度查询适配器").selectOption("newapi");
    await page.locator(".quota-result-header").waitFor();
    assert.equal(await page.locator(".quota-metric").count(), 0);
    await page.getByLabel("额度查询适配器").selectOption("sub2api");
    await page.locator(".quota-metric").first().waitFor();
    await page.setViewportSize({ width: 390, height: 844 });
    assert(await page.evaluate(() => document.querySelector('.provider-quota-dialog').scrollWidth <= document.querySelector('.provider-quota-dialog').clientWidth));
    await page.screenshot({ path: path.join(output, "quota-390.png"), fullPage: true });
    await page.keyboard.press("Escape");
    await page.setViewportSize({ width: 1180, height: 760 });
    await page.getByRole("button", { name: "切换到深色主题", exact: true }).click();
    assert.equal(await page.locator("html").getAttribute("data-theme"), "dark");
    await page.screenshot({ path: path.join(output, "providers-dark.png"), fullPage: true });
    await page.getByRole("button", { name: "切换到浅色主题", exact: true }).click();
    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
    await page.getByRole("button", { name: "备份并写入配置", exact: true }).click();
    await page.getByText("C:/fixture/.codex/config.toml.backup-test", { exact: true }).waitFor();
    // CLI 检测：先只读检测（未安装的工具不得伪造成已安装），再查最新版本并更新。
    await page.getByRole("button", { name: "检测本机 CLI", exact: true }).click();
    const cliCard = page.locator(".card").filter({ hasText: "本机 CLI 工具" });
    await cliCard.locator("table").waitFor();
    let cliText = await cliCard.innerText();
    assert(cliText.includes("Claude Code") && cliText.includes("2.0.0"), cliText);
    assert(cliText.includes("未检测到"), cliText);
    // 未查询最新版本时「更新」按钮存在但必须不可点，避免用户以为随便就能升级。
    for (const button of await cliCard.getByRole("button", { name: "更新", exact: true }).all()) {
      assert.equal(await button.isDisabled(), true, "未查询最新版本时更新按钮必须禁用");
    }
    await page.getByRole("button", { name: "检测并检查更新", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "可更新 1 个" }).waitFor();
    cliText = await cliCard.innerText();
    assert(cliText.includes("2.1.97"), cliText);
    assert(cliText.includes("可更新"), cliText);
    await cliCard.locator("tr").filter({ hasText: "Claude Code" }).getByRole("button", { name: "更新", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "更新命令已执行" }).waitFor();
    assert.equal(await page.evaluate(() => window.__fixtureUpdatedTool), "claude_code");
    // 更新后必须重新检测：版本刷新为新值，且不再提示可更新。
    const updatedRow = await cliCard.locator("tr").filter({ hasText: "Claude Code" }).innerText();
    assert(updatedRow.includes("2.1.97"), `更新后应重新检测出版本：${updatedRow}`);
    assert(!updatedRow.includes("可更新"), `更新后不应再提示可更新：${updatedRow}`);
    await page.screenshot({ path: path.join(output, "cli-tools-desktop.png"), fullPage: true });
    for (const width of [900, 390]) {
      await page.setViewportSize({ width, height: 844 });
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), `settings overflow at ${width}`);
      await page.screenshot({ path: path.join(output, `cli-tools-${width}.png`), fullPage: true });
    }
    await page.setViewportSize({ width: 1440, height: 1000 });
    // 接管后自检：先失败（上游不可用的真实场景），再成功，两种结果都要解释清楚。
    await page.evaluate(() => { window.__fixtureSelfCheckFails = true; });
    await page.getByRole("button", { name: "运行连通性自检", exact: true }).click();
    await page.getByRole("alert").filter({ hasText: "自检未通过" }).waitFor();
    await page.evaluate(() => { window.__fixtureSelfCheckFails = false; });
    await page.getByRole("button", { name: "运行连通性自检", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "自检通过" }).waitFor();
    const selfCheckText = await page.getByRole("status").filter({ hasText: "自检通过" }).innerText();
    assert(selfCheckText.includes("openrouter/vendor/chat:free"), selfCheckText);
    // 自检必须常驻
    const selfCheckCard = page.locator(".card").filter({ hasText: "网关连通性自检" });
    assert((await selfCheckCard.innerText()).includes("端到端"), "自检卡片必须常驻可点");

    // 配置包导入：逐项报告结果、备份路径与需要重填的 Key 必须都展示出来。
    await page.getByRole("button", { name: "导入配置包", exact: true }).click();
    await page.getByRole("alert").filter({ hasText: "异地服务" }).waitFor();
    const importMsg = await page.getByRole("alert").filter({ hasText: "异地服务" }).innerText();
    assert(importMsg.includes("2 个 Provider") && importMsg.includes("5 个模型"));
    assert(importMsg.includes("C:/fixture/backup-20260912-010203"));
    assert(importMsg.includes("重新填写"));
    // 导入确实触发了本机 IPC，且导出的 config.toml 路径被传入。
    assert((await page.evaluate(() => window.__fixtureCalls)).includes("import_bundle"));
    // Existing configurations remain undisturbed, while help can be reopened explicitly.
    assert.equal(await page.getByTestId("onboarding-dialog").count(), 0);
    await page.getByTestId("help-menu-trigger").click();
    await page.getByTestId("open-user-manual").click();
    await page.getByTestId("user-manual-search").waitFor();
    assert.equal(await page.getByTestId("user-manual-section").count(), 14);
    await page.getByTestId("user-manual-search").fill("不会匹配的内容-xyz");
    await page.getByTestId("user-manual-empty").waitFor();
    await page.getByTestId("user-manual-search").fill("订阅");
    assert(await page.getByTestId("user-manual-section").count() > 0);
    assert(await page.getByTestId("user-manual-section").count() < 14);
    await page.getByTestId("user-manual-search").fill("");
    await page.getByTestId("user-manual-toc-item").filter({ hasText: "手动接入其他客户端" }).click();
    await page.getByTestId("user-manual-code").scrollIntoViewIfNeeded();
    assert((await page.getByTestId("user-manual-code").innerText()).includes("Read-Host"));
    for (const width of [1180, 900, 390]) {
      await page.setViewportSize({ width, height: 844 });
      assert(await page.evaluate(() => document.querySelector('.user-manual-dialog').scrollWidth <= document.querySelector('.user-manual-dialog').clientWidth), `manual overflow at ${width}`);
      await page.screenshot({ path: path.join(output, `user-manual-${width}.png`) });
    }
    await page.keyboard.press("Escape");
    assert.equal(await page.locator(".user-manual-dialog").count(), 0);
    const firstUse = await browser.newContext({ viewport: { width: 1180, height: 760 }, reducedMotion: "reduce" });
    await firstUse.addInitScript(fixture, { empty: true });
    const onboarding = await firstUse.newPage();
    onboarding.on("pageerror", error => errors.push(error.message));
    await onboarding.goto(baseUrl);
    await onboarding.getByTestId("onboarding-dialog").waitFor();
    assert(await onboarding.getByTestId("onboarding-previous").isDisabled());
    await onboarding.getByTestId("onboarding-next").click();
    await onboarding.getByTestId("onboarding-step-models").waitFor();
    await onboarding.getByTestId("onboarding-previous").click();
    await onboarding.getByTestId("onboarding-step-provider").waitFor();
    for (const width of [1180, 900, 390]) {
      await onboarding.setViewportSize({ width, height: 844 });
      assert(await onboarding.evaluate(() => document.querySelector('.onboarding-dialog').scrollWidth <= document.querySelector('.onboarding-dialog').clientWidth), `onboarding overflow at ${width}`);
      await onboarding.screenshot({ path: path.join(output, `onboarding-${width}.png`) });
    }
    await onboarding.getByTestId("onboarding-skip").click();
    assert.equal(await onboarding.evaluate(() => localStorage.getItem("llm-gateway-onboarding-v1")), "skipped");
    await onboarding.reload();
    await onboarding.getByRole("button", { name: "添加第一个供应商", exact: true }).waitFor();
    assert.equal(await onboarding.getByTestId("onboarding-dialog").count(), 0);
    await onboarding.getByTestId("help-menu-trigger").click();
    await onboarding.getByTestId("open-onboarding").click();
    for (let step = 0; step < 4; step++) await onboarding.getByTestId("onboarding-next").click();
    await onboarding.getByTestId("onboarding-complete").click();
    assert.equal(await onboarding.evaluate(() => localStorage.getItem("llm-gateway-onboarding-v1")), "completed");
    await onboarding.getByTestId("help-menu-trigger").click();
    await onboarding.getByTestId("open-onboarding").click();
    for (let step = 0; step < 3; step++) await onboarding.getByTestId("onboarding-next").click();
    await onboarding.getByTestId("onboarding-navigate-settings").click();
    await onboarding.getByRole("button", { name: "备份并写入配置", exact: true }).waitFor();
    const guideCalls = await onboarding.evaluate(() => window.__fixtureCalls);
    assert(!guideCalls.some(cmd => ["upsert_provider", "update_config", "apply_takeover", "test_provider", "discover_provider_models"].includes(cmd)));
    await firstUse.close();
    for (const failure of ["configFailure", "providerFailure"]) {
      const failedContext = await browser.newContext();
      await failedContext.addInitScript(fixture, { empty: true, [failure]: true });
      const failedPage = await failedContext.newPage();
      await failedPage.goto(baseUrl);
      await failedPage.getByRole("alert").filter({ hasText: "加载失败" }).waitFor();
      assert.equal(await failedPage.getByTestId("onboarding-dialog").count(), 0);
      await failedContext.close();
    }
    const offlineManual = await browser.newPage();
    const manualRequests = [];
    offlineManual.on("request", request => { if (/^https?:/.test(request.url())) manualRequests.push(request.url()); });
    await offlineManual.goto(pathToFileURL(path.resolve(__dirname, "../docs/使用手册.html")).href);
    assert.equal(await offlineManual.locator("main > section").count(), 14);
    await offlineManual.getByRole("link", { name: "查询额度、订阅与剩余时间", exact: true }).click();
    assert(offlineManual.url().endsWith("#quota"));
    for (const width of [900, 390]) {
      await offlineManual.setViewportSize({ width, height: 844 });
      assert(await offlineManual.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
      await offlineManual.screenshot({ path: path.join(output, `manual-html-${width}.png`) });
    }
    assert.deepEqual(manualRequests, []);
    assert.deepEqual(errors, []);
    const preview = await browser.newPage();
    await preview.goto(baseUrl);
    await preview.getByRole("heading", { name: "浏览器预览已隔离", exact: true }).waitFor();
    assert.equal(await preview.locator(".provider-card").count(), 0);
    console.log(`UI_SMOKE_OK: discovery, defaults, overrides, deduplication, sessions, switching races, compression, backup display, quota/expiry, first-use guidance, persisted skip/completion, manual search, offline HTML, themes, responsive layouts and preview isolation; screenshots=${output}`);
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });

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
  const model = (id, context = 32768) => ({ alias: id, upstream: id, context_window: context, supports_tools: true, supports_vision: false, supports_stream: true });
  const provider = (id, name, dialect, url, models) => ({ id, name, dialect, base_url: url, api_key_masked: "已保存", models, enabled: true, priority: 10, rpm_limit: 0, intelligence: 70, note: null, is_active: false, health: { health: "healthy", success_rate: 1, avg_latency_ms: 100 } });
  window.__fixtureProviders = [
    { ...provider("openrouter", "OpenRouter", "openai", "https://openrouter.ai/api/v1", [model("openrouter/free", 131072), model("my-chat")]), is_active: true },
    provider("anthropic", "Anthropic", "anthropic", "https://api.anthropic.com/v1", [model("claude-sonnet", 200000)]),
    provider("ollama", "本地 Ollama", "ollama", "http://localhost:11434", [model("qwen-local")]),
    { ...provider("disabled", "备用服务", "openai", "https://example.test/v1", [model("backup-chat")]), enabled: false },
  ];
  window.__fixtureConfig = { bind: "127.0.0.1", port: 15721, allow_lan: false, unified_key: "fixture-only", routing_strategy: "balanced", custom_rules: [], max_fallback_attempts: 3, upstream_timeout_secs: 90, sticky_ttl_secs: 1800, compact_threshold_tokens: 60000, compact_keep_recent: 12, analytics_retention_days: 30, log_request_body: false, http_proxy: null, failover_enabled: true, catalog_auto_update: false, catalog_feed_url: null, remote_mode: { enabled: false, public_url: null }, takeover: { claude_code: false, codex: false, gemini_cli: false } };
  window.__fixtureSaved = [];
  if (empty) window.__fixtureProviders = [];
  window.__fixtureCalls = [];
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
          { id: "sample/chat", name: "Sample Chat", context_window: 131072, context_source: "provider", supports_tools: true, supports_vision: true, supports_stream: true, is_free: true },
          { id: "unknown/chat", name: "Unknown Chat", context_window: 32768, context_source: "default", supports_tools: null, supports_vision: null, supports_stream: null, is_free: false },
          { id: "sample/reasoner", name: "Sample Reasoner", context_window: 65536, context_source: "provider", supports_tools: true, supports_vision: false, supports_stream: true, is_free: true },
        ] };
      case "upsert_provider": {
        const input = structuredClone(args.input);
        window.__fixtureSaved.push(input);
        const next = { ...input, id: input.id || "fixture-new", api_key: undefined, api_key_masked: "已保存", is_active: false };
        window.__fixtureProviders = [...window.__fixtureProviders.filter(p => p.id !== next.id), next]; return next.id;
      }
      case "update_config": window.__fixtureConfig = structuredClone(args.cfg); return { config: args.cfg, restart_required: false, restart_reasons: [] };
      case "test_provider": return { ok: true, latency_ms: 42, model: "sample/chat" };
      case "list_snapshots": case "list_remote_access_keys": case "recent_requests": return [];
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
    await page.getByLabel("筛选模型能力").selectOption("all");
    await page.locator(".catalog-item").filter({ hasText: "Sample Chat" }).getByRole("checkbox").check();
    await page.locator(".catalog-item").filter({ hasText: "Unknown Chat" }).getByRole("checkbox").check();
    await page.getByRole("button", { name: "添加所选模型" }).click();
    const configured = page.locator(".configured-model");
    assert.equal(await configured.count(), 2);
    assert.equal(await configured.nth(0).getByLabel("上下文长度（tokens）").inputValue(), "131072");
    assert.equal(await configured.nth(1).getByLabel("上下文长度（tokens）").inputValue(), "32768");
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

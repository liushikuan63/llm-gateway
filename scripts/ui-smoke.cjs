// Isolated browser regression: the IPC fixture never reaches the user's gateway or providers.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { pathToFileURL } = require("node:url");
const { chromium } = require(process.env.LLMGW_PLAYWRIGHT_PATH || "playwright");
const output = process.env.LLMGW_UI_OUTPUT || path.join(os.tmpdir(), "llm-gateway-ui");
const baseUrl = process.env.LLMGW_UI_URL || "http://127.0.0.1:5173";
// 手册章节数从单一内容来源读取：增删章节时这里自动跟随，不需要同步修改断言。
const manualSections = JSON.parse(fs.readFileSync(path.resolve(__dirname, "../src/content/user-manual.json"), "utf8")).sections;
assert(manualSections.length >= 10, `手册章节只有 ${manualSections.length} 个，内容来源可能已损坏`);

async function fixture({ empty = false, configFailure = false, providerFailure = false, bootWarning = null } = {}) {
  window.isTauri = true;
  const model = (id, context = 32768, price = null, extra = {}) => ({ alias: id, upstream: id, enabled: true, model_type: "chat", upstream_path: null, context_window: context, supports_tools: true, supports_vision: false, supports_audio: false, supports_video: false, supports_stream: true, price, overrides: null, ...extra });
  // 带峰谷价的模型：谷时（UTC 16:30–00:30）打五折，用于验证时段规则的往返保存。
  const peakValleyPrice = { prompt: 2, completion: 8, currency: "cny", tiers: [], source: "manual", rules: [
    { label: "谷时", start_minute: 990, end_minute: 30, prompt_multiplier: 0.5, completion_multiplier: 0.25 },
  ] };
  const provider = (id, name, dialect, url, models) => ({ id, name, dialect, base_url: url, api_key_masked: "已保存", models, enabled: true, priority: 10, rpm_limit: 0, intelligence: 70, note: null, is_active: false, health: { health: "healthy", success_rate: 1, avg_latency_ms: 100 } });
  window.__fixtureProviders = [
    { ...provider("openrouter", "OpenRouter", "openai", "https://openrouter.ai/api/v1", [model("openrouter/free", 131072), model("my-chat")]), is_active: true },
    // A5：走账号型上游的一家（runtime_id 指向下面那个运行时）。
    { ...provider("anthropic", "Anthropic", "anthropic", "https://api.anthropic.com/v1", [model("claude-sonnet", 200000)]), runtime_id: "codex-work" },
    provider("ollama", "本地 Ollama", "ollama", "http://localhost:11434", [model("qwen-local")]),
    provider("multimodal", "多模态服务", "openai", "https://example.test/v1", [model("vision-model", 65536, peakValleyPrice, { supports_vision: true, supports_audio: true, supports_video: true })]),
    // 指向一个**不存在**的运行时：界面必须当场说出来，而不是等请求时才报错。
    { ...provider("disabled", "备用服务", "openai", "https://example.test/v1", [model("backup-chat")]), enabled: false, runtime_id: "ghost" },
  ];
  // B2 远程 Key 夹具：两条 —— 一条设了预算+白名单、一条全不限，
  // 这样「不限」与「已设」两种渲染都能被断言到。
  // 必须带预算三件套：给空数组的话新列永远不渲染，
  // 漏字段导致的白屏也永远不会被发现（卡片点名过这个坑）。
  window.__fixtureRemoteKeys = [
    {
      id: "rk-fixture-limited", label: "受限设备", enabled: true, rpm_limit: 60,
      monthly_budget_micros: 5000000, budget_currency: "USD",
      allowed_models: ["gpt-4o", "claude-*"],
      created_at: "2026-09-01T00:00:00Z", updated_at: "2026-09-01T00:00:00Z",
    },
    {
      id: "rk-fixture-open", label: "不限额设备", enabled: false, rpm_limit: 120,
      monthly_budget_micros: 0, budget_currency: "", allowed_models: [],
      created_at: "2026-09-02T00:00:00Z", updated_at: "2026-09-02T00:00:00Z",
    },
  ];
  window.__fixtureConfig = { bind: "127.0.0.1", port: 15721, allow_lan: false, unified_key: "fixture-only", routing_strategy: "balanced", custom_rules: [], max_fallback_attempts: 3, upstream_timeout_secs: 90, sticky_ttl_secs: 1800, compact_threshold_tokens: 60000, compact_keep_recent: 12, analytics_retention_days: 30, http_proxy: null, failover_enabled: true, catalog_auto_update: false, catalog_feed_url: null, remote_mode: { enabled: false, public_url: null }, takeover: { claude_code: false, codex: false, gemini_cli: false, opencode: false, crush: false },
    // D4 级联：夹具必须带这一段，否则 `cfg.cascade` 在真环境里是 undefined，
    // 页面会白屏而测试全绿（CLAUDE.md 铁律 10 点名的正是这个坑）。
    cascade: { max_escalations: 0, min_confidence: 0.6 },
    smart_routing: {
      enabled: true, classifier: "jev",
      jev: { base_url: "http://127.0.0.1:8009/v1/systemone", model: "rl-agent", timeout_ms: 1200, max_state_chars: 4000,
        auto_start: { enabled: false, binary: "", port: 8009 } },
      timeout_ms: 1200, min_confidence: 0.35, min_margin: 0.25,
      // 预优化默认关闭：先用关闭态验"开关关着时字段不生效"，
      // 再打开验 UI 能提交并回填。
      prompt_refine: { enabled: false, provider_id: null, model: null, timeout_ms: 2000, max_chars: 2000, clarity_noul: 0.72, min_chars: 24 },
    },
    search: { enabled: true, backend: "tavily", searxng_url: null, max_results: 5, timeout_ms: 8000, inject_as: "text" },
    local_models: {
      enabled: true, probe_timeout_ms: 2000,
      endpoints: [
        { id: "ollama", label: "Ollama", base_url: "http://127.0.0.1:11434", kind: "ollama" },
        { id: "lmstudio", label: "LM Studio", base_url: "http://127.0.0.1:1234", kind: "open_ai_compatible" },
        { id: "dead", label: "已停止的服务", base_url: "http://127.0.0.1:1", kind: "open_ai_compatible" },
      ],
    },
  };
  // D2 能力账本：三种情形各一条 —— 真冲突、多来源但一致、只有单一来源。
  // 「一致」那条是必须的：把「两个来源说了同一个值」也标成冲突，
  // 会让用户去处理一个根本不存在的问题。
  window.__fixtureCapabilities = {
    "openrouter/openrouter/free": {
      values: {
        coding: { manual: { value: 0.9, source: "manual" }, community: { value: 0.4, source: "community" } },
        reasoning: { manual: { value: 0.7, source: "manual" } },
      },
    },
    "anthropic/claude-sonnet": {
      values: {
        reasoning: { manual: { value: 0.8, source: "manual" }, catalog: { value: 0.8, source: "catalog" } },
      },
    },
  };
  // A5 运行时清单。夹具里**故意不包含** "ghost" —— 好让
  // 「runtime_id 指向一个不存在的运行时」这条分支真的被渲染出来
  // （它是最容易被写成静默的那种情况：只显示 id 而不说找不到）。
  window.__fixtureAgentRuntimes = [
    { id: "codex-work", kind: "codex", label: "Codex（工作）", options: null, enabled: true,
      created_at: "2026-10-01T00:00:00Z", updated_at: "2026-10-01T00:00:00Z" },
  ];
  window.__fixtureSearchKey = { masked: "tvly-****-abc", configured: true };  // 搜索设置快照。`get_search_settings` 与 `update_search_settings` 共用它，
  // 这样"保存后重新打开页面设置仍是新值"这条断言才不是自说自话。
  const searchSettings = () => {
    const s = window.__fixtureConfig.search;
    // 必须与 commands.rs 的 matches!(Tavily | Brave) 同一套口径：
    // 白名单判定，不是「除 DuckDuckGo 外都要 Key」。新加免 Key 后端时
    // 只有白名单写法不会漏 —— 实测踩过：夹具写成 `!== "duckduckgo"`，
    // 必应中国（免 Key）会被误报成需要 Key，断言跟着一起错。
    const needsKey = s.backend === "tavily" || s.backend === "brave";
    return { enabled: s.enabled, backend: s.backend, searxng_url: s.searxng_url, max_results: s.max_results, timeout_ms: s.timeout_ms, inject_as: s.inject_as, api_key_masked: window.__fixtureSearchKey.masked, has_key: window.__fixtureSearchKey.configured, backend_needs_key: needsKey };
  };
  window.__fixtureSaved = [];
  if (empty) window.__fixtureProviders = [];
  window.__fixtureCalls = [];
  // 本地模型 / 智能模式 / 联网搜索的夹具。
  //
  // **这个块必须完整**：页面是 `cfg.smart_routing.enabled` 这种直接解构，
  // 缺一个字段就是运行时报错而不是"显示为空"。夹具少给字段，
  // 冒烟测试就测不到页面，而测试全绿看起来像页面是好的。
  //
  // 命令名与 `src/api.ts` 逐一对应；改后端命令名时这里必须同步，
  // 否则冒烟会抛 `Unexpected fixture IPC` 而不是安静地跳过。
  window.__fixtureLocalRuntimes = [
    { id: "ollama", label: "Ollama", base_url: "http://127.0.0.1:11434", kind: "ollama", reachable: true, version: "0.35.1", model_count: 2, error: null },
    { id: "lmstudio", label: "LM Studio", base_url: "http://127.0.0.1:1234", kind: "open_ai_compatible", reachable: true, version: null, model_count: 1, error: null },
    // 不可达端点：界面上必须显示原因，不能只显示一个「不可达」。
    { id: "dead", label: "已停止的服务", base_url: "http://127.0.0.1:1", kind: "open_ai_compatible", reachable: false, version: null, model_count: 0, error: "连接被拒绝（127.0.0.1:1）" },
  ];
  // 能力位刻意不一致：同族的 q4_K_M 有 vision，q3 没有。
  // 两者都列出来，才能验「能力保守」这条规则在界面上是真的按上游元数据走。
  const localModel = (upstream, extra = {}) => ({
    upstream, alias: upstream.split("/").pop(), context_window: 40960,
    supports_tools: true, supports_vision: false, supports_audio: false, supports_video: false,
    supports_thinking: true, supports_stream: true, model_type: "chat",
    meta: { runtime: "ollama", family: null, parameter_size: "27B", quantization: null, disk_bytes: 17_760_000_000, capabilities: ["completion", "tools", "thinking"] },
    ...extra,
  });
  window.__fixtureLocalModels = {
    ollama: [
      localModel("qwen3.8:27b-q4_K_M", { supports_vision: true, meta: { runtime: "ollama", family: null, parameter_size: "27B", quantization: "Q4_K_M", disk_bytes: 17_760_000_000, capabilities: ["completion", "vision", "tools", "thinking"] } }),
      localModel("batiai/qwen3.8-27b:q3", { meta: { runtime: "ollama", family: null, parameter_size: "27B", quantization: "Q3", disk_bytes: 13_300_000_000, capabilities: ["completion", "tools", "thinking"] } }),
    ],
    lmstudio: [
      localModel("local-model", { context_window: 8192, supports_tools: false, supports_thinking: false, meta: { runtime: "openai-compatible", family: null, parameter_size: null, quantization: null, disk_bytes: null, capabilities: [] } }),
    ],
  };
  window.__fixtureJevPreview = [
    { name: "complexity", kind: "choice", detail: "simple 0.20 / moderate 0.60 / complex 0.20 · 置信度 0.60" },
    { name: "clarity", kind: "noul", detail: "0.10" },
  ];
  // Jev 原始判定试跑：返回 heuristic + 弃权原因，这正是本机的真实形态。
  window.__fixtureJevProbe = {
    classifier: "heuristic",
    intent: { class: "simple", complexity: 8, needs_web: false, needs_refine: false, classifier: "heuristic", jev_note: "置信度不足，已弃权", jev_evidence: null },
    jev_note: "置信度不足，已弃权",
    jev_evidence: { complexity: { choice: "simple", confidence: 0.117, probabilities: { simple: 0.55, moderate: 0.30, complex: 0.15 } } },
  };
  // CLI 检测夹具：覆盖 npm 与官方脚本两类来源，以及已安装 / 未安装 / 可更新 / 缺前置条件等分支。
  const cliTool = (id, label, source, installTarget, installed, version, latest, canInstall = true) => ({
    id, label, installed,
    path: installed ? `C:/Users/fixture/AppData/Roaming/npm/${id}.cmd` : null,
    version, source, install_target: installTarget,
    docs_url: `https://fixture.test/${id}`,
    can_install: canInstall,
    install_command: source === "npm"
      ? `npm install -g ${installTarget}@latest`
      : `irm https://fixture.test/${id}/install.ps1 | iex`,
    latest_version: latest,
    update_available: installed && version !== null && latest !== null && version !== latest,
    check_error: null,
  });
  window.__fixtureCliTools = [
    cliTool("claude_code", "Claude Code", "npm", "@anthropic-ai/claude-code", true, "2.0.0", null),
    cliTool("codex", "Codex CLI", "npm", "@openai/codex", true, "0.44.0", null),
    cliTool("gemini_cli", "Gemini CLI", "npm", "@google/gemini-cli", false, null, null),
    // 缺前置条件（例如本机没有 npm）时必须禁用按钮并在卡片上说明原因。
    cliTool("continue_cli", "Continue CLI", "npm", "@continuedev/cli", false, null, null, false),
    cliTool("grok_build", "Grok Build", "script", "官方安装脚本", false, null, null),
    cliTool("cursor_cli", "Cursor CLI", "script", "官方安装脚本", true, "2026.9.1", null),
  ];
  // 未安装的 npm 工具也要能看到将安装的版本；脚本类没有可比的版本。
  const latestByTool = { claude_code: "2.1.97", codex: "0.44.0", gemini_cli: "0.59.0" };
  window.__fixtureCliToolsWithUpdates = window.__fixtureCliTools.map(item => {
    const latest = latestByTool[item.id] ?? null;
    return { ...item, latest_version: latest, update_available: item.installed && item.version !== null && latest !== null && item.version !== latest };
  });
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
  // 桌宠夹具：一个工作中状态、运行中的 CLI 与桌面应用进程各一个、一个已安装宠物包与一个进行中的任务。
  window.__fixturePetStatus = {
    status: "working",
    reason: "[Qoder CLI] 进行中：edit",
    gateway_status: "idle",
    requests_last_minute: 3,
    failed_last_minute: 1,
    installed_pets: [{
      slug: "snow-plum-lillia", display_name: "Snow Plum Lillia", description: "雪梅莉利娅示例宠物", version: "1.1.0",
      spritesheet_file: "spritesheet.webp", directory: "C:/Users/fixture/.petdex/pets/snow-plum-lillia",
    }],
    ai_processes: [
      { tool_id: "codex", tool_label: "Codex CLI", kind: "cli", process_name: "codex.exe", pid: 4321, memory_kb: 56380 },
      { tool_id: "codex_desktop", tool_label: "Codex Desktop", kind: "app", process_name: "ChatGPT.exe", pid: 2468, memory_kb: 474684 },
      { tool_id: "qoder", tool_label: "Qoder CLI", kind: "cli", process_name: "qoder.exe", pid: 6789, memory_kb: 120000 },
      { tool_id: "qoder_ide", tool_label: "Qoder IDE", kind: "app", process_name: "Qoder CN.exe", pid: 5678, memory_kb: 512000 },
    ],
    active_tasks: [
      { tool_id: "qoder", source: "qoder_cli", source_label: "Qoder CLI", project: "C:\\Users\\fixture", session_id: "sess-1", status: "running", detail: "进行中：edit", title: "清理界面乱码与提交历史", last_message: "已完成 configHash 调整；剩余释义卡分层与跨术借用门禁仍待收尾。", deep_link: null, updated_at: 1789000000 },
      { tool_id: "codex_desktop", source: "codex", source_label: "Codex Desktop", project: "D:\\Java\\GitHub\\llm-auto", session_id: "sess-codex", status: "running", detail: "任务进行中", title: "完善桌宠任务列表", last_message: "已提取任务标题和最近内容，正在联调任务跳转。", deep_link: "codex://threads/sess-codex", updated_at: 1789000100 },
      { tool_id: "codex_desktop", source: "codex", source_label: "Codex Desktop", project: "D:\\Java\\GitHub\\llm-auto", session_id: "sess-codex-2", status: "done", detail: "任务已完成", title: "检查窗口定位回归", last_message: "定位 Qoder 与 Codex 桌面窗口通过。", deep_link: "codex://threads/sess-codex-2", updated_at: 1789000200 },
    ],
    pet_window_open: false,
  };
  window.__fixturePetScale = 1;
  window.__fixturePetExpanded = false;
  window.__fixturePetBubbleHidden = false;
  window.__fixturePetBubbleLeft = false;
  // 气泡尺寸公式必须与 src-tauri/src/pet_window.rs 保持一致
  //（行高 66 / 间距 8 / 收起步进 18 / 顶部留白 12 / 底部留白 16）。
  window.__fixturePetLayout = (scale = window.__fixturePetScale) => {
    const visible = window.__fixturePetBubbleCount ?? window.__fixturePetStatus.active_tasks.length;
    const bubbleCount = Math.min(Math.max(visible, 1), 3);
    const expandedStack = !!window.__fixturePetBubblesExpanded;
    const rows = bubbleCount > 1 && !expandedStack
      ? 66 + 18 * (bubbleCount - 1)
      : 66 * bubbleCount + 8 * (bubbleCount - 1);
    const stackHeight = rows + 12 + 16;
    const petHeight = 130 * scale + 12;
    return {
      scale,
      expanded: !!window.__fixturePetExpanded,
      bubble_hidden: !!window.__fixturePetBubbleHidden,
      bubble_left: !!window.__fixturePetBubbleLeft,
      bubble_count: bubbleCount,
      bubble_limit: 3,
      bubbles_expanded: expandedStack,
      width: window.__fixturePetExpanded || !window.__fixturePetBubbleHidden ? 120 * scale + 8 + 340 : 120 * scale,
      height: window.__fixturePetExpanded
        ? Math.max(petHeight, 372)
        : window.__fixturePetBubbleHidden
          ? petHeight
          : Math.max(petHeight, stackHeight),
      window_open: !!window.__fixturePetWindowOpen,
    };
  };
  // 1×1 透明 PNG：桌宠窗口只需要能解码的图片，验证动画逻辑而不依赖真实素材。
  window.__fixturePetAsset = {
    slug: "snow-plum-lillia", display_name: "Snow Plum Lillia", columns: 8, rows: 9, cell_width: 192, cell_height: 208,
    animations: {
      idle: { row: 0, delays_ms: [120, 120] }, running: { row: 7, delays_ms: [120, 120] },
      failed: { row: 5, delays_ms: [120, 120] }, jumping: { row: 4, delays_ms: [120, 120] },
    },
    spritesheet_data_url: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
  };
  const session = (id, title, compactCount = 0) => ({ id, title, snapshot_id: compactCount ? "snapshot-fixture" : null, sticky_provider_id: "openrouter", sticky_model: "vendor/chat:free", sticky_expires_at: null, total_tokens: 24680, compact_count: compactCount, summary: compactCount ? "任务目标：完善通用网关的会话界面。\n已完成：保留上下文、工具交换与降级处理。\n下一步：验证长文本排版和快速切换。" : null, created_at: "2026-09-10T08:00:00Z", updated_at: "2026-09-10T08:30:00Z", message_count: 4 });
  window.__fixtureSessions = [session("session-slow", "通用网关长上下文与工具调用验收", 2), session("session-fast", "快速切换验证会话"), session("session-empty", "空会话")];
  const message = (id, role, content, compacted = false) => ({ id, session_id: "session-slow", role, content, tool_calls: null, tool_call_id: null, name: null, routed_provider: role === "assistant" ? "OpenRouter" : null, routed_model: role === "assistant" ? "vendor/chat:free" : null, compacted, prompt_tokens: 2000, completion_tokens: 500, created_at: "2026-09-10T08:10:00Z" });
  window.__fixtureMessages = [
    message(1, "user", "请保留任务目标和已确认的配置，并优化这个较长的上下文页面。".repeat(20), true),
    { ...message(2, "assistant", "开始检查配置文件。", true), tool_calls: JSON.stringify([{ id: "call-fixture", type: "function", function: { name: "read_config", arguments: '{"path":"example/config.toml"}' } }]) },
    { ...message(3, "tool", "读取结果：配置有效。\n" + "路径/".repeat(140), true), tool_call_id: "call-fixture", name: "read_config" },
    message(4, "assistant", "这里是最新的完整答复。\n\n" + "长文本应保留换行，在有限宽度内自然换行；工具调用记录可单独展开。\n".repeat(24)),
  ];
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  window.__TAURI_INTERNALS__ = {
    // @tauri-apps/api 的事件与窗口模块依赖这两个基础设施桩。
    transformCallback: (callback) => { const id = Math.floor(Math.random() * 1e9); window[`_${id}`] = callback; return id; },
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
    invoke: async (cmd, args) => {
    window.__fixtureCalls.push(cmd);
    switch (cmd) {
      case "list_providers": if (providerFailure) throw new Error("模拟供应商读取失败"); return structuredClone(window.__fixtureProviders);
      case "get_config": if (configFailure) throw new Error("模拟配置读取失败"); return structuredClone(window.__fixtureConfig);
      // D2：能力账本导出。**夹具必须给出与后端 `CapabilitySet` 相同的形状**
      // （`values[维度][来源] = { value, source }`）—— 形状不对时页面会静默
      // 显示「数据不足」，那与「真的没有数据」看起来一模一样。
      case "export_capabilities": return JSON.stringify(window.__fixtureCapabilities ?? {});
      // A5：运行时清单与适配器表。徽标要靠前者把 runtime_id 翻成人话；
      // 漏了这个 case 会让整页 `Promise.all` 失败 —— 那是白屏，不是「没有徽标」。
      case "list_agent_runtimes": return structuredClone(window.__fixtureAgentRuntimes ?? []);
      case "list_agent_adapters": return [["fake", "假适配器（不需要登录）"], ["codex", "Codex CLI"], ["qoder", "Qoder CLI"]];
      case "discover_provider_models":
        if (args.input.base_url.includes("broken")) throw new Error("上游返回 HTTP 401，请检查密钥权限");
        return { base_url: "https://example.test/v1", warnings: [], models: [
          { id: "sample/chat", name: "Sample Chat", model_type: "chat", context_window: 131072, context_source: "provider", supports_tools: true, supports_vision: true, supports_audio: false, supports_video: true, supports_stream: true, is_free: true,
            price: { prompt: 1.5, completion: 6, cache_read: 0.15, cache_creation: 1.8, currency: "usd", source: "catalog", rules: [], tiers: [{ min_prompt_tokens: 272000, prompt: 3, completion: 12, cache_read: 0.3, cache_creation: 3.6 }] } },
          { id: "unknown/chat", name: "Unknown Chat", model_type: null, context_window: 32768, context_source: "default", supports_tools: null, supports_vision: null, supports_audio: null, supports_video: null, supports_stream: null, is_free: false, price: null },
          { id: "sample/reasoner", name: "Sample Reasoner", model_type: "chat", context_window: 65536, context_source: "provider", supports_tools: true, supports_vision: false, supports_audio: true, supports_video: false, supports_stream: true, is_free: true,
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
      case "list_snapshots": return [];
      // B2：远程 Key 夹具**必须带预算三件套**。给空数组的话新列永远不渲染，
      // 漏字段导致的白屏也永远不会被发现 —— 卡片点名过这个坑。
      case "list_remote_access_keys": return structuredClone(window.__fixtureRemoteKeys);
      case "create_remote_access_key": {
        const created = {
          id: "rk-created", label: args.input.label, enabled: true,
          rpm_limit: args.input.rpm_limit,
          monthly_budget_micros: 0, budget_currency: "", allowed_models: [],
          created_at: "2026-09-12T00:00:00Z", updated_at: "2026-09-12T00:00:00Z",
        };
        window.__fixtureRemoteKeys = [...window.__fixtureRemoteKeys, created];
        return { key: created, secret: "rk-fixture-secret" };
      }
      case "update_remote_access_key": {
        // 与后端同口径：未传（undefined）表示不改，传了就更新。
        // 夹具若把 undefined 当成「清空」，界面上「只改个名字」就会丢预算，
        // 而测试还会全绿。
        const idx = window.__fixtureRemoteKeys.findIndex(k => k.id === args.input.id);
        if (idx < 0) throw new Error("远程访问 Key 不存在");
        const prev = window.__fixtureRemoteKeys[idx];
        window.__fixtureRemoteKeys[idx] = {
          ...prev,
          label: args.input.label, enabled: args.input.enabled, rpm_limit: args.input.rpm_limit,
          monthly_budget_micros: args.input.monthly_budget_micros ?? prev.monthly_budget_micros,
          budget_currency: args.input.budget_currency ?? prev.budget_currency,
          allowed_models: args.input.allowed_models ?? prev.allowed_models,
          updated_at: "2026-09-12T01:00:00Z",
        };
        return structuredClone(window.__fixtureRemoteKeys[idx]);
      }
      case "delete_remote_access_key": {
        window.__fixtureRemoteKeys = window.__fixtureRemoteKeys.filter(k => k.id !== args.id);
        return null;
      }
      case "recent_requests": return structuredClone(window.__fixtureRequests);
      // B3 审计检索与导出。
      //
      // `query_requests` 必须**真的按条件过滤**，不能一律返回全部 ——
      // 那样「筛选生效」的断言会因为「什么都没筛也一样」而恒真，
      // 两边都算过。这里实现与后端同口径的几个条件。
      case "query_requests": {
        const f = args.filter || {};
        let rows = structuredClone(window.__fixtureRequests);
        if (f.provider) rows = rows.filter(r => r.routed_provider === f.provider);
        if (f.model) rows = rows.filter(r => r.routed_model === f.model);
        if (f.only_errors) rows = rows.filter(r => r.error !== null);
        if (f.only_fallbacks) rows = rows.filter(r => r.fallback_attempts > 0);
        if (f.currency) rows = rows.filter(r => r.currency === f.currency);
        if (typeof f.min_cost === "number") rows = rows.filter(r => r.cost !== null && r.cost >= f.min_cost);
        if (typeof f.max_cost === "number") rows = rows.filter(r => r.cost !== null && r.cost <= f.max_cost);
        if (f.status) {
          const s = String(f.status).toLowerCase();
          const m = /^([1-5])xx$/.exec(s);
          rows = m
            ? rows.filter(r => r.status !== null && Math.floor(r.status / 100) === Number(m[1]))
            : rows.filter(r => String(r.status) === s);
        }
        const total = rows.length;
        const limit = f.limit ?? 100;
        const offset = f.offset ?? 0;
        const page = rows.slice(offset, offset + limit);
        // 给每行补上 AuditRow 比 RequestLog 多出来的列。
        // 漏了会让前端读 undefined 而不报错（默认值恰好是 falsy），
        // 正是卡片警告的那类静默失败。
        const withAuditCols = page.map((r, i) => ({
          id: offset + i + 1,
          session_id: "sess-fixture",
          route_intent: "simple",
          route_classifier: "heuristic",
          route_search: null,
          route_search_hits: null,
          route_refined: false,
          route_refine_note: null,
          access_key_id: null,
          refined_prompt: null,
          ...r,
        }));
        return { rows: withAuditCols, total, truncated: offset + page.length < total };
      }
      case "export_requests": {
        const f = args.filter || {};
        let n = window.__fixtureRequests.length;
        if (f.only_errors) n = window.__fixtureRequests.filter(r => r.error !== null).length;
        if (f.only_fallbacks) n = window.__fixtureRequests.filter(r => r.fallback_attempts > 0).length;
        window.__lastExport = { format: args.format, dest: args.destPath, filter: f, written: n };
        return {
          written: n,
          path: args.destPath,
          // 只有 CSV 有伴随列说明，与后端同口径（后端用 file_stem 拼，
          // 所以无论用户选的路径带不带 .csv 都会得到 `<stem>_columns.md`）。
          columns_doc: args.format === "csv"
            ? args.destPath.replace(/\.csv$/, "") + "_columns.md"
            : null,
        };
      }
      case "plugin:dialog|save": return "C:/fixture/audit-export";
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
      case "detect_cli_tools": {
        // 安装动作之后重新检测必须反映新状态，否则界面会显示"已安装但仍提示可更新"。
        const afterInstall = window.__fixtureCliAfterInstall ?? {};
        return structuredClone(window.__fixtureCliTools)
          .map(item => afterInstall[item.id] ? { ...item, ...afterInstall[item.id] } : item)
          .map(item => ({ ...item, update_available: item.installed && item.version !== null && item.latest_version !== null && item.version !== item.latest_version }));
      }
      case "detect_cli_tools_with_updates": return structuredClone(window.__fixtureCliToolsWithUpdates);
      case "install_cli_tool": {
        window.__fixtureInstalledCli = args.id;
        const current = window.__fixtureCliTools.find(item => item.id === args.id);
        // npm 类装出可查询到的最新版；脚本类没有版本可比，给一个具体版本表示装成功了。
        const version = current?.latest_version ?? (args.id === "claude_code" ? "2.1.97" : "1.2.3");
        window.__fixtureCliAfterInstall = { ...(window.__fixtureCliAfterInstall ?? {}), [args.id]: { installed: true, version, latest_version: null, path: current?.path ?? `C:/Users/fixture/.local/bin/${args.id}` } };
        return "added 1 package in 2s";
      }
      case "get_boot_state": return { status: "ready", error: null, warning: bootWarning };
      case "get_pet_status": return { ...structuredClone(window.__fixturePetStatus), pet_window_open: !!window.__fixturePetWindowOpen };
      case "get_pet_asset": window.__fixturePetAssetSlug = args.slug; return structuredClone(window.__fixturePetAsset);
      case "open_pet_window": window.__fixturePetWindowOpen = true; return null;
      case "close_pet_window": window.__fixturePetWindowOpen = false; window.__fixturePetClosed = true; return null;
      case "set_pet_window_size":
        window.__fixturePetScale = args.scale;
        return window.__fixturePetLayout(args.scale);
      case "set_pet_window_expanded":
        window.__fixturePetExpanded = args.expanded;
        if (args.expanded) window.__fixturePetBubbleHidden = false;
        return window.__fixturePetLayout();
      case "set_pet_window_bubble_hidden":
        window.__fixturePetBubbleHidden = args.hidden;
        return window.__fixturePetLayout();
      case "get_pet_window_layout": return window.__fixturePetLayout();
      // 任务数量变化时前端会请求重算窗口高度；夹具按同一公式返回。
      case "refresh_pet_window_layout": return window.__fixturePetLayout();
      // 前端上报可见气泡数量与堆叠状态（关闭单个气泡 / 悬浮展开）。
      case "set_pet_bubbles":
        window.__fixturePetBubbleCount = args.bubbleCount;
        window.__fixturePetBubblesExpanded = args.bubblesExpanded;
        return window.__fixturePetLayout();
      case "show_pet_menu": window.__fixturePetMenu = { slug: args.currentSlug, paused: args.paused, expanded: args.expanded }; return null;
      case "focus_main_window": window.__fixtureFocusedSection = args.section ?? "none"; return null;
      case "focus_ai_tool": window.__fixtureFocusedTool = args.toolId; return "已跳转到 " + args.toolId;
      case "open_ai_task": window.__fixtureOpenedTask = { toolId: args.toolId, sessionId: args.sessionId }; return "已打开任务 " + args.sessionId;
      case "open_task_project": window.__fixtureOpenedProject = args.path; return "已打开项目目录：" + args.path;
      case "stop_ai_tool": window.__fixtureStoppedTool = args.toolId; return "已结束 " + args.toolId + " 的 1 个进程";
      case "petdex_catalog": return "snow-plum-lillia   Snow Plum Lillia   by fixture-author\nmoon-rabbit        Moon Rabbit         by fixture-author";
      case "petdex_install_pet": window.__fixturePetInstalled = args.slug; return "installed";
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
      // 本地模型 / 智能模式 / 联网搜索
      case "list_local_runtimes": return structuredClone(window.__fixtureLocalRuntimes);
      case "list_local_models": {
        const found = window.__fixtureLocalModels[args.endpointId];
        if (!found) throw new Error(`未知的本地端点：${args.endpointId}`);
        return structuredClone(found);
      }
      case "register_local_model": return {
        provider_id: args.input.endpoint_id, alias: args.input.alias ?? args.input.upstream,
        added_models: 1, all_models: structuredClone(window.__fixtureLocalModels[args.input.endpointId] ?? []),
      };
      case "pull_local_model": return null;
      case "calibrate_classifier": case "calibrate_default_samples": {
        // 净收益为负是本机实测的真实形态：edgeJev 有一条高置信度错判，
        // 而启发式判对。报告必须如实显示出来，而不是挑个好看的样本集。
        return {
          total: 3,
          matrix: {
            "简单任务": { "简单任务": 1 },
            "复杂思考": { "简单任务": 1, "复杂思考": 1 },
          },
          adopted_count: 2, adopted_correct: 1, adopted_wrong: 1,
          abstained_count: 1, abstained_but_heuristic_right: 1,
          heuristic_correct: 3, net_gain: -1,
          wrong_confidences: [0.747],
          worst_wrong: { text: "线上服务 500 白屏，帮我定位根因", expected: "reasoning",
            heuristic: "reasoning", adopted: "simple", adopted_from_jev: true,
            abstain_reason: null, confidence: 0.747, margin: 0.4, raw_choice: "simple" },
          per_sample: [
            { text: "把变量名 x 改成 userName", expected: "simple", heuristic: "simple", adopted: "simple",
              adopted_from_jev: true, abstain_reason: null, confidence: 0.641, margin: 0.4, raw_choice: "simple" },
            { text: "线上服务 500 白屏，帮我定位根因", expected: "reasoning", heuristic: "reasoning", adopted: "simple",
              adopted_from_jev: true, abstain_reason: null, confidence: 0.747, margin: 0.4, raw_choice: "simple" },
            { text: "你好", expected: "simple", heuristic: "simple", adopted: "simple",
              adopted_from_jev: false, abstain_reason: "置信度不足，已弃权", confidence: 0.117, margin: 0.05, raw_choice: "simple" },
          ],
          verdict: "Jev 在 3 条样本里被采纳 2 条，只判对 1 条（50%），净收益 -1 条：采纳它反而更差。错判最高置信度高达 0.747，阈值挡不住——它可以又自信又错。建议关掉，或加一条「启发式越过阈值就不许降级」的否决规则。",
        };
      }
      case "jev_probe": return structuredClone(window.__fixtureJevProbe);
      case "classify_preview": return structuredClone(window.__fixtureJevProbe.intent);
      // 失效模型扫描：夹具必须给出**三种判定**都要覆盖，
      // 否则前端里「可删」与「不可删」的渲染差异测不出来。
      case "scan_stale_models":
        return {
          probed: false,
          catalog_unavailable: ["目录不可用的供应商"],
          entries: [
            { provider_id: "openrouter", provider_name: "openrouter", alias: "vendor/dead-model", upstream: "vendor/dead-model", verdict: "missing_from_catalog", detail: "上游目录中已无此 id" },
            { provider_id: "openrouter", provider_name: "openrouter", alias: "vendor/live-model", upstream: "vendor/live-model", verdict: "healthy", detail: "上游目录中仍存在" },
            { provider_id: "undetectable", provider_name: "undetectable", alias: "vendor/undetected", upstream: "vendor/undetected", verdict: "catalog_unavailable", detail: "上游目录不可用，无法判定是否失效" },
          ],
        };
      case "delete_models": return 1;
      case "get_search_settings": return searchSettings();
      case "update_search_settings": {
        // 必须同时改夹具配置，否则「保存后回到页面设置被还原」这条断言能通过。
        const next = args.input;
        // **必须复现后端 `search::validate()` 的拒绝规则**，否则界面测试永远发现不了这类 bug。
        // 真实 commands.rs 在保存前会调 validate()：选 SearXNG 时 searxng_url 为空就整条拒绝。
        // 夹具此前不做任何校验 → 「选了 SearXNG 却填不了地址」这条路径在测试里是绿的，真机却卡死。
        if (next.backend === "sear_xng") {
          const url = ("searxng_url" in next ? next.searxng_url : window.__fixtureConfig.search.searxng_url) ?? "";
          if (!String(url).trim()) throw "选择 SearXNG 后端时必须填写实例地址";
        }
        // 记录每次保存的后端值：用来断言「切换真的发出了 IPC」，
      // 而不是只断言控件长得对。少了它，「保存成功但界面没回填」这类
      // 回归会一路滑到「保存没发生」才被看见。
      (window.__fixtureDiag = window.__fixtureDiag || []).push(next.backend);
        window.__fixtureConfig.search = { ...window.__fixtureConfig.search, ...next };
        if ("api_key" in next && next.api_key) window.__fixtureSearchKey = { masked: "tvly-****-zzzz", configured: true };
        // 返回**裸的** SearchSettingsView，不包一层 —— commands.rs 的
        // update_search_settings 结尾是直接 `get_search_settings(state)`。
        // 包成 `{ settings: … }` 时前端 `setSettings(saved)` 拿到的是
        // `undefined` 的 backend，控件静默回落到第一个选项（实测症状：
        // 保存成功了但下拉还是 tavily）。
        return searchSettings();
      }
      case "test_search_backend": return {
        backend: window.__fixtureConfig.search.backend, hits: 2, error: null, latency_ms: 214,
        results: [
          { title: "edgeJev 部署说明", url: "https://example.test/jev", snippet: "本机 edgeJev 默认监听 8009 端口。" },
          { title: "Ollama 能力位说明", url: "https://example.test/ollama", snippet: "/api/tags 的 capabilities 是能力位的唯一可靠来源。" },
        ],
      };
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
      // Tauri 的窗口/事件插件调用属于基础设施；在夹具中视为成功，但不掩盖业务命令的意外调用。
      default:
        if (cmd === "plugin:event|listen") {
          // 记录事件处理器，测试可以模拟后端 emit（如原生菜单动作）。
          window.__fixtureEventHandlers = window.__fixtureEventHandlers ?? {};
          window.__fixtureEventHandlers[args.event] = window[`_${args.handler}`] ?? null;
          return 0;
        }
        if (cmd.startsWith("plugin:event|")) return 0;
        if (cmd === "plugin:window|start_dragging") {
          window.__fixtureDragStarted = true;
          window.__fixtureDragStarts = (window.__fixtureDragStarts ?? 0) + 1;
          return 0;
        }
        if (cmd.startsWith("plugin:window|")) return 0;
        throw new Error(`Unexpected fixture IPC: ${cmd}`);
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

    // 「接口协议」下拉必须列出全部五种方言。少一种的症状是**静默的**：
    // 新建的供应商压根没法选那个协议，而测试全绿、界面也不报错。
    // 2026-10-05 新增 Responses 后在这里钉住。
    await page.getByRole("button", { name: "＋ 添加供应商" }).click();
    const dialectSelect = page.locator("#provider-dialect");
    await dialectSelect.waitFor({ state: "visible", timeout: 5000 });
    const dialectValues = await dialectSelect.locator("option").evaluateAll(nodes => nodes.map(n => n.value));
    for (const expected of ["openai", "anthropic", "gemini", "ollama", "responses"]) {
      if (!dialectValues.includes(expected)) {
        throw new Error(`接口协议下拉缺少 ${expected}，实际只有：${dialectValues.join(", ")}`);
      }
    }
    // 新建供应商一个模型都没有，先加一个 —— 否则下面的模型级开关无从断言。
    await page.locator(".selected-heading button").click();
    await page.locator("#upstream-path-0").waitFor({ state: "visible", timeout: 5000 });
    await page.screenshot({ path: path.join(output, "provider-dialect-options.png"), fullPage: true });
    // 模型级禁用开关：默认勾上（参与路由），取消勾选后文案要跟着变。
    // 反向判据是「文案变了」而不是「复选框变了」—— 前者证明状态真的落进了
    // 组件状态，而不只是 DOM 属性被改了。
    const enableToggles = page.locator(".enable-toggle");
    assert((await enableToggles.count()) >= 1, "配置页的模型行应有启用开关");
    const firstToggle = page.locator(".enable-toggle input").first();
    assert.equal(await firstToggle.isChecked(), true, "新模型默认参与路由");
    // uncheck() 会等 actionability，这里元素可能被 sticky 头部遮住，直接派发 click。
    await firstToggle.click({ force: true });
    await page.waitForTimeout(200);
    assert((await page.locator(".enable-toggle.off").count()) >= 1, "取消勾选后应显示为已停用");
    await page.screenshot({ path: path.join(output, "model-disabled.png"), fullPage: true });
    await firstToggle.check();
    await page.waitForTimeout(150);
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "刷新列表" }).click().catch(() => {});
    // 「更多」菜单互斥：原生 <details> 各自独立，实测可同时展开多个互相压盖。
    // 判据是「同时最多一个菜单可见」，而不是「点开的那张有菜单」——
    // 后者在没有互斥时同样成立，两边都算过。
    const menuSummaries = page.locator(".provider-more > summary");
    const menuCount = await menuSummaries.count();
    assert(menuCount >= 2, `至少要有两张卡片才能验互斥，实际 ${menuCount}`);
    await menuSummaries.nth(0).click();
    await page.waitForTimeout(150);
    assert.equal(await page.locator(".provider-more[open]").count(), 1, "点开一张后只应有一个菜单展开");
    await menuSummaries.nth(1).click();
    await page.waitForTimeout(150);
    assert.equal(await page.locator(".provider-more[open]").count(), 1, "再点开第二张时，第一个必须自动收起（互斥）");
    await menuSummaries.nth(1).click();
    await page.waitForTimeout(150);
    assert.equal(await page.locator(".provider-more[open]").count(), 0, "再点一次当前这张应全部收起");
    // 置顶 / 置底必须在菜单里
    await menuSummaries.nth(0).click();
    await page.waitForTimeout(150);
    const menuText = await page.locator(".provider-more[open] .provider-more-menu").innerText();
    assert(menuText.includes("置顶") && menuText.includes("置底"), `菜单应有置顶与置底，实际：${menuText.replace(/\n/g, "|")}`);
    await page.keyboard.press("Escape");
    await page.locator("body").click({ position: { x: 5, y: 5 } }).catch(() => {});
    // 失效模型扫描：面板必须能分清「可删」与「不可删」。
    // 反向断言是重点 —— 若「目录不可用」的项也能勾选，用户一次网络抖动
    // 就能误删整家供应商的全部模型。
    await page.getByRole("button", { name: "扫描失效模型" }).click();
    await page.locator(".stale-panel").waitFor({ timeout: 5000 });
    const staleBoxes = page.locator(".stale-list input[type=checkbox]");
    assert.equal(await staleBoxes.count(), 3, "夹具给了 3 条扫描结果");
    assert.equal(await staleBoxes.nth(1).isDisabled(), true, "healthy 项不可勾选");
    assert.equal(await staleBoxes.nth(2).isDisabled(), true, "目录不可用的项不可勾选");
    await staleBoxes.nth(0).check();
    assert((await page.locator(".stale-panel").innerText()).includes("目录不可用"), "面板要说明哪些供应商未能判定");
    await page.screenshot({ path: path.join(output, "stale-models.png"), fullPage: true });

    await page.screenshot({ path: path.join(output, "providers-desktop.png"), fullPage: true });

    // D4 级联设置：只在策略选成 cascade 时出现，且改动必须往返到配置。
    //
    // 「别的档位下不渲染」这条是重点：级联的两个参数在其余七档里
    // 不参与任何决策，摆在那里就是「开着没反应的开关」。
    const strategySelect = page.getByLabel("路由策略");
    await strategySelect.selectOption("balanced");
    await page.waitForTimeout(150);
    assert.equal(await page.locator(".cascade-strip").count(), 0, "非级联档位下不该出现级联设置");
    assert.equal(
      await strategySelect.locator("option").count(), 8,
      "策略下拉必须给出全部 8 档（含 smart 与 cascade），漏档等于用户选不到",
    );

    await strategySelect.selectOption("cascade");
    const cascadeStrip = page.locator(".cascade-strip");
    await cascadeStrip.waitFor({ timeout: 5000 });
    assert.equal(
      await page.evaluate(() => window.__fixtureConfig.cascade.max_escalations), 0,
      "夹具默认必须是关闭状态",
    );
    const cascadeText = await cascadeStrip.innerText();
    assert(cascadeText.includes("当前未启用"), `max_escalations=0 时必须说明级联未启用：${cascadeText}`);
    // 代价必须写在用户看得见的地方：改这个数字会把一次请求变成多次真实账单。
    assert(/非流式/.test(cascadeText), `必须写明只在非流式上生效：${cascadeText}`);
    assert(/不可用/.test(cascadeText), `必须写明决策端点不可用时不升级：${cascadeText}`);
    // 页面滚动发生在内部容器上，`fullPage` 截不到 route-strip 这一段 ——
    // 实测第一版截图里根本没有级联区（而断言全绿）。必须滚进视口再截。
    await cascadeStrip.scrollIntoViewIfNeeded();
    assert.equal(await cascadeStrip.isVisible(), true, "级联设置必须真的渲染在视口里");
    await page.screenshot({ path: path.join(output, "cascade-settings.png") });
    await cascadeStrip.screenshot({ path: path.join(output, "cascade-strip.png") });

    await cascadeStrip.getByLabel("最多升级次数", { exact: true }).fill("2");
    await page.waitForFunction(
      () => window.__fixtureConfig?.cascade?.max_escalations === 2, null, { timeout: 5000 },
    );
    assert.equal(
      await page.evaluate(() => window.__fixtureConfig.cascade.min_confidence), 0.6,
      "改升级次数不得抹掉阈值（spread 写错就是这种症状）",
    );
    await cascadeStrip.getByLabel("置信度低于它才升级", { exact: true }).fill("0.8");
    await page.waitForFunction(
      () => window.__fixtureConfig?.cascade?.min_confidence === 0.8, null, { timeout: 5000 },
    );
    assert.equal(
      await page.evaluate(() => window.__fixtureConfig.cascade.max_escalations), 2,
      "改阈值不得抹掉升级次数",
    );
    assert((await cascadeStrip.innerText()).includes("最多升 2 次"), "说明文字要跟着实际值走");
    await cascadeStrip.scrollIntoViewIfNeeded();
    await cascadeStrip.screenshot({ path: path.join(output, "cascade-settings-enabled.png") });

    // 越界值必须被界面夹住：配置可能被手改过，而 max_escalations=9
    // 在界面上看不出任何异常，却意味着最多 10 次真实上游调用。
    await cascadeStrip.getByLabel("最多升级次数", { exact: true }).fill("9");
    await page.waitForFunction(
      () => window.__fixtureConfig?.cascade?.max_escalations === 3, null, { timeout: 5000 },
    );
    assert.equal(
      await cascadeStrip.getByLabel("最多升级次数", { exact: true }).inputValue(), "3",
      "输入框本身也要显示夹取后的值",
    );

    // 切回去：后面的断言不该被这一次策略切换污染。
    await strategySelect.selectOption("balanced");
    await page.waitForFunction(
      () => window.__fixtureConfig?.routing_strategy === "balanced", null, { timeout: 5000 },
    );

    /* ------------------------------------------------------------------ */
    /* A5 账号型上游徽标                                                    */
    /* ------------------------------------------------------------------ */
    // 夹具里两家配了 runtime_id：一张指向存在的运行时、一张指向不存在的。
    //
    // 截图前必须收起「更多」菜单：它展开时会**盖住徽标文字**，
    // 而断言读的是 innerText —— 那种情况下断言全绿、图上看不见，
    // 视觉验证就白做了（这一次真的踩到了）。
    //
    // 【为什么要循环点】这个菜单是**受控的** `<details open={...}>`，
    // Escape 与点空白都不会关它，只有点 summary 才 toggle。
    // 顺带记一笔：「Escape 关不掉菜单」本身是个可用性缺陷（已写进交接单）。
    while (await page.locator(".provider-more[open] > summary").count() > 0) {
      await page.locator(".provider-more[open] > summary").first().click();
      await page.waitForTimeout(120);
    }
    assert.equal(await page.locator(".provider-more[open]").count(), 0, "截图前菜单必须已收起");
    const runtimeBadges = page.locator(".provider-card .provider-runtime");
    assert.equal(
      await runtimeBadges.count(), 2,
      `夹具里有两家配了运行时（一条正常、一条指向不存在的），实际 ${await runtimeBadges.count()}`,
    );
    const runtimeCard = page.locator(".provider-card").filter({ hasText: "Anthropic" }).first();
    await runtimeCard.scrollIntoViewIfNeeded();
    const runtimeCardText = await runtimeCard.innerText();
    assert(runtimeCardText.includes("账号型上游"), `徽标要说明这是账号型上游：${runtimeCardText}`);
    // 只显示 id 等于没显示：用户配的是 label，界面上却是 codex-work。
    assert(runtimeCardText.includes("Codex（工作）"), `runtime_id 必须翻成人话：${runtimeCardText}`);
    assert(
      runtimeCardText.includes("不走上面的地址"),
      `必须说清请求不走 base_url —— 否则那一行「API Key：已保存」看起来仍然在生效：${runtimeCardText}`,
    );

    // 指向不存在的运行时必须在**卡片上**就报出来。
    // 少了这条，用户要等到真发请求时才看到「未知账号运行时」，离操作已经很远。
    const ghostCard = page.locator(".provider-card").filter({ hasText: "备用服务" }).first();
    const ghostText = await ghostCard.innerText();
    assert(ghostText.includes("找不到这个运行时"), `指向不存在的运行时必须当场说出来：${ghostText}`);

    // 反向：没配运行时的供应商**不该**出现这个徽标。
    const plainCard = page.locator(".provider-card").filter({ hasText: "本地 Ollama" }).first();
    assert(
      !(await plainCard.innerText()).includes("账号型上游"),
      "没配运行时的供应商不该出现这个徽标 —— 否则用户以为所有请求都走本机 CLI",
    );
    await runtimeCard.screenshot({ path: path.join(output, "provider-runtime-badge.png") });

    // 工具栏按钮组必须单行排开，且完整落在视口内。
    // 2026-10-05 用户报：标题文字长时三个按钮折成两行，「添加供应商」被挤到
    // 第二行右侧留下空档。`.row` 是 flex-wrap: wrap，只靠它自己必然折行。
    const toolbarButtons = page.locator(".providers-toolbar .row > button");
    const btnCount = await toolbarButtons.count();
    assert.equal(btnCount, 4, `工具栏应有 4 个按钮，实际 ${btnCount}`);
    const tops = [];
    for (let i = 0; i < btnCount; i++) {
      tops.push((await toolbarButtons.nth(i).boundingBox()).y);
    }
    const firstTop = tops[0];
    for (const [i, y] of tops.entries()) {
      // 同一行意味着 y 相同；差 1px 容差避免亚像素抖动误报
      assert.ok(
        Math.abs(y - firstTop) < 1.5,
        `工具栏第 ${i + 1} 个按钮换行了：y=${y.toFixed(1)}，第 1 个 y=${firstTop.toFixed(1)}`,
      );
    }
    const lastBox = await toolbarButtons.nth(btnCount - 1).boundingBox();
    const vp = page.viewportSize();
    assert.ok(
      lastBox.x + lastBox.width <= vp.width,
      `「添加供应商」超出视口右边界：右缘 ${(lastBox.x + lastBox.width).toFixed(0)}px > ${vp.width}px`,
    );
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
    assert.equal(await configured.nth(0).getByLabel("缓存命中价").inputValue(), "0.15");
    assert.equal(await configured.nth(0).getByLabel("缓存创建价").inputValue(), "1.8");
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
    assert.equal(saved.models[0].price.cache_read, 0.15);
    assert.equal(saved.models[0].price.cache_creation, 1.8);
    assert.equal(saved.models[0].price.currency, "usd");
    // 目录提供的输入长度分档必须随模型保留，长上下文估算才不会被低估。
    assert.deepEqual(saved.models[0].price.tiers, [{ min_prompt_tokens: 272000, prompt: 3, completion: 12, cache_read: 0.3, cache_creation: 3.6 }]);
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
    await page.locator(".selected-heading button").click();
    assert.equal(await page.locator(".configured-model").count(), 1);
    await page.getByRole("button", { name: "取消", exact: true }).click();
    for (const viewport of [{ width: 900, height: 650 }, { width: 390, height: 844 }]) {
      await page.setViewportSize(viewport);
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), `page overflow at ${viewport.width}`);
      // 工具栏按钮**不得折行**：2026-10-05 用户报障时三个按钮折成两行，
      // 「添加供应商」被挤到第二行左侧留下大片空档。
      //
      // 判据要量「按钮组容器宽度」而不是只量折不折 —— 折不折取决于容器有没有
      // 被左侧标题挤窄，而 flex 子项默认 min-width: auto 不会被压缩。
      // 扫描净宽区间（左侧导航 420px，工具栏净宽 = 视口 − 420）。
      const sidebarW = 420;
for (const net of [1080, 1000, 900, 820, 780, 700, 660, 620, 580, 520, 460, 420, 400]) {
        await page.setViewportSize({ width: net + sidebarW, height: 800 });
        await page.waitForTimeout(120);
        const m = await page.evaluate(() => {
          const bar = document.querySelector(".providers-toolbar");
          const row = bar && bar.querySelector(".row");
          const btns = [...document.querySelectorAll(".providers-toolbar .row > button")];
          return {
            rowW: row ? Math.round(row.getBoundingClientRect().width) : -1,
            rows: new Set(btns.map(n => Math.round(n.getBoundingClientRect().y))).size,
          };
        });
        assert.equal(m.rows, 1, `工具栏按钮在净宽 ${net}px（视口 ${net + sidebarW}）下折成 ${m.rows} 行`);
        assert.ok(m.rowW >= 290, `净宽 ${net}px 下按钮组被压到 ${m.rowW}px（应 >= 290）`);
      }
      await page.setViewportSize(viewport);
      await page.waitForTimeout(150);
      const probe1 = await page.locator(".providers-toolbar .row > button").evaluateAll(ns => ns.map(n => { const r = n.getBoundingClientRect(); return { y: Math.round(r.y), right: Math.round(r.right) }; }));
      console.log(`  [toolbar] 视口${viewport.width}px -> ${probe1.length} 个按钮, ${new Set(probe1.map(p => p.y)).size} 行, 最右 ${Math.max(...probe1.map(p => p.right))}`);
      await page.setViewportSize(viewport);
      await page.waitForTimeout(150);
      const probe2 = await page.locator(".providers-toolbar .row > button").evaluateAll(ns => ns.map(n => { const r = n.getBoundingClientRect(); return { y: Math.round(r.y), right: Math.round(r.right) }; }));
      console.log(`  [toolbar] ${viewport.width}px -> ${probe2.length} 个按钮, ${new Set(probe2.map(p => p.y)).size} 行, 最右 ${Math.max(...probe2.map(p => p.right))}`);
      // 工具栏按钮在 **900px**（用户报障时的宽度）不得折行。
      // 390px 是纵向布局，媒体查询已放开 wrap，三个按钮本就该分两行，
      // 所以只断言「都在视口内」。
      const barButtons = page.locator(".providers-toolbar .row > button");
      const barCount = await barButtons.count();
      assert.equal(barCount, 4, `工具栏应有 4 个按钮（刷新列表/刷新定价/扫描失效模型/添加供应商），实际 ${barCount} @${viewport.width}`);
      const ys = [];
      const boxes = [];
      for (let i = 0; i < barCount; i++) {
        const b = await barButtons.nth(i).boundingBox();
        ys.push(b.y); boxes.push(b);
      }
      if (viewport.width > 700) {
        for (const [i, y] of ys.entries()) {
          assert.ok(Math.abs(y - ys[0]) < 1.5, `按钮 ${i + 1} 在 ${viewport.width}px 下换行了：y=${y.toFixed(1)} vs ${ys[0].toFixed(1)}`);
        }
      }
      const lastBox = boxes[barCount - 1];
      assert.ok(lastBox.x + lastBox.width <= viewport.width + 1, `「添加供应商」超出视口 @${viewport.width}：右缘 ${(lastBox.x + lastBox.width).toFixed(0)}`);
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

    /* ------------------------------------------------------------------ */
    /* D2 能力冲突（多来源账本）                                            */
    /* ------------------------------------------------------------------ */
    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "能力与取舍", exact: true }).click();
    await page.locator(".capability-summary").waitFor({ timeout: 5000 });
    const capSummary = await page.locator(".capability-summary").innerText();
    assert(/2 个模型有能力数据/.test(capSummary), `汇总要给出模型总数：${capSummary}`);
    assert(/1 个存在冲突/.test(capSummary), `汇总要给出冲突数：${capSummary}`);
    assert(/1 个各来源一致/.test(capSummary), `汇总要给出「一致」的个数：${capSummary}`);

    const conflictCards = page.locator(".capability-card");
    assert.equal(await conflictCards.count(), 1, "夹具里只有一个真冲突（coding 0.9 vs 0.4）");
    const conflictText = await conflictCards.first().innerText();
    assert(conflictText.includes("openrouter/openrouter/free"), conflictText);
    assert(conflictText.includes("有冲突"), `必须标出「有冲突」：${conflictText}`);
    // 卡片要求的正是这一条：**把各来源的值都列出来**。
    // 只给胜出者的话，用户看到「我填的没生效」时没有任何线索。
    assert(
      conflictText.includes("0.90") && conflictText.includes("0.40"),
      `各来源的值都要列出来：${conflictText}`,
    );
    assert(
      conflictText.includes("手工") && conflictText.includes("社区"),
      `来源徽标都要有：${conflictText}`,
    );
    assert(conflictText.includes("生效"), `必须标出哪个来源生效：${conflictText}`);
    // 反向断言：两个来源给出**同一个值**不是冲突。
    // 少了这一条，把「互相印证」也标成冲突的实现照样全绿。
    assert(!conflictText.includes("claude-sonnet"), "一致的模型不该出现在冲突卡里");
    const agreeing = page.locator(".capability-agreeing");
    assert.equal(await agreeing.count(), 1, "一致的模型要收进折叠区，而不是消失");
    // 必须**展开**再读：折叠的 `<details>` 里 `innerText` 拿不到内容
    // （它只返回渲染出来的文本），断言会以「没列出」的形式假红。
    await agreeing.locator("summary").click();
    await page.waitForTimeout(150);
    assert((await agreeing.innerText()).includes("claude-sonnet"), "展开后要列出它");
    await page.screenshot({ path: path.join(output, "capability-conflicts.png"), fullPage: true });

    /* ------------------------------------------------------------------ */
    /* 本地模型与智能模式                                                  */
    /* ------------------------------------------------------------------ */
    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "本地模型与智能", exact: true }).click();
    // 端点表：不可达的那行必须带原因，只显示「不可达」等于让用户自己去猜。
    // `tr:has-text()` 会连表头一起匹配到，用「整行文本完全等于」收紧。
    const runtimeRow = (url) => page.locator("tr").filter({
      has: page.locator(`td.mono:text-is("${url}")`),
    });
    const deadRow = runtimeRow("http://127.0.0.1:1");
    await deadRow.first().waitFor();
    assert((await deadRow.first().innerText()).includes("连接被拒绝"), `不可达端点必须显示具体原因：${await deadRow.first().innerText()}`);
    // 模型表：同族的两个模型只有 vision 不同，能力列必须按上游元数据区分。
    const q4 = page.locator("tr").filter({ has: page.locator(`td:text-is("qwen3.8:27b-q4_K_M")`) }).first();
    const q3 = page.locator("tr").filter({ has: page.locator(`td:text-is("batiai/qwen3.8-27b:q3")`) }).first();
    await q4.waitFor();
    await q3.waitFor();
    const q4Text = await q4.innerText();
    const q3Text = await q3.innerText();
    assert(q4Text.includes("视觉"), `有 vision 的模型必须标出来：${q4Text}`);
    assert(!q3Text.includes("视觉"), `上游说没有 vision 就不许显示有：${q3Text}`);
    assert(q4Text.includes("思考") && q4Text.includes("16.54 GB"), `磁盘占用与思考能力都要可见：${q4Text}`);
    await page.screenshot({ path: path.join(output, "local-models-desktop.png"), fullPage: true });

    // 智能模式页。tab 按钮同时带标签和提示文字，用 getByRole(name) 会被两个文本一起匹配。
    await page.locator(".local-tab").filter({ hasText: "智能模式" }).click();
    await page.getByText("Jev 决策端点", { exact: false }).first().waitFor();
    // 「置信度 ≠ 正确性」这条实测结论必须写在界面上，否则用户只会去调阈值。
    const smartCard = page.locator(".card").filter({ hasText: "智能模式" }).first();
    const smartText = await smartCard.innerText();
    assert(smartText.includes("置信度高不等于判得对"), `实测结论必须写进界面：${smartText}`);

    // 提示词预优化：先验关闭态——开关关着时其余输入框必须是禁用的。
    const refineCard = page.locator(".card").filter({ hasText: "提示词预优化" });
    await refineCard.waitFor();
    // 必须在界面上说明「Jev 产不出文本」，否则用户会以为开了就有改写。
    assert((await refineCard.innerText()).includes("产不出文本"), "必须说明 Jev 只判要不要改，不产文本");
    assert.equal(await refineCard.getByRole("checkbox").isChecked(), false, "预优化默认必须关闭");
    assert.equal(await refineCard.getByLabel("硬超时（毫秒）", { exact: true }).isDisabled(), true, "开关关着时其余字段应禁用");

    // 打开开关 → 字段解禁 → 改值 → 保存 → 配置里真的变了（往返）。
    //
    // 用 click + 轮询而不是 check()：`check()` 点击后**立刻**读 checked，
    // 而保存是异步的（onChange → patch → IPC → setCfg），
    // 那一刻读到的还是旧值，于是 check 判定「状态没变」并重试，
    // 重试的第二次点击又把它点回去了。这是竞态，不是应用缺陷。
    const refineCheckbox = refineCard.getByRole("checkbox");
    await refineCheckbox.click();
    await page.waitForFunction(
      () => window.__fixtureConfig?.smart_routing?.prompt_refine?.enabled === true,
      null, { timeout: 5000 },
    );
    await page.waitForTimeout(200);
    assert.equal(await refineCheckbox.isChecked(), true, "开关打开后受控状态必须跟着变");
    assert.equal(await refineCard.getByLabel("硬超时（毫秒）", { exact: true }).isDisabled(), false, "开关打开后字段必须解禁");

    await refineCard.getByLabel("硬超时（毫秒）", { exact: true }).fill("3500");
    await refineCard.getByLabel("含糊阈值（clarity 低于则改写）", { exact: true }).fill("0.6");
    await page.waitForFunction(
      () => window.__fixtureConfig?.smart_routing?.prompt_refine?.timeout_ms === 3500,
      null, { timeout: 5000 },
    );
    assert.equal(await page.evaluate(() => window.__fixtureConfig.smart_routing.prompt_refine.clarity_noul), 0.6);
    // 改别的字段时不得把预优化其它字段抹掉（spread 写错就是这种症状）。
    assert.equal(await page.evaluate(() => window.__fixtureConfig.smart_routing.prompt_refine.min_chars), 24, "改预优化时其它字段必须保持");
    // 回到配置再读一次，确认是持久化的结果而不是组件内的临时状态。
    assert.equal(await page.evaluate(() => window.__fixtureConfig.smart_routing.jev.model), "rl-agent", "改预优化不得影响同一对象里的其它配置段");
    await page.screenshot({ path: path.join(output, "prompt-refine-desktop.png"), fullPage: true });

    // 试跑分类器：必须显示 needs_refine（这是预优化的触发条件，界面上看不见就没法调）。
    await page.getByRole("button", { name: /查看 Jev 原始判定|试跑/ }).first().click();
    await page.getByText("值得改写", { exact: false }).first().waitFor();
    const probeCard = page.locator(".card").filter({ hasText: "值得改写" }).last();
    const probeText = await probeCard.innerText();
    // 判定来源必须写出来，否则用户分不清「启发式兜底」和「Jev 判的」。
    assert(/硬规则|Jev 决策|启发式/.test(probeText), `判定来源必须可见：${probeText}`);
    await page.screenshot({ path: path.join(output, "smart-probe-desktop.png"), fullPage: true });

    // 校准面板：必须真的跑一次，并把「净收益为负」如实显示。
    const calibCard = page.locator(".card").filter({ hasText: "校准：这个决策模型到底值不值得用" });
    await calibCard.waitFor();
    const calibIntro = await calibCard.innerText();
    assert(calibIntro.includes("只跑样本"), "必须说明校准不改配置：缺这句用户会以为命令擅自调了阈值");
    assert(calibIntro.includes("已知错判样本"), "必须说明默认样本里刻意留了错判那条");
    await calibCard.getByRole("button", { name: "用实测样本校准", exact: true }).click();
    await calibCard.locator(".calib-verdict").waitFor();
    const verdict = await calibCard.locator(".calib-verdict").innerText();
    assert(verdict.includes("净收益"), `结论必须给出净收益：${verdict}`);
    // 净收益为负要显示成错误态，不能是中性色——那是"别用它"的信号。
    const verdictClass = await calibCard.locator(".calib-verdict").getAttribute("class");
    assert(verdictClass.includes("err"), `净收益为负必须标成错误态：${verdictClass}`);
    // 混淆矩阵：3×3 里只该点亮实际出现的格子。
    const hit1 = await calibCard.locator(".calib-matrix td.hit").count();
    const miss = await calibCard.locator(".calib-matrix td.miss").count();
    assert.equal(hit1, 2, "矩阵里判对的格子数");
    assert.equal(miss, 1, "矩阵里判错的格子数");
    assert((await calibCard.locator(".calib-numbers").innerText()).includes("-1"), "净收益数字要可见");
    // 最危险的那条错判必须点名——它是"调阈值解决不了"的证据。
    const calibText = await calibCard.innerText();
    assert(calibText.includes("0.747"), `最高错判置信度要可见：${calibText.slice(0, 400)}`);
    assert(calibText.includes("高置信度不等于判得对"), "必须点明核心结论");
    await page.screenshot({ path: path.join(output, "calibration-desktop.png"), fullPage: true });

    // 自动拉起：默认关闭时其余字段必须禁用，且界面上要写明不可逆。
    const autoStartCard = page.locator(".card").filter({ hasText: "端点不可达时自动拉起" });
    await autoStartCard.waitFor();
    assert((await autoStartCard.innerText()).includes("不可逆"), "必须写明启动外部进程不可逆");
    assert.equal(await autoStartCard.getByRole("checkbox").isChecked(), false, "自动拉起默认必须关闭");
    // 关闭时整块路径输入**不渲染**（而不是 disabled）：
    // 渲染出来会让用户以为填了就生效。
    assert.equal(await autoStartCard.getByLabel("edgejev.exe 路径", { exact: true }).count(), 0, "开关关着时不该出现路径输入框");
    await autoStartCard.getByRole("checkbox").check();
    await page.waitForTimeout(200);
    const exePathBox = autoStartCard.getByLabel("edgejev.exe 路径", { exact: true });
    assert.equal(await exePathBox.count(), 1, "开关打开后路径输入框必须出现");
    assert.equal(await exePathBox.isDisabled(), false, "开关打开后路径字段可填");
    // 开关状态必须真的落到配置，而不是组件内临时状态。
    await page.waitForFunction(() => window.__fixtureConfig?.smart_routing?.jev?.auto_start?.enabled === true, null, { timeout: 5000 });
    assert.equal(await page.evaluate(() => window.__fixtureConfig.smart_routing.jev.base_url), "http://127.0.0.1:8009/v1/systemone", "开自动拉起不得改掉端点地址");
    assert.equal(await page.evaluate(() => window.__fixtureConfig.smart_routing.jev.auto_start.port), 8009, "端口默认值必须保留");
    // 明细必须能展开看逐条，否则用户无法自己核对。
    await calibCard.locator("details > summary").click();
    const detail = await calibCard.locator("details").innerText();
    assert(detail.includes("Jev 弃权"), "弃权条目要写明原因：${detail}");
    assert(detail.includes("置信度不足"), "弃权原因要具体：${detail}");

    // 联网搜索：Key 必须只显示掩码，完整值绝不能出现在界面上。
    await page.locator(".local-tab").filter({ hasText: "联网搜索" }).click();
    // 掩码只出现在 password 输入框的 placeholder 上，不在文本里——
    // 所以断言必须读属性，读 textContent 会永远为假。
    const keyBox = page.locator(".workspace-content input[type=password]").first();
    await keyBox.waitFor({ timeout: 5000 });
    const masked = await keyBox.getAttribute("placeholder");
    assert(masked === "tvly-****-abc", `Key 必须只以掩码出现：${masked}`);
    assert(!(await page.locator(".workspace-content").innerText()).includes("tvyl-abcdef123456"), "完整 Key 绝不能出现在界面上");

    // 「测试后端」是卡片标题（<h3>），触发按钮叫「搜索一次」。
    // 真跑一次搜索：结果必须逐条列出标题与链接，命中条数也要回显，
    // 否则用户无从判断后端是不是真的活着。

    // 真跑一次搜索：结果必须逐条列出标题与链接。
    await page.getByRole("textbox").last().fill("edgeJev 本机部署");
    await page.getByRole("button", { name: "搜索一次", exact: true }).click();
    await page.getByText("命中 2 条", { exact: false }).first().waitFor();
    const resultText = await page.locator(".local-result").first().innerText();
    assert(resultText.includes("edgeJev 部署说明"), `结果标题必须可见：${resultText}`);
    assert(resultText.includes("https://example.test/jev"), `结果链接必须可见：${resultText}`);
    await page.screenshot({ path: path.join(output, "web-search-desktop.png"), fullPage: true });

    // 后端下拉必须包含全部五个，且**值必须与后端 serde 名逐字一致**。
    // 写错值（bingcn / searxng）的后果不是「选不中」，而是保存后整个应用起不来。
    const backendOptions = await page.locator(".workspace-content select option").evaluateAll((els) =>
      els.map((e) => ({ value: e.getAttribute("value"), label: e.textContent ?? "" })),
    );
    const backendValues = backendOptions.map((o) => o.value);
    for (const expected of ["bing_cn", "duck_duck_go", "tavily", "brave", "sear_xng"]) {
      assert(backendValues.includes(expected), `后端下拉缺少 ${expected}，实际：${backendValues.join(",")}`);
    }
    assert(
      !backendValues.includes("bingcn") && !backendValues.includes("searxng"),
      `下拉里混进了错误拼法（会让应用起不来）：${backendValues.join(",")}`,
    );
    const bingLabel = backendOptions.find((o) => o.value === "bing_cn")?.label ?? "";
    assert(bingLabel.includes("必应"), `必应应有中文标签，实际：${bingLabel}`);
    await page.screenshot({ path: path.join(output, "search-backends.png"), fullPage: true });

    // DOM 属性断言过了**不等于人能看见** —— 下拉默认收起，截图里根本看不到选项。
    // 所以真选中一次再截，让必应中国出现在交付证据里而不只活在断言里。
    //
    // `selectOption` 之后**必须等保存落库**再读控件值：onChange → save() → IPC →
    // setCfg 是异步的，中间那一瞬组件重渲染，读到的仍是旧值。直接
    // `inputValue()` 会读到 `tavily` 而失败（实测踩过）。
    // 页面上有**多个** select（端点选择器在后端下拉之前），
    // 用 `.first()` 会选到端点选择器 —— 实测踩过：保存成功了但断言读的是另一个控件。
    // 必须按「含 bing_cn 选项」这个语义定位。
    const backendSelect = page.locator(".workspace-content select").filter({ has: page.locator("option[value=bing_cn]") });
    assert.equal(await backendSelect.count(), 1, "必须唯一定位到后端下拉");
    await backendSelect.selectOption("bing_cn");
    await page.waitForFunction(
      () => window.__fixtureConfig?.search?.backend === "bing_cn",
      null,
      { timeout: 5000 },
    );
    await backendSelect.waitFor({ state: "visible" });
    const calls = await page.evaluate(() => window.__fixtureDiag || []);
    assert.deepEqual(calls.slice(-1), ["bing_cn"], `后端切换必须真的发出 IPC：实际调用=${JSON.stringify(calls)}`);
    // 断言的是**控件当前值**而不是「保存调用发生过」：
    // 后端已经成功收到 bing_cn 但控件还显示 tavily，就是保存结果没回填到界面。
    assert.equal(
      await backendSelect.inputValue(),
      "bing_cn",
      `保存完成后控件值必须是 bing_cn，实际=${await backendSelect.inputValue()}`,
    );
    const bingHint = await page.locator(".workspace-content").innerText();
    assert(
      !bingHint.includes("必应中国") || bingHint.includes("免 Key"),
      `选中必应中国后不应出现需要 Key 的措辞：${bingHint.slice(0, 300)}`,
    );
    await page.screenshot({ path: path.join(output, "search-backend-bing-cn.png"), fullPage: true });

    // ── SearXNG 实例地址输入框（用户 2026-10-05 报告：选了 SearXNG 但没法填地址）──
    // 根因：后端 search::validate() 规定「选了 SearXNG 而 searxng_url 为空 → 整条保存拒绝」，
    // 而输入框的显隐条件是 `settings.backend === "sear_xng"`（**已保存**的值）。
    // 于是：选中的保存必然失败 → settings.backend 不变 → 输入框永不出现 → 用户被锁死。
    //
    // 所以断言必须打在「保存**之前**输入框就出现」上 ——
    // 等保存成功再断言，等于把 bug 本身当成了通过条件。
    await backendSelect.selectOption("sear_xng");
    const searxInput = page.getByPlaceholder("http://127.0.0.1:8888");
    await searxInput.waitFor({ timeout: 5000 });
    assert.equal(await searxInput.count(), 1, "刚选中 SearXNG（尚未保存）就必须出现地址输入框");
    // 输入框出现时，**已保存**的后端仍应停在旧值 —— 地址为空时后端必然拒绝。
    // 这两条合起来才是修复点：输入框不依赖保存（靠 shownBackend），提交仍然延后。
    //
    // 刻意**不**断言「有没有发过 IPC」：守卫去掉后它同样为 0（因为压根没发），
    // 断言它等于什么都测不到 —— 实测踩过：注入回归后测试仍然绿。
    // 真正的不变量是「输入框出现 ⟹ 用户能填」，由上面那条 waitFor + count 守住。
    const savedBeforeFill = await page.evaluate(() => window.__fixtureConfig?.search?.backend);
    assert.notEqual(
      savedBeforeFill,
      "sear_xng",
      `地址为空时后端不该被切成 SearXNG（配置仍应是 ${savedBeforeFill}）`,
    );
    // 真填一次并落库，确认这条路径通（不是只渲染了一个框）。
    await searxInput.fill("http://127.0.0.1:8888");
    await page.waitForFunction(
      () => window.__fixtureConfig?.search?.searxng_url === "http://127.0.0.1:8888",
      null,
      { timeout: 5000 },
    );
    // 后端也必须同步切过去：只存地址不改后端的话，界面显示的选中态是假的。
    await page.waitForFunction(
      () => window.__fixtureConfig?.search?.backend === "sear_xng",
      null,
      { timeout: 5000 },
    );
    await page.screenshot({ path: path.join(output, "search-backend-searxng.png"), fullPage: true });

    // 对照组：切走 SearXNG 后这个输入框必须消失 —— 免 Key 后端下留个
    // 无用的地址框会让人误以为所有后端都要配地址。
    await backendSelect.selectOption("bing_cn");
    await page.waitForFunction(
      () => window.__fixtureConfig?.search?.backend === "bing_cn",
      null,
      { timeout: 5000 },
    );
    assert.equal(
      await page.getByPlaceholder("http://127.0.0.1:8888").count(),
      0,
      "切走后端后 SearXNG 地址框必须消失",
    );
    await backendSelect.selectOption("bing_cn");
    // 对照组：换回需要 Key 的后端，Key 输入框必须重新出现。
    // 只断言「免 Key 时不提示」是不够的 —— 那可能只是因为输入框压根没渲染。
    await backendSelect.selectOption("tavily");
    await page.waitForFunction(
      () => window.__fixtureConfig?.search?.backend === "tavily",
      null,
      { timeout: 5000 },
    );
    assert.equal(await backendSelect.inputValue(), "tavily", "保存完成后应能切回 Tavily");
    const tavilyKeyBox = page.locator(".workspace-content input[type=password]").first();
    await tavilyKeyBox.waitFor({ timeout: 5000 });
    assert(
      (await tavilyKeyBox.getAttribute("placeholder")) === "tvly-****-abc",
      "切回 Tavily 后掩码 Key 框必须重新出现",

    );
    for (const width of [900, 390]) {
      await page.setViewportSize({ width, height: 844 });
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), `local-models overflow at ${width}`);
      await page.screenshot({ path: path.join(output, `local-models-${width}.png`), fullPage: true });
    }
    await page.setViewportSize({ width: 1440, height: 1000 });

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

    // ---------- B3 审计筛选与导出 ----------
    // 筛选器必须真的改变结果集。反向断言是重点：夹具里的 query_requests
    // 若不按条件过滤（一律返回全部），「筛掉了一些行」这件事就永远成立不了。
    const rowCount = () => page.locator(".requests-table tbody tr.request-row").count();
    const allRows = await rowCount();
    assert(allRows >= 3, `素材至少 3 条，实际 ${allRows}`);
    // 命中总数要露出来 —— 只看当前页会让人以为「一共就这么点」
    const filterBar = page.locator(".audit-filters");
    assert((await filterBar.innerText()).includes("命中"), "筛选栏必须显示命中总数");

    await page.locator("#audit-only-errors").check();
    await page.waitForTimeout(400);
    const errRows = await rowCount();
    assert(errRows > 0 && errRows < allRows,
      `「只看有错误」必须筛掉一部分：全部 ${allRows} → 现在 ${errRows}`);
    await page.screenshot({ path: path.join(output, "audit-filter-errors.png"), fullPage: true });

    // 再叠一个「只看降级过」，结果只能更少（条件是 AND）
    await page.locator("#audit-only-fallbacks").check();
    await page.waitForTimeout(400);
    const bothRows = await rowCount();
    assert(bothRows <= errRows, `叠加条件后不该变多：${errRows} → ${bothRows}`);

    // 重置必须回到全量
    await page.locator("#audit-reset").click();
    await page.waitForTimeout(400);
    assert.equal(await rowCount(), allRows, "重置筛选后必须回到全部行");

    // 按状态类筛选：夹具里有一条 401
    await page.locator("#audit-status").fill("4xx");
    await page.waitForTimeout(400);
    const fourXX = await rowCount();
    assert(fourXX > 0 && fourXX < allRows, `4xx 应筛出部分行，实际 ${fourXX}/${allRows}`);
    await page.locator("#audit-status").fill("5xx");
    await page.waitForTimeout(400);
    assert.equal(await rowCount(), 0, "夹具里没有 5xx，应筛成空表");
    assert((await page.locator(".empty").innerText()).includes("暂无"),
      "筛成空表时要显示空态，而不是一张没有表头的怪表");
    await page.locator("#audit-reset").click();
    await page.waitForTimeout(400);

    // 导出：CSV 要带伴随列说明，JSONL 不带 —— 与后端同口径
    await page.locator("#audit-export-csv").click();
    await page.waitForTimeout(400);
    const csvExport = await page.evaluate(() => window.__lastExport);
    assert(csvExport && csvExport.format === "csv", `应记录一次 CSV 导出：${JSON.stringify(csvExport)}`);
    assert((await page.locator(".msg.ok").innerText()).includes("_columns.md"),
      "CSV 导出的提示里要带列说明文件路径");

    await page.locator("#audit-export-jsonl").click();
    await page.waitForTimeout(400);
    const jsonlExport = await page.evaluate(() => window.__lastExport);
    assert(jsonlExport && jsonlExport.format === "jsonl", "应记录一次 JSONL 导出");
    const okText = await page.locator(".msg.ok").innerText();
    assert(!okText.includes("_columns.md"), `JSONL 不该有列说明文件：${okText}`);

    // 导出要带上当前筛选条件：先筛再导，写出去的条件必须与界面一致
    await page.locator("#audit-only-errors").check();
    await page.waitForTimeout(400);
    await page.locator("#audit-export-jsonl").click();
    await page.waitForTimeout(400);
    const filteredExport = await page.evaluate(() => window.__lastExport);
    assert(filteredExport.filter && filteredExport.filter.only_errors === true,
      `导出必须带上当前筛选条件：${JSON.stringify(filteredExport.filter)}`);
    await page.locator("#audit-reset").click();
    await page.waitForTimeout(400);
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
    // B2 预算闸门与模型白名单：列表必须先能显示出这两列。
    // 反向断言是重点 —— 夹具漏字段时前端读 `undefined.length` 会整页白屏，
    // 而「页面还在、表还在」这件事本身要能被断言到。
    // 用夹具里唯一的行标签定位，而不是按卡片标题文本找 ——
    // 表格所在的那张卡里并没有「远程访问 Key」这几个字（实测定位器超时）。
    const keyCard = page.locator("table").filter({ hasText: "受限设备" });
    await keyCard.waitFor({ timeout: 5000 });
    const keyText = await keyCard.innerText();
    assert(keyText.includes("月度预算") && keyText.includes("模型白名单"),
      `远程 Key 表必须有预算与白名单两列，实际：${keyText.replace(/\n/g, "|")}`);
    // micros 要按元显示，不能把 5000000 直接印出来
    assert(keyText.includes("5 USD"), `预算要按元显示，实际：${keyText.replace(/\n/g, "|")}`);
    assert(keyText.includes("gpt-4o, claude-*"), `白名单要逐条显示，实际：${keyText.replace(/\n/g, "|")}`);
    // 「不限」必须显示成不限，而不是 0
    const openRow = keyCard.locator("tr").filter({ hasText: "不限额设备" });
    assert((await openRow.innerText()).includes("不限"),
      "未设预算的 Key 要显示「不限」，显示 0 会让人以为上限是 0");
    await page.screenshot({ path: path.join(output, "remote-key-budget.png"), fullPage: true });

    // 打开编辑器：三个新字段都要在，且要把 micros 还原成元
    await keyCard.locator("tr").filter({ hasText: "受限设备" })
      .getByRole("button", { name: "编辑", exact: true }).click();
    const budgetInput = page.locator("#remote-key-budget");
    await budgetInput.waitFor({ timeout: 5000 });
    assert.equal(await budgetInput.inputValue(), "5",
      "5000000 micros 必须回填成 5（元），直接印 micros 会让用户改错量级");
    assert.equal(await page.locator("#remote-key-currency").inputValue(), "USD");
    assert.equal(await page.locator("#remote-key-models").inputValue(),
      "gpt-4o, claude-*");
    await page.screenshot({ path: path.join(output, "remote-key-budget-editor.png"), fullPage: true });

    // 改预算后保存，列表要跟着变 —— 证明 change 真的落到了夹具（= 后端）。
    await budgetInput.fill("12.5");
    await page.getByRole("button", { name: "保存更改", exact: true }).click();
    await page.waitForTimeout(300);
    assert((await keyCard.innerText()).includes("12.5 USD"),
      `保存后列表要显示新预算，实际：${(await keyCard.innerText()).replace(/\n/g, "|")}`);
    // 反向：清空预算表示「不限」，不能变成 0
    await keyCard.locator("tr").filter({ hasText: "受限设备" })
      .getByRole("button", { name: "编辑", exact: true }).click();
    await page.locator("#remote-key-budget").fill("");
    await page.getByRole("button", { name: "保存更改", exact: true }).click();
    await page.waitForTimeout(300);
    const clearedRow = keyCard.locator("tr").filter({ hasText: "受限设备" });
    assert((await clearedRow.innerText()).includes("不限"),
      "清空预算必须回到「不限」，而不是变成 0 元上限");

    assert.equal(await page.getByLabel("OpenCode", { exact: true }).count(), 1, "接管面板必须提供 OpenCode");
    assert.equal(await page.getByLabel("Crush", { exact: true }).count(), 1, "接管面板必须提供 Crush");
    await page.getByRole("button", { name: "备份并写入配置", exact: true }).click();
    await page.getByText("C:/fixture/.codex/config.toml.backup-test", { exact: true }).waitFor();
    // CLI 检测：先只读检测（未安装的工具不得伪造成已安装），再查最新版本并更新与安装。
    await page.getByRole("button", { name: "检测本机 CLI", exact: true }).click();
    const cliCard = page.locator(".card").filter({ hasText: "本机 CLI 工具" });
    await cliCard.locator("table").waitFor();
    let cliText = await cliCard.innerText();
    assert(cliText.includes("Claude Code") && cliText.includes("2.0.0"), cliText);
    assert(cliText.includes("未检测到"), cliText);
    // 两类安装来源都要在界面上标出来，用户才能判断安装方式。
    assert(cliText.includes("官方脚本"), `脚本类工具必须标注来源：${cliText}`);
    // 未安装（没有本机路径可显示）时必须显示安装目标，而不是留空。
    assert((await cliCard.locator("tr").filter({ hasText: "Gemini CLI" }).innerText()).includes("@google/gemini-cli"), "未安装的 npm 工具要显示包名");
    assert((await cliCard.locator("tr").filter({ hasText: "Grok Build" }).innerText()).includes("官方安装脚本"), "未安装的脚本类工具要显示安装目标");
    // 逻辑自洽：每个按钮都能完成它声称的事。未查询最新版本时已安装工具显示「重新安装」（装到最新版），不是禁用的「更新」。
    const codexRow = cliCard.locator("tr").filter({ hasText: "Codex CLI" });
    assert.equal(await codexRow.getByRole("button", { name: "重新安装", exact: true }).isEnabled(), true, "未查询最新版本时也应能重新安装到最新版");
    assert.equal(await cliCard.getByRole("button", { name: "更新", exact: true }).count(), 0, "没有查到新版本时不应出现「更新」按钮");
    // 缺前置条件的分支：按钮禁用且卡片上必须给出原因。
    const continueRow = cliCard.locator("tr").filter({ hasText: "Continue CLI" });
    assert.equal(await continueRow.getByRole("button", { name: "安装", exact: true }).isDisabled(), true, "缺前置条件时必须禁用按钮");
    assert(cliText.includes("无法一键安装或更新"), `禁用原因必须显示在卡片上：${cliText}`);
    // 未安装的 npm 工具可以直接下载安装；脚本类已安装的提供「重新安装」且不比对版本。
    const geminiRow = cliCard.locator("tr").filter({ hasText: "Gemini CLI" });
    assert.equal(await geminiRow.getByRole("button", { name: "安装", exact: true }).isEnabled(), true, "未安装的 npm 工具必须可安装");
    const grokRow = cliCard.locator("tr").filter({ hasText: "Grok Build" });
    assert.equal(await grokRow.getByRole("button", { name: "安装", exact: true }).isEnabled(), true, "未安装的脚本类工具必须可安装");
    const cursorRow = cliCard.locator("tr").filter({ hasText: "Cursor CLI" });
    assert.equal(await cursorRow.getByRole("button", { name: "重新安装", exact: true }).isEnabled(), true, "脚本类已安装工具必须可重新安装");
    assert((await cursorRow.innerText()).includes("以脚本为准"), "脚本类不比对版本，必须说明以脚本为准");
    assert.equal(await page.evaluate(() => window.__fixtureInstalledCli ?? null), null, "检测阶段不得触发安装");
    await page.getByRole("button", { name: "检测并检查更新", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "可更新 1 个" }).waitFor();
    cliText = await cliCard.innerText();
    assert(cliText.includes("2.1.97"), cliText);
    assert(cliText.includes("可更新"), cliText);
    // 未安装的工具也要能看到将安装的版本，用户才知道点「安装」会装什么。
    assert((await geminiRow.innerText()).includes("0.59.0"), `未安装工具应显示可安装版本：${await geminiRow.innerText()}`);
    // 已安装且已是最新的工具不能提示「可更新」。
    const codexQueried = await codexRow.innerText();
    assert(codexQueried.includes("0.44.0"), codexQueried);
    assert(!codexQueried.includes("可更新"), `最新版本不应提示可更新：${codexQueried}`);
    await cliCard.locator("tr").filter({ hasText: "Claude Code" }).getByRole("button", { name: "更新", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "安装命令已执行" }).waitFor();
    assert.equal(await page.evaluate(() => window.__fixtureInstalledCli), "claude_code");
    // 更新后必须重新检测：版本刷新为新值，且不再提示可更新。
    const updatedRow = await cliCard.locator("tr").filter({ hasText: "Claude Code" }).innerText();
    assert(updatedRow.includes("2.1.97"), `更新后应重新检测出版本：${updatedRow}`);
    assert(!updatedRow.includes("可更新"), `更新后不应再提示可更新：${updatedRow}`);
    // 下载安装：未安装的 npm 工具点「安装」后，重新检测必须显示已安装。
    await geminiRow.getByRole("button", { name: "安装", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "Gemini CLI 安装命令已执行" }).waitFor();
    assert.equal(await page.evaluate(() => window.__fixtureInstalledCli), "gemini_cli");
    const geminiAfter = await cliCard.locator("tr").filter({ hasText: "Gemini CLI" }).innerText();
    assert(geminiAfter.includes("已安装"), `安装后应重新检测出新状态：${geminiAfter}`);
    assert(!geminiAfter.includes("未检测到"), `安装后不应仍显示未检测到：${geminiAfter}`);
    // 脚本类：确认弹窗自动接受后执行官方安装脚本命令，并回显输出。
    await grokRow.getByRole("button", { name: "安装", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "Grok Build 安装命令已执行" }).waitFor();
    assert.equal(await page.evaluate(() => window.__fixtureInstalledCli), "grok_build");
    await page.screenshot({ path: path.join(output, "cli-tools-desktop.png"), fullPage: true });
    // 桌宠与 AI 监控：状态、进程列表、结束进程、Petdex 安装都必须走真实 IPC。
    const petCard = page.locator('[data-testid="pet-card"]');
    await petCard.scrollIntoViewIfNeeded();
    let petText = await petCard.innerText();
    assert(petText.includes("工作中"), `桌宠状态应显示工作中：${petText}`);
    assert(petText.includes("Codex CLI") && petText.includes("4321"), `运行中的 AI 工具必须列出进程与 PID：${petText}`);
    // 监控范围必须覆盖 AI 桌面应用，并标注类型（CLI / 桌面应用）；多进程按软件聚合。
    assert(petText.includes("Qoder IDE") && petText.includes("5678"), `AI 桌面应用必须在监控列表中：${petText}`);
    assert(petText.includes("CLI") && petText.includes("桌面应用"), `进程类型必须标注：${petText}`);
    assert(petText.includes("1 个"), `聚合视图必须显示进程数：${petText}`);
    assert(petText.includes("Snow Plum Lillia"), `已安装宠物必须列出：${petText}`);
    // 任务列表：来自工具会话日志，必须显示状态与项目（宠物动作据此切换）。
    assert(petText.includes("Qoder CLI") && petText.includes("进行中"), `任务状态必须展示：${petText}`);
    assert(petText.includes("Codex Desktop"), `任务前必须标记实际 AI 软件名：${petText}`);
    assert(petText.includes("清理界面乱码与提交历史"), `任务必须展示具体内容：${petText}`);
    assert(petText.includes("完善桌宠任务列表") && petText.includes("检查窗口定位回归"), `同一软件的任务必须全部列出：${petText}`);
    assert(petText.includes("最近内容" ) || petText.includes("已完成 configHash 调整"), `任务必须展示最近内容：${petText}`);
    assert(petText.includes("C:\\Users\\fixture"), `任务项目必须展示：${petText}`);
    // 任务表截图：状态标签必须是单行胶囊（不能把「进行中」竖排）。
    const statusChip = petCard.locator('[data-testid="pet-card-task-status"]').first();
    const chipBox = await statusChip.boundingBox();
    assert(
      chipBox.height < 32 && chipBox.width > 56,
    );
    await petCard.locator(".pet-task-table").screenshot({ path: path.join(output, "pet-task-table.png") });
    // 任务行定向操作：打开指定任务、打开项目目录，不依赖任意 PID。
    const qoderTaskRow = petCard.locator('[data-testid="pet-card-task-row"]').filter({ hasText: "Qoder CLI" });
    await qoderTaskRow.getByRole("button", { name: "定位", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "已打开任务 sess-1" }).waitFor();
    assert.deepEqual(await page.evaluate(() => window.__fixtureOpenedTask), { toolId: "qoder", sessionId: "sess-1" }, "任务定位必须下发任务标识与会话 ID");
    const codexTaskRow = petCard.locator('[data-testid="pet-card-task-row"]').filter({ hasText: "完善桌宠任务列表" });
    await codexTaskRow.getByRole("button", { name: "打开任务", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "已打开任务 sess-codex" }).waitFor();
    assert.deepEqual(await page.evaluate(() => window.__fixtureOpenedTask), { toolId: "codex_desktop", sessionId: "sess-codex" }, "Codex 任务必须走官方深链参数");
    await qoderTaskRow.getByRole("button", { name: "项目", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "已打开项目目录" }).waitFor();
    assert.equal(await page.evaluate(() => window.__fixtureOpenedProject), "C:\\Users\\fixture", "项目操作必须下发任务项目路径");
    // 宠物选择用下拉，选择后必须持久化（桌宠窗口据此切换）。
    await petCard.getByTestId("pet-select").selectOption("snow-plum-lillia");
    assert.equal(await page.evaluate(() => localStorage.getItem("llm-gateway-pet-slug")), "snow-plum-lillia", "选择宠物必须持久化");
    await petCard.getByRole("button", { name: "开启桌宠", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "桌宠已开启" }).waitFor();
    assert.equal(await page.evaluate(() => window.__fixturePetWindowOpen), true, "开启桌宠必须调用 open_pet_window");
    // 大小调整：滑块必须实时下发到窗口。
    await petCard.locator("#pet-scale").fill("150");
    assert.equal(await petCard.locator("#pet-scale").getAttribute("min"), "50", "大小滑块最小必须是 50%");
    await page.waitForTimeout(300);
    assert.equal(await page.evaluate(() => window.__fixturePetScale), 1.5, "滑块必须把缩放比例下发给桌宠窗口");
    assert((await petCard.innerText()).includes("150%"), "界面必须回显当前大小");
    const codexProcessRow = petCard.locator("tr").filter({ hasText: "Codex CLI" });
    await codexProcessRow.getByRole("button", { name: "结束", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "已结束" }).waitFor();
    assert.equal(await page.evaluate(() => window.__fixtureStoppedTool), "codex", "结束操作必须按工具标识下发（由后端自行枚举进程）");
    await petCard.getByRole("button", { name: "浏览 Petdex 商店", exact: true }).click();
    await page.getByTestId("petdex-catalog").waitFor();
    assert((await page.getByTestId("petdex-catalog").innerText()).includes("moon-rabbit"), "商店输出必须原样展示");
    await petCard.getByLabel("宠物 slug").fill("moon-rabbit");
    await petCard.getByRole("button", { name: "从 Petdex 安装", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "已安装" }).waitFor();
    assert.equal(await page.evaluate(() => window.__fixturePetInstalled), "moon-rabbit", "安装宠物必须把经校验的 slug 传给后端");
    await page.screenshot({ path: path.join(output, "pet-card-desktop.png"), fullPage: true });
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
    assert.equal(await page.getByTestId("user-manual-section").count(), manualSections.length);
    await page.getByTestId("user-manual-search").fill("不会匹配的内容-xyz");
    await page.getByTestId("user-manual-empty").waitFor();
    await page.getByTestId("user-manual-search").fill("订阅");
    assert(await page.getByTestId("user-manual-section").count() > 0);
    assert(await page.getByTestId("user-manual-section").count() < manualSections.length);
    // 新增章节必须真的可被检索到，否则等于没写进手册。
    await page.getByTestId("user-manual-search").fill("重新安装");
    assert.equal(await page.getByTestId("user-manual-section").count(), 1, "CLI 工具章节必须能被检索");
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

    // 启动降级横幅：配置读坏时后端**仍要起来**（status 仍是 ready），
    // 但必须把「原配置没生效 + 备份在哪」显式告诉用户。
    // 少了横幅就是静默降级：界面上一切正常，用户以为自己的设置生效了。
    const degradedContext = await browser.newContext();
    await degradedContext.addInitScript(fixture, { bootWarning: "配置文件格式有误（TOML parse error at line 86），已回退到默认设置。原文件已保留在 config.corrupt-1791137918.toml" });
    const degradedPage = await degradedContext.newPage();
    await degradedPage.goto(baseUrl);
    await degradedPage.getByTestId("boot-notice").waitFor({ timeout: 8000 });
    const noticeText = await degradedPage.getByTestId("boot-notice").innerText();
    assert(noticeText.includes("配置未生效"), `横幅必须说明配置没生效：${noticeText}`);
    assert(noticeText.includes("config.corrupt-"), `横幅必须给出备份文件名：${noticeText}`);
    assert(noticeText.includes("已回退到默认设置"), `横幅必须说明回退行为：${noticeText}`);
    // 应用本身必须能用：横幅是警告，不是错误页。
    assert.equal(await degradedPage.getByRole("alert").filter({ hasText: "加载失败" }).count(), 0, "降级不等于启动失败");
    await degradedPage.screenshot({ path: path.join(output, "boot-degraded.png"), fullPage: true });
    // 可关闭
    await degradedPage.getByTestId("boot-notice-close").click();
    await degradedPage.waitForFunction(() => document.querySelector('[data-testid="boot-notice"]') === null, null, { timeout: 3000 });
    await degradedPage.screenshot({ path: path.join(output, "boot-degraded-dismissed.png"), fullPage: true });
    await degradedContext.close();

    // 对照组：没有降级时横幅**不得**出现。
    const normalContext = await browser.newContext();
    await normalContext.addInitScript(fixture);
    const normalPage = await normalContext.newPage();
    await normalPage.goto(baseUrl);
    await normalPage.locator(".workspace-header").waitFor({ timeout: 8000 });
    assert.equal(
      await normalPage.getByTestId("boot-notice").count(),
      0,
    );
    await normalContext.close();
    const offlineManual = await browser.newPage();
    const manualRequests = [];
    offlineManual.on("request", request => { if (/^https?:/.test(request.url())) manualRequests.push(request.url()); });
    await offlineManual.goto(pathToFileURL(path.resolve(__dirname, "../docs/使用手册.html")).href);
    assert.equal(await offlineManual.locator("main > section").count(), manualSections.length);
    await offlineManual.getByRole("link", { name: "查询额度、订阅与剩余时间", exact: true }).click();
    assert(offlineManual.url().endsWith("#quota"));
    for (const width of [900, 390]) {
      await offlineManual.setViewportSize({ width, height: 844 });
      assert(await offlineManual.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
      await offlineManual.screenshot({ path: path.join(output, `manual-html-${width}.png`) });
    }
    assert.deepEqual(manualRequests, []);
    // 启动动画：后端仍在 loading 时必须保持可见，不能退回白屏。
    const startupContext = await browser.newContext({ viewport: { width: 1180, height: 760 }, reducedMotion: "reduce" });
    await startupContext.addInitScript(() => {
      window.isTauri = true;
      window.__TAURI_INTERNALS__ = {
        invoke: async (cmd) => {
          if (cmd === "get_boot_state") return { status: "loading", error: null };
          throw new Error("unexpected startup IPC: " + cmd);
        },
      };
    });
    const startupPage = await startupContext.newPage();
    await startupPage.goto(baseUrl);
    await startupPage.locator("#boot-splash").waitFor();
    const startupStatus = await startupPage.locator("[data-boot-status]").innerText();
    assert(startupStatus.includes("正在"), `启动动画必须保持可见并显示进度：${startupStatus}`);
    await startupPage.screenshot({ path: path.join(output, "boot-splash.png") });
    await startupContext.close();

    // 桌宠窗口：加载宠物资源、单击跳转主窗口、右键菜单可打开主窗口与隐藏桌宠。
    // 3 个任务 → 收起堆叠高度 = 66 + 18 * 2 + 12 + 12 = 126；窗口 = max(130 + 12, 126) = 142。
    const petContext = await browser.newContext({ viewport: { width: 468, height: 142 }, reducedMotion: "reduce" });
    await petContext.addInitScript(fixture);
    const petWindow = await petContext.newPage();
    petWindow.on("pageerror", error => errors.push(`pet: ${error.message}`));
    petWindow.on("dialog", dialog => dialog.accept());
    await petWindow.goto(`${baseUrl}/pet.html`);
    await petWindow.getByTestId("pet-root").waitFor();
    await petWindow.waitForTimeout(500);
    assert.equal(await petWindow.getByTestId("pet-placeholder").count(), 0, "宠物资源加载完成后不应保留占位提示");
    // 气泡态：只显示 AI 软件、任务标题和一行缩略，状态与时间保留在展开面板中。
    await petWindow.getByTestId("pet-bubble").waitFor();
    const bubbleText = await petWindow.getByTestId("pet-bubble").innerText();
    assert(bubbleText.includes("Qoder CLI") && bubbleText.includes("清理界面乱码与提交历史"), `气泡必须显示来源与任务标题：${bubbleText}`);
    assert(bubbleText.includes("configHash"), `气泡必须显示一行任务缩略：${bubbleText}`);
    // 多个任务 → 多个气泡自动堆叠；每个气泡可单独关闭，右上角是放大窗口图标。
    const bubbleItems = petWindow.getByTestId("pet-bubble-item");
    assert.equal(await bubbleItems.count(), 3, "每个任务一个气泡");
    const itemHeights = await bubbleItems.evaluateAll((nodes) =>
      nodes.map((node) => Math.round(node.getBoundingClientRect().height)),
    );
    assert(
      itemHeights.every((height) => height === 66),
      `每个气泡固定 66px：${itemHeights}`,
    );
    assert(
      await petWindow.getByTestId("pet-bubble").evaluate((element) => element.classList.contains("is-compact")),
      "多个气泡默认收起堆叠",
    );
    const compactTops = await bubbleItems.evaluateAll((nodes) =>
      nodes.map((node) => Math.round(node.getBoundingClientRect().top)),
    );
    assert.deepEqual(
      compactTops.map((top, index) => (index === 0 ? 0 : top - compactTops[index - 1])),
      [0, 18, 18],
      `堆叠步进必须是 18px：${compactTops}`,
    );
    let tailFlags = await bubbleItems.evaluateAll((nodes) =>
      nodes.map((node) => node.classList.contains("is-tail")),
    );
    assert.deepEqual(tailFlags, [true, false, false], `收起时尾巴挂在最前面的气泡：${tailFlags}`);
    assert.equal(await petWindow.getByTestId("pet-bubble-dismiss").count(), 3, "每个气泡都有自己的关闭按钮");
    assert.equal(await petWindow.getByTestId("pet-bubble-status").count(), 3, "每个气泡都有状态徽标");
    // 运行中的气泡文字带流光动画（无动画偏好下才应用，先模拟成 no-preference）。
    await petWindow.emulateMedia({ reducedMotion: "no-preference" });
    const runningTitle = petWindow.locator('[data-testid="pet-bubble-item"].is-running .pet-bubble-title strong').first();
    assert.equal(await runningTitle.count(), 1, "运行中的气泡必须带 is-running 标记");
    assert.equal(
      await runningTitle.evaluate((node) => getComputedStyle(node).animationName),
      "pet-running-shimmer",
    );
      "运行中的标题必须应用流光动画",
    assert(
      (await runningTitle.evaluate((node) => getComputedStyle(node).backgroundImage)).includes("linear-gradient"),
      "运行中的标题必须有渐变底色用于流光",
    );
    await petWindow.emulateMedia({ reducedMotion: "reduce" });
    // 运行中 = 实心方块，点击方块结束该软件进程（确认框由对话框处理器自动接受）。
    const runningBadge = petWindow.getByTestId("pet-bubble-status").first();
    assert.equal(await runningBadge.evaluate((node) => node.tagName.toLowerCase()), "button", "运行中的徽标必须是可点击按钮");
    assert.equal(await runningBadge.locator(".pet-bubble-stop-square").count(), 1, "运行中必须显示实心方块");
    await runningBadge.click();
    await petWindow.waitForTimeout(250);
    assert.equal(await petWindow.evaluate(() => window.__fixtureStoppedTool), "qoder", "点方块必须结束对应软件进程");
    assert.equal(
      await petWindow.getByTestId("pet-bubble-expand").locator("svg").count(),
      1,
      "右上角展开按钮必须是 Windows 放大图标",
    );
    const stackBox = await petWindow.getByTestId("pet-bubble").boundingBox();
    const petLayout = await petWindow.evaluate(() => window.__fixturePetLayout());
    assert(
      stackBox.height <= petLayout.height,
      `气泡堆叠不能被窗口裁掉：${JSON.stringify({ stackBox, petLayout })}`,
    );
    assert.equal(await petWindow.evaluate(() => window.__fixturePetExpanded), false, "默认显示任务气泡而不是完整面板");
    const expandBox = await petWindow.getByTestId("pet-bubble-expand").boundingBox();
    assert(
      expandBox.x + expandBox.width / 2 > stackBox.x + stackBox.width - 40,
      `展开按钮必须贴在气泡右侧：${JSON.stringify({ expandBox, stackBox })}`,
    );
    await petWindow.screenshot({ path: path.join(output, "pet-bubble.png") });
    // 鼠标悬浮：堆叠展开，尾巴移到最下面那个气泡上。
    await petWindow.getByTestId("pet-bubble").hover();
    await petWindow.waitForTimeout(250);
    assert.equal(await petWindow.evaluate(() => window.__fixturePetBubblesExpanded), true, "悬浮必须展开堆叠");
    assert.equal(await petWindow.evaluate(() => window.__fixturePetBubbleCount), 3, "展开必须把气泡数量上报给后端");
    assert(
      !(await petWindow.getByTestId("pet-bubble").evaluate((element) => element.classList.contains("is-compact"))),
      "展开后不再是收起态",
    );
    tailFlags = await bubbleItems.evaluateAll((nodes) => nodes.map((node) => node.classList.contains("is-tail")));
    assert.deepEqual(tailFlags, [false, false, true], `展开后尾巴挂在最下面那个气泡：${tailFlags}`);
    await petWindow.setViewportSize({ width: 468, height: 242 });
    await petWindow.screenshot({ path: path.join(output, "pet-bubble-expanded.png") });
    // 鼠标移开后堆叠必须收起，避免窗口一直占着高位。
    await petWindow.mouse.move(4, 4);
    await petWindow.waitForTimeout(300);
    assert.equal(await petWindow.evaluate(() => window.__fixturePetBubblesExpanded), false, "鼠标移开必须收起堆叠");
    // 关闭按钮默认隐藏，鼠标放到对应气泡上才出现。
    const hiddenDiag = await petWindow.evaluate(() => {
      const item = document.querySelector('[data-testid="pet-bubble-item"]');
      const btn = document.querySelector('[data-testid="pet-bubble-dismiss"]');
      return {
        itemHover: item ? item.matches(":hover") : null,
        btnClass: btn ? btn.className : null,
        btnOpacity: btn ? getComputedStyle(btn).opacity : null,
      };
    });
    assert.equal(hiddenDiag.btnOpacity, "0", `关闭按钮默认必须隐藏：${JSON.stringify(hiddenDiag)}`);
    await bubbleItems.first().hover();
    await petWindow.waitForTimeout(250);
    const dismissDiag = await petWindow.evaluate(() => {
      const item = document.querySelector('[data-testid="pet-bubble-item"]');
      const btn = document.querySelector('[data-testid="pet-bubble-dismiss"]');
      return {
        itemClass: item ? item.className : null,
        itemHover: item ? item.matches(":hover") : null,
        btnClass: btn ? btn.className : null,
        btnOpacity: btn ? getComputedStyle(btn).opacity : null,
      };
    });
    assert.equal(
      await petWindow.getByTestId("pet-bubble-dismiss").first().evaluate((node) => getComputedStyle(node).opacity),
      "1",
      `鼠标放在气泡上必须显示关闭按钮：${JSON.stringify(dismissDiag)}`,
    );
    await petWindow.getByTestId("pet-bubble").hover();
    await petWindow.waitForTimeout(250);
    // 单独关闭一个气泡：悬浮会让堆叠展开、气泡位移，所以点“鼠标当前所在气泡”的关闭按钮。
    await bubbleItems.first().hover();
    await petWindow.waitForTimeout(300);
    await petWindow
      .locator('[data-testid="pet-bubble-item"]:hover [data-testid="pet-bubble-dismiss"]')
      .first()
      .click();
    await petWindow.waitForTimeout(250);
    assert.equal(await bubbleItems.count(), 2, "关闭单个气泡后只移除那一个");
    assert.equal(await petWindow.evaluate(() => window.__fixturePetBubbleCount), 2, "关闭单个气泡必须上报新数量");
    // 屏幕右侧时后端返回 bubble_left，前端切换为“气泡在左、宠物在右”。
    await petWindow.evaluate(() => {
      window.__fixturePetBubbleLeft = true;
      window.__fixturePetBubblesExpanded = false;
      window.__fixtureEventHandlers["pet-layout-changed"]({ payload: window.__fixturePetLayout() });
    });
    assert(await petWindow.getByTestId("pet-root").evaluate((element) => element.classList.contains("is-bubble-left")), "靠屏幕右侧时气泡必须切到左侧");
    await petWindow.setViewportSize({ width: 468, height: 142 });
    await petWindow.screenshot({ path: path.join(output, "pet-bubble-left.png") });
    await petWindow.evaluate(() => {
      window.__fixturePetBubbleLeft = false;
      window.__fixtureEventHandlers["pet-layout-changed"]({ payload: window.__fixturePetLayout() });
    });
    // 逐个关闭剩余气泡后缩回宠物本体；任务徽标把气泡（含单独关闭的）一起恢复。
    for (let index = 0; index < 2; index += 1) {
      // 先把鼠标移开再悬浮，确保每次都会触发新的 mouseenter（按钮才会显示出来）。
      await petWindow.mouse.move(4, 4);
      await petWindow.waitForTimeout(200);
      await petWindow.getByTestId("pet-bubble-item").first().hover();
      await petWindow.waitForTimeout(300);
      await petWindow
        .locator('[data-testid="pet-bubble-item"]:hover [data-testid="pet-bubble-dismiss"]')
        .first()
        .click();
      await petWindow.waitForTimeout(300);
      assert.equal(
        await petWindow.getByTestId("pet-bubble-item").count(),
        1 - index,
        `第 ${index + 1} 次关闭后剩余气泡数量不对`,
    );
    }
    await petWindow.getByTestId("pet-bubble").waitFor({ state: "hidden" });
    assert.equal(await petWindow.evaluate(() => window.__fixturePetBubbleHidden), true, "关闭全部气泡必须同步窗口布局");
    await petWindow.getByTestId("pet-task-badge").click();
    await petWindow.getByTestId("pet-bubble").waitFor();
    assert.equal(await petWindow.evaluate(() => window.__fixturePetBubbleHidden), false, "任务徽标必须恢复气泡");
    assert.equal(await petWindow.getByTestId("pet-bubble-item").count(), 3, "恢复时被单独关闭的气泡也要回来");
    // 从气泡展开完整面板，继续验证状态、时间与定向操作。
    await petWindow.setViewportSize({ width: 468, height: 242 });
    await petWindow.getByTestId("pet-bubble-expand").click();
    await petWindow.setViewportSize({ width: 468, height: 372 });
    await petWindow.getByTestId("pet-hud").waitFor();
    assert.equal(await petWindow.evaluate(() => window.__fixturePetExpanded), true, "展开按钮必须恢复完整 HUD");
    const hudText = await petWindow.getByTestId("pet-hud").innerText();
    assert(hudText.includes("Qoder CLI") && hudText.includes("Codex Desktop"), `HUD 必须前置显示 AI 软件名：${hudText}`);
    assert(hudText.includes("清理界面乱码与提交历史") && hudText.includes("完善桌宠任务列表"), `HUD 必须显示具体任务内容：${hudText}`);
    assert(hudText.includes("2 个任务") && hudText.includes("已完成"), `HUD 必须保留状态与任务数量：${hudText}`);
    await petWindow.screenshot({ path: path.join(output, "pet-hud.png") });
    const qoderPetTask = petWindow.getByTestId("pet-task-row").filter({ hasText: "清理界面乱码与提交历史" });
    await qoderPetTask.click();
    await qoderPetTask.getByTestId("pet-task-detail").waitFor();
    await qoderPetTask.getByRole("button", { name: "定位", exact: true }).click();
    await petWindow.getByRole("status").filter({ hasText: "已打开任务 sess-1" }).waitFor();
    assert.deepEqual(await petWindow.evaluate(() => window.__fixtureOpenedTask), { toolId: "qoder", sessionId: "sess-1" }, "HUD 定位必须下发对应任务标识");
    const codexPetTask = petWindow.getByTestId("pet-task-row").filter({ hasText: "完善桌宠任务列表" });
    await codexPetTask.click();
    await codexPetTask.getByTestId("pet-task-detail").waitFor();
    await codexPetTask.getByRole("button", { name: "打开任务", exact: true }).click();
    await petWindow.getByRole("status").filter({ hasText: "已打开任务 sess-codex" }).waitFor();
    assert.deepEqual(await petWindow.evaluate(() => window.__fixtureOpenedTask), { toolId: "codex_desktop", sessionId: "sess-codex" }, "HUD 必须把指定 Codex 任务交给后端深链");
    await codexPetTask.getByRole("button", { name: "项目", exact: true }).click();
    await petWindow.getByRole("status").filter({ hasText: "已打开项目目录" }).waitFor();
    assert.equal(await petWindow.evaluate(() => window.__fixtureOpenedProject), "D:\\Java\\GitHub\\llm-auto", "HUD 项目操作必须下发任务项目路径");
    await qoderPetTask.click();
    await qoderPetTask.getByTestId("pet-task-detail").waitFor();
    await qoderPetTask.getByRole("button", { name: "结束", exact: true }).click();
    await petWindow.getByRole("status").filter({ hasText: "已结束 qoder" }).waitFor();
    assert.equal(await petWindow.evaluate(() => window.__fixtureStoppedTool), "qoder", "HUD 结束操作必须定向到任务对应的 AI 软件");
    // 收起完整面板后回到气泡态。
    await petWindow.getByTestId("pet-collapse-button").click();
    await petWindow.getByTestId("pet-hud").waitFor({ state: "hidden" });
    await petWindow.setViewportSize({ width: 468, height: 140 });
    await petWindow.getByTestId("pet-bubble").waitFor();
    assert.equal(await petWindow.evaluate(() => window.__fixturePetExpanded), false, "收起面板必须回到任务气泡");
    await petWindow.getByTestId("pet-stage").click();
    await petWindow.waitForTimeout(250);
    assert.equal(await petWindow.evaluate(() => window.__fixtureFocusedSection), "stats", "单击宠物本体必须跳转到用量页");
    assert.equal(await petWindow.evaluate(() => window.__fixtureDragStarted ?? false), false, "短按点击不应触发窗口拖动");
    // 轻移超过阈值必须立即开始拖动，不再依赖“按住 160ms 且指针不能离开宠物区域”。
    await petWindow.evaluate(() => {
      window.__fixturePetBubbleLeft = false;
      window.__fixtureDragStarted = false;
      window.__fixtureDragStarts = 0;
    });
    const dragBox = await petWindow.getByTestId("pet-stage").boundingBox();
    assert(dragBox, "宠物区域必须可用于拖动回归");
    const dragX = dragBox.x + dragBox.width / 2;
    const dragY = dragBox.y + dragBox.height / 2;
    await petWindow.mouse.move(dragX, dragY);
    await petWindow.mouse.down();
    await petWindow.mouse.move(dragX + 8, dragY, { steps: 2 });
    await petWindow.waitForTimeout(50);
    assert.equal(await petWindow.evaluate(() => window.__fixtureDragStarted), true, "移动超过阈值必须启动拖动");
    assert.equal(await petWindow.evaluate(() => window.__fixtureDragStarts), 1, "一次拖动只能发起一次 start_dragging");
    // 拖动期间即使后端返回左右切换，也要等窗口停稳后再应用，避免桌宠来回闪动。
    await petWindow.evaluate(() => {
      window.__fixturePetBubbleLeft = true;
      window.__fixtureEventHandlers["pet-layout-changed"]({ payload: window.__fixturePetLayout() });
    });
    assert.equal(
      await petWindow.getByTestId("pet-root").evaluate((element) => element.classList.contains("is-bubble-left")),
      false,
      "拖动过程中不得切换气泡方向",
    );
    await petWindow.mouse.up();
    await petWindow.waitForTimeout(260);
    assert.equal(
      await petWindow.getByTestId("pet-root").evaluate((element) => element.classList.contains("is-bubble-left")),
      true,
      "拖动停止后必须应用最终左右布局",
    );
    await petWindow.evaluate(() => {
      window.__fixturePetBubbleLeft = false;
      window.__fixtureEventHandlers["pet-layout-changed"]({ payload: window.__fixturePetLayout() });
    });
    // 右键弹出的是系统原生菜单（由 Rust 侧构建与消费），这里断言请求参数正确。
    await petWindow.getByTestId("pet-root").click({ button: "right" });
    await petWindow.waitForTimeout(250);
    const menuRequest = await petWindow.evaluate(() => window.__fixturePetMenu ?? null);
    assert(menuRequest !== null, "右键必须请求原生菜单（页面内菜单在极小窗口会显示不全）");
    assert.equal(menuRequest.slug, "snow-plum-lillia", "菜单必须带上当前宠物以便勾选");
    assert.equal(menuRequest.paused, false, "菜单必须带上暂停状态以显示正确文案");
    assert.equal(menuRequest.expanded, false, "菜单必须带上气泡/面板状态以显示正确文案");
    // 模拟原生菜单动作：切换宠物 → 持久化并重新加载资源。
    await petWindow.evaluate(() => {
      window.__fixturePetStatus.installed_pets.push({ slug: "moon-rabbit", display_name: "Moon Rabbit", description: null, version: "0.9.0", spritesheet_file: "spritesheet.webp", directory: "C:/Users/fixture/.petdex/pets/moon-rabbit" });
      window.__fixtureEventHandlers["pet-menu-action"]({ payload: { action: "select-pet", slug: "moon-rabbit" } });
    });
    assert.equal(await petWindow.evaluate(() => localStorage.getItem("llm-gateway-pet-slug")), "moon-rabbit", "菜单选择宠物必须持久化");
    await petWindow.waitForTimeout(400);
    assert.equal(await petWindow.evaluate(() => window.__fixturePetAssetSlug), "moon-rabbit", "切换宠物后必须重新加载对应资源");
    await petContext.close();
    assert.deepEqual(errors, []);
    const preview = await browser.newPage();
    await preview.goto(baseUrl);
    await preview.getByRole("heading", { name: "浏览器预览已隔离", exact: true }).waitFor();
    assert.equal(await preview.locator(".provider-card").count(), 0);
    console.log(`UI_SMOKE_OK: discovery, defaults, overrides, deduplication, sessions, switching races, compression, backup display, quota/expiry, first-use guidance, persisted skip/completion, manual search, offline HTML, themes, startup splash, pet HUD/actions, responsive layouts and preview isolation; screenshots=${output}`);
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });

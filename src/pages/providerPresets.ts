import type { Dialect, ModelRef, ProviderInput } from "../api";

export const DEFAULT_CONTEXT_WINDOW = 32768;
export type ProviderForm = Omit<ProviderInput, "note"> & { note: string };
export const emptyModel = (context = DEFAULT_CONTEXT_WINDOW): ModelRef => ({
  alias: "", upstream: "", context_window: context,
  supports_tools: false, supports_vision: false, supports_stream: true,
});
export const blankForm = (): ProviderForm => ({
  name: "", dialect: "openai", base_url: "", api_key: "", enabled: true,
  priority: 10, models: [], rpm_limit: 0, intelligence: 60, note: "",
});

export const PRESETS: Array<{ name: string; dialect: Dialect; base_url: string; models: string[] }> = [
  { name: "自定义 API", dialect: "openai", base_url: "", models: [] },
  { name: "OpenRouter", dialect: "openai", base_url: "https://openrouter.ai/api/v1", models: ["openrouter/free"] },
  { name: "DeepSeek", dialect: "openai", base_url: "https://api.deepseek.com/v1", models: ["deepseek-chat", "deepseek-reasoner"] },
  { name: "SenseNova", dialect: "openai", base_url: "https://token.sensenova.cn/v1", models: ["sensenova-6.8-flash-lite", "sensenova-u1.5-lite", "sensenova-u1-fast", "deepseek-v4-flash", "glm-5.2"] },
  { name: "智谱 GLM", dialect: "openai", base_url: "https://open.bigmodel.cn/api/paas/v4", models: ["glm-4.5-flash", "glm-4.7-flash"] },
  { name: "智谱 Anthropic 兼容", dialect: "anthropic", base_url: "https://open.bigmodel.cn/api/anthropic", models: ["glm-4-flash-250414", "glm-4-flash", "glm-4.5-flash", "glm-4.7-flash"] },
  { name: "Air Outer", dialect: "openai", base_url: "https://ps.air-outer.com/v1", models: ["claude-opus-4-8", "claude-opus-5", "gpt-5.6-sol", "deepseek-v4-flash", "glm-5.3"] },
  { name: "OpenAI", dialect: "openai", base_url: "https://api.openai.com/v1", models: ["gpt-4o"] },
  { name: "Anthropic", dialect: "anthropic", base_url: "https://api.anthropic.com/v1", models: ["claude-sonnet-4-6"] },
  { name: "Google Gemini", dialect: "gemini", base_url: "https://generativelanguage.googleapis.com/v1beta", models: ["gemini-2.5-flash"] },
  { name: "本地 Ollama", dialect: "ollama", base_url: "http://localhost:11434", models: [] },
  { name: "通义千问", dialect: "openai", base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", models: ["qwen-plus", "qwen-max"] },
  { name: "月之暗面 Kimi", dialect: "openai", base_url: "https://api.moonshot.cn/v1", models: ["kimi-k2-0905-preview"] },
  { name: "豆包", dialect: "openai", base_url: "https://ark.cn-beijing.volces.com/api/v3", models: ["doubao-seed-1-6-250615"] },
];

export const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);
export const formatContext = (value: number) => new Intl.NumberFormat("zh-CN").format(value);

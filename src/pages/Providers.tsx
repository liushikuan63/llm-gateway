import { useEffect, useState } from "react";
import {
  api,
  AppConfig,
  DIALECT_LABEL,
  HEALTH_LABEL,
  ModelRef,
  ProviderInput,
  ProviderView,
} from "../api";

function emptyModel(): ModelRef {
  return {
    alias: "",
    upstream: "",
    context_window: 128000,
    supports_tools: true,
    supports_vision: false,
    supports_stream: true,
  };
}

type Form = Omit<ProviderInput, "note"> & { note: string };

function blankForm(): Form {
  return {
    name: "",
    dialect: "openai",
    base_url: "",
    api_key: "",
    enabled: true,
    priority: 10,
    models: [emptyModel()],
    rpm_limit: 0,
    intelligence: 60,
    note: "",
  };
}

function providerInput(
  provider: ProviderView,
  overrides: Partial<ProviderInput> = {},
): ProviderInput {
  return {
    id: provider.id,
    name: provider.name,
    dialect: provider.dialect,
    base_url: provider.base_url,
    // 空值表示保留密文，不能也不需要把 Key 回传给前端。
    api_key: "",
    enabled: provider.enabled,
    priority: provider.priority,
    models: provider.models,
    rpm_limit: provider.rpm_limit,
    intelligence: provider.intelligence,
    note: provider.note,
    ...overrides,
  };
}

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

/// 常见厂商预设。用户只填 Key，地址和方言自动带出。
const PRESETS: Array<{ name: string; dialect: Form["dialect"]; base_url: string; models: string[] }> = [
  { name: "DeepSeek", dialect: "openai", base_url: "https://api.deepseek.com/v1", models: ["deepseek-chat", "deepseek-reasoner"] },
  { name: "智谱 GLM", dialect: "openai", base_url: "https://open.bigmodel.cn/api/paas/v4", models: ["glm-4.6", "glm-4.5-flash"] },
  { name: "月之暗面 Kimi", dialect: "openai", base_url: "https://api.moonshot.cn/v1", models: ["kimi-k2-0905-preview"] },
  { name: "通义千问", dialect: "openai", base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", models: ["qwen-plus", "qwen-max"] },
  { name: "豆包", dialect: "openai", base_url: "https://ark.cn-beijing.volces.com/api/v3", models: ["doubao-seed-1-6-250615"] },
  { name: "OpenAI", dialect: "openai", base_url: "https://api.openai.com/v1", models: ["gpt-4o"] },
  { name: "Anthropic 官方", dialect: "anthropic", base_url: "https://api.anthropic.com/v1", models: ["claude-sonnet-4-6"] },
  { name: "Google Gemini", dialect: "gemini", base_url: "https://generativelanguage.googleapis.com/v1beta", models: ["gemini-2.5-flash"] },
  { name: "OpenRouter 免费路由", dialect: "openai", base_url: "https://openrouter.ai/api/v1", models: ["openrouter/free"] },
  {
    name: "SenseNova 免费模型",
    dialect: "openai",
    base_url: "https://token.sensenova.cn/v1",
    models: ["sensenova-6.8-flash-lite", "sensenova-u1.5-lite", "sensenova-u1-fast", "deepseek-v4-flash", "glm-5.2"],
  },
  {
    name: "智谱 Anthropic 兼容",
    dialect: "anthropic",
    base_url: "https://open.bigmodel.cn/api/anthropic",
    models: ["glm-4-flash-250414", "glm-4-flash", "glm-4.5-flash", "glm-4.7-flash"],
  },
  {
    name: "Air Outer",
    dialect: "openai",
    base_url: "https://ps.air-outer.com/v1",
    models: ["claude-opus-4-8", "claude-opus-5", "gpt-5.6-sol", "deepseek-v4-flash", "glm-5.3"],
  },
  { name: "本地 Ollama", dialect: "ollama", base_url: "http://localhost:11434", models: ["qwen2.5:14b"] },
];

export default function ProvidersPage() {
  const [list, setList] = useState<ProviderView[]>([]);
  const [open, setOpen] = useState(false);
  const [form, setForm] = useState<Form>(blankForm());
  const [testing, setTesting] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const [cfg, setCfg] = useState<AppConfig | null>(null);

  const load = async () => {
    try {
      const [providers, config] = await Promise.all([api.listProviders(), api.getConfig()]);
      setList(providers);
      setCfg(config);
    } catch (error) {
      setMsg({ kind: "err", text: `加载供应商失败：${errorText(error)}` });
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const save = async () => {
    if (!form.name.trim() || !form.base_url.trim()) {
      setMsg({ kind: "err", text: "名称与 Base URL 必填" });
      return;
    }
    const models = form.models.filter((model) => model.alias.trim() && model.upstream.trim());
    if (models.length === 0) {
      setMsg({ kind: "err", text: "至少配置一个模型" });
      return;
    }

    setSaving(true);
    try {
      await api.upsertProvider({
        ...form,
        name: form.name.trim(),
        base_url: form.base_url.trim(),
        models,
        note: form.note.trim() || null,
      });
      setOpen(false);
      setForm(blankForm());
      await load();
      setMsg({ kind: "ok", text: "供应商已保存，路由缓存已刷新" });
    } catch (error) {
      setMsg({ kind: "err", text: `保存失败：${errorText(error)}` });
    } finally {
      setSaving(false);
    }
  };

  const test = async (provider: ProviderView) => {
    setTesting(provider.id);
    try {
      const result = await api.testProvider(provider.id);
      setMsg(
        result.ok
          ? { kind: "ok", text: `连通正常：${result.latency_ms}ms · ${result.model ?? "默认模型"}` }
          : { kind: "err", text: `连接失败：${result.error ?? "上游未返回详情"}` },
      );
      await load();
    } catch (error) {
      setMsg({ kind: "err", text: `连接测试失败：${errorText(error)}` });
    } finally {
      setTesting(null);
    }
  };

  const setEnabled = async (provider: ProviderView) => {
    setBusy(provider.id);
    try {
      await api.upsertProvider(providerInput(provider, { enabled: !provider.enabled }));
      await load();
      setMsg({
        kind: "ok",
        text: provider.enabled ? `已停用 ${provider.name}` : `已启用 ${provider.name}`,
      });
    } catch (error) {
      setMsg({ kind: "err", text: `更新状态失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const setActive = async (provider: ProviderView) => {
    setBusy(provider.id);
    try {
      await api.setActive(provider.id);
      await load();
      setMsg({ kind: "ok", text: `已将 ${provider.name} 设为主用，变更立即参与后续路由` });
    } catch (error) {
      setMsg({ kind: "err", text: `切换主用失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const moveProvider = async (provider: ProviderView, direction: -1 | 1) => {
    const ordered = list.slice().sort((a, b) => a.priority - b.priority || a.name.localeCompare(b.name));
    const index = ordered.findIndex((item) => item.id === provider.id);
    const target = index + direction;
    if (index < 0 || target < 0 || target >= ordered.length) return;

    const next = [...ordered];
    [next[index], next[target]] = [next[target], next[index]];
    setBusy(provider.id);
    try {
      // 统一重排以修复历史重复优先级，且不会读取或写回明文 Key。
      for (const [position, item] of next.entries()) {
        await api.upsertProvider(providerInput(item, { priority: (position + 1) * 10 }));
      }
      await load();
      setMsg({ kind: "ok", text: `已调整 ${provider.name} 的优先级` });
    } catch (error) {
      setMsg({ kind: "err", text: `调整优先级失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const remove = async (provider: ProviderView) => {
    if (!window.confirm(`确定删除供应商“${provider.name}”吗？其模型映射也会一并删除。`)) {
      return;
    }
    setBusy(provider.id);
    try {
      await api.deleteProvider(provider.id);
      await load();
      setMsg({ kind: "ok", text: `已删除 ${provider.name}` });
    } catch (error) {
      setMsg({ kind: "err", text: `删除失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const usePreset = (name: string) => {
    const preset = PRESETS.find((item) => item.name === name);
    if (!preset) return;
    setForm({
      ...form,
      name: preset.name,
      dialect: preset.dialect,
      base_url: preset.base_url,
      models: preset.models.map((model) => ({ ...emptyModel(), alias: model, upstream: model })),
    });
  };

  const ordered = list.slice().sort((a, b) => a.priority - b.priority || a.name.localeCompare(b.name));

  return (
    <div>
      <div className="spread page-heading">
        <div>
          <h2>供应商</h2>
          <div className="sub">
            每家填一次 Key，网关按策略路由；优先级模式下，上下箭头决定首选与降级顺序。
          </div>
        </div>
        <div className="row">
          <button onClick={() => void load()}>刷新</button>
          <button
            className="primary"
            onClick={() => {
              setForm(blankForm());
              setOpen(true);
            }}
          >
            添加供应商
          </button>
        </div>
      </div>

      {msg && <div className={`msg ${msg.kind}`}>{msg.text}</div>}

      <div className="card table-card">
        {ordered.length === 0 ? (
          <div className="empty">还没有供应商。添加一个预设并完成连接测试后即可开始路由。</div>
        ) : (
          <table className="provider-table">
            <thead>
              <tr>
                <th style={{ width: 74 }}>优先级</th>
                <th>名称与 Key</th>
                <th>方言</th>
                <th>Base URL</th>
                <th>模型</th>
                <th>状态</th>
                <th style={{ width: 350 }}>操作</th>
              </tr>
            </thead>
            <tbody>
              {ordered.map((provider, index) => (
                <tr key={provider.id}>
                  <td>
                    <div className="priority-control">
                      <span className="muted">{provider.priority}</span>
                      <button
                        className="icon-button"
                        title="上移优先级"
                        aria-label="上移优先级"
                        disabled={index === 0 || busy !== null}
                        onClick={() => void moveProvider(provider, -1)}
                      >
                        ↑
                      </button>
                      <button
                        className="icon-button"
                        title="下移优先级"
                        aria-label="下移优先级"
                        disabled={index === ordered.length - 1 || busy !== null}
                        onClick={() => void moveProvider(provider, 1)}
                      >
                        ↓
                      </button>
                    </div>
                  </td>
                  <td>
                    <div className="row" style={{ gap: 6 }}>
                      <span>{provider.name}</span>
                      {provider.is_active && <span className="tag purple">主用</span>}
                    </div>
                    <div className="muted mono" style={{ fontSize: 11, marginTop: 3 }}>
                      {provider.api_key_masked || "未设置密钥"}
                    </div>
                  </td>
                  <td><span className="tag">{DIALECT_LABEL[provider.dialect]}</span></td>
                  <td className="mono muted breakable">{provider.base_url}</td>
                  <td>
                    {provider.models.slice(0, 3).map((model) => (
                      <span key={model.alias} className="tag purple" style={{ marginRight: 4 }}>
                        {model.alias}
                      </span>
                    ))}
                    {provider.models.length > 3 && <span className="muted">+{provider.models.length - 3}</span>}
                  </td>
                  <td>
                    {!provider.enabled ? (
                      <span className="tag">已停用</span>
                    ) : (
                      <span
                        className={`tag ${
                          provider.health?.health === "healthy" || !provider.health
                            ? "ok"
                            : provider.health.health === "invalid"
                              ? "err"
                              : "warn"
                        }`}
                      >
                        {provider.health ? HEALTH_LABEL[provider.health.health] ?? provider.health.health : "正常"}
                      </span>
                    )}
                  </td>
                  <td>
                    <div className="row compact-actions">
                      <button
                        className="ghost"
                        disabled={testing === provider.id || busy !== null}
                        onClick={() => void test(provider)}
                      >
                        {testing === provider.id ? "测试中" : "测试"}
                      </button>
                      <button
                        className="ghost"
                        disabled={busy !== null}
                        onClick={() => void setEnabled(provider)}
                      >
                        {provider.enabled ? "停用" : "启用"}
                      </button>
                      <button
                        className="ghost"
                        disabled={!provider.enabled || provider.is_active || busy !== null}
                        onClick={() => void setActive(provider)}
                      >
                        {provider.is_active ? "已主用" : "设为主用"}
                      </button>
                      <button
                        className="ghost"
                        disabled={busy !== null}
                        onClick={() => {
                          setForm({
                            id: provider.id,
                            name: provider.name,
                            dialect: provider.dialect,
                            base_url: provider.base_url,
                            api_key: "",
                            enabled: provider.enabled,
                            priority: provider.priority,
                            models: provider.models.length ? provider.models : [emptyModel()],
                            rpm_limit: provider.rpm_limit,
                            intelligence: provider.intelligence,
                            note: provider.note ?? "",
                          });
                          setOpen(true);
                        }}
                      >
                        编辑
                      </button>
                      <button
                        className="danger ghost"
                        disabled={busy !== null}
                        onClick={() => void remove(provider)}
                      >
                        删除
                      </button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {cfg && (
        <div className="card">
          <div className="spread">
            <div>
              <strong>路由策略</strong>
              <div className="sub" style={{ marginBottom: 0 }}>
                priority 使用上表顺序；其他策略仍会把手动主用 Provider 放在候选链首位。
              </div>
            </div>
            <select
              value={cfg.routing_strategy}
              onChange={(event) => {
                void (async () => {
                  try {
                    const result = await api.updateConfig({ ...cfg, routing_strategy: event.target.value });
                    setCfg(result.config);
                    setMsg({ kind: "ok", text: "路由策略已保存并热生效" });
                  } catch (error) {
                    setMsg({ kind: "err", text: `保存路由策略失败：${errorText(error)}` });
                  }
                })();
              }}
            >
              <option value="priority">priority · 手工优先级</option>
              <option value="balanced">balanced · 综合均衡</option>
              <option value="smartest">smartest · 能力优先</option>
              <option value="fastest">fastest · 速度优先</option>
              <option value="reliable">reliable · 稳定优先</option>
              <option value="custom">custom · 自定义规则</option>
            </select>
          </div>
        </div>
      )}

      {open && (
        <div className="modal-mask" onClick={() => !saving && setOpen(false)}>
          <div className="modal" onClick={(event) => event.stopPropagation()}>
            <h3>{form.id ? "编辑供应商" : "添加供应商"}</h3>

            {!form.id && (
              <div className="field">
                <label>快速预设（选择后自动带出地址与模型）</label>
                <select onChange={(event) => usePreset(event.target.value)} defaultValue="">
                  <option value="" disabled>选择预设</option>
                  {PRESETS.map((preset) => (
                    <option key={preset.name} value={preset.name}>
                      {preset.name} · {preset.base_url}
                    </option>
                  ))}
                </select>
              </div>
            )}

            <div className="grid2">
              <div className="field">
                <label>名称</label>
                <input value={form.name} onChange={(event) => setForm({ ...form, name: event.target.value })} placeholder="例如 DeepSeek" />
              </div>
              <div className="field">
                <label>协议方言</label>
                <select value={form.dialect} onChange={(event) => setForm({ ...form, dialect: event.target.value as Form["dialect"] })}>
                  <option value="openai">OpenAI 兼容</option>
                  <option value="anthropic">Anthropic 原生</option>
                  <option value="gemini">Gemini 原生</option>
                  <option value="ollama">Ollama 原生</option>
                </select>
              </div>
            </div>

            <div className="field">
              <label>Base URL</label>
              <input value={form.base_url} onChange={(event) => setForm({ ...form, base_url: event.target.value })} placeholder="https://api.deepseek.com/v1" />
            </div>

            <div className="field">
              <label>API Key（加密后落库，编辑时留空即可保留现有 Key）</label>
              <input
                type="password"
                autoComplete="new-password"
                value={form.api_key}
                onChange={(event) => setForm({ ...form, api_key: event.target.value })}
                placeholder={form.id ? "留空表示不修改" : "sk-..."}
              />
            </div>

            <div className="field">
              <label>模型映射（对外别名 → 上游真实模型名）</label>
              {form.models.map((model, index) => (
                <div className="model-row" key={index}>
                  <input
                    value={model.alias}
                    onChange={(event) => {
                      const models = [...form.models];
                      models[index] = { ...models[index], alias: event.target.value };
                      setForm({ ...form, models });
                    }}
                    placeholder="对外别名"
                  />
                  <input
                    value={model.upstream}
                    onChange={(event) => {
                      const models = [...form.models];
                      models[index] = { ...models[index], upstream: event.target.value };
                      setForm({ ...form, models });
                    }}
                    placeholder="上游模型名"
                  />
                  <input
                    type="number"
                    value={model.context_window}
                    title="上下文窗口"
                    onChange={(event) => {
                      const models = [...form.models];
                      models[index] = { ...models[index], context_window: Number(event.target.value) };
                      setForm({ ...form, models });
                    }}
                  />
                  <label title="支持工具调用"><input type="checkbox" checked={model.supports_tools} onChange={(event) => {
                    const models = [...form.models];
                    models[index] = { ...models[index], supports_tools: event.target.checked };
                    setForm({ ...form, models });
                  }} />工具</label>
                  <label title="支持视觉输入"><input type="checkbox" checked={model.supports_vision} onChange={(event) => {
                    const models = [...form.models];
                    models[index] = { ...models[index], supports_vision: event.target.checked };
                    setForm({ ...form, models });
                  }} />视觉</label>
                  <label title="支持流式响应"><input type="checkbox" checked={model.supports_stream} onChange={(event) => {
                    const models = [...form.models];
                    models[index] = { ...models[index], supports_stream: event.target.checked };
                    setForm({ ...form, models });
                  }} />流式</label>
                  <button
                    className="danger ghost icon-button"
                    title="移除模型"
                    aria-label="移除模型"
                    onClick={() => setForm({ ...form, models: form.models.filter((_, itemIndex) => itemIndex !== index) })}
                  >
                    ×
                  </button>
                </div>
              ))}
              <button className="ghost" onClick={() => setForm({ ...form, models: [...form.models, emptyModel()] })}>添加模型</button>
            </div>

            <div className="grid3">
              <div className="field">
                <label>优先级（越小越优先）</label>
                <input type="number" value={form.priority} onChange={(event) => setForm({ ...form, priority: Number(event.target.value) })} />
              </div>
              <div className="field">
                <label>本地 RPM 上限（0=不限制）</label>
                <input type="number" value={form.rpm_limit} onChange={(event) => setForm({ ...form, rpm_limit: Number(event.target.value) })} />
              </div>
              <div className="field">
                <label>能力分（0-100）</label>
                <input type="number" value={form.intelligence} onChange={(event) => setForm({ ...form, intelligence: Number(event.target.value) })} />
              </div>
            </div>

            <div className="field">
              <label>备注</label>
              <input value={form.note} onChange={(event) => setForm({ ...form, note: event.target.value })} placeholder="仅本机保存" />
            </div>

            <label className="row" style={{ gap: 6 }}>
              <input type="checkbox" checked={form.enabled} onChange={(event) => setForm({ ...form, enabled: event.target.checked })} />
              启用（参与路由与降级）
            </label>

            <div className="row end" style={{ marginTop: 18 }}>
              <button disabled={saving} onClick={() => setOpen(false)}>取消</button>
              <button className="primary" disabled={saving} onClick={() => void save()}>{saving ? "保存中" : "保存"}</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

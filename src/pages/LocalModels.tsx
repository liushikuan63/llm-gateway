import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  api,
  AppConfig,
  CalibrationReport,
  CLASSIFIER_LABEL,
  JevProbeResult,
  LocalModelInfo,
  ProbeOutcome,
  SearchOutcome,
  SearchSettingsView,
  TASK_CLASS_LABEL,
  TaskClass,
  TaskIntent,
} from "../api";
import { errorText } from "./providerPresets";
import "./local-models.css";

type Tab = "runtimes" | "smart" | "search";

const TABS: { id: Tab; label: string; hint: string }[] = [
  { id: "runtimes", label: "本地运行时", hint: "扫描本机推理服务，把已装模型登记进路由候选链" },
  { id: "smart", label: "智能模式", hint: "先给请求定性，再按类型挑模型" },
  { id: "search", label: "联网搜索", hint: "需要最新事实时由网关代为检索并注入上下文" },
];

function formatBytes(bytes: number | null): string {
  if (bytes === null || bytes <= 0) return "—";
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024).toFixed(0)} KB`;
}

function errorMessage(error: unknown): string {
  return typeof error === "string" ? error : errorText(error);
}

export default function LocalModelsPage() {
  const [cfg, setCfg] = useState<AppConfig | null>(null);
  const [tab, setTab] = useState<Tab>("runtimes");

  const reloadConfig = useCallback(async () => {
    try {
      setCfg(await api.getConfig());
    } catch (error) {
      setCfg(null);
      console.warn("读取配置失败", errorMessage(error));
    }
  }, []);

  useEffect(() => {
    void reloadConfig();
  }, [reloadConfig]);

  // 其它页面改动路由策略后回到本页时，配置要跟着刷新。
  useEffect(() => {
    const handler = () => void reloadConfig();
    window.addEventListener("llm-gateway-config-changed", handler);
    return () => window.removeEventListener("llm-gateway-config-changed", handler);
  }, [reloadConfig]);

  const saveConfig = useCallback(
    async (next: AppConfig) => {
      const result = await api.updateConfig(next);
      setCfg(result.config);
      window.dispatchEvent(new CustomEvent("llm-gateway-config-changed", { detail: result.config }));
      return result.config;
    },
    []
  );

  if (cfg === null) {
    return (
      <div className="card">
        <div className="empty">正在读取配置…</div>
      </div>
    );
  }

  return (
    <>
      <div className="local-tabbar">
        {TABS.map((item) => (
          <button
            key={item.id}
            className={`local-tab ${tab === item.id ? "active" : ""}`}
            onClick={() => setTab(item.id)}
          >
            <strong>{item.label}</strong>
            <span>{item.hint}</span>
          </button>
        ))}
      </div>

      {tab === "runtimes" && <RuntimesSection cfg={cfg} onSave={saveConfig} />}
      {tab === "smart" && <SmartSection cfg={cfg} onSave={saveConfig} />}
      {tab === "search" && <SearchSection cfg={cfg} onSave={saveConfig} />}
    </>
  );
}

/* ----------------------------- 本地运行时 ----------------------------- */

function RuntimesSection({ cfg, onSave }: { cfg: AppConfig; onSave: (c: AppConfig) => Promise<AppConfig> }) {
  const [runtimes, setRuntimes] = useState<ProbeOutcome[]>([]);
  const [models, setModels] = useState<Record<string, LocalModelInfo[]>>({});
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const [pulling, setPulling] = useState<Record<string, string>>({});
  const loadVersion = useRef(0);

  const probe = useCallback(async () => {
    const version = ++loadVersion.current;
    setLoading(true);
    try {
      const list = await api.listLocalRuntimes();
      if (version !== loadVersion.current) return;
      setRuntimes(list);
      const reachable = list.filter((item) => item.reachable).map((item) => item.id);
      const entries = await Promise.all(
        reachable.map(async (id) => {
          try {
            return [id, await api.listLocalModels(id)] as const;
          } catch {
            return [id, [] as LocalModelInfo[]] as const;
          }
        })
      );
      if (version !== loadVersion.current) return;
      setModels(Object.fromEntries(entries));
    } catch (error) {
      if (version === loadVersion.current) setMessage({ kind: "err", text: errorMessage(error) });
    } finally {
      if (version === loadVersion.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void probe();
    return () => {
      loadVersion.current += 1;
    };
  }, [probe]);

  // 拉取进度由后端逐行推过来；组件卸载后要退订，否则会往已销毁的 state 里写。
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void import("@tauri-apps/api/event").then(({ listen }) =>
      listen<{ model: string; status: string; completed: number | null; total: number | null; done: boolean }>(
        "local-model://pull-progress",
        (event) => {
          if (disposed) return;
          const payload = event.payload;
          setPulling((prev) => {
            const next = { ...prev };
            if (payload.done) {
              delete next[payload.model];
            } else {
              const pct =
                payload.completed !== null && payload.total
                  ? ` (${Math.round((payload.completed / payload.total) * 100)}%)`
                  : "";
              next[payload.model] = payload.status + pct;
            }
            return next;
          });
        }
      ).then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
    );
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const registered = useMemo(() => {
    const set = new Set<string>();
    for (const model of Object.values(models).flat()) set.add(`${model.meta.runtime}:${model.upstream}`);
    return set;
  }, [models]);

  const run = async (key: string, operation: () => Promise<string>) => {
    setBusy(key);
    setMessage(null);
    try {
      setMessage({ kind: "ok", text: await operation() });
      await probe();
    } catch (error) {
      setMessage({ kind: "err", text: errorMessage(error) });
    } finally {
      setBusy(null);
    }
  };

  const anyReachable = runtimes.some((item) => item.reachable);

  return (
    <>
      <div className="card">
        <div className="row">
          <div>
            <h3>本机推理运行时</h3>
            <p className="sub">
              登记后的本地模型与云端模型完全平权：同样参与打分、降级与审计。
              能力位来自运行时元数据，<strong>取不到就是「不支持」</strong>——宁可少标也不多标。
            </p>
          </div>
          <div className="row-actions">
            <label className="field-inline">
              <input
                type="checkbox"
                checked={cfg.local_models.enabled}
                disabled={busy !== null}
                onChange={async (event) => {
                  const next = { ...cfg, local_models: { ...cfg.local_models, enabled: event.target.checked } };
                  try {
                    await onSave(next);
                  } catch (error) {
                    setMessage({ kind: "err", text: errorMessage(error) });
                  }
                }}
              />
              本地模型参与路由
            </label>
            <button className="button ghost" disabled={loading || busy !== null} onClick={() => void probe()}>
              {loading ? "扫描中…" : "重新扫描"}
            </button>
          </div>
        </div>

        {!loading && !anyReachable && (
          <div className="empty">
            未检测到本地运行时。确认 Ollama 已启动，或在设置中把端点地址改成你实际使用的端口。
          </div>
        )}

        <div className="table-card">
          <table>
            <thead>
              <tr>
                <th>运行时</th>
                <th>地址</th>
                <th>状态</th>
                <th>版本</th>
                <th>模型数</th>
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              {runtimes.map((item) => (
                <tr key={item.id}>
                  <td>{item.label}</td>
                  <td className="mono">{item.base_url}</td>
                  <td>
                    {item.reachable ? (
                      <span className="badge ok">可达</span>
                    ) : (
                      <span className="badge warn" title={item.error ?? ""}>
                        不可达
                      </span>
                    )}
                    {!item.reachable && item.error && <div className="sub">{item.error}</div>}
                  </td>
                  <td>{item.version ?? "—"}</td>
                  <td>{item.reachable ? item.model_count : "—"}</td>
                  <td>
                    {item.reachable && (
                      <button
                        className="button ghost"
                        disabled={busy !== null}
                        onClick={() =>
                          void run(`${item.id}:list`, async () => {
                            const found = await api.listLocalModels(item.id);
                            return `${item.label} 上一共 ${found.length} 个模型`;
                          })
                        }
                      >
                        刷新模型
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        {message && <div className={`msg ${message.kind}`}>{message.text}</div>}
      </div>

      {Object.entries(models).map(([endpointId, list]) => {
        const endpoint = runtimes.find((item) => item.id === endpointId);
        if (!endpoint || list.length === 0) return null;
        return (
          <div className="card" key={endpointId}>
            <h3>
              {endpoint.label} · 已装模型（{list.length}）
            </h3>
            <div className="table-card">
              <table>
                <thead>
                  <tr>
                    <th>模型</th>
                    <th>参数量 / 量化</th>
                    <th>磁盘</th>
                    <th>上下文</th>
                    <th>能力</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {list.map((model) => (
                    <tr key={model.upstream}>
                      <td className="mono">{model.upstream}</td>
                      <td>
                        {model.meta.parameter_size ?? "—"}
                        {model.meta.quantization ? ` · ${model.meta.quantization}` : ""}
                      </td>
                      <td>{formatBytes(model.meta.disk_bytes)}</td>
                      <td>{model.context_window.toLocaleString()}</td>
                      <td>
                        <div className="caps">
                          {model.supports_vision && <span className="badge">视觉</span>}
                          {model.supports_tools && <span className="badge">工具</span>}
                          {model.supports_thinking && <span className="badge accent">思考</span>}
                          {model.supports_audio && <span className="badge">音频</span>}
                          {!model.supports_vision && !model.supports_tools && !model.supports_thinking && (
                            <span className="sub">未提供能力元数据</span>
                          )}
                        </div>
                      </td>
                      <td>
                        {pulling[model.upstream] ? (
                          <span className="sub">{pulling[model.upstream]}</span>
                        ) : (
                          <button
                            className="button ghost"
                            disabled={busy !== null}
                            onClick={() =>
                              void run(`${endpointId}:${model.upstream}`, async () => {
                                const outcome = await api.registerLocalModel({
                                  endpoint_id: endpointId,
                                  upstream: model.upstream,
                                  alias: null,
                                  provider_name: null,
                                  enabled: true,
                                });
                                return `已登记 ${outcome.alias}，本次新增 ${outcome.added_models} 个模型`;
                              })
                            }
                          >
                            登记为供应商
                          </button>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <p className="sub">
              「思考」标记来自运行时的 <code>capabilities</code>，智能模式用它把复杂任务派给会思考的模型。
            </p>
          </div>
        );
      })}

      <div className="card">
        <h3>拉取新模型（仅 Ollama）</h3>
        <PullBox
          endpoints={runtimes.filter((item) => item.kind === "ollama" && item.reachable)}
          busy={busy}
          pulling={pulling}
          onPull={(endpointId, model) =>
            run(`pull:${model}`, async () => {
              await api.pullLocalModel(endpointId, model);
              await probe();
              return `${model} 拉取完成`;
            })
          }
        />
      </div>
    </>
  );
}

function PullBox({
  endpoints,
  busy,
  pulling,
  onPull,
}: {
  endpoints: ProbeOutcome[];
  busy: string | null;
  pulling: Record<string, string>;
  onPull: (endpointId: string, model: string) => Promise<void>;
}) {
  const [endpointId, setEndpointId] = useState(endpoints[0]?.id ?? "");
  const [model, setModel] = useState("qwen3:8b");

  // 下拉建议放在前端而不是后端：这是一份纯展示清单，
  // 让 Rust 维护它只会变成一个没人调用的 pub 函数。
  const SUGGESTED = [
    { name: "qwen3:8b", note: "中文强、8B，带工具与视觉" },
    { name: "qwen3:4b", note: "更小更快，适合简单任务" },
    { name: "gemma3:12b", note: "多模态，视觉 + 工具" },
    { name: "llama3.2:3b", note: "极轻量，适合本地简单任务" },
    { name: "deepseek-r1:8b", note: "带思维链，适合复杂任务" },
  ];

  if (endpoints.length === 0) {
    return <div className="empty">未检测到可达的 Ollama 运行时。</div>;
  }
  const active = endpointId || endpoints[0].id;
  return (
    <>
      <div className="row">
        <label className="field">
          <span>运行时</span>
          <select value={active} onChange={(event) => setEndpointId(event.target.value)}>
            {endpoints.map((item) => (
              <option key={item.id} value={item.id}>
                {item.label}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>模型名</span>
          <input value={model} onChange={(event) => setModel(event.target.value)} placeholder="例如 qwen3:8b" />
        </label>
        <button
          className="button"
          disabled={busy !== null || model.trim().length === 0}
          onClick={() => void onPull(active, model.trim())}
        >
          拉取
        </button>
      </div>
      <div className="caps">
        {SUGGESTED.map((item) => (
          <button
            key={item.name}
            className="badge"
            title={item.note}
            onClick={() => setModel(item.name)}
            type="button"
          >
            {item.name}
          </button>
        ))}
      </div>
      {Object.entries(pulling).length > 0 && (
        <div className="sub">
          {Object.entries(pulling)
            .map(([name, status]) => `${name}: ${status}`)
            .join("　|　")}
        </div>
      )}
      <p className="sub">
        下载体积由模型本身决定，可能几 GB 到几十 GB。网关不会自动拉任何模型，需要什么由你决定。
      </p>
    </>
  );
}

/* ------------------------------ 智能模式 ------------------------------ */

function SmartSection({ cfg, onSave }: { cfg: AppConfig; onSave: (c: AppConfig) => Promise<AppConfig> }) {
  const [probeText, setProbeText] = useState("为千万级用户的系统设计一套限流、降级、熔断方案");
  const [probeHasImage, setProbeHasImage] = useState(false);
  const [probeHasTools, setProbeHasTools] = useState(false);
  const [intent, setIntent] = useState<TaskIntent | null>(null);
  const [jev, setJev] = useState<JevProbeResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ kind: "ok" | "err"; text: string } | null>(null);

  const smart = cfg.smart_routing;
  // 老配置文件里没有 prompt_refine，反序列化会填默认值；
  // 这里仍兜一层，避免界面在读到旧快照时崩掉。
  const refine = smart.prompt_refine ?? {
    enabled: false,
    provider_id: null,
    model: null,
    timeout_ms: 2000,
    max_chars: 2000,
    clarity_noul: 0.72,
    min_chars: 24,
  };

  const patch = async (next: Partial<typeof smart>) => {
    setBusy(true);
    setMessage(null);
    try {
      await onSave({
        ...cfg,
        smart_routing: {
          ...smart,
          ...next,
          // 注意是**合并**而不是直接赋值。写成 `prompt_refine: refine` 会把
          // `next.prompt_refine` 整个盖掉，改预优化的任何一项都不会生效——
          // 症状是开关点了没反应，而且界面上看不出任何异常。
          prompt_refine: { ...refine, ...next.prompt_refine },
        },
      });
    } catch (error) {
      setMessage({ kind: "err", text: errorMessage(error) });
    } finally {
      setBusy(false);
    }
  };

  const patchJev = async (next: Partial<typeof smart.jev>) => {
    await patch({ jev: { ...smart.jev, ...next } });
  };

  const patchRefine = async (next: Partial<typeof refine>) => {
    await patch({ prompt_refine: { ...refine, ...next } });
  };

  return (
    <>
      <div className="card">
        <div className="row">
          <div>
            <h3>智能模式</h3>
            <p className="sub">
              请求进来后先定性为 <strong>简单任务 / 图像识别 / 复杂思考</strong>，再按类型挑模型。
              把全局路由策略设为 <code>smart</code>，或让客户端按请求用虚拟模型名 <code>smart</code>，两者都生效。
            </p>
          </div>
          <label className="field-inline">
            <input
              type="checkbox"
              checked={smart.enabled}
              disabled={busy}
              onChange={(event) => void patch({ enabled: event.target.checked })}
            />
            启用智能模式
          </label>
        </div>

        <div className="grid2">
          <label className="field">
            <span>分类器</span>
            <select
              value={smart.classifier}
              disabled={busy}
              onChange={(event) => void patch({ classifier: event.target.value as typeof smart.classifier })}
            >
              <option value="auto">自动（有 Jev 就用，不可用回落启发式）</option>
              <option value="jev">只用 Jev（不可用时回落启发式）</option>
              <option value="heuristic">只用启发式（完全本地、最快）</option>
            </select>
          </label>
          <label className="field">
            <span>分类超时（毫秒）</span>
            <input
              type="number"
              min={100}
              max={30000}
              value={smart.timeout_ms}
              disabled={busy}
              onChange={(event) => void patch({ timeout_ms: Number(event.target.value) || 1200 })}
            />
          </label>
          <label className="field">
            <span>Jev 最低置信度（低于则弃权）</span>
            <input
              type="number"
              step={0.05}
              min={0}
              max={1}
              value={smart.min_confidence}
              disabled={busy}
              onChange={(event) => void patch({ min_confidence: Number(event.target.value) })}
            />
          </label>
          <label className="field">
            <span>Jev 最低边际（低于则弃权）</span>
            <input
              type="number"
              step={0.05}
              min={0}
              max={1}
              value={smart.min_margin}
              disabled={busy}
              onChange={(event) => void patch({ min_margin: Number(event.target.value) })}
            />
          </label>
        </div>
        <p className="sub">
          弃权是<strong>正常路径</strong>：Jev 对「任务复杂度」这类离域问题常常分不清，
          此时应该让启发式说了算。先用下面的「试跑」看它的真实表现，再决定要不要放宽阈值。
        </p>
        <p className="sub">
          <strong>置信度高不等于判得对。</strong>本机实测里 edgeJev 曾以 0.747 的置信度
          把「线上排查根因」判成简单任务，而启发式是对的。所以只要启发式复杂度越过 50，
          Jev 说「简单」也不许把它降级——这条否决规则没有开关。
        </p>
        {message && <div className={`msg ${message.kind}`}>{message.text}</div>}
      </div>

      <div className="card">
        <h3>提示词预优化</h3>
        <p className="sub">
          Jev 只判断「这条提示词值不值得改写」，<strong>它产不出文本</strong>——
          edgeJev 走的是打分通道，<code>output_tokens</code> 恒为 0。
          改写本身会另调一次小模型，优先挑不思考的最轻模型。
        </p>
        <div className="row">
          <label className="check">
            <input
              type="checkbox"
              checked={refine.enabled}
              disabled={busy}
              onChange={(event) => void patchRefine({ enabled: event.target.checked })}
            />
            <span>启用提示词预优化</span>
          </label>
        </div>
        <div className="grid2">
          <label className="field">
            <span>改写供应商（留空 = 自动挑最轻的非思考模型）</span>
            <input
              value={refine.provider_id ?? ""}
              placeholder="留空自动"
              disabled={busy || !refine.enabled}
              onChange={(event) =>
                void patchRefine({ provider_id: event.target.value.trim() || null })
              }
            />
          </label>
          <label className="field">
            <span>改写模型名（留空 = 用该供应商的第一个）</span>
            <input
              value={refine.model ?? ""}
              placeholder="留空自动"
              disabled={busy || !refine.enabled}
              onChange={(event) =>
                void patchRefine({ model: event.target.value.trim() || null })
              }
            />
          </label>
          <label className="field">
            <span>硬超时（毫秒）</span>
            <input
              type="number"
              step={100}
              min={200}
              max={30000}
              value={refine.timeout_ms}
              disabled={busy || !refine.enabled}
              onChange={(event) => void patchRefine({ timeout_ms: Number(event.target.value) })}
            />
          </label>
          <label className="field">
            <span>结果长度上限（超过判失败，用原文）</span>
            <input
              type="number"
              step={100}
              min={100}
              max={8000}
              value={refine.max_chars}
              disabled={busy || !refine.enabled}
              onChange={(event) => void patchRefine({ max_chars: Number(event.target.value) })}
            />
          </label>
          <label className="field">
            <span>含糊阈值（clarity 低于则改写）</span>
            <input
              type="number"
              step={0.02}
              min={0}
              max={1}
              value={refine.clarity_noul}
              disabled={busy || !refine.enabled}
              onChange={(event) => void patchRefine({ clarity_noul: Number(event.target.value) })}
            />
          </label>
          <label className="field">
            <span>最短长度（短于此不改写）</span>
            <input
              type="number"
              step={4}
              min={0}
              max={500}
              value={refine.min_chars}
              disabled={busy || !refine.enabled}
              onChange={(event) => void patchRefine({ min_chars: Number(event.target.value) })}
            />
          </label>
        </div>
        <p className="sub">
          改写会<strong>替换发给上游的最后一条用户消息</strong>。任何一步失败都自动退回原文，
          一次请求最多改写一次且<strong>不重试</strong>；改动会在「用量与审计」里留下改写前后的长度。
        </p>
      </div>

      <div className="card">
        <h3>Jev 决策端点</h3>
        <p className="sub">
          本机 <code>edgeJev</code> 默认在 <code>http://127.0.0.1:8009</code>，与 Ollama 的
          <code>/v1/systemone</code> 同协议。网关不会自动下载任何决策模型。
        </p>
        <div className="grid2">
          <label className="field">
            <span>端点</span>
            <input
              value={smart.jev.base_url}
              disabled={busy}
              onChange={(event) => void patchJev({ base_url: event.target.value })}
            />
          </label>
          <label className="field">
            <span>模型名（edgeJev 会忽略，Ollama 需要）</span>
            <input value={smart.jev.model} disabled={busy} onChange={(event) => void patchJev({ model: event.target.value })} />
          </label>
          <label className="field">
            <span>请求超时（毫秒）</span>
            <input
              type="number"
              min={100}
              max={30000}
              value={smart.jev.timeout_ms}
              disabled={busy}
              onChange={(event) => void patchJev({ timeout_ms: Number(event.target.value) || 1200 })}
            />
          </label>
          <label className="field">
            <span>state 字符上限（保留尾部）</span>
            <input
              type="number"
              min={64}
              max={16000}
              value={smart.jev.max_state_chars}
              disabled={busy}
              onChange={(event) => void patchJev({ max_state_chars: Number(event.target.value) || 2000 })}
            />
          </label>
        </div>

        <div className="row">
          <label className="field-inline">
            <input
              type="checkbox"
              checked={smart.jev.auto_start.enabled}
              disabled={busy}
              onChange={(event) =>
                void patchJev({ auto_start: { ...smart.jev.auto_start, enabled: event.target.checked } })
              }
            />
            端点不可达时自动拉起 edgeJev
          </label>
        </div>
        {smart.jev.auto_start.enabled && (
          <div className="grid2">
            <label className="field">
              <span>edgejev.exe 路径</span>
              <input
                value={smart.jev.auto_start.exe_path}
                disabled={busy}
                onChange={(event) =>
                  void patchJev({ auto_start: { ...smart.jev.auto_start, exe_path: event.target.value } })
                }
              />
            </label>
            <label className="field">
              <span>模型目录</span>
              <input
                value={smart.jev.auto_start.model_dir}
                disabled={busy}
                onChange={(event) =>
                  void patchJev({ auto_start: { ...smart.jev.auto_start, model_dir: event.target.value } })
                }
              />
            </label>
          </div>
        )}
        <p className="sub">启动外部进程不可逆，因此默认关闭；路径与端口都由你显式填写。</p>
      </div>

      <div className="card">
        <h3>试跑</h3>
        <p className="sub">走的是<strong>与线上完全相同的分类链路</strong>，不是另写一份逻辑。</p>
        <textarea
          value={probeText}
          rows={3}
          onChange={(event) => setProbeText(event.target.value)}
          placeholder="输入一句你平时真会问的话"
        />
        <div className="row">
          <label className="field-inline">
            <input type="checkbox" checked={probeHasImage} onChange={(e) => setProbeHasImage(e.target.checked)} />
            含图片
          </label>
          <label className="field-inline">
            <input type="checkbox" checked={probeHasTools} onChange={(e) => setProbeHasTools(e.target.checked)} />
            带工具
          </label>
          <button
            className="button"
            disabled={busy || probeText.trim().length === 0}
            onClick={async () => {
              setBusy(true);
              setMessage(null);
              try {
                setIntent(await api.classifyPreview(probeText, probeHasImage, probeHasTools));
              } catch (error) {
                setMessage({ kind: "err", text: errorMessage(error) });
              } finally {
                setBusy(false);
              }
            }}
          >
            试跑分类器
          </button>
          <button
            className="button ghost"
            disabled={busy || probeText.trim().length === 0}
            onClick={async () => {
              setBusy(true);
              setMessage(null);
              try {
                setJev(await api.jevProbe(probeText));
              } catch (error) {
                setMessage({ kind: "err", text: errorMessage(error) });
              } finally {
                setBusy(false);
              }
            }}
          >
            查看 Jev 原始判定
          </button>
        </div>

        {intent && (
          <div className="local-result">
            <div>
              判定：<strong>{TASK_CLASS_LABEL[intent.class]}</strong>
            </div>
            <div>来源：{CLASSIFIER_LABEL[intent.classifier]}</div>
            <div>复杂度：{intent.complexity} / 100</div>
            <div>需要联网：{intent.needs_web ? "是" : "否"}</div>
            <div>值得改写：{intent.needs_refine ? "是" : "否"}</div>
            {intent.jev_note && <div className="sub">Jev 说明：{intent.jev_note}</div>}
          </div>
        )}

        {jev && (
          <div className="local-result">
            <div>
              端点：<code>{jev.endpoint}</code>
            </div>
            {!jev.ok && <div className="sub">不可用：{jev.error}</div>}
            {jev.rows?.map((row) => (
              <div key={row.name} className="sub">
                {row.name}（{row.kind}）= {row.summary}，置信度 {row.confidence.toFixed(3)}
                {row.margin !== null && `，边际 ${row.margin.toFixed(3)}`} →{" "}
                {row.adopted ? "会被采纳" : "弃权"}
              </div>
            ))}
          </div>
        )}
        {message && <div className={`msg ${message.kind}`}>{message.text}</div>}
      </div>

      <CalibrationCard busy={busy} onMessage={setMessage} />
    </>
  );
}

/* ------------------------------ 校准报告 ------------------------------ */

const CLASS_ORDER: TaskClass[] = ["simple", "vision", "reasoning"];

function CalibrationCard({
  busy,
  onMessage,
}: {
  busy: boolean;
  onMessage: (m: { kind: "ok" | "err"; text: string } | null) => void;
}) {
  const [report, setReport] = useState<CalibrationReport | null>(null);
  const [running, setRunning] = useState(false);

  const run = async (fn: () => Promise<CalibrationReport>) => {
    setRunning(true);
    onMessage(null);
    try {
      setReport(await fn());
    } catch (error) {
      onMessage({ kind: "err", text: errorMessage(error) });
    } finally {
      setRunning(false);
    }
  };

  return (
    <div className="card">
      <h3>校准：这个决策模型到底值不值得用</h3>
      <p className="sub">
        跑一组带正确答案的样本，算出混淆矩阵，再对比「采纳 Jev」与「不采纳（走启发式）」各对几条。
        <strong>净收益</strong>是唯一的决策依据：<strong>正数</strong>说明可以放宽阈值，
        <strong>负数</strong>说明采纳它反而更差，该加否决规则或直接关掉。
      </p>
      <p className="sub">
        校准<strong>只跑样本、不改任何配置</strong>——看完数据自己决定阈值，命令擅自调就越权了。
        默认样本是本机 edgeJev 实测的五条，其中「线上排查根因」是<strong>已知错判样本</strong>，
        刻意留在集里：删掉它报告会显示成一切正常，那种样本集没有诊断价值。
      </p>
      <div className="row">
        <button className="button" disabled={busy || running} onClick={() => void run(api.calibrateDefaultSamples)}>
          用实测样本校准
        </button>
      </div>

      {report && (
        <div className="local-result">
          <div className={`calib-verdict ${report.net_gain > 0 ? "ok" : report.net_gain < 0 ? "err" : "warn"}`}>
            {report.verdict}
          </div>
          <div className="calib-numbers">
            <span>净收益 <strong>{report.net_gain > 0 ? `+${report.net_gain}` : report.net_gain}</strong> 条</span>
            <span>采纳 {report.adopted_count}</span>
            <span>其中判对 {report.adopted_correct}、判错 {report.adopted_wrong}</span>
            <span>弃权 {report.abstained_count}（其中启发式恰好判对 {report.abstained_but_heuristic_right}）</span>
            <span>只用启发式会判对 {report.heuristic_correct} / {report.total}</span>
          </div>

          <table className="calib-matrix">
            <thead>
              <tr>
                <th>真实 ＼ 判定</th>
                {CLASS_ORDER.map((c) => (
                  <th key={c}>{TASK_CLASS_LABEL[c]}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {CLASS_ORDER.filter((expected) => report.matrix[TASK_CLASS_LABEL[expected]]).map((expected) => (
                <tr key={expected}>
                  <th>{TASK_CLASS_LABEL[expected]}</th>
                  {CLASS_ORDER.map((actual) => {
                    const count = report.matrix[TASK_CLASS_LABEL[expected]]?.[TASK_CLASS_LABEL[actual]] ?? 0;
                    return (
                      <td key={actual} className={count === 0 ? "zero" : expected === actual ? "hit" : "miss"}>
                        {count}
                      </td>
                    );
                  })}
                </tr>
              ))}
            </tbody>
          </table>
          <p className="sub">对角线是判对的格子；非对角线且非零的是判错。</p>

          {report.worst_wrong && (
            <div className="sub">
              最危险的一条错判：置信度 <strong>{report.worst_wrong.confidence.toFixed(3)}</strong>，
              却把「{report.worst_wrong.text}」判成了{TASK_CLASS_LABEL[report.worst_wrong.adopted]}，
              而正确答案是{TASK_CLASS_LABEL[report.worst_wrong.expected]}。
              <strong>高置信度不等于判得对</strong>——这就是「调阈值」解决不了的那类错。
            </div>
          )}

          <details>
            <summary>逐条明细（{report.per_sample.length} 条）</summary>
            {report.per_sample.map((row) => (
              <div key={row.text} className="sub">
                {row.text} → 正确{TASK_CLASS_LABEL[row.expected]}，
                启发式{TASK_CLASS_LABEL[row.heuristic]}，
                {row.adopted_from_jev ? (
                  <>
                    Jev 采纳为{TASK_CLASS_LABEL[row.adopted]}
                    {row.adopted === row.expected ? " ✓" : " ✗"}
                    （原始 {row.raw_choice}，置信度 {row.confidence.toFixed(3)}）
                  </>
                ) : (
                  <>Jev 弃权：{row.abstain_reason ?? "未说明"}</>
                )}
              </div>
            ))}
          </details>
        </div>
      )}
    </div>
  );
}

/* ----------------------------- 联网搜索 ----------------------------- */

function SearchSection({ cfg, onSave }: { cfg: AppConfig; onSave: (c: AppConfig) => Promise<AppConfig> }) {
  const [settings, setSettings] = useState<SearchSettingsView | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [clearKey, setClearKey] = useState(false);
  const [probeText, setProbeText] = useState("Rust 1.99 最近有什么新特性");
  const [outcome, setOutcome] = useState<SearchOutcome | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  /**
   * 「用户当下正在选的后端」，与**已保存**的 `settings.backend` 分开。
   *
   * 为什么必须有它：SearXNG 唯一能识别它的字段是 `searxng_url`，而后端
   * `search::validate()` 规定「选了 SearXNG 而地址为空 → 整条保存拒绝」。
   * 于是选中的那一刻保存必然失败、`settings.backend` 不变、地址输入框
   * （条件是 `settings.backend === "sear_xng"`）永远不渲染 ——
   * **用户被锁死在「选了却没法填地址」的死循环里**（用户 2026-10-05 报告）。
   *
   * 拆成「先在本地显示输入框 → 填完再提交」就绕开了：
   * 输入框的显隐取决于用户意图，后端校验只在地址非空后才会通过。
   */
  const [pendingBackend, setPendingBackend] = useState<SearchSettingsView["backend"] | null>(null);
  const shownBackend = pendingBackend ?? settings?.backend ?? null;
  const shownSearxngUrl = pendingBackend === "sear_xng" ? settings?.searxng_url ?? "" : settings?.searxng_url ?? "";

  const load = useCallback(async () => {
    try {
      setSettings(await api.getSearchSettings());
    } catch (error) {
      setMessage({ kind: "err", text: errorMessage(error) });
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async (next: Partial<SearchSettingsView>) => {
    const current = settings;
    if (!current) return;
    setBusy(true);
    setMessage(null);
    try {
      const saved = await api.updateSearchSettings({
        enabled: next.enabled ?? current.enabled,
        backend: next.backend ?? current.backend,
        searxng_url: next.searxng_url ?? current.searxng_url,
        max_results: next.max_results ?? current.max_results,
        timeout_ms: next.timeout_ms ?? current.timeout_ms,
        inject_as: next.inject_as ?? current.inject_as,
        api_key: keyInput.trim().length > 0 ? keyInput.trim() : null,
        clear_api_key: clearKey,
      });
      setSettings(saved);
      setKeyInput("");
      setClearKey(false);
      // 搜索开关与结果条数也影响网关运行时行为，必须落回 config。
      const nextCfg = await api.getConfig();
      setCfgThroughParent(nextCfg);
    } catch (error) {
      setMessage({ kind: "err", text: errorMessage(error) });
    } finally {
      setBusy(false);
    }
  };

  // 搜索设置有一部分存在 config.toml（开关、后端、条数），有一部分在加密表里。
  // 保存后要把 config 同步回父组件，路由运行时才拿得到最新值。
  const setCfgThroughParent = async (next: AppConfig) => {
    await onSave(next);
  };

  if (!settings) {
    return (
      <div className="card">
        <div className="empty">正在读取搜索设置…</div>
      </div>
    );
  }

  return (
    <>
      <div className="card">
        <div className="row">
          <div>
            <h3>联网搜索</h3>
            <p className="sub">
              分类判定需要最新事实时，网关<strong>在第一次上游调用之前</strong>完成检索并注入上下文。
              流式与非流式行为一致，客户端不需要改任何工具定义。
            </p>
          </div>
          <label className="field-inline">
            <input type="checkbox" checked={settings.enabled} disabled={busy} onChange={(e) => void save({ enabled: e.target.checked })} />
            启用联网搜索
          </label>
        </div>

        <div className="grid2">
          <label className="field">
            <span>后端</span>
            <select value={shownBackend ?? ""} disabled={busy} onChange={(e) => {
              const next = e.target.value as SearchSettingsView["backend"];
              setPendingBackend(next);
              // SearXNG 必须延后到地址填好再提交：直接保存会被后端 validate 拒绝，
              // 于是「选中的状态」永远存不下来，地址输入框也就永远等不到自己出现。
              if (next === "sear_xng" && !(settings?.searxng_url ?? "").trim()) return;
              void save({ backend: next });
            }}>
              <option value="bing_cn">必应中国（免 Key，国内可达）</option>
              <option value="tavily">Tavily（需 API Key）</option>
              <option value="brave">Brave（需 Subscription Token）</option>
              <option value="sear_xng">SearXNG（自建实例，免 Key）</option>
              <option value="duck_duck_go">DuckDuckGo（免 Key，部分网络不可达）</option>
            </select>
          </label>
          <label className="field">
            <span>结果条数（1–10）</span>
            <input
              type="number"
              min={1}
              max={10}
              value={settings.max_results}
              disabled={busy}
              onChange={(e) => void save({ max_results: Number(e.target.value) })}
            />
          </label>
          <label className="field">
            <span>超时（毫秒）</span>
            <input
              type="number"
              min={500}
              max={60000}
              value={settings.timeout_ms}
              disabled={busy}
              onChange={(e) => void save({ timeout_ms: Number(e.target.value) })}
            />
          </label>
          <label className="field">
            <span>注入身份</span>
            <select value={settings.inject_as} disabled={busy} onChange={(e) => void save({ inject_as: e.target.value as SearchSettingsView["inject_as"] })}>
              <option value="system">system 消息（推荐）</option>
              <option value="user">user 消息</option>
            </select>
          </label>
          {shownBackend === "sear_xng" && (
            <label className="field">
              <span>SearXNG 实例地址</span>
              <input
                value={shownSearxngUrl}
                disabled={busy}
                placeholder="http://127.0.0.1:8888"
                onChange={(e) => {
                  const url = e.target.value;
                  // 后端和地址**一起**提交：只存地址时后端仍是上一个后端，
                  // 用户看到的「已切到 SearXNG」就不成立。
                  void save({ searxng_url: url, backend: "sear_xng" }).then(() => {
                    // 保存成功才清 pending：失败（地址非法/网络问题）时保持选中态，
                    // 否则用户会看到下拉跳回旧后端、输入框消失，且不知道输入去哪了。
                    setPendingBackend(null);
                  }).catch(() => { /* 错误已在 save() 里落到 message */ });
                }}
              />
            </label>
          )}
          {shownBackend === "sear_xng" && !(settings?.searxng_url ?? "").trim() && (
            <p className="sub">
              填入实例地址后才会保存。留空时后端会拒绝这条配置（选择 SearXNG 后端时必须填写实例地址）。
            </p>
          )}
        </div>

        {/* 与 SearXNG 地址框同一个道理：`backend_needs_key` 是**已保存**配置的回传值，
            刚选中 Tavily/Brave 时它还是旧值，Key 框要等保存成功才出现。改用
            `shownBackend` 直接判断，让输入框跟着用户意图出现，不用等一次往返。 */}
        {(shownBackend === "tavily" || shownBackend === "brave") && (
          <>
            <label className="field">
              <span>API Key（加密存库，不进 config.toml）</span>
              <input
                type="password"
                value={keyInput}
                disabled={busy}
                placeholder={settings.api_key_masked ?? "尚未配置"}
                onChange={(event) => setKeyInput(event.target.value)}
              />
            </label>
            <label className="field-inline">
              <input type="checkbox" checked={clearKey} disabled={busy} onChange={(event) => setClearKey(event.target.checked)} />
              保存时删除已存 Key
            </label>
            <button className="button" disabled={busy || (keyInput.trim().length === 0 && !clearKey)} onClick={() => void save({})}>
              保存 Key
            </button>
          </>
        )}

        <p className="sub">
          Key 返回 401/403 时<strong>不会</strong>自动回落到免 Key 后端——那会用错误的凭据反复打别人的服务。
          搜索失败也不会阻断请求，响应头 <code>X-Route-Search: failed</code> 会如实说明。
        </p>
        {message && <div className={`msg ${message.kind}`}>{message.text}</div>}
      </div>

      <div className="card">
        <h3>测试后端</h3>
        <div className="row">
          <input
            className="field-grow"
            value={probeText}
            onChange={(event) => setProbeText(event.target.value)}
            placeholder="输入一个真实的检索词"
          />
          <button
            className="button"
            disabled={busy || probeText.trim().length === 0}
            onClick={async () => {
              setBusy(true);
              setMessage(null);
              try {
                const result = await api.testSearchBackend(probeText);
                setOutcome(result);
                setMessage(
                  result.error
                    ? { kind: "err", text: result.error }
                    : { kind: "ok", text: `命中 ${result.hits} 条` }
                );
              } catch (error) {
                setMessage({ kind: "err", text: errorMessage(error) });
              } finally {
                setBusy(false);
              }
            }}
          >
            搜索一次
          </button>
        </div>
        {outcome && (
          <div className="local-result">
            {outcome.results.map((item, index) => (
              <div key={item.url} className="sub">
                {index + 1}. {item.title} — {item.url}
                {item.snippet ? `　${item.snippet.slice(0, 120)}` : ""}
              </div>
            ))}
          </div>
        )}
      </div>
    </>
  );
}

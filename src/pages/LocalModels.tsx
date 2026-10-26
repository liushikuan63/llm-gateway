import { MdnReferenceCard, mdnReferenceStatus, parseMdnReference } from "../components/MdnReference";
import { StackOverflowProtection, parseStackOverflowState, stackOverflowErrorText } from "../components/StackOverflowProtection";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  api,
  AppConfig,
  CalibrationReport,
  CLASSIFIER_LABEL,
  JevProbeResult,
  LocalModelInfo,
  ProbeOutcome,
  FreeSearchLocale,
  SearchOutcome,
  SearchSettingsView,
  SEARCH_BACKEND_LABEL,
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

function searchResultHref(raw: string): string | undefined {
  try {
    const url = new URL(raw);
    return ["http:", "https:"].includes(url.protocol) && !url.username && !url.password ? url.href : undefined;
  } catch { return undefined; }
}

function searchSourceLabel(source: string): string {
  return source === "stack_overflow" ? "Stack Overflow" : source === "bing_rss" ? "Bing RSS" : source === "duckduckgo" ? "DuckDuckGo" : source === "npm_registry" ? "npm 软件包" : source === "hacker_news" ? "Hacker News 讨论" : "搜索来源";
}

function searchResourceStats(value: unknown): NonNullable<SearchSettingsView["resources"]> | null {
  if(!value || typeof value!=="object" || Array.isArray(value))return null;
  const s=value as NonNullable<SearchSettingsView["resources"]>;
  return s.schema_version===1&&[s.entries,s.inflight,s.cache_hits,s.cache_misses,s.coalesced,s.queries_started,s.rejected].every(n=>Number.isSafeInteger(n)&&n>=0)&&s.entries<=128&&s.inflight<=32?s:null;
}

function searchProtection(value:unknown,scope:SearchSettingsView["free_search_scope"]):NonNullable<SearchSettingsView["source_protection"]>|null {
  if(!value||typeof value!=="object"||Array.isArray(value))return null;
  const v=value as NonNullable<SearchSettingsView["source_protection"]>;
  if(scope!==undefined&&scope!=="web"&&scope!=="npm_packages"&&scope!=="hacker_news")return null;
  const expected=scope==="npm_packages"?["npm_registry"]:scope==="hacker_news"?["hacker_news"]:["bing_rss","duckduckgo"];
  if(v.schema_version!==1||!Array.isArray(v.sources)||v.sources.length!==expected.length||new Set(v.sources.map(s=>s?.source)).size!==expected.length)return null;
  return v.sources.every(s=>s&&expected.includes(s.source)&&["cooldown","cooldown_pending","not_blocked_locally","unavailable"].includes(s.state)&&(s.not_before_utc===null||typeof s.not_before_utc==="string"&&Number.isFinite(Date.parse(s.not_before_utc)))&&(["cooldown","cooldown_pending"].includes(s.state)?s.not_before_utc!==null:true)&&(s.state==="unavailable"?s.not_before_utc===null:true))?v:null;
}

function selectableSearchScopes(settings:SearchSettingsView):Array<NonNullable<SearchSettingsView["free_search_scope"]>>|null {
  const values=settings.free_search_scopes;
  if(values===undefined)return settings.free_search_scope==="web"||settings.free_search_scope==="npm_packages"?["web","npm_packages"]:null;
  const known=["web","npm_packages","hacker_news","stack_overflow"] as const;
  return Array.isArray(values)&&values.length>0&&values.length<=4&&new Set(values).size===values.length&&values.every(v=>known.includes(v))&&values.includes(settings.free_search_scope!)?values:null;
}

const SEARCH_LOCALE_LABEL:Record<FreeSearchLocale,string>={source_default:"来源默认",cn_zh:"中国中文",us_en:"美国英文",gb_en:"英国英文"};
function selectableSearchLocales(settings:SearchSettingsView):FreeSearchLocale[]|null {
  const values=settings.free_search_locales;
  return Array.isArray(values)&&values.length>0&&values.length<=4&&new Set(values).size===values.length&&values.every(v=>Object.prototype.hasOwnProperty.call(SEARCH_LOCALE_LABEL,v))&&values.includes(settings.free_search_locale!)?values:null;
}
function searchLocaleLabel(value:unknown):string {
  return typeof value==="string"&&Object.prototype.hasOwnProperty.call(SEARCH_LOCALE_LABEL,value)?SEARCH_LOCALE_LABEL[value as FreeSearchLocale]:"未知（接口未提供可用偏好）";
}

function searchProxyValid(raw:string):boolean {
  const text=raw.trim();if(text==="")return true;
  if(text.length>2048||/[\s\u0000-\u001f\u007f-\u009f]/.test(text)||text.includes('@'))return false;
  try {const u=new URL(text);if(!["http:","socks5h:"].includes(u.protocol)||!u.hostname||u.username||u.password||!["","/"].includes(u.pathname)||u.search||u.hash||u.port==="0")return false;
    if(u.protocol==="socks5h:"){const ipv4=u.hostname.split('.');return u.hostname.startsWith('[')&&u.hostname.endsWith(']')||ipv4.length===4&&ipv4.every(p=>/^\d+$/.test(p)&&Number(p)<=255&&String(Number(p))===p);}
    return true;
  } catch {return false;}
}

// Source/deadline/partial-result design: docs/wiki/search.md.
type SearchProbeJob = { id: string | null; ticket: number; ready: boolean; leaving: boolean; cancelPending: boolean; cancelAccepted: boolean };
function SearchSection({ cfg, onSave }: { cfg: AppConfig; onSave: (c: AppConfig) => Promise<AppConfig> }) {
  const [settings, setSettings] = useState<SearchSettingsView | null>(null);
  const [readLoading, setReadLoading] = useState(true);
  const [readError, setReadError] = useState<string | null>(null);
  const readFlight = useRef<number | null>(null);
  const readOrigin = useRef<HTMLElement | null>(null);
  const queryInput = useRef<HTMLInputElement | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [clearKey, setClearKey] = useState(false);
  const [probeText, setProbeText] = useState("Rust 1.99 最近有什么新特性");
  const [outcome, setOutcome] = useState<SearchOutcome | null>(null);
  const [outcomeInput, setOutcomeInput] = useState<string | null>(null);
  const [outcomeUnconfirmed, setOutcomeUnconfirmed] = useState(false);
  const [probeCancelled, setProbeCancelled] = useState(false);
  const [probe, setProbe] = useState<SearchProbeJob | null>(null);
  const probeJob = useRef<SearchProbeJob | null>(null);
  const [busy, setBusy] = useState(false);
  const locked = busy || readError !== null;
  const [message, setMessage] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const [cacheDraft,setCacheDraft]=useState("");
  const [intervalDraft,setIntervalDraft]=useState("");
  const [proxyDraft,setProxyDraft]=useState("");
  const [resourcesReceivedAt,setResourcesReceivedAt]=useState<string|null>(null);
  const cacheDirty=useRef(false),alive=useRef(false),saving=useRef(false),sequence=useRef(0);
  const intervalDirty=useRef(false);
  const proxyDirty=useRef(false);
  const returnFocus=useRef<HTMLElement|null>(null);
  useEffect(() => {
    if (readLoading || !readOrigin.current) return;
    const origin = readOrigin.current; readOrigin.current = null;
    if (document.activeElement === document.body || document.activeElement === origin) {
      if (origin.isConnected) origin.focus(); else queryInput.current?.focus();
    }
  }, [readLoading]);
  const currentProbe = (job: SearchProbeJob) => alive.current && probeJob.current === job && sequence.current === job.ticket;
  const cancelProbe = async (job: SearchProbeJob | null) => {
    if (!job?.id || !job.ready || job.cancelPending || job.cancelAccepted) return;
    job.cancelPending = true;
    if (currentProbe(job)) setProbe({ ...job });
    try {
      const accepted = await api.cancelSearchBackend(job.id);
      if (!currentProbe(job)) return;
      job.cancelAccepted = accepted;
      setMessage({ kind: "ok", text: accepted ? "已请求取消，等待检索结果确认。" : "未接受取消，继续等待检索结果。" });
    } catch (error) {
      // Unmount is best-effort: no false terminal claim or automatic retry.
      if (currentProbe(job)) setMessage({ kind: "err", text: `取消请求失败：${errorMessage(error)}；检索仍在进行。` });
    } finally {
      job.cancelPending = false;
      if (currentProbe(job)) setProbe({ ...job });
    }
  };
  useEffect(()=>{if(!busy&&returnFocus.current){const origin=returnFocus.current;returnFocus.current=null;if(!origin.isConnected&&origin.dataset.stackRecovery==="true"&&document.activeElement===document.body){queryInput.current?.focus();}else if(origin.isConnected&&(document.activeElement===document.body||document.activeElement===origin)){const group=origin.closest("fieldset");const preferred=origin.dataset.focusReturn?document.getElementById(origin.dataset.focusReturn):null;const target=origin.matches(":disabled")?(preferred??group?.querySelector<HTMLElement>('input[type="number"]:not(:disabled)')??group?.querySelector<HTMLElement>('input:not(:disabled)')):origin;target?.focus();}}});
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
    if(!alive.current || saving.current || readFlight.current !== null)return;
    const ticket=++sequence.current;
    readFlight.current=ticket;setReadLoading(true);
    try {
      const value=await api.getSearchSettings();
      if(alive.current&&ticket===sequence.current){setSettings(value);setReadError(null);setResourcesReceivedAt(new Date().toISOString());if(!cacheDirty.current)setCacheDraft(value.query_cache_ttl_secs===undefined?"":String(value.query_cache_ttl_secs));if(!intervalDirty.current)setIntervalDraft(value.source_min_interval_ms===undefined?"":String(value.source_min_interval_ms));if(!proxyDirty.current)setProxyDraft(typeof value.free_proxy_url==="string"?value.free_proxy_url:"");}
    } catch (error) {
      if(alive.current&&ticket===sequence.current)setReadError(`读取搜索设置失败：${errorMessage(error)}`);
    } finally {
      if(readFlight.current===ticket)readFlight.current=null;
      if(alive.current&&ticket===sequence.current)setReadLoading(false);
    }
  }, []);

  const reread = () => {
    if (saving.current || readFlight.current !== null) return;
    readOrigin.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    void load();
  };

  useEffect(() => {
    alive.current=true;
    void load();
    return()=>{
      alive.current=false;++sequence.current;readFlight.current=null;readOrigin.current=null;
      const job=probeJob.current;
      if(job){job.leaving=true;void cancelProbe(job);probeJob.current=null;}
    };
  }, [load]);

  const save = async (next: Partial<SearchSettingsView>) => {
    const current = settings;
    if (!current || saving.current || readError !== null) return;
    // A user mutation supersedes a healthy background read; its late result
    // cannot publish over the newer commit or clear another read owner.
    readFlight.current=null;readOrigin.current=null;setReadLoading(false);
    saving.current=true;const ticket=++sequence.current;
    returnFocus.current=document.activeElement instanceof HTMLElement?document.activeElement:null;
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
        query_cache_ttl_secs: next.query_cache_ttl_secs ?? (Number.isSafeInteger(current.query_cache_ttl_secs)&&current.query_cache_ttl_secs!==undefined&&current.query_cache_ttl_secs>=0&&current.query_cache_ttl_secs<=3600?current.query_cache_ttl_secs:undefined),
        coalesce_queries: next.coalesce_queries ?? (typeof current.coalesce_queries==="boolean"?current.coalesce_queries:undefined),
        source_min_interval_ms: next.source_min_interval_ms ?? (Number.isSafeInteger(current.source_min_interval_ms)&&current.source_min_interval_ms!==undefined&&current.source_min_interval_ms>=0&&current.source_min_interval_ms<=3600000?current.source_min_interval_ms:undefined),
        free_search_scope: "free_search_scope" in next?next.free_search_scope:undefined,
        free_search_locale: "free_search_locale" in next?next.free_search_locale:undefined,
        // Only an explicit endpoint save may change transport; another window
        // may have committed a newer route since this view was loaded.
        free_proxy_url: "free_proxy_url" in next?next.free_proxy_url:undefined,
        page_text_enabled: "page_text_enabled" in next?next.page_text_enabled:undefined,
        api_key: keyInput.trim().length > 0 ? keyInput.trim() : null,
        clear_api_key: clearKey,
      });
      if(!alive.current||ticket!==sequence.current)return;
      setSettings(saved);
      setResourcesReceivedAt(new Date().toISOString());
      if(next.query_cache_ttl_secs!==undefined){cacheDirty.current=false;setCacheDraft(String(saved.query_cache_ttl_secs));}
      else if(!cacheDirty.current)setCacheDraft(saved.query_cache_ttl_secs===undefined?"":String(saved.query_cache_ttl_secs));
      if(next.source_min_interval_ms!==undefined){intervalDirty.current=false;setIntervalDraft(saved.source_min_interval_ms===undefined?"":String(saved.source_min_interval_ms));}
      else if(!intervalDirty.current)setIntervalDraft(saved.source_min_interval_ms===undefined?"":String(saved.source_min_interval_ms));
      if("free_proxy_url" in next){proxyDirty.current=false;setProxyDraft(typeof saved.free_proxy_url==="string"?saved.free_proxy_url:"");}
      else if(!proxyDirty.current)setProxyDraft(typeof saved.free_proxy_url==="string"?saved.free_proxy_url:"");
      setOutcome(null); setOutcomeInput(null); setOutcomeUnconfirmed(false);
      setKeyInput("");
      setClearKey(false);
      // 搜索开关与结果条数也影响网关运行时行为，必须落回 config。
      const nextCfg = await api.getConfig();
      if(alive.current&&ticket===sequence.current)await setCfgThroughParent(nextCfg);
    } catch (error) {
      if(alive.current&&ticket===sequence.current)setMessage({ kind: "err", text: errorMessage(error) });
    } finally {
      saving.current=false;if(alive.current&&ticket===sequence.current)setBusy(false);
    }
  };

  // 搜索设置有一部分存在 config.toml（开关、后端、条数），有一部分在加密表里。
  // 保存后要把 config 同步回父组件，路由运行时才拿得到最新值。
  const setCfgThroughParent = async (next: AppConfig) => {
    await onSave(next);
  };

  const recoverStack = async () => {
    if(saving.current||readError!==null||!settings||settings.backend!=="free_aggregate"||settings.free_search_scope!=="stack_overflow"
      ||parseStackOverflowState(settings.stack_overflow_state)?.state!=="reserved")return;
    readFlight.current=null;readOrigin.current=null;setReadLoading(false);
    saving.current=true;const ticket=++sequence.current;
    returnFocus.current=document.activeElement instanceof HTMLElement?document.activeElement:null;
    setBusy(true);setMessage(null);
    try{
      const raw=await api.recoverStackOverflowUnsent();
      if(!alive.current||ticket!==sequence.current)return;
      const status=parseStackOverflowState(raw);
      setSettings(v=>v?.backend==="free_aggregate"&&v.free_search_scope==="stack_overflow"?{...v,stack_overflow_state:status??undefined}:v);
      setMessage(status?{kind:"ok",text:"本机预留状态已确认，原等待限制保留；未发送检索。"}:{kind:"err",text:"恢复回执未确认，请重新读取本机状态；不自动重试。"});
    }catch{
      if(alive.current&&ticket===sequence.current){
        setSettings(v=>v?{...v,stack_overflow_state:undefined}:v);
        setMessage({kind:"err",text:"恢复结果未确认，请重新读取本机状态；不自动重试或重置。"});
      }
    }finally{saving.current=false;if(alive.current&&ticket===sequence.current)setBusy(false);}
  };

  const startProbe = async () => {
    if (saving.current || readError !== null) return;
    if(settings?.backend==="free_aggregate"&&settings.free_search_scope==="stack_overflow"
      &&(shownBackend!=="free_aggregate"||parseStackOverflowState(settings.stack_overflow_state)?.state!=="ready"))return;
    readFlight.current=null;readOrigin.current=null;setReadLoading(false);
    const controlled = settings?.manual_search_cancel_supported === true;
    if (controlled && typeof crypto.randomUUID !== "function") {
      setMessage({ kind: "err", text: "当前环境无法创建安全请求标识，未发起检索。" }); return;
    }
    saving.current = true;
    const job: SearchProbeJob = { id: controlled ? crypto.randomUUID() : null, ticket: ++sequence.current, ready: false, leaving: false, cancelPending: false, cancelAccepted: false };
    probeJob.current = job; setProbe(controlled ? { ...job } : null);
    returnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setBusy(true); setMessage(null); setProbeCancelled(false); setOutcomeUnconfirmed(true);
    const submittedInput = probeText;
    try {
      const result = await api.testSearchBackend(submittedInput, job.id ? {
        requestId: job.id,
        onReady: ready => {
          if (!ready || ready.request_id !== job.id) return;
          job.ready = true;
          if (job.leaving) { void cancelProbe(job); return; }
          if (currentProbe(job)) setProbe({ ...job });
        },
      } : undefined);
      if (!currentProbe(job)) return;
      setOutcome(result); setOutcomeInput(submittedInput); setOutcomeUnconfirmed(false);
      setMessage(result.error ? { kind: "err", text: settings?.backend==="free_aggregate"&&settings.free_search_scope==="stack_overflow"?stackOverflowErrorText(result.error):result.error } : { kind: result.unstructured_reference != null && !parseMdnReference(result.unstructured_reference) ? "err" : "ok", text: mdnReferenceStatus(result.unstructured_reference) ?? `命中 ${result.hits} 条` });
    } catch (error) {
      if (!currentProbe(job)) return;
      if (error === "search_backend_cancelled") {
        setProbeCancelled(true);
        setMessage({ kind: "ok", text: "本次检索已取消，输入已保留。" });
      } else setMessage({ kind: "err", text: settings?.backend==="free_aggregate"&&settings.free_search_scope==="stack_overflow"?stackOverflowErrorText(errorMessage(error)):errorMessage(error) });
    } finally {
      if(currentProbe(job)&&settings?.backend==="free_aggregate"&&settings.free_search_scope==="stack_overflow"){
        try{
          const read=await api.getSearchSettings();
          if(currentProbe(job))setSettings(v=>v?.backend==="free_aggregate"&&v.free_search_scope==="stack_overflow"?{...v,stack_overflow_state:read.backend==="free_aggregate"&&read.free_search_scope==="stack_overflow"?read.stack_overflow_state:undefined}:v);
        }catch{
          if(currentProbe(job)){
            setSettings(v=>v?{...v,stack_overflow_state:undefined}:v);
            setMessage({kind:"err",text:"检索结果保留；本机保护状态读取失败，重新读取后再尝试。"});
          }
        }
      }
      if (probeJob.current === job) {
        probeJob.current = null; saving.current = false;
        if (alive.current && job.ticket === sequence.current) { setProbe(null); setBusy(false); }
      }
    }
  };

  if (!settings) {
    return (
      <div className="card">
        <h3>联网搜索</h3>
        {readLoading && <div className="empty" role="status">正在读取搜索设置…</div>}
        {readError && <div className="msg err" role="alert">{readError}。当前设置尚未确认。</div>}
        <button type="button" disabled={readLoading || busy} onClick={reread}>重试读取搜索设置</button>
      </div>
    );
  }

  const resourceFields=Number.isSafeInteger(settings.query_cache_ttl_secs)&&settings.query_cache_ttl_secs!==undefined&&settings.query_cache_ttl_secs>=0&&settings.query_cache_ttl_secs<=3600&&typeof settings.coalesce_queries==="boolean";
  const ttl=Number(cacheDraft),validTtl=cacheDraft.trim()!==""&&Number.isSafeInteger(ttl)&&ttl>=0&&ttl<=3600;
  const interval=Number(intervalDraft),validInterval=intervalDraft.trim()!==""&&Number.isSafeInteger(interval)&&interval>=0&&interval<=3600000;
  const intervalField=Number.isSafeInteger(settings.source_min_interval_ms)&&settings.source_min_interval_ms!==undefined&&settings.source_min_interval_ms>=0&&settings.source_min_interval_ms<=3600000;
  const resourceStats=searchResourceStats(settings.resources);
  const protection=searchProtection(settings.source_protection,settings.free_search_scope);
  const stackSelected=shownBackend==="free_aggregate"&&settings.free_search_scope==="stack_overflow";
  const stackProbeBlocked=settings.backend==="free_aggregate"&&settings.free_search_scope==="stack_overflow"&&(shownBackend!=="free_aggregate"||parseStackOverflowState(settings.stack_overflow_state)?.state!=="ready");
  const availableScopes=selectableSearchScopes(settings);
  const availableLocales=selectableSearchLocales(settings);
  const proxySupported=settings.free_proxy_url===null||typeof settings.free_proxy_url==="string"&&settings.free_proxy_url!==""&&searchProxyValid(settings.free_proxy_url);
  const validProxy=searchProxyValid(proxyDraft);

  return (
    <>
      {readLoading && <p role="status">正在读取搜索设置；当前显示上次读取结果。</p>}
      {readError && <div className="msg err" role="alert">{readError}。保留上次读取结果，当前设置尚未确认。<button type="button" disabled={readLoading || busy} onClick={reread}>重试读取搜索设置</button></div>}
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
            <input type="checkbox" checked={settings.enabled} disabled={locked} onChange={(e) => void save({ enabled: e.target.checked })} />
            启用联网搜索
          </label>
        </div>

        <div className="grid2">
          <label className="field">
            <span>后端</span>
            <select value={shownBackend ?? ""} disabled={locked} onChange={(e) => {
              const next = e.target.value as SearchSettingsView["backend"];
              setPendingBackend(next);
              // SearXNG 必须延后到地址填好再提交：直接保存会被后端 validate 拒绝，
              // 于是「选中的状态」永远存不下来，地址输入框也就永远等不到自己出现。
              if (next === "sear_xng" && !(settings?.searxng_url ?? "").trim()) return;
              void save({ backend: next });
            }}>
              <option value="bing_cn">必应中国（免 Key，国内可达）</option>
              <option value="free_aggregate">{settings.free_search_scope==="stack_overflow"?"免费检索（Stack Overflow 问题）":settings.free_search_scope==="npm_packages"?"免费检索（npm 软件包范围）":settings.free_search_scope==="hacker_news"?"免费检索（Hacker News 范围）":"免费聚合（Bing RSS + DuckDuckGo）"}</option>
              <option value="mdn_mcp">MDN 实验性 MCP（无需 Key）</option>
              <option value="tavily">Tavily（需 API Key）</option>
              <option value="brave">Brave（需 Subscription Token）</option>
              <option value="sear_xng">SearXNG（自建实例，免 Key）</option>
              <option value="duck_duck_go">DuckDuckGo（免 Key，部分网络不可达）</option>
            </select>
          </label>
          <label className="field">
            <span>{shownBackend === "mdn_mcp" ? "文档数未提供" : "结果条数（1–10）"}</span>
            <input
              type="number"
              min={1}
              max={10}
              value={shownBackend === "mdn_mcp" ? "" : settings.max_results}
              disabled={locked || shownBackend === "mdn_mcp"}
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
              disabled={locked}
              onChange={(e) => void save({ timeout_ms: Number(e.target.value) })}
            />
          </label>
          <label className="field">
            <span>注入身份</span>
            <select value={shownBackend === "mdn_mcp" ? "user" : settings.inject_as} disabled={locked || shownBackend === "mdn_mcp"} onChange={(e) => void save({ inject_as: e.target.value as SearchSettingsView["inject_as"] })}>
              <option value="system">system 消息（推荐）</option>
              <option value="user">user 消息</option>
            </select>
          </label>
          {shownBackend === "sear_xng" && (
            <label className="field">
              <span>SearXNG 实例地址</span>
              <input
                value={shownSearxngUrl}
                disabled={locked}
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

        {shownBackend === "mdn_mcp" && <p role="note" className="sub">
          实验来源直接向 mcp.mdn.mozilla.net 发送检索词，仅用于公开文档查询，请勿提交私密内容；不携带账号 Key 或 Cookie。
          服务可能留存查询；第一方分析退出头不是不记录的保证。费用与配额尚未独立核验，服务可用性无保证。
          每次最多三个固定协议 POST，服务内部可能继续读取文档元数据；无自动工具扩展、不自动回落或重试。
          结果为不可信文本，文档数未知，始终以 user 资料消息注入；条数、正文读取与通用网页代理设置不适用。
        </p>}

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
                disabled={locked}
                placeholder={settings.api_key_masked ?? "尚未配置"}
                onChange={(event) => setKeyInput(event.target.value)}
              />
            </label>
            <label className="field-inline">
              <input type="checkbox" checked={clearKey} disabled={locked} onChange={(event) => setClearKey(event.target.checked)} />
              保存时删除已存 Key
            </label>
            <button className="button" disabled={locked || (keyInput.trim().length === 0 && !clearKey)} onClick={() => void save({})}>
              保存 Key
            </button>
          </>
        )}

        {shownBackend === "free_aggregate" && <>
          {availableScopes?<label className="field"><span>免费聚合检索范围</span><select aria-label="免费聚合检索范围" value={settings.free_search_scope} disabled={locked} onChange={e=>void save({free_search_scope:e.target.value as NonNullable<SearchSettingsView["free_search_scope"]>})}>{availableScopes.map(value=><option key={value} value={value}>{{web:"通用网页（Bing RSS + DuckDuckGo）",npm_packages:"npm 软件包（仅 npm）",hacker_news:"Hacker News 技术讨论（仅 HN）",stack_overflow:"Stack Overflow 问题（仅问题元数据）"}[value]}</option>)}</select></label>:<p role="note">当前后端未提供可用的检索范围设置；检索范围能力不可用，不能推断支持新的来源。</p>}
          {availableLocales?<label className="field"><span>网页地区／语言请求偏好</span><select aria-label="网页地区／语言请求偏好" value={settings.free_search_locale} disabled={locked||shownBackend!=="free_aggregate"||settings.free_search_scope!=="web"} onChange={e=>void save({free_search_locale:e.target.value as FreeSearchLocale})}>{availableLocales.map(v=><option key={v} value={v}>{SEARCH_LOCALE_LABEL[v]}</option>)}</select></label>:<p role="note">当前后端未确认地区／语言请求偏好能力；旧值保持未知。</p>}
          <p className="sub">仅免费聚合的通用网页使用此请求偏好，不保证结果严格按地区或语言过滤。其他后端、npm 和 Hacker News 不应用；切换偏好不会清除来源等待。</p>
          {settings.free_search_scope==="hacker_news"&&<p className="sub">检索词只发送到 Hacker News 搜索服务，不混发网页／npm；需编程、数据库、open source、Show HN等明确技术查询，仅搜索story元数据，不读取讨论／目标网页正文。至少360毫秒本机接口间隔，较长用户间隔或Retry-After仍优先；不证明共享IP额度或真实可用性。</p>}
          {settings.free_search_scope==="stack_overflow"?<p className="sub">查询只发给 Stack Exchange API 的 Stack Overflow 站点，只取问题元数据；不抓取正文、不携带模型或账号凭据。至少保留一分钟本机间隔，并遵守更长设置、上游等待与共享 IP 限制。</p>:settings.free_search_scope==="npm_packages"?<p className="sub">检索词只发送到 npm 注册表，不发送给 Bing／DuckDuckGo；仅接受 npm package、npm 包、package.json 等明确包查询，普通中英文本会在发送前拒绝。不安装或执行软件包，结果不代表包安全性或推荐质量。</p>:settings.free_search_scope===undefined||settings.free_search_scope==="web"?<p className="sub">检索词将直接发送给 Bing RSS 和 DuckDuckGo；至少一源成功时保留结果。</p>:<p role="note">未知检索范围，无法确认目标；请读取兼容设置。</p>}
        </>}
        {shownBackend === "free_aggregate" && <fieldset aria-label="搜索独立出口" disabled={locked}>
          <legend>搜索独立出口</legend>
          {proxySupported?<>
            <label className="field"><span>搜索专用出口 URL（留空直连，不含认证）</span><input id="search-free-proxy" value={proxyDraft} autoComplete="off" spellCheck={false} placeholder="http://127.0.0.1:8080 或 socks5h://127.0.0.1:1080" onChange={e=>{proxyDirty.current=true;setProxyDraft(e.target.value);}}/></label>
            <button type="button" className="button" data-focus-return="search-free-proxy" disabled={!validProxy||proxyDraft.trim()===(settings.free_proxy_url??"")} onClick={()=>void save({free_proxy_url:proxyDraft.trim()||null})}>保存搜索专用出口</button>
            {proxyDirty.current&&!validProxy&&<p role="alert">出口须为不含认证、路径或参数的HTTP／SOCKS5h地址；SOCKS代理端点须使用IP，尚未保存。</p>}
            <p role="status" aria-live="polite">{busy?"正在保存搜索设置；当前显示已保存出口。 ":""}{settings.free_proxy_url===null?"已保存：直连":"已保存：自定义出口 "+settings.free_proxy_url}</p>
          </>:<p role="note">当前后端未提供可用的搜索出口设置，不能推断为直连或已配置。</p>}
          <small>仅免费检索范围使用，不修改模型／系统代理或读取其凭据。失败不改用直连；已发出的请求保留原出口，切换不清除源等待。SOCKS5h目标域名交由代理解析；不在此测试外网可用性。</small>
        </fieldset>}
        {shownBackend === "free_aggregate" && <p className="sub">免费聚合整体最多等待 10 秒；设置更短期限时按更短期限执行。</p>}
        {shownBackend === "free_aggregate" && <fieldset aria-label="公开结果正文" disabled={locked}>
          <legend>公开结果正文（默认关闭）</legend>
          {typeof settings.page_text_enabled === "boolean" ? <label className="field-inline"><input type="checkbox" checked={settings.page_text_enabled} onChange={e=>void save({page_text_enabled:e.target.checked})}/>可选读取公开结果正文</label> : <p role="note">当前后端未提供正文开关状态，不能推断已启用；其他设置保存不会改变它。</p>}
          <p className="sub">仅通用网页范围；正文摘录作为不可信引用发送给所选模型。最多2页／4秒且不超过剩余搜索期限，每页256KiB，只读公开HTTPS／UTF-8，不执行网页脚本。</p>
          <p className="sub">摘录仅用于本次请求，不写入查询缓存／审计／会话。来源等待仍优先；配置搜索代理时不读取正文，不降级直连。软件包／HN范围不读取目标正文。读取时间不是发布时间或实时保证。</p>
          <p role="status" aria-live="polite">{busy?"正在保存；当前显示已保存的正文开关。":""}</p>
        </fieldset>}
        {shownBackend === "free_aggregate" && <fieldset aria-label="本机检索复用" disabled={locked}>
          <legend>本机检索复用（默认关闭）</legend>
          {busy&&<p role="status" aria-live="polite">正在处理搜索操作；设置以成功回传为准，当前显示已保存值。</p>}
          {resourceFields?<>
            <label className="field-inline"><input type="checkbox" checked={settings.coalesce_queries??false} onChange={e=>void save({coalesce_queries:e.target.checked})}/>合并相同的在途免费检索</label>
            <label className="field"><span>本机检索缓存有效期（秒，0关闭）</span><input id="search-cache-ttl" type="number" min={0} max={3600} step={1} value={cacheDraft} onChange={e=>{cacheDirty.current=true;setCacheDraft(e.target.value);}}/></label>
            <button className="button" data-focus-return="search-cache-ttl" disabled={!validTtl||ttl===settings.query_cache_ttl_secs} onClick={()=>void save({query_cache_ttl_secs:ttl})}>保存检索缓存有效期</button>
            {cacheDirty.current&&!validTtl&&<p role="alert">有效期须为0到3600之间的整数，尚未保存。</p>}
          </>:<p role="note">当前后端未提供可用的检索复用设置；原有搜索设置仍可使用。</p>}
          {intervalField?<>
            <label className="field"><span>每个免费源的发送预留间隔（毫秒，0不设新间隔）</span><input id="source-min-interval" type="number" min={0} max={3600000} step={1} value={intervalDraft} onChange={e=>{intervalDirty.current=true;setIntervalDraft(e.target.value);}}/></label>
            <button className="button" data-focus-return="source-min-interval" disabled={!validInterval||interval===settings.source_min_interval_ms} onClick={()=>void save({source_min_interval_ms:interval})}>保存源发送预留间隔</button>
            {intervalDirty.current&&!validInterval&&<p role="alert">源发送预留间隔须为0到3600000之间的整数毫秒，尚未保存。</p>}
          </>:<p role="note">当前后端未提供可用的源发送预留间隔设置，不能推断为0。</p>}
          <p className="sub">这是用户设定的本机准入间隔，不是供应商额度或精确网络发送时刻保证。发送前先提交时间预留；取消不退回时间，设为0不清除已有等待。较晚的服务端 Retry-After 仍优先。</p>
          <p className="sub">仅免费聚合使用；缓存最多128条完整非空成功结果，部分／失败／取消不缓存。新设置不接收旧查询的迟到回填；单个调用取消不影响其他等待者，全部取消才撤销自有检索。内存状态不会跨重启保留。</p>
          <button type="button" className="button ghost" disabled={readLoading || busy} onClick={reread}>刷新本机检索计数</button>
          {resourceStats?<p role="status" aria-live="polite">上次本机返回 {resourcesReceivedAt?new Date(resourcesReceivedAt).toLocaleString():"时间未知"} · 非实时监测。缓存 {resourceStats.entries} 条 · 共享项 {resourceStats.inflight} · 命中 {resourceStats.cache_hits} · 未命中 {resourceStats.cache_misses} · 合并等待 {resourceStats.coalesced} · 协调启动 {resourceStats.queries_started} · 上限拒绝 {resourceStats.rejected}</p>:<p role="note">当前后端未提供可用的检索复用计数，不能当作零。</p>}
          <small>这些是本机复用计数，不证明网页实时性或外网可用。“搜索一次”不读取本机查询缓存，仍遵守源保护，可能在发起请求前拒绝。</small>
          {!stackSelected&&<section aria-label="免费源等待状态">
            <p>源保护只表示已记录的本机等待，不是厂商额度／外网可用性证明；设置更换或重启不会清除已提交期限。</p>
            {protection?<ul>{protection.sources.map(s=><li key={s.source}>{searchSourceLabel(s.source)}：{s.state==="unavailable"?"保护状态不可读取，不能据此放行新源请求":s.state==="not_blocked_locally"?"当前没有本机等待阻止（未探测源可用性）":`本机等待至 ${new Date(s.not_before_utc!).toISOString()}${s.state==="cooldown_pending"?"（写入尚未确认，不称已持久化）":"（已提交）"}${s.reason==="local_interval"?" · 用户设置的发送预留间隔":s.reason==="retry_after"?" · 服务端 Retry-After":s.reason==="service_policy"?" · 接口保护间隔（不代表共享IP额度）":" · 等待来源未提供"}`}</li>)}</ul>:<p role="note">当前后端未提供可用的源保护快照，不能推断源可用性。</p>}
          </section>}
          {stackSelected&&<StackOverflowProtection raw={settings.stack_overflow_state} readDisabled={busy||readLoading} allowRecovery={!locked&&!readLoading} onRead={reread} onRecover={()=>void recoverStack()}/>}
        </fieldset>}
        <p className="sub">
          Key 返回 401/403 时<strong>不会</strong>自动回落到免 Key 后端——那会用错误的凭据反复打别人的服务。
          搜索失败也不会阻断请求，响应头 <code>X-Route-Search: failed</code> 会如实说明。
        </p>
        {message && <div role={message.kind === "err" ? "alert" : "status"} className={`msg ${message.kind}`}>{message.text}</div>}
      </div>

      <div className="card">
        <h3>测试后端</h3>
        <div className="row">
          <input
            aria-label="检索输入"
            ref={queryInput}
            className="field-grow"
            value={probeText}
            onChange={(event) => setProbeText(event.target.value)}
            placeholder="输入一个真实的检索词"
          />
          <button
            className="button"
            disabled={locked || stackProbeBlocked || probeText.trim().length === 0}
            onClick={() => void startProbe()}
          >
            搜索一次
          </button>
          {probe && <button type="button" disabled={!probe.ready || probe.cancelPending || probe.cancelAccepted} onClick={() => void cancelProbe(probeJob.current)}>取消本次检索</button>}
        </div>
        {settings.manual_search_cancel_supported !== true && <p className="sub">当前后端的单请求取消能力尚未确认。</p>}
        {outcome && (
          <div className="local-result">
            <section aria-label="检索结果事实" className="sub breakable">
              {outcomeUnconfirmed && <p role="status">上次已完成检索结果；{probeCancelled ? "本次检索已取消，结果未更新。" : busy ? "本次检索尚未确认，等待返回或取消。" : "本次检索尚未确认，可显式重试。"}</p>}
              <p>提交的检索输入：{outcomeInput?.slice(0,512)??"未知"}{outcomeInput&&outcomeInput.length>512?"（截断展示）":""}。这是提交输入，不代表远端规范化检索词。</p>
              <p>本机检索元数据完成时间：{typeof outcome.retrieved_at_utc==="string"&&Number.isFinite(Date.parse(outcome.retrieved_at_utc))?outcome.retrieved_at_utc:"时间未知（接口未提供可用时间）"}。</p>
              <p>{outcome.reuse==="fresh"?"本机新执行":outcome.reuse==="cached"?"本机查询缓存，保留原完成时间":outcome.reuse==="shared"?"共享在途检索，保留原完成时间":outcome.reuse==="bypass"?"未使用本机复用":"复用状态未知"}；后端：{outcome.backend==="free_aggregate"?"免费聚合检索":SEARCH_BACKEND_LABEL[outcome.backend]??"未知"}。</p>
              <p>当次网页请求偏好：{searchLocaleLabel(outcome.requested_locale)}。</p>
              <p>不是网页发布时间或源站实时性证明；此时间不包含可选正文读取。草稿编辑不会改变已有结果的提交输入。</p>
            </section>
            {outcome.results.length > 0 && (outcome.failed_sources?.length ?? 0) > 0 && <p className="sub" role="status" data-testid="search-partial-status">部分来源不可用：{outcome.failed_sources!.map(searchSourceLabel).join("、")}；已保留其他来源结果。</p>}
            {outcome.page_text && <section aria-label="本次公开正文读取"><p className="sub">仅显示当次读取状态；正文不另行保存，读取时间不是发布时间。</p>
              {outcome.page_text.pages.length===0 ? <p>{outcome.page_text.state==="destination_verification_unavailable"?"搜索使用代理，无法验证正文目标，未读取且未降级直连。":outcome.page_text.state==="scope_unavailable"?"当前检索范围不读取正文。":outcome.page_text.state==="busy"?"正文读取繁忙，本次保留标题与摘要。":"本次未读取正文，保留标题与摘要。"}</p> : outcome.page_text.pages.map(p=><p key={p.result_index} className="sub">结果{p.result_index+1}：{p.state==="available"?`已读取 · ${p.read_at_utc??"读取时间未知"}`:p.state==="source_wait"?"来源要求等待，本次未读取":p.state==="deadline"?"已到搜索期限，本次未读完":"正文不可用，保留原标题与摘要"}</p>)}
            </section>}
            {!outcome.error && <MdnReferenceCard value={outcome.unstructured_reference} />}
            {outcome.results.map((item, index) => (
              <div key={item.url} className="sub">
                <span data-testid="search-source-label">{outcome.result_sources?.[index] ? searchSourceLabel(outcome.result_sources[index]) : SEARCH_BACKEND_LABEL[outcome.backend]}</span>{" · "}
                {index + 1}. {searchResultHref(item.url) ? <a href={searchResultHref(item.url)} target="_blank" rel="noopener noreferrer">{item.title}</a> : item.title} — {item.url}
                {item.snippet ? `　${item.snippet.slice(0, 120)}` : ""}
              </div>
            ))}
          </div>
        )}
      </div>
    </>
  );
}

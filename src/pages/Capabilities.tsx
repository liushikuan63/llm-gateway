import { useEffect, useRef, useState } from "react";
import { api, CandidateExplanation, CapabilitySet, PriceObservationView, TpsReference, formatPricePerMillion, ParetoView, ProviderView } from "../api";
import { errorText } from "./providerPresets";
import "./capabilities.css";
import CapabilityTransfer from "../components/CapabilityTransfer";
import PriceObservation from "../components/PriceObservation";
import CapabilityEvidence, { SourceEvidenceItem } from "../components/CapabilityEvidence";
import ModelCapabilityDeclaration, { usesChatDeclaration } from "../components/ModelCapabilityDeclaration";

/** 七个打分维度的中文名。**只是标签** —— 判定逻辑不在前端。 */
const FACTOR_LABEL: Record<string, string> = {
  health: "健康", headroom: "剩余额度", capability: "能力", latency: "延迟",
  intent_fit: "任务匹配", cost: "成本", efficiency: "效率",
};

/**
 * D2：多来源能力账本的**冲突视图**。
 *
 * 卡片要的是「某维度有 2 个以上不同取值时标提示，并把各来源的值都列出来」。
 * 只给胜出者的话，用户看到「我填的没生效」时没有任何线索 ——
 * 他既不知道还有谁说过话，也不知道谁赢了。
 *
 * 【冲突视图只读】单模型写入路径在 `ProviderEditor`。文件迁移另有独立显式入口。一个既能看冲突、
 * 又能就地改值的页面会让人分不清「我刚改的是哪个来源」，
 * 而来源正是这个功能唯一的解释对象。
 */

/** 维度与来源的中文名。**与后端 `capability::Dimension::label()` 同源**。 */
const DIMENSION_LABEL: Record<string, string> = {
  coding: "代码", reasoning: "推理", knowledge: "知识", math: "数学",
};
const SOURCE_LABEL: Record<string, string> = {
  measured: "实测", manual: "手工", community: "社区", catalog: "目录",
};

/**
 * 信任度从高到低。**与后端 `CapabilitySource` 的 `Ord` 一致**（D1 已钉住）。
 * 界面靠它标出胜出者 —— 标错一个来源比不标更糟。
 */
const TRUST_ORDER = ["measured", "manual", "community", "catalog"];

interface DimensionRow {
  dimension: string;
  items: SourceEvidenceItem[];
  conflict: boolean;
}

/** 把一个模型的账本摊成「维度 → 各来源的值」。 */
function rowsOf(set: CapabilitySet | undefined): DimensionRow[] {
  const values = set?.values ?? {};
  return Object.entries(values)
    .map(([dimension, bySource]) => {
      const items = Object.entries(bySource ?? {})
        // 老形态是裸数字，新形态是 `{ value, source }`。两种都要认 ——
        // 导出文件可能来自更老的版本。
        .map(([source, raw]) => ({
          source,
          value: typeof raw === "number" ? raw : raw?.value,
          observed_at: typeof raw === "number" ? undefined : raw?.observed_at,
          sample_count: typeof raw === "number" ? undefined : raw?.sample_count,
        }))
        .filter((item): item is { source: string; value: number; observed_at: string | null | undefined; sample_count: number | null | undefined } =>
          typeof item.value === "number" && Number.isFinite(item.value)
          && item.value >= 0 && item.value <= 1 && TRUST_ORDER.includes(item.source))
        .sort((a, b) => TRUST_ORDER.indexOf(a.source) - TRUST_ORDER.indexOf(b.source));
      // 冲突 = **不止一个来源，且它们说的不全一样**。
      // 两个来源给出同一个值是互相印证，不是冲突。
      const conflict = items.length > 1 && new Set(items.map((i) => i.value)).size > 1;
      return { dimension, items, conflict };
    })
    .filter((row) => row.items.length > 0)
    // 有冲突的排前面：这一页存在的理由就是它们。
    .sort((a, b) => Number(b.conflict) - Number(a.conflict));
}

/** Labels the read snapshot; UI does not run a wall-clock expiry or infer freshness. */
function TpsFreshness({ reference }: { reference?: TpsReference | null }) {
  if (!reference || reference.schema_version !== 1) return <small className="muted" data-testid="tps-reference">时效信息未知</small>;
  const labels: Record<TpsReference["state"], string> = {
    no_sample: "暂无有效样本", current: "参考当前", expired: "已过期：参考未知",
    source_mismatch: "来源不符：参考未知", clock_unknown: "时钟未知：参考未知",
  };
  const count = reference.state !== "source_mismatch" && reference.cohort_sample_count != null
    ? `；代次累计${reference.cohort_sample_count}` : "";
  const age = reference.age_seconds == null ? "" : `；最近有效样本距本次读取${reference.age_seconds}秒`;
  return <small className="muted" data-testid="tps-reference">{labels[reference.state] ?? "时效信息未知"}{age}{count}</small>;
}

export default function CapabilitiesPage() {
  const [priceObservations, setPriceObservations] = useState<PriceObservationView[] | null>(null);
  const [ledger, setLedger] = useState<Record<string, CapabilitySet> | null>(null);
  const [pareto, setPareto] = useState<ParetoView | null>(null);
  const [explain, setExplain] = useState<CandidateExplanation[] | null>(null);
  const [explainModel, setExplainModel] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [providers, setProviders] = useState<ProviderView[] | null>(null);
  const [query, setQuery] = useState("");
  const [factsPage, setFactsPage] = useState(1);
  // Explain is a readonly snapshot for one normalized query, not the editable draft.
  const canonical = (value: string) => value.trim() || "auto";
  const modelDraft = useRef(explainModel);
  const loadVersion = useRef(0);
  const mounted = useRef(true);
  const [explainQuery, setExplainQuery] = useState<string | null>(null);
  const [pendingQuery, setPendingQuery] = useState<string | null>(null);


  const load = async (model = explainModel) => {
    const requestId = ++loadVersion.current;
    const query = canonical(model);
    const isCurrent = () => mounted.current && requestId === loadVersion.current;
    setPendingQuery(query);
    setBusy(true);
    setError(null);
    try {
      const [raw, view, ranking, facts, observations] = await Promise.allSettled([
        api.exportCapabilities().then(value => JSON.parse(value) as Record<string, CapabilitySet>),
        api.capabilityPareto(),
        api.explainRouting(model.trim() || undefined),
        api.listProviders(),
        api.priceObservations().then(value => { if (!Array.isArray(value)) throw new Error("价格来源记录形状未确认"); return value; }),
      ]);
      if (!isCurrent()) return;
      if (raw.status === "fulfilled") setLedger(raw.value);
      if (view.status === "fulfilled") setPareto(view.value);
      const matchesDraft = canonical(modelDraft.current) === query;
      if (ranking.status === "fulfilled" && matchesDraft) {
        setExplain(ranking.value);
        setExplainQuery(query);
      }
      if (facts.status === "fulfilled") setProviders(facts.value);
      if (observations.status === "fulfilled") setPriceObservations(observations.value);
      const failures = [raw, view, ranking, facts, observations].flatMap((result, index) =>
        result.status === "rejected" && (index !== 2 || matchesDraft) ? [`${["质量来源", "比较", `路由解释（${query}）`, "模型事实", "价格来源记录"][index]}：${errorText(result.reason)}`] : []);
      if (failures.length) setError(`${failures.join("；")}。其他结果仍可查看；失败区域如有旧内容，保留上次成功读取的结果。`);
    } catch (cause) {
      if (isCurrent()) setError(errorText(cause));
    } finally {
      if (isCurrent()) { setBusy(false); setPendingQuery(null); }
    }
  };

  useEffect(() => {
    mounted.current = true;
    void load();
    return () => { mounted.current = false; ++loadVersion.current; };
  }, []);

  const entries = Object.entries(ledger ?? {}).map(([key, set]) => ({ key, rows: rowsOf(set) }));
  const withData = entries.filter((entry) => entry.rows.length > 0);
  const conflicting = withData.filter((entry) => entry.rows.some((row) => row.conflict));
  const agreeing = withData.length - conflicting.length;
  const facts = (providers ?? []).flatMap(provider => provider.models.map(model => ({
    provider, model, rows: rowsOf(ledger?.[`${provider.id}/${model.alias}`]),
  }))).filter(({ provider, model }) => `${provider.name} ${model.alias} ${model.upstream}`.toLowerCase().includes(query.trim().toLowerCase()));
  const pageCount = Math.max(1, Math.ceil(facts.length / 20));
  const currentPage = Math.min(factsPage, pageCount);
  const visibleFacts = facts.slice((currentPage - 1) * 20, currentPage * 20);

  return (
    <div className="page capabilities-page">
      <header className="page-heading">
        <div>
          <h2>能力与取舍</h2>
          <p>先看价格、声明能力和数据覆盖，再比较路由依据与质量来源。</p>
        </div>
        <button type="button" disabled={busy} onClick={() => void load()}>重新读取</button>
      </header>

      {error && <div className="capability-error" role="alert">部分数据读取失败：{error}</div>}

      <CapabilityTransfer disabled={busy} onImported={() => void load()} />

      {providers !== null && <section className="capability-facts" data-testid="capability-facts" aria-labelledby="capability-facts-title">
        <div className="facts-heading"><div><h3 id="capability-facts-title">模型事实与覆盖</h3><p className="capability-hint">价格来自当前配置；声明支持不等于账号已实测。覆盖只计有效数值和已知来源，缺失／无效不补0。非聊天端点不套用聊天能力／质量口径；类型专用向量尚未定义，未改变评分。</p></div>
          <label>搜索模型事实<input value={query} onChange={event => { setQuery(event.target.value); setFactsPage(1); }} placeholder="模型、上游名或供应商" /></label>
        </div>
        <div className="facts-table-scroll" tabIndex={0} role="region" aria-label="模型事实表格，可横向滚动"><table className="facts-table">
          <thead><tr><th>模型 / 供应商</th><th>价格与来源</th><th>上下文 / 用途</th><th>声明 / 适用范围</th><th>质量覆盖</th></tr></thead>
          <tbody>{visibleFacts.map(({ provider, model, rows }) => {
            const chatDeclaration = usesChatDeclaration(model);
            const contextKnown = Number.isSafeInteger(model.context_window) && model.context_window > 0;
            const covered = rows.filter(row => DIMENSION_LABEL[row.dimension]).length;
            const configuredQuality = Object.keys(DIMENSION_LABEL).filter(dimension => {
              const value = model.capabilities?.[dimension as "coding" | "reasoning" | "knowledge" | "math"];
              return typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 1;
            });
            const price = model.price;
            return <tr key={`${provider.id}/${model.alias}`}>
              <td><strong>{model.alias}</strong><small>{provider.name}{!provider.enabled || !model.enabled ? " · 已停用" : ""}</small>{model.alias !== model.upstream && <small>上游：{model.upstream}</small>}</td>
              <td>{price ? <><span>{formatPricePerMillion(price)}</span><small>{price.currency.toUpperCase()} · {price.source === "manual" ? "手工配置" : "目录参考价"}</small><small>缓存读 / 写：{price.cache_read ?? price.prompt} / {price.cache_creation ?? price.prompt} 每 100 万</small>{(price.tiers.length > 0 || price.rules.length > 0) && <small>含分档 / 时段条件，实际费用按请求计算</small>}<PriceObservation provider={provider} model={model} observations={priceObservations} />{!chatDeclaration && <small>token参考配置，是否适用于此类型计价尚未核验</small>}</> : <span className="missing">未配置价格；费用未知</span>}</td>
              <td>{contextKnown ? `${chatDeclaration ? "" : "旧窗口配置："}${model.context_window.toLocaleString()} tokens` : "上下文未声明，不代表0"}<small>{{ chat: "聊天", embedding: "Embedding", image: "图像生成", speech: "语音合成" }[model.model_type] ?? "类型未确认"}</small><small>{chatDeclaration ? "配置值，未自动探测" : "本类型端点未按聊天窗口验证"}</small></td>
              <td><ModelCapabilityDeclaration model={model} declaration={provider.capability_declarations?.[model.alias]} /></td>
              <td>{!chatDeclaration && <strong style={{ display: "block", marginBottom: 4 }}>专用质量口径未定义</strong>}{ledger === null ? <span className="missing">质量来源尚未读取</span> : <><span>{chatDeclaration ? `${covered} / 4 质量维度有来源` : `原通用资料：${covered} / 4 维度有有效分值`}</span><small>{covered > 0 ? rows.filter(row => DIMENSION_LABEL[row.dimension]).map(row => DIMENSION_LABEL[row.dimension]).join("、") : configuredQuality.length > 0 ? `${configuredQuality.length} / 4 维度有配置分值，无多来源账本` : chatDeclaration ? `供应商默认回退 ${provider.intelligence}/100，非质量实测` : "不以缺少聊天质量分推断本类型能力低"}</small></>}{model.capabilities?.updated_at && <small>{chatDeclaration ? "质量记录时间" : "通用质量记录时间"}：{model.capabilities.updated_at}</small>}{ledger !== null && <CapabilityEvidence rows={rows} />}<small>延迟 / 任务匹配 / 效率：持续补充使用样本</small></td>
            </tr>;
          })}</tbody>
        </table></div>
        {facts.length === 0 && <p className="capability-hint"><span>{query ? "没有符合筛选条件的模型。" : "尚未配置模型，请先在供应商页接入。"}</span>{query && <button type="button" onClick={() => { setQuery(""); setFactsPage(1); }}>清空筛选</button>}</p>}
        <div className="facts-pagination"><span>{facts.length} 个模型</span><span>第 {currentPage} / {pageCount} 页</span>
          <button type="button" disabled={currentPage <= 1} onClick={() => setFactsPage(currentPage - 1)}>上一页</button><button type="button" disabled={currentPage >= pageCount} onClick={() => setFactsPage(currentPage + 1)}>下一页</button>
        </div>
      </section>}

      {/* D5：打分分解。卡片原话——「现有界面只显示排序结果，用户无法判断
          路由器在想什么，这正是『改了权重排序变了但没人知道为什么』的根源」。 */}
      {explain && (
        <section className="explain-section">
          <h3>为什么选了它</h3>
          <p className="capability-hint">
            {explain.some(item => item.free_weight) ? "免费模式先核验费用、健康和额度，再展示同源相对权重。实际轮转和合格会话粘性会影响下一次首选，预演不推进轮次或占执行许可。" : "每个候选展示该次读取的原始值、权重和贡献项，复用排序函数。这里是模型名的只读解释；实际分派还会应用当时的请求约束、健康和会话策略，不保证后续请求顺序。"}
          </p>
          <div className="explain-input">
            <input
              aria-label="解释哪个模型"
              placeholder="模型名（留空按 auto 解释）"
              value={explainModel}
              onChange={(e) => { modelDraft.current = e.target.value; setExplainModel(e.target.value); }}
            />
            <button type="button" disabled={busy} onClick={() => void load()}>重新解释</button>
          </div>
          <p className="capability-hint" role="status" aria-live="polite" data-testid="capability-explain-state">
            显示结果：{explainQuery ?? "未读取"}。
            {busy && pendingQuery && `正在读取：${pendingQuery}。`}
            {canonical(explainModel) !== explainQuery && `当前输入 ${canonical(explainModel)} 尚未重新解释。`}
            这是该次读取的只读解释，不是历史请求证据或后续请求的分派保证。
          </p>
          {explain.length === 0 && (
            <p className="capability-hint">没有解析出候选 —— 检查模型名是否存在于已启用的供应商里。</p>
          )}
          {explain.map((item, index) => (
            <details className="explain-candidate" key={`${item.provider_id}/${item.model}`} open={index === 0}>
              <summary>
                <span>{item.provider_name} · {item.model}</span>
                <span className="explain-score">{item.score.toFixed(4)}</span>
                {!item.eligible && (
                  <span className="tag ineligible-tag">不参与：{item.ineligible_reason}</span>
                )}
              </summary>
              {item.free_weight ? <p className="capability-hint">免费相对权重 {item.free_weight.value.toFixed(4)}；有效用量/耗时样本 {item.free_weight.usable_completion_samples}/{item.free_weight.minimum_samples}，{item.free_weight.metrics_used ? "采用被动 EWMA" : "保持中性指标起点"}；本地限流余量 {(item.free_weight.local_quota_headroom * 100).toFixed(0)}%。费用观察截至 {item.free_eligibility?.expires_at == null ? "未知" : new Date(item.free_eligibility.expires_at).toLocaleString()}。</p> : <table className="explain-table">
                <thead>
                  <tr><th>维度</th><th>原始值</th><th>权重</th><th>贡献</th><th>说明</th></tr>
                </thead>
                <tbody>
                  {item.breakdown.factors.map((f) => (
                    <tr key={f.name}>
                      <td>{FACTOR_LABEL[f.name] ?? f.name}</td>
                      <td>{f.raw === null ? <span className="missing">{f.weight === 0 ? "未参与评分" : f.name === "cost" ? "无可比价格" : "缺少本项样本"}</span> : f.raw.toFixed(3)}</td>
                      <td>{f.weight.toFixed(2)}</td>
                      <td>{f.contribution.toFixed(4)}</td>
                      <td className="muted">{f.reason}</td>
                    </tr>
                  ))}
                  <tr className="explain-total">
                    <td colSpan={3}>
                      最终分（精确命中加成 ×{item.breakdown.exact_match_bonus}）
                    </td>
                    <td>{item.breakdown.total.toFixed(4)}</td>
                    <td />
                  </tr>
                </tbody>
              </table>}
              <p className="capability-hint"><TpsFreshness reference={item.tps_reference} /></p>
              <p className="capability-hint">
                配置的上游模型名：<code>{item.upstream}</code>
                {item.upstream !== item.model && "（与别名不同；不表示已实际调用或完成账号端别名解析）"}
              </p>
            </details>
          ))}
        </section>
      )}

      {/* D5：三维前沿。缺数据的维度显示「数据不足」而不是 0 或空白 ——
          0 会被读成「这一维得了 0 分」，而事实是「我们没有这一维的数据」。 */}
      {pareto && (
        <section className="pareto-section">
          <h3>质量 × 速度 × 价格</h3>
          <p className="capability-hint">
            三个维度互不相让时，没有哪个候选「最好」—— 只有「不被任何人全面胜过」。
            下面把每个候选的三个原始值都摊开：<strong>前沿</strong>上的候选没人能全面胜过它，
            被支配的会写明是被谁支配。
          </p>
          {pareto.currency_note && <p className="pareto-note" role="note">{pareto.currency_note}</p>}
          {pareto.points.length === 0 ? (
            <p className="capability-hint">还没有启用中的模型可供比较。</p>
          ) : (
            <>
              <table className="pareto-table">
                <thead>
                  <tr><th>候选</th><th>质量</th><th>旧调用窗速率</th><th>价格</th><th>结论</th></tr>
                </thead>
                <tbody>
                  {pareto.points.map((pt) => {
                    const onFront = pareto.front.includes(pt.id);
                    const dominators = pareto.dominated_by[pt.id] ?? [];
                    const labelOf = (id: string) => pareto.points.find((p) => p.id === id)?.label ?? id;
                    return (
                      <tr key={pt.id} className={onFront ? "on-front" : "dominated"}>
                        <td>{pt.label}</td>
                        <td>{pt.quality === null ? <span className="missing">数据不足</span> : pt.quality.toFixed(2)}</td>
                        <td>{pt.speed === null ? <span className="missing">数据不足</span> : `${pt.speed.toFixed(1)} tok/s`}<br /><TpsFreshness reference={pt.speed_reference} /></td>
                        <td>{pt.unit_price === null ? <span className="missing">无价格</span> : `${pt.unit_price} ${pt.currency ?? ""}`}</td>
                        <td>
                          {onFront
                            ? <span className="tag front-tag">前沿</span>
                            : <span className="tag dominated-tag">被 {dominators.map(labelOf).join("、")} 支配</span>}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
              <p className="capability-hint">
                此处仍是既有调用窗输出速率，可能含等待和本地开销，不等于供应商纯生成速率；新原生报告没有接入该前沿或评分。<strong>样本不足的显示「数据不足」而不是 0</strong> ——
                0 在约定里表示「没有样本」，把它当成最慢会让刚接入的模型立刻被判成被支配。
                24小时无有效TPS样本会空闲失效，不是滚动24小时统计窗口；下一有效样本重新累计，仍须满足配置的最小样本数。时效为本次只读快照，重新读取更新；旧后端缺元数据时显示未知。
                价格取的是参考单价（分档价与峰谷价都会生效）。
              </p>
            </>
          )}
        </section>
      )}

      {ledger !== null && withData.length === 0 && !error && (
        <div className="capability-empty">
          <strong>尚无有效质量来源分值</strong>
          <p>
            已有来源条目也可能缺值或无效，未将它们补为0。价格、上下文和声明能力仍在上方展示。质量维度（代码 / 推理 / 知识 / 数学）需要有来源的评测或手工标定，
            可在「供应商」里导入能力集；成功返回本身不代表任务质量。
          </p>
          <p>
            没有质量分的模型使用供应商默认能力分；已有配置分值仍可参与路由。
            来源账本用于说明分值由谁提供，不替代价格或声明能力。
          </p>
        </div>
      )}

      {ledger !== null && withData.length > 0 && (
        <div className="capability-summary">
          共 {withData.length} 个模型有能力数据：<strong>{conflicting.length} 个存在冲突</strong>
          {agreeing > 0 && `，${agreeing} 个各来源一致`}
        </div>
      )}

      {conflicting.map((entry) => (
        <article className="card capability-card" key={entry.key}>
          <header>
            <strong>{entry.key}</strong>
            <span className="capability-badge conflict">有冲突</span>
          </header>
          {entry.rows.filter((row) => row.conflict).map((row) => (
            <div className="capability-row" key={row.dimension}>
              <div className="capability-dimension">
                {DIMENSION_LABEL[row.dimension] ?? row.dimension}
              </div>
              <ul className="capability-values">
                {row.items.map((item, index) => (
                  <li key={item.source} className={index === 0 ? "winner" : undefined}>
                    <span className="capability-source">{SOURCE_LABEL[item.source] ?? item.source}</span>
                    <span className="capability-value">{item.value.toFixed(2)}</span>
                    {index === 0 && <span className="capability-winner-tag">生效</span>}
                  </li>
                ))}
              </ul>
            </div>
          ))}
          <p className="capability-hint">
            生效的是信任度最高的那个来源；其余的值不会被丢弃 ——
            下次刷新目录或别人导入一份能力集时，它们仍然在这里可比。
          </p>
        </article>
      ))}

      {agreeing > 0 && (
        <details className="capability-agreeing">
          <summary>{agreeing} 个模型各来源一致（无需处理）</summary>
          <ul>
            {withData
              .filter((entry) => !entry.rows.some((row) => row.conflict))
              .map((entry) => (
                <li key={entry.key}>
                  {entry.key}：
                  {entry.rows
                    .map((row) => `${DIMENSION_LABEL[row.dimension] ?? row.dimension} ${row.items[0].value.toFixed(2)}`)
                    .join("、")}
                </li>
              ))}
          </ul>
        </details>
      )}
    </div>
  );
}

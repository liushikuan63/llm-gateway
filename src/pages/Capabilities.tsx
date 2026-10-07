import { useEffect, useState } from "react";
import { api, CapabilitySet, ParetoView } from "../api";
import { errorText } from "./providerPresets";
import "./capabilities.css";

/**
 * D2：多来源能力账本的**冲突视图**。
 *
 * 卡片要的是「某维度有 2 个以上不同取值时标提示，并把各来源的值都列出来」。
 * 只给胜出者的话，用户看到「我填的没生效」时没有任何线索 ——
 * 他既不知道还有谁说过话，也不知道谁赢了。
 *
 * 【为什么是只读页】写入路径在 `ProviderEditor`。一个既能看冲突、
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
  items: Array<{ source: string; value: number }>;
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
          value: typeof raw === "number" ? raw : Number(raw?.value ?? 0),
        }))
        .filter((item) => Number.isFinite(item.value))
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

export default function CapabilitiesPage() {
  const [ledger, setLedger] = useState<Record<string, CapabilitySet> | null>(null);
  const [pareto, setPareto] = useState<ParetoView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = async () => {
    setBusy(true);
    setError(null);
    try {
      const [raw, view] = await Promise.all([api.exportCapabilities(), api.capabilityPareto()]);
      setLedger(JSON.parse(raw) as Record<string, CapabilitySet>);
      setPareto(view);
    } catch (cause) {
      // 读不到就明确报出来，并且**不留半张表** —— 空表看起来像「没有冲突」，
      // 而那与「读失败」是完全相反的结论。
      setError(errorText(cause));
      setLedger({});
      setPareto(null);
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const entries = Object.entries(ledger ?? {}).map(([key, set]) => ({ key, rows: rowsOf(set) }));
  const withData = entries.filter((entry) => entry.rows.length > 0);
  const conflicting = withData.filter((entry) => entry.rows.some((row) => row.conflict));
  const agreeing = withData.length - conflicting.length;

  return (
    <div className="page capabilities-page">
      <header className="page-heading">
        <div>
          <h2>能力与取舍</h2>
          <p>同一个模型的同一项能力，不同来源可能给出不同数值。这里把冲突摊开，说明谁赢了、为什么。</p>
        </div>
        <button type="button" disabled={busy} onClick={() => void load()}>重新读取</button>
      </header>

      {error && <div className="capability-error" role="alert">读取能力账本失败：{error}</div>}

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
                  <tr><th>候选</th><th>质量</th><th>速度</th><th>价格</th><th>结论</th></tr>
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
                        <td>{pt.speed === null ? <span className="missing">数据不足</span> : `${pt.speed.toFixed(1)} tok/s`}</td>
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
                速度按实测 tok/s，<strong>样本不足的显示「数据不足」而不是 0</strong> ——
                0 在约定里表示「没有样本」，把它当成最慢会让刚接入的模型立刻被判成被支配。
                价格取的是参考单价（分档价与峰谷价都会生效）。
              </p>
            </>
          )}
        </section>
      )}

      {ledger !== null && withData.length === 0 && !error && (
        <div className="capability-empty">
          <strong>数据不足</strong>
          <p>
            本机还没有任何模型的能力数据。目录来源目前只给出模态布尔（能不能看图、能不能调工具），
            质量维度（代码 / 推理 / 知识 / 数学）需要手工填写或在「供应商」里导入一份能力集。
          </p>
          <p>
            在补上数据之前，路由不会用这些维度比较模型 —— 它宁可回落供应商级的能力分，
            也不会拿一个猜出来的数字排序。
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

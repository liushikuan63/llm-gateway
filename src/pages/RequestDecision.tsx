import type { RouteDecision } from "../api";

/** The saved plan is an observation, not a reconstructed score or delivery receipt. */
export default function RequestDecision({ value }: { value?: RouteDecision | null }) {
  if (!value || value.state === "unobserved") return <section aria-label="当次候选计划"><h4>当次候选计划</h4><p>未记录；原因未知，不代表没有候选或未派发。</p></section>;
  if (value.state !== "recorded") return <section aria-label="当次候选计划"><h4>当次候选计划</h4><p>记录不可用（格式、版本或大小不受支持）；不重新计算历史。</p></section>;
  const s = value.snapshot;
  const exact = s.kind === "exact_selection";
  return <section aria-label="当次候选计划" data-testid="request-decision">
    <h4>当次候选计划</h4>
    <p>{exact ? "精确选择已记录（未进行候选排序）；评分不适用。" : <>{s.kind === "not_reached" ? "未形成分派计划（准备阶段）" : "当次计划已记录"} · {s.score_state === "recorded" ? "已保存当时的比较评分观察（未必覆盖全部候选）" : "评分分解未记录"}。</>}</p>
    <p>捕获时间：{new Date(s.captured_at_unix_ms).toLocaleString()} · 阶段 {s.phase} · 配置策略 {s.configured_strategy}{exact && "（本入口不应用此排序）"}</p>
    {exact ? <p>本入口按精确别名选择目标，不进行候选排序、活动提升、粘性或公共免费选择器选择；既有准入与终态证据独立。</p> : <>
    <p>个人测试 {s.personal_test ? "是（免费资格／费用未核验）" : "否"}；活动供应商提升 {s.active_promoted ? "改变顺序" : "未改变顺序"}；免费选择器 {s.free_selector ? "经过" : "未经过"}；级联费用排序处理 {s.cascade_order_applied ? "是" : "否"}。</p>
    <p>当时粘性目标：{s.sticky ? `${s.sticky[0]} / ${s.sticky[1]}` : s.sticky_unavailable ? "标识不可安全表示" : "无"}。该链不包含此前被过滤的候选或原因。</p>
    </>}
    {s.score_capture === "first_sort_comparison" && <p>首次排序比较使用的事实；实际评分策略 {s.effective_strategy ?? "未知"}，不是最终顺序或质量评价。后续健康状态、优先级、粘性、活动或级联可能影响最终顺序；不重新计算历史。</p>}
    <p>{exact ? "精确目标数" : "候选总数"} {s.total_candidates}；已保存 {s.candidates.length}；{s.truncated ? `已截断，省略 ${s.omitted_candidates} 项` : "未截断"}。</p>
    {s.candidates.length > 0 && <ol>{s.candidates.map(c => <li key={c.ordinal} value={c.ordinal + 1}>
      {c.target ? <>{c.target.provider_id} / {c.target.model_alias}；配置上游 {c.target.configured_upstream}；显式匹配 {c.target.exact_match ? "是" : "否"}</> : "该序位的目标标识不可安全表示（并非空候选）"}
      {s.score_capture === "first_sort_comparison" && (c.score_state === "recorded" && c.score ? <div>
        <p>当时总分 {c.score.total}；显式匹配乘数 {c.score.exact_match_bonus}。</p>
        <table aria-label={`当时评分分项 ${c.ordinal + 1}`}><thead><tr><th>分项</th><th>原始值</th><th>权重</th><th>乘积因子</th><th>状态</th></tr></thead>
          <tbody>{c.score.factors.map(f => <tr key={f.name}><td>{f.name}</td><td>{f.raw == null ? "未知" : f.raw}</td><td>{f.weight}</td><td>{f.contribution}</td><td>{f.reason === "missing" ? "缺测，因子1不是测得满分" : f.reason === "disabled" ? "权重为0，未参与" : "按当时权重计入"}</td></tr>)}</tbody>
        </table>
      </div> : <p>{c.score_state === "omitted_byte_budget" ? "当时评分分项因16 KiB字节预算省略；候选身份和原序号保留。" : c.score_state === "unavailable" ? "当时评分不可用（标识不唯一或数据无法安全表示）。" : "当时评分未记录（没有可用的比较观察），不是0分。"}</p>)}
    </li>)}</ol>}
    {(s.auxiliary_calls?.length ?? 0) > 0 && <section aria-label="内部调用观察">
      <h5>内部调用观察</h5><p>仅这些已记录的逻辑调用；另省略 {s.auxiliary_omitted ?? 0} 条。调用开始不是物理 POST 证明，也不代表全部网络调用。</p>
      <ol>{s.auxiliary_calls!.map(call => <li key={call.ordinal} value={call.ordinal + 1}>
        {call.purpose === "classify" ? "分类批调用" : "提示词改写"} · {call.target_unavailable ? "目标标识不可安全表示" : `${call.provider_id ?? "未绑定供应商"} / ${call.model ?? "未记录模型"}`}；
        {call.terminal === "returned" ? "已返回" : call.terminal === "interrupted_unknown" ? "中断，交付未知" : "调用开始，尚未返回"}；HTTP {call.http_status ?? "未知"}；本地调用耗时 {call.elapsed_ms == null ? "未知" : `${call.elapsed_ms} ms`}；
        主用量{call.primary_usage ? `已知 ${call.primary_usage.prompt_tokens} / ${call.primary_usage.completion_tokens} / ${call.primary_usage.total_tokens}` : "未知"}。
        {call.result_ok != null && `决策接口结果 ${call.result_ok ? "可用" : "错误"}（不是分类采纳证明）。`}
        {call.applied != null && `改写${call.applied ? "已采用" : "未采用"}。`}
        物理 POST 证明未记录。
      </li>)}</ol>
      <small>主计划未形成时仍可能已有内部调用。这里不合并主尝试数、费用或预算；HTTP 成功和可解析计数均不是权威耗费、免费资格或权益结算证明。</small>
    </section>}
    <small>配置上游不等于账号运行时最终物理模型。实际尝试及结果以同一审计行的尝试链／实际选路为准；计划不证明已派发、收费、免费资格或可信结算。CSV 暂不包含此计划，JSONL 保留版本化记录。</small>
  </section>;
}

import { useEffect, useRef, useState } from "react";
import { api, ModelPrice, ModelRef, PriceObservationView, ProviderView } from "../api";
import PriceObservation from "./PriceObservation";

function amount(value: unknown): string | null {
  // Keep tiny nonzero reference values nonzero; this view does not round or
  // convert configured per-million prices into a quote or a free entitlement.
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? String(value) : null;
}

function BasePrice({ price }: { price: ModelPrice | null }) {
  const prompt = amount(price?.prompt), completion = amount(price?.completion);
  if (!price || prompt === null || completion === null || !["usd", "cny"].includes(price.currency)) {
    return <p>价格未知／价格不可用：没有可用的配置参考，不能按0计算。</p>;
  }
  const currency = price.currency.toUpperCase();
  const cache = (value: unknown) => value === undefined || value === null
    ? "未单列（按现有输入价参考规则处理）" : amount(value) === null ? "未知，不能按0计算" : `${amount(value)} ${currency}`;
  const tiers = Array.isArray(price.tiers) ? price.tiers.length : null;
  const rules = Array.isArray(price.rules) ? price.rules.length : null;
  return <div data-testid="chat-configured-price">
    <p>基础输入：{prompt} {currency}；基础输出：{completion} {currency}（每100万 token，非本次总费用）。</p>
    <p>缓存读取：{cache(price.cache_read)}；缓存写入：{cache(price.cache_creation)}（单价同为每100万 token）。</p>
    <p>配置来源：{price.source === "manual" ? "手工配置" : price.source === "catalog" ? "目录参考" : "来源未知"}。</p>
    <p>{tiers === null || rules === null ? "条件信息不完整，不能判断是否存在条件价。"
      : tiers || rules ? `含${tiers}个输入分档、${rules}个UTC时段规则；基础价不能当作最终费用，详细条件见供应商模型设置。`
      : "当前配置未列输入分档或时段规则；这不是供应商最终费用证明。"}</p>
  </div>;
}

type SelectedMapping = { provider: ProviderView; mapping: ModelRef };

function LocalPriceRecord({ selected, stale }: { selected: SelectedMapping | undefined; stale: boolean }) {
  // UI ownership only, not a credential/entitlement proof. Keep this child
  // mounted for auto/removed targets so their outstanding local read retains
  // the single-flight lock. Epoch also prevents A->B->A from reviving a result.
  const owner = selected ? JSON.stringify([selected.provider.id, selected.mapping.alias,
    selected.provider.dialect, selected.provider.base_url, selected.provider.runtime_id,
    selected.mapping.upstream, selected.mapping.model_type, selected.mapping.upstream_path,
    selected.mapping.price, stale]) : null;
  const identity = useRef<{ owner: string | null; epoch: number }>({ owner, epoch: 0 });
  const button = useRef<HTMLButtonElement>(null);
  const returnFocus = useRef<number | null>(null);
  if (identity.current.owner !== owner) {
    identity.current = { owner, epoch: identity.current.epoch + 1 };
    returnFocus.current = null;
  }
  const epoch = identity.current.epoch;
  const mounted = useRef(true);
  const inFlight = useRef(false);
  const readingOwner = useRef<number | null>(null);
  const [reading, setReading] = useState(false);
  const [snapshot, setSnapshot] = useState<{ epoch: number; records: PriceObservationView[] } | null>(null);
  const [failure, setFailure] = useState<{ epoch: number; message: string } | null>(null);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; identity.current.epoch++; }; }, []);
  useEffect(() => {
    if (!reading && returnFocus.current === epoch && !stale
      && (document.activeElement === document.body || document.activeElement === button.current)) {
      button.current?.focus();
      returnFocus.current = null;
    }
  }, [reading, epoch, stale]);

  const records = snapshot?.epoch === epoch ? snapshot.records : null;
  const message = failure?.epoch === epoch ? failure.message : null;
  const read = async () => {
    if (!selected?.mapping.price || stale || inFlight.current) return;
    const ticket = identity.current.epoch;
    inFlight.current = true; readingOwner.current = ticket;
    returnFocus.current = document.activeElement === button.current ? ticket : null;
    setReading(true); setFailure(null);
    try {
      const value = await api.priceObservations();
      if (!Array.isArray(value)) throw new Error("本机价格来源记录结构无效，请重试");
      if (mounted.current && identity.current.epoch === ticket) setSnapshot({ epoch: ticket, records: value });
    } catch (error) {
      if (mounted.current && identity.current.epoch === ticket) setFailure({ epoch: ticket,
        message: typeof error === "string" ? error : error instanceof Error ? error.message : "本机价格来源记录不可读取，请重试" });
    } finally {
      inFlight.current = false; readingOwner.current = null;
      if (mounted.current) setReading(false);
    }
  };
  if (!selected) return null;
  return <div>
    <button ref={button} type="button" aria-label="读取本机价格来源记录" disabled={reading || stale || !selected.mapping.price} onClick={() => void read()}>
      {reading ? "读取中…" : "读取本机价格来源记录"}
    </button>
    {reading && <p role="status">{readingOwner.current === epoch ? "正在读取本机记录；不会访问账号、目录或模型。" : "上次目标的本机读取仍在完成；其结果不会用于当前目标。"}</p>}
    {message && <p role="alert">{message}。{records ? "保留上次读取的匹配记录，本次未成功确认；可重试。" : "本次来源记录未知；可重试。"}</p>}
    {reading && records && <small>以下为上次读取的匹配记录，等待本次确认。</small>}
    <PriceObservation provider={selected.provider} model={selected.mapping} observations={records} />
    <small>仅在明确点击时读取本机保存／刷新记录；记录时间不是价格生效时间，也不证明供应商收费或免费资格。</small>
  </div>;
}

export default function ChatModelReference({ automatic, selected, stale }: {
  automatic: boolean;
  selected: SelectedMapping | undefined;
  stale: boolean;
}) {
  return <section aria-label="所选模型配置参考" className="chat-model-reference">
    <h3>所选模型配置参考</h3>
    {automatic ? <p>自动路由尚未确定实际模型，不能提供单一目标的配置价格。</p>
      : !selected ? <p>所选映射已不可用，价格未知；请刷新并明确重新选择。</p>
      : <>
        <p>供应商：{selected.provider.name}；配置别名：{selected.mapping.alias}；配置上游模型：<code>{selected.mapping.upstream}</code>。</p>
        {stale && <p role="note">以下为上次读取的配置，无法确认仍是当前配置；不会据此改变发送目标。</p>}
        <BasePrice price={selected.mapping.price} />
      </>}
    <LocalPriceRecord selected={automatic ? undefined : selected} stale={stale} />
    <p className="sub">仅展示最近一次界面读取的配置，不是实际账单、模型可用性或免费资格证明。未知用量不补0，零配置价不代表免费；本区不请求账号、模型或价格源。</p>
  </section>;
}

import { ModelPrice, ModelRef, PriceObservationView, ProviderView } from "../api";

function ordered(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(ordered);
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).sort(([a],[b])=>a.localeCompare(b)).map(([k,v])=>[k,ordered(v)]));
  return value;
}
function shape(price: ModelPrice) {
  return ordered({prompt:price.prompt,completion:price.completion,currency:price.currency,source:price.source,
    cache_read:price.cache_read??null,cache_creation:price.cache_creation??null,tiers:price.tiers??[],rules:price.rules??[]});
}
export default function PriceObservation({ provider, model, observations }: {provider: ProviderView; model: ModelRef; observations: PriceObservationView[] | null}) {
  // The reads can finish from different snapshots. Reject an observation whose
  // public target or price differs; this is display provenance, never fee proof.
  const record = model.price && observations?.find(item => item?.provider_id===provider.id && item.alias===model.alias
    && item.dialect===provider.dialect && item.base_url===provider.base_url && (item.runtime_id??null)===(provider.runtime_id??null)
    && item.upstream===model.upstream && item.model_type===(model.model_type??"chat") && (item.upstream_path??null)===(model.upstream_path??null)
    && item.price && JSON.stringify(shape(item.price))===JSON.stringify(shape(model.price!))
    && ["manual_save","catalog_refresh"].includes(item.origin)
    && typeof item.recorded_at==="string" && Number.isFinite(Date.parse(item.recorded_at)));
  return <>
    {record ? <div data-testid="price-observation">
      <small>本机{record.origin==="manual_save"?"手工保存":"目录刷新"}价格记录：<time dateTime={record.recorded_at}>{record.recorded_at}</time></small>
      {record.origin==="catalog_refresh" && <small>来源站点：{record.feed_origin??"未知"}；目录模型：{record.feed_model_id??"未知"}</small>}
    </div> : <small>{observations===null?"单模型价格记录尚未读取；来源时间尚未记录":"本机价格记录时间未知；来源时间尚未记录"}</small>}
    <small>来源生效／核验时间未知；本机记录不保证供应商实际收费或免费资格。</small>
  </>;
}

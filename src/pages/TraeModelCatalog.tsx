import { useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { api, TraeLoginStatus, TraeModelCatalogView } from "../api";
import "./trae-model-catalog.css";

type Props = {
  accountId: string;
  accountEnabled: boolean;
  benefitsEnabled: boolean;
  loaded: boolean;
  busy: boolean;
  login: TraeLoginStatus | null;
};
const channels = { solo_agent_remote: "Agent", solo_work_remote: "Work", solo_design_remote: "Design" };
const failure = "无法读取官方模型目录，请先检查当前账号登录状态，再手动重试读取。";
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
const keys = (value: Record<string, unknown>, allowed: string[]) => Object.keys(value).length === allowed.length && allowed.every(key => Object.prototype.hasOwnProperty.call(value, key));
const text = (value: unknown): value is string => typeof value === "string" && !!value.trim() && value.length <= 4096;
const flag = (value: unknown) => value === null || typeof value === "boolean";

function verifiedCatalog(value: unknown, accountId: string): TraeModelCatalogView {
  if (!object(value) || !keys(value, ["account_id", "region", "origin", "fetched_at_ms", "groups", "warnings"]) ||
    value.account_id !== accountId || value.region !== "cn" || value.origin !== "https://trae-api-cn.mchost.guru" ||
    typeof value.fetched_at_ms !== "number" || !Number.isSafeInteger(value.fetched_at_ms) || value.fetched_at_ms <= 0 || value.fetched_at_ms > 8640000000000000 ||
    !Array.isArray(value.groups) || value.groups.length > 3 || !Array.isArray(value.warnings) || value.warnings.length > 100 || !value.warnings.every(text)) throw Error("Invalid catalog");
  const functions = new Set<string>();
  for (const group of value.groups) {
    if (!object(group) || !keys(group, ["function", "models"]) || typeof group.function !== "string" ||
      !Object.prototype.hasOwnProperty.call(channels, group.function) || functions.has(group.function) || !Array.isArray(group.models) || group.models.length > 10000) throw Error("Invalid catalog group");
    functions.add(group.function);
    const ids = new Set<string>();
    for (const model of group.models) {
      if (!object(model) || !keys(model, ["id", "name", "supports_vision", "supports_thinking"]) || !text(model.id) || !text(model.name) ||
        !flag(model.supports_vision) || !flag(model.supports_thinking) || ids.has(model.id)) throw Error("Invalid catalog model");
      ids.add(model.id);
    }
  }
  return value as unknown as TraeModelCatalogView;
}
const declaration = (value: boolean | null) => value === null ? "未知（未声明）" : value ? "声明支持" : "声明不支持";

export default function TraeModelCatalog({ accountId, accountEnabled, benefitsEnabled, loaded, busy, login }: Props) {
  const heading = useId();
  // A changed account, login snapshot or parent operation retires the entire read generation.
  const scope = useMemo(() => ({}), [accountId, accountEnabled, benefitsEnabled, loaded, busy, login]);
  const live = useRef<object | null>(null);
  const pending = useRef<object | null>(null);
  const [snapshot, setSnapshot] = useState<{ scope: object; loading: boolean; data: TraeModelCatalogView | null; error: boolean } | null>(null);
  useLayoutEffect(() => {
    live.current = scope; pending.current = null; setSnapshot(null);
    return () => { live.current = null; pending.current = null; };
  }, [scope]);
  const current = snapshot?.scope === scope ? snapshot : null;
  const ready = loaded && !!accountId && accountEnabled && benefitsEnabled && !busy && login?.account_id === accountId && login.status === "authenticated";
  const read = async () => {
    if (!ready || live.current !== scope || pending.current === scope) return;
    pending.current = scope;
    setSnapshot({ scope, loading: true, data: null, error: false });
    try {
      const data = verifiedCatalog(await api.traeModelCatalog(accountId), accountId);
      if (live.current === scope) setSnapshot({ scope, loading: false, data, error: false });
    } catch {
      if (live.current === scope) setSnapshot({ scope, loading: false, data: null, error: true });
    } finally { if (pending.current === scope) pending.current = null; }
  };
  const data = current?.data;
  const rows = data?.groups.reduce((total, group) => total + group.models.length, 0) ?? 0;
  return <section className="trae-model-catalog" data-testid="trae-model-catalog" aria-labelledby={heading} aria-busy={!!current?.loading}>
    <h4 id={heading}>Trae CN 官方模型目录</h4>
    <p>手动读取官方 Remote 目录，仅发送一次只读目录请求，不发起模型调用。页面打开、切换账号和刷新状态均不会自动读取。</p>
    <p>目录可见性及视觉、思考字段仅为官方声明，不代表当前账号拥有模型调用权益，也不代表网关已实际支持调用。</p>
    <button type="button" disabled={!ready} aria-disabled={!ready || !!current?.loading} onClick={() => void read()} aria-busy={!!current?.loading}>读取官方模型目录</button>
    {!ready && <p className="workbuddy-login__footnote">请先启用权益与当前 Trae 账号，确认已保存登录，并等待当前操作完成。</p>}
    {current?.loading && <p role="status">正在只读获取官方模型目录…</p>}
    {current?.error && <p role="alert">{failure}</p>}
    {data && <>
      <p role="status">已读取 {data.groups.length} 个通道，共 {rows} 条模型记录。采样时间：{new Date(data.fetched_at_ms).toLocaleString("zh-CN", { hour12: false })}。</p>
      {rows === 0 && <p>官方目录本次未返回模型记录。</p>}
      {data.groups.map(group => <div key={group.function} className="trae-model-catalog__group" data-channel={group.function}>
        <h5>{channels[group.function]}（{group.models.length} 条）</h5>
        {group.models.length ? <div className="trae-model-catalog__scroll" role="region" aria-label={`${channels[group.function]} 模型表格，可横向滚动`} tabIndex={0}>
          <table aria-label={`${channels[group.function]} 官方模型目录`}><thead><tr><th scope="col">精确模型 ID</th><th scope="col">显示名称</th><th scope="col">视觉声明</th><th scope="col">思考声明</th></tr></thead>
            <tbody>{group.models.map(model => <tr key={model.id}><th scope="row"><code>{model.id}</code></th><td>{model.name === model.id ? "与 ID 相同" : model.name}</td><td>{declaration(model.supports_vision)}</td><td>{declaration(model.supports_thinking)}</td></tr>)}</tbody>
          </table>
        </div> : <p>此通道本次未返回模型记录。</p>}
      </div>)}
      {!!data.warnings.length && <div className="trae-model-catalog__warnings"><h5>目录提示</h5><ul>{data.warnings.map((warning, index) => <li key={index}>{warning}</li>)}</ul></div>}
    </>}
  </section>;
}

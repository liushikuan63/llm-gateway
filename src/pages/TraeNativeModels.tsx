import { useCallback, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { api, TraeLoginStatus, TraeNativeCatalogView, TraeNativeModelView, TraeProviderRegistrationResult } from "../api";
import { nativeCatalogFailure, NativeCatalogDiagnostic } from "./trae-native-errors";
import "./trae-native-models.css";

type Props = {
  accountId: string; accountEnabled: boolean; benefitsEnabled: boolean; loaded: boolean; busy: boolean;
  login: (Pick<TraeLoginStatus, "account_id" | "identity" | "access_expires_at_ms"> & { status: string }) | null; onBusyChange: (busy: boolean) => void;
  webRegion?: "cn" | "global";
};
type Choice = { alias: string; images: boolean };
const profile = "cn-ordinary-3.4.3-aicc20260212-v1";
const source = "solo_work_remote";
const ttl = 300_000;
const registerError = "添加结果未确认，请检查模型列表；如需重试，请手动重新读取目录。";
const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
const keys = (v: Record<string, unknown>, allowed: string[]) => Object.keys(v).length === allowed.length && allowed.every(k => Object.prototype.hasOwnProperty.call(v, k));
const text = (v: unknown, max = 256): v is string => typeof v === "string" && !!v.trim() && [...v].length <= max && !/[\u0000-\u001f\u007f-\u009f<>]/.test(v);
const flags = (v: unknown) => v === null || typeof v === "boolean";
const limit = (v: unknown) => v === null || typeof v === "number" && Number.isSafeInteger(v) && v >= 1 && v <= 16_777_216;
const timestamp = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v) && v > 0 && v <= 8640000000000000;
const warnings = (v: unknown): v is string[] => Array.isArray(v) && v.length <= 100 && v.every(s => text(s, 4096));
const reserved = new Set(["auto", "priority", "balanced", "smartest", "fastest", "reliable", "custom", "smart", "cascade"]);
const validAlias = (v: string) => /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(v) && !reserved.has(v.toLowerCase());
const declaration = (v: boolean | null) => v === null ? "未声明" : v ? "声明支持" : "声明不支持";
const imageCandidate = (entry: TraeNativeModelView) => entry.declared_vision === null && entry.protocol_carriers.images;

function catalogExclusionNotice(catalog: TraeNativeCatalogView): string | null {
  if (!catalog.excluded_count) return null;
  // Only closed product codes become presentation text. Unknown warnings from an
  // older/future backend must never expose rejected model or connection metadata.
  const known: Record<string, string> = {
    native_catalog_source_unverified_excluded: "来源未核验",
    native_catalog_custom_connection_excluded: "自定义连接",
    native_catalog_empty_physical_models_excluded: "缺少物理模型候选",
    native_catalog_detail_or_reasoning_excluded: "详情或推理声明无法一致投影",
  };
  const reasons = [...new Set(catalog.warnings.filter(code => Object.prototype.hasOwnProperty.call(known, code)).map(code => known[code]))];
  return `${reasons.length ? `本次已报告的排除原因：${reasons.join("、")}。` : "当前接口未细分排除原因。"}未展示被排除条目的名称或内容；本目录不代表供应商的全部模型。`;
}

function verifiedCatalog(value: unknown, accountId: string, webRegion?: "cn" | "global"): TraeNativeCatalogView {
  const expectedProfile = webRegion === "global" ? "global-web-raw-sg-v1" : webRegion === "cn" ? "cn-web-raw-v1" : profile;
  if (!object(value) || !keys(value, ["account_id", "region", "profile_id", "source_function", "fetched_at_ms", "catalog_revision", "entries", "excluded_count", "warnings"]) ||
    value.account_id !== accountId || value.region !== (webRegion ?? "cn") || value.profile_id !== expectedProfile || value.source_function !== (webRegion ? "chat_v3" : source) || !timestamp(value.fetched_at_ms) ||
    value.fetched_at_ms > Date.now() + 60_000 || typeof value.catalog_revision !== "string" || !/^[\x21-\x7e]{1,256}$/.test(value.catalog_revision) ||
    !Number.isSafeInteger(value.excluded_count) || (value.excluded_count as number) < 0 || !warnings(value.warnings) || !Array.isArray(value.entries) || value.entries.length > 512) throw Error("Invalid native catalog");
  const ids = new Set<string>();
  for (const entry of value.entries) {
    if (!object(entry) || !keys(entry, ["selector_id", "config_name", "display_name", "prompt_max_tokens", "max_tokens", "declared_vision", "declared_tools", "declared_thinking", "reasoning_effort_options", "protocol_carriers", "account_model_evidence", ...(webRegion ? ["raw_model"] : [])]) ||
      typeof entry.selector_id !== "string" || !/^[a-f0-9]{64}$/.test(entry.selector_id) || ids.has(entry.selector_id) || !text(entry.config_name) || !text(entry.display_name) ||
      !limit(entry.prompt_max_tokens) || !limit(entry.max_tokens) || ![entry.declared_vision, entry.declared_tools, entry.declared_thinking].every(flags) ||
      !Array.isArray(entry.reasoning_effort_options) || entry.reasoning_effort_options.length > 32 || !entry.reasoning_effort_options.every(s => text(s, 64)) || new Set(entry.reasoning_effort_options).size !== entry.reasoning_effort_options.length ||
      !object(entry.protocol_carriers) || !keys(entry.protocol_carriers, ["text", "tools", "images", "audio", "video"]) || !Object.values(entry.protocol_carriers).every(v => typeof v === "boolean") || entry.account_model_evidence !== "unverified") throw Error("Invalid native entry");
    ids.add(entry.selector_id);
    if (webRegion && (!object(entry.raw_model) || !keys(entry.raw_model, ["model_name", "config_source"]) || !text(entry.raw_model.model_name) || entry.raw_model.config_source !== 1)) throw Error("Invalid raw model identity");
  }
  return value as unknown as TraeNativeCatalogView;
}
function verifiedResult(value: unknown, count: number): TraeProviderRegistrationResult {
  if (!object(value) || !keys(value, ["provider_id", "runtime_id", "model_count", "warnings"]) || !text(value.provider_id) || !text(value.runtime_id) || value.model_count !== count || !warnings(value.warnings)) throw Error("Invalid registration result");
  return value as unknown as TraeProviderRegistrationResult;
}
function registrationNotice(result: TraeProviderRegistrationResult): string {
  const known: Record<string, string> = {
    native_registration_saved_account_currently_unavailable: "账号当前不可用，请确认已启用账号及权益，并检查独立登录状态。",
    native_existing_provider_disabled: "供应商当前不可用，请在供应商列表启用对应供应商。",
    native_existing_runtime_disabled: "运行时当前不可用，请在账号运行时列表启用对应运行时。",
    native_existing_model_disabled: "模型当前不可用，请在供应商的模型设置中启用对应模型。",
    native_existing_model_settings_preserved: "已保留现有模型设置，包括价格、启用状态和用户策略。",
    native_model_service_unverified: "本次登记未实测模型调用，目录不代表可用或免费。",
  };
  const messages = [...new Set(result.warnings.map(code => Object.prototype.hasOwnProperty.call(known, code) ? known[code] : "保存后的可用状态尚未确认，请检查模型列表及当前账号状态。"))];
  return `已保存 ${result.model_count} 个模型到本地模型列表（包含已存在的模型）。${messages.join(" ")}未发起模型测试。`;
}
function defaultAlias(entry: TraeNativeModelView, webRegion?: "cn" | "global") {
  if (webRegion) return `trae-${webRegion}-${entry.selector_id.slice(0, 16)}`;
  const candidate = `trae-cn-${entry.config_name}`;
  return validAlias(candidate) ? candidate : `trae-${entry.selector_id.slice(0, 16)}`;
}

export default function TraeNativeModels({ accountId, accountEnabled, benefitsEnabled, loaded, busy, login, onBusyChange, webRegion }: Props) {
  const id = useId();
  // Primitive values preserve a directory across equivalent status snapshots/renders.
  const scope = useMemo(() => ({}), [webRegion, accountId, accountEnabled, benefitsEnabled, loaded, busy, login?.account_id, login?.status, login?.identity?.identity_hash, login?.identity?.captured_at, login?.access_expires_at_ms]);
  const live = useRef<object | null>(null);
  const pending = useRef<{ scope: object; token: object } | null>(null);
  const [catalog, setCatalog] = useState<TraeNativeCatalogView | null>(null);
  const [choices, setChoices] = useState<Record<string, Choice>>({});
  const [operation, setOperation] = useState<"read" | "register" | null>(null);
  const [notice, setNotice] = useState<{ error: boolean; text: string; code?: NativeCatalogDiagnostic["code"] } | null>(null);
  const [stale, setStale] = useState(false);
  const [expired, setExpired] = useState(false);
  const callback = useRef(onBusyChange); callback.current = onBusyChange;
  useLayoutEffect(() => {
    live.current = scope; pending.current = null; setCatalog(null); setChoices({}); setNotice(null); setOperation(null); setStale(false); setExpired(false);
    callback.current(false);
    const expires = login?.access_expires_at_ms;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // Expiry changes eligibility locally; this timer never sends a request.
    if (timestamp(expires)) {
      const check = () => {
        if (live.current !== scope) return;
        if (Date.now() >= expires) {
          live.current = null; pending.current = null; setExpired(true); setCatalog(null); setChoices({}); setNotice(null); setOperation(null); callback.current(false);
        } else timer = setTimeout(check, Math.min(2147483647, expires - Date.now()));
      };
      check();
    }
    return () => { live.current = null; pending.current = null; if (timer) clearTimeout(timer); callback.current(false); };
  }, [scope, login?.access_expires_at_ms]);
  useLayoutEffect(() => {
    if (!catalog) return;
    const remaining = catalog.fetched_at_ms + ttl - Date.now();
    if (remaining <= 0) { setStale(true); return; }
    const timer = setTimeout(() => setStale(true), remaining);
    return () => clearTimeout(timer);
  }, [catalog]);
  const eligible = () => loaded && !!accountId && accountEnabled && benefitsEnabled && !busy && !expired && login?.account_id === accountId &&
    login.status === "authenticated" && text(login.identity?.identity_hash) && timestamp(login.access_expires_at_ms) && login.access_expires_at_ms > Date.now();
  const ready = eligible();
  const begin = (kind: "read" | "register") => {
    if (!eligible() || live.current !== scope || pending.current) return null;
    const owner = { scope, token: {} }; pending.current = owner; setOperation(kind); setNotice(null); callback.current(true); return owner;
  };
  const owns = (owner: NonNullable<typeof pending.current>) => live.current === owner.scope && pending.current === owner && eligible();
  const finish = (owner: NonNullable<typeof pending.current>) => {
    // A retired A promise cannot release B's parent busy lease.
    if (pending.current !== owner) return;
    pending.current = null; setOperation(null); callback.current(false);
  };
  const read = async () => {
    const owner = begin("read"); if (!owner) return;
    setCatalog(null); setChoices({}); setStale(false);
    try {
      const data = verifiedCatalog(await (webRegion ? api.traeWebModelCatalog(webRegion, accountId) : api.traeNativeModelCatalog(accountId)), accountId, webRegion);
      if (owns(owner)) { setCatalog(data); setStale(data.fetched_at_ms + ttl <= Date.now()); }
    } catch (error) { if (owns(owner)) setNotice({ error: true, ...nativeCatalogFailure(error) }); }
    finally { finish(owner); }
  };
  const selected = catalog?.entries.filter(entry => Object.prototype.hasOwnProperty.call(choices, entry.selector_id)) ?? [];
  const aliases = selected.map(entry => choices[entry.selector_id].alias);
  const aliasError = aliases.some(alias => !validAlias(alias)) ? "别名须为 1–128 个 ASCII 字母、数字、点、下划线或连字符，以字母或数字开头，且不能使用 auto、smart 等路由名称。" : new Set(aliases.map(alias => alias.toLowerCase())).size !== aliases.length ? "已选模型的本地别名不能重复。" : null;
  const register = async () => {
    if (!catalog || !selected.length || aliasError || stale || catalog.fetched_at_ms + ttl <= Date.now()) { if (catalog && catalog.fetched_at_ms + ttl <= Date.now()) setStale(true); return; }
    const owner = begin("register"); if (!owner) return;
    const selections = selected.map(entry => ({ selector_id: entry.selector_id, alias: choices[entry.selector_id].alias, allow_undeclared_images: imageCandidate(entry) && choices[entry.selector_id].images }));
    try {
      const result = verifiedResult(await api.registerTraeProvider({ accountId, profileId: catalog.profile_id, catalogRevision: catalog.catalog_revision, selections }), selections.length);
      if (owns(owner)) { setChoices({}); setStale(true); setNotice({ error: false, text: registrationNotice(result) }); }
    } catch { if (owns(owner)) { setChoices({}); setStale(true); setNotice({ error: true, text: registerError }); } }
    finally { finish(owner); }
  };
  const edit = useCallback((selector: string, patch: Partial<Choice>) => setChoices(current => ({ ...current, [selector]: { ...current[selector], ...patch } })), []);
  return <section className="trae-native-models" data-testid="trae-native-models" aria-labelledby={`${id}-heading`} aria-busy={!!operation}>
    <h4 id={`${id}-heading`}>独立模型</h4>
    <p>当前 Trae {webRegion === "global" ? "Global" : "CN"} 账号：<strong>{login?.account_id === accountId ? login.identity?.display_name || accountId || "未选择" : accountId || "未选择"}</strong>。手动读取后选择模型，添加到本地模型列表。</p>
    {webRegion && <p>网页登录聊天候选：{webRegion === "global" ? "固定 SG 接入" : "固定 CN 接入"}。模型权限、服务端接收与费用尚未核验；不会自动换区、换模型或补充设备标识。</p>}
    <p>协议可承载的请求类型仅为接入候选；目录不包含当前账号的模型实测结论，不能证明模型可用或免费。</p>
    <button type="button" disabled={!ready || !!operation} onClick={() => void read()}>读取可接入模型</button>
    {!ready && <p>{webRegion ? "请读取本区已保存的网页登录身份，确认已知有效期，并等待当前操作完成。" : "请启用权益与当前账号，确认独立登录身份及已知的有效期，并等待当前操作完成。"}</p>}
    {operation && <p role="status">{operation === "read" ? "正在读取可接入模型…" : "正在添加所选模型…"}</p>}
    {notice && <>
      <p role={notice.error ? "alert" : "status"}>{notice.text}</p>
      {notice.code && <details className="trae-native-models__diagnostic"><summary>诊断信息</summary><code>{notice.code}</code></details>}
    </>}
    {catalog && <>
      <p role="status">已读取 {catalog.entries.length} 个可接入模型{catalog.excluded_count ? `，已排除 ${catalog.excluded_count} 条不适用记录` : ""}。</p>
      {catalogExclusionNotice(catalog) && <p role="note" aria-label="目录排除原因">{catalogExclusionNotice(catalog)}</p>}
      {stale && <p role="status">本次目录已不可用于添加，请手动重新读取。</p>}
      {!catalog.entries.length ? <p>本次目录未返回可接入模型。</p> : <form onSubmit={event => { event.preventDefault(); void register(); }}>
        <fieldset disabled={!ready || !!operation || stale}><legend className="trae-native-models__legend">选择需要添加的模型</legend>
          <div className="trae-native-models__scroll" role="region" aria-label="可接入模型表格，可横向滚动" tabIndex={0}>
            <table><thead><tr><th scope="col">选择 / 模型</th><th scope="col">能力声明</th><th scope="col">上下文 / 输出</th><th scope="col">本地设置</th></tr></thead><tbody>
              {catalog.entries.map(entry => {
                const choice = choices[entry.selector_id]; const row = `${id}-${entry.selector_id}`;
                return <tr key={entry.selector_id} data-selector={entry.selector_id}>
                  <th scope="row"><label htmlFor={`${row}-select`}><input id={`${row}-select`} type="checkbox" checked={!!choice} onChange={event => setChoices(current => {
                    const next = { ...current }; if (event.target.checked) next[entry.selector_id] = { alias: defaultAlias(entry, webRegion), images: false }; else delete next[entry.selector_id]; return next;
                  })}/><span>{entry.display_name}</span></label><code>{entry.config_name}</code>{entry.raw_model && <code>{entry.raw_model.model_name}</code>}</th>
                  <td><span>视觉：{declaration(entry.declared_vision)}</span><span>工具：{declaration(entry.declared_tools)}</span><span>思考：{declaration(entry.declared_thinking)}</span>
                    {!!entry.reasoning_effort_options.length && <span>思考强度候选：{entry.reasoning_effort_options.join("、")}</span>}
                  </td>
                  <td><span>输入：{entry.prompt_max_tokens?.toLocaleString() ?? "未知"}</span><span>输出：{entry.max_tokens?.toLocaleString() ?? "未知"}</span>{entry.max_tokens !== null && <small>输出为请求限制，非计费上限。</small>}</td>
                  <td><label htmlFor={`${row}-alias`}>本地别名 · {entry.display_name}</label><input id={`${row}-alias`} type="text" value={choice?.alias ?? defaultAlias(entry, webRegion)} disabled={!choice} maxLength={128} autoComplete="off" spellCheck={false} aria-invalid={!!choice && (!validAlias(choice.alias) || aliases.filter(alias => alias.toLowerCase() === choice.alias.toLowerCase()).length > 1)} aria-describedby={choice && aliasError ? `${id}-alias-error` : undefined} onChange={event => edit(entry.selector_id, { alias: event.target.value })}/>
                    <label htmlFor={`${row}-images`}><input id={`${row}-images`} type="checkbox" disabled={!choice || !imageCandidate(entry)} checked={!!choice?.images && imageCandidate(entry)} aria-describedby={imageCandidate(entry) ? `${row}-image-help` : undefined} onChange={event => edit(entry.selector_id, { images: event.target.checked })}/>允许未实测的图片请求</label>
                    {imageCandidate(entry) && <small id={`${row}-image-help`}>模型未声明视觉能力；仅允许尝试发送图片，服务可能拒绝。</small>}
                    {!imageCandidate(entry) && <small>{entry.declared_vision === false ? "模型声明不支持视觉。" : !entry.protocol_carriers.images ? "当前协议不承载图片。" : "模型已声明视觉能力，无需此授权。"}</small>}
                  </td>
                </tr>;
              })}
            </tbody></table>
          </div>
          {aliasError && <p role="alert" id={`${id}-alias-error`}>{aliasError}</p>}
          <div className="trae-native-models__actions"><span>已选 {selected.length} 个</span><button type="submit" disabled={!selected.length || !!aliasError}>添加到模型列表</button></div>
        </fieldset>
      </form>}
    </>}
  </section>;
}

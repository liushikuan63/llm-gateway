import { useEffect, useRef, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { api, type ProviderView, type ProxyControlStatus } from "../api";
import type { OnboardingTarget } from "./Onboarding";

type Gateway = ProxyControlStatus["gateway"];
type Reading = {
  phase: "loading" | "ready" | "unconfirmed" | "preview";
  providers: ProviderView[] | null;
  gateway: Gateway | null;
  providersCurrent: boolean;
  gatewayCurrent: boolean;
};

function providersOf(value: ProviderView[]) {
  if (!Array.isArray(value) || value.some(p => typeof p.enabled !== "boolean" || !Array.isArray(p.models)
    || p.models.some(m => !m || typeof m.alias !== "string" || !m.alias.trim()
      || m.enabled !== undefined && typeof m.enabled !== "boolean"
      || m.model_type !== undefined && !["chat", "embedding", "image", "speech"].includes(m.model_type)))) {
    throw new Error("providers_unconfirmed");
  }
  return value;
}

function gatewayOf(value: ProxyControlStatus): Gateway {
  if (!value?.gateway || typeof value.gateway.running !== "boolean"
    || value.gateway.bound_addr != null && typeof value.gateway.bound_addr !== "string") {
    throw new Error("gateway_unconfirmed");
  }
  return value.gateway;
}

/** Configuration observations are not account/model/entitlement verification.
 * Each mounted read generation owns its responses; no timers, probes or writes. */
export default function OnboardingCheck({ onNavigate }: { onNavigate: (page: OnboardingTarget) => void }) {
  const [revision, setRevision] = useState(0);
  const [reading, setReading] = useState<Reading>({ phase: "loading", providers: null, gateway: null, providersCurrent: false, gatewayCurrent: false });
  const refresh = useRef<HTMLButtonElement>(null);
  const inFlight = useRef(false);
  const restoreRefresh = useRef(false);
  const desktop = isTauri();

  useEffect(() => {
    if (!desktop) {
      setReading({ phase: "preview", providers: null, gateway: null, providersCurrent: false, gatewayCurrent: false });
      return;
    }
    let active = true;
    inFlight.current = true;
    setReading(previous => ({ ...previous, phase: "loading", providersCurrent: false, gatewayCurrent: false }));
    void Promise.allSettled([
      api.listProviders().then(providersOf),
      api.getProxyControlStatus().then(gatewayOf),
    ]).then(([providers, gateway]) => {
      if (!active) return;
      inFlight.current = false;
      const providersCurrent = providers.status === "fulfilled";
      const gatewayCurrent = gateway.status === "fulfilled";
      setReading(previous => ({
        phase: providersCurrent && gatewayCurrent ? "ready" : "unconfirmed",
        providers: providers.status === "fulfilled" ? providers.value : previous.providers,
        gateway: gateway.status === "fulfilled" ? gateway.value : previous.gateway,
        providersCurrent, gatewayCurrent,
      }));
    });
    return () => { active = false; inFlight.current = false; };
  }, [desktop, revision]);

  // Fast IPC may batch the loading transition. Do not steal focus from another
  // control if the user moved while waiting, or from a later dialog generation.
  useEffect(() => {
    if (reading.phase !== "loading" && restoreRefresh.current) {
      restoreRefresh.current = false;
      if (document.activeElement === document.body || document.activeElement === refresh.current) refresh.current?.focus();
    }
  });

  const loading = reading.phase === "loading";
  const enabled = reading.providers?.filter(p => p.enabled) ?? null;
  // Legacy model payloads omit enabled/model_type; their documented defaults
  // remain enabled/chat. Empty mappings do not become a tested default model.
  const chatMappings = enabled?.reduce((n, p) => n + p.models.filter(m => m.enabled !== false && (m.model_type ?? "chat") === "chat").length, 0) ?? null;
  const listening = reading.gateway?.running && !!reading.gateway.bound_addr?.trim();
  let title = "正在读取配置状态…", action = "查看供应商配置", target: OnboardingTarget = "providers";
  if (reading.phase === "preview") title = "请在桌面应用读取本机状态";
  else if (!loading) {
    if (!reading.providersCurrent) title = "供应商配置尚未确认";
    else if (!reading.providers?.length) { title = "下一步：添加供应商"; action = "前往供应商配置"; }
    else if (!enabled?.length) { title = "下一步：检查供应商启用状态"; action = "查看供应商配置"; }
    else if (!chatMappings) { title = "下一步：配置聊天模型"; action = "前往模型配置"; }
    else if (!reading.gatewayCurrent || !listening) { title = "下一步：核对网关设置"; action = "查看网关设置"; target = "settings"; }
    else { title = "下一步：手动验证聊天"; action = "前往聊天测试"; target = "chat"; }
  }

  return <section className="onboarding-check" aria-label="当前配置检查" aria-busy={loading}>
    <strong>{title}</strong>
    {reading.phase === "ready" && <p role="status">配置已读取</p>}
    {reading.phase === "unconfirmed" && <p role="alert">{!reading.providersCurrent ? "供应商配置读取失败；供应商配置尚未确认。" : "网关状态读取失败；监听状态尚未确认。"}</p>}
    <ul>
      <li>{reading.providers && !reading.providersCurrent ? "上次成功读取：" : ""}已保存供应商：{reading.providers?.length ?? "未确认"}；已启用：{enabled?.length ?? "未确认"}</li>
      <li>{reading.providers && !reading.providersCurrent ? "上次成功读取：" : ""}启用聊天映射：{chatMappings ?? "未确认"}</li>
      <li>{reading.gateway && !reading.gatewayCurrent ? "上次成功读取：" : ""}网关：{!reading.gateway ? "未确认" : reading.gateway.running ? listening ? "已监听" : "监听地址未确认" : "未监听"}</li>
    </ul>
    <p className="muted">这是分别读取的本机配置与监听状态；尚未验证模型调用、账号或免费资格。</p>
    <div className="onboarding-check-actions">
      <button ref={refresh} type="button" disabled={loading || !desktop} onClick={() => {
        if (inFlight.current || loading || !desktop) return;
        inFlight.current = true;
        setReading(previous => ({ ...previous, phase: "loading", providersCurrent: false, gatewayCurrent: false }));
        restoreRefresh.current = document.activeElement === refresh.current;
        setRevision(value => value + 1);
      }}>重新读取配置状态</button>
      <button type="button" data-testid="onboarding-next-action" disabled={loading || !desktop} onClick={() => onNavigate(target)}>{action}</button>
    </div>
  </section>;
}

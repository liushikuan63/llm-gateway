import { useEffect, useRef, useState } from "react";

export type FavoriteTarget = { provider_id: string; alias: string };
export const FAVORITES_KEY = "llm-gateway-chat-favorites-v1";
const LOCK = `${FAVORITES_KEY}-write`;
type View = { ready: boolean; items: FavoriteTarget[]; error: string | null };
export const favoriteKey = (value: FavoriteTarget) => JSON.stringify([value.provider_id, value.alias]);
const validIdentifier = (value: unknown): value is string => typeof value === "string" && value.trim().length > 0
  && value.length <= 256 && !/[\u0000-\u001f\u007f]/.test(value);
const validTarget = (value: unknown): value is FavoriteTarget => {
  if (!value || typeof value !== "object") return false;
  const v = value as Partial<FavoriteTarget>;
  return Object.keys(v).length === 2 && validIdentifier(v.provider_id) && validIdentifier(v.alias);
};
function read(): View {
  try {
    const raw = window.localStorage.getItem(FAVORITES_KEY);
    if (raw === null) return { ready: true, items: [], error: null };
    if (raw.length > 8192 || new TextEncoder().encode(raw).length > 8192) throw new Error();
    const value = JSON.parse(raw);
    if (!value || typeof value !== "object" || Object.keys(value).length !== 2 || value.schema_version !== 1
      || !Array.isArray(value.favorites) || value.favorites.length > 32 || !value.favorites.every(validTarget)
      || new Set(value.favorites.map(favoriteKey)).size !== value.favorites.length) throw new Error();
    return { ready: true, items: value.favorites, error: null };
  } catch {
    return { ready: false, items: [], error: "常用模型偏好不可读取，或格式／版本未知；未覆盖原值。若显示列表，仍是上次读取，不能保存；可重读或明确重置。" };
  }
}
function locksAvailable(): boolean {
  try { return typeof navigator.locks?.request === "function"; } catch { return false; }
}

export default function useChatFavorites(selected: FavoriteTarget | null) {
  const [view, setView] = useState<View>(read);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ error: boolean; text: string } | null>(null);
  const [confirmReset, setConfirmReset] = useState(false);
  const alive = useRef(true), pending = useRef<AbortController | null>(null);
  const actionButton = useRef<HTMLButtonElement>(null);
  const resetButton = useRef<HTMLButtonElement>(null);
  const confirmationButton = useRef<HTMLButtonElement>(null);
  const selectedKey = selected ? favoriteKey(selected) : null;
  const selection = useRef({ key: selectedKey, epoch: 0 });
  if (selection.current.key !== selectedKey) selection.current = { key: selectedKey, epoch: selection.current.epoch + 1 };
  const focusAfterWrite = useRef<{ button: HTMLButtonElement; epoch: number } | null>(null);
  const refresh = () => {
    const next = read();
    setView(previous => next.ready ? next : { ...next, items: previous.items });
  };
  useEffect(() => {
    alive.current = true;
    const changed = (event: StorageEvent) => {
      if (event.key !== FAVORITES_KEY && event.key !== null) return;
      // Read current bytes, never replay an out-of-order event's old value.
      refresh(); setNotice(null);
    };
    window.addEventListener("storage", changed);
    return () => { alive.current = false; pending.current?.abort(); window.removeEventListener("storage", changed); };
  }, []);
  useEffect(() => {
    const focus = focusAfterWrite.current;
    if (!busy && focus && focus.epoch === selection.current.epoch
      && (document.activeElement === document.body || document.activeElement === focus.button)) {
      focus.button.focus(); focusAfterWrite.current = null;
    }
  }, [busy]);
  useEffect(() => { if (confirmReset) confirmationButton.current?.focus(); }, [confirmReset]);

  const mutate = async (kind: "add" | "remove" | "reset", target: FavoriteTarget | null, label: string) => {
    if (pending.current) return;
    if (!locksAvailable()) { setNotice({ error: true, text: "当前环境缺少本机偏好写入保护，只读保留常用列表，不能保存。" }); return; }
    const controller = new AbortController(); pending.current = controller;
    const focused = document.activeElement;
    if (focused instanceof HTMLButtonElement) focusAfterWrite.current = { button: kind === "reset" ? resetButton.current ?? focused : focused, epoch: selection.current.epoch };
    setBusy(true); setNotice(null);
    try {
      // Atomic among cooperating windows. No API/IPC/model call, await or
      // selection mutation inside this short read-latest/write critical section.
      await navigator.locks.request(LOCK, { signal: controller.signal }, () => {
        if (controller.signal.aborted || !alive.current) throw new Error("已取消保存");
        if (kind === "reset") { window.localStorage.removeItem(FAVORITES_KEY); return; }
        if (!validTarget(target)) throw new Error("目标标识无效或过长，未保存");
        const current = read();
        if (!current.ready) throw new Error("偏好格式／版本未知，未保存；请重读或明确重置");
        const contains = current.items.some(value => favoriteKey(value) === favoriteKey(target));
        if ((kind === "add" && contains) || (kind === "remove" && !contains)) return;
        const items = kind === "add" ? [...current.items, { provider_id: target.provider_id, alias: target.alias }]
          : current.items.filter(value => favoriteKey(value) !== favoriteKey(target));
        const raw = JSON.stringify({ schema_version: 1, favorites: items });
        if (items.length > 32 || new TextEncoder().encode(raw).length > 8192) throw new Error("常用偏好最多32项／8 KiB，请先移除部分项目");
        window.localStorage.setItem(FAVORITES_KEY, raw);
      });
      if (alive.current) {
        refresh();
        setNotice({ error: false, text: kind === "reset" ? "已仅重置常用模型偏好，当前模型与对话未改变。"
          : `已将 ${label} ${kind === "add" ? "加入" : "移出"}常用；没有切换或发送模型。` });
        if (kind === "reset") setConfirmReset(false);
      }
    } catch (error) {
      if (alive.current && !controller.signal.aborted) {
        refresh(); setNotice({ error: true, text: `保存常用模型失败：${error instanceof Error ? error.message : "本机存储不可写"}。保留当前选择与草稿，可重试。` });
      }
    } finally {
      if (pending.current === controller) pending.current = null;
      if (alive.current) setBusy(false);
    }
  };
  return { ...view, busy, notice, confirmReset, canWrite: locksAvailable(), actionButton, resetButton, confirmationButton,
    refresh, add: (target: FavoriteTarget, label: string) => mutate("add", target, label),
    remove: (target: FavoriteTarget, label: string) => mutate("remove", target, label),
    requestReset: () => setConfirmReset(true),
    cancelReset: () => { setConfirmReset(false); resetButton.current?.focus(); },
    reset: () => mutate("reset", null, "") };
}

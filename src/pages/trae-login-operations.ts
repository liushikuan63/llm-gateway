import { api } from "../api";

// This registry lives for the renderer's lifetime, beyond a page/StrictMode
// unmount. It contains only local account IDs and callback IDs, never secrets.
export type Cancellation = { account: string; id: string | null };
type Snapshot = { preparations: number; cancellations: readonly Cancellation[]; cancellationPending: number };
const preparations = new Set<symbol>();
const cancellations = new Map<string, Cancellation>();
const pending = new Map<string, Promise<void>>();
const listeners = new Set<() => void>();
let snapshot: Snapshot = { preparations: 0, cancellations: [], cancellationPending: 0 };
function publish() {
  snapshot = { preparations: preparations.size, cancellations: [...cancellations.values()], cancellationPending: pending.size };
  for (const listener of listeners) listener();
}
export function subscribeTraeOperations(listener: () => void) {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}
export function getTraeOperations() { return snapshot; }
export function traeOperationBlocked() { return preparations.size > 0 || cancellations.size > 0; }
export function beginTraePreparation() {
  const lease = Symbol("Trae preparation"); preparations.add(lease); publish(); return lease;
}
export function finishTraePreparation(lease: symbol) {
  if (preparations.delete(lease)) publish();
}
export function cancelTraeOperation(job: Cancellation): Promise<void> {
  const key = `${job.account}:${job.id ?? "refresh"}`;
  const existing = pending.get(key);
  if (existing) return existing;
  cancellations.set(key, job);
  const task = Promise.resolve().then(async () => {
    if (job.id === null) await api.traeLoginCancelRefresh(job.account);
    else await api.traeLoginCancel(job.account, job.id);
    cancellations.delete(key);
  }).catch(() => { cancellations.set(key, job); }).finally(() => { pending.delete(key); publish(); });
  pending.set(key, task); publish();
  return task;
}
export async function retryTraeCancellations() {
  if (pending.size) return;
  for (const job of [...cancellations.values()]) await cancelTraeOperation(job);
}

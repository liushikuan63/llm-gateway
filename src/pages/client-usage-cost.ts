import type { ClientUsageRow } from "../api";

export function costBounds(row: ClientUsageRow): { min: number; max: number; currency: string } | null {
  const min = row.estimated_cost_min ?? row.estimated_cost;
  const max = row.estimated_cost_max ?? row.estimated_cost;
  const currency = row.estimated_currency?.toLowerCase();
  if (min == null || max == null || !Number.isFinite(min) || !Number.isFinite(max) || min < 0 || max < min || !currency || !["usd", "cny"].includes(currency)) return null;
  return { min, max, currency };
}
export function costNumber(value: number) {
  return value > 0 && value < 0.000001 ? value.toExponential(3) : value.toLocaleString("zh-CN", { minimumFractionDigits: 2, maximumFractionDigits: 9 });
}
export function costText(min: number, max: number, currency: string) {
  return `≈ ${currency.toUpperCase()} ${costNumber(min)}${min === max ? "" : `–${costNumber(max)}`}`;
}
/** Keep currencies separate; only compare amounts within the same currency. */
export function compareClientUsageCost(a: ClientUsageRow, b: ClientUsageRow): number {
  const left = costBounds(a), right = costBounds(b);
  if (left && right) {
    const currency = left.currency.localeCompare(right.currency);
    if (currency) return currency;
    if (left.max !== right.max) return right.max - left.max;
    if (left.min !== right.min) return right.min - left.min;
  } else if (left || right) {
    return left ? -1 : 1;
  }
  for (const field of ["source", "profile", "provider", "model"] as const) {
    const order = (a[field] ?? "").localeCompare(b[field] ?? "");
    if (order) return order;
  }
  return 0;
}
export function feeTotals(rows: ClientUsageRow[]) {
  const totals = new Map<string, { min: number; max: number; models: number }>();
  let unknown = 0;
  for (const row of rows) {
    const bounds = costBounds(row);
    if (!bounds) { unknown++; continue; }
    const total = totals.get(bounds.currency) ?? { min: 0, max: 0, models: 0 };
    total.min += bounds.min; total.max += bounds.max; total.models++;
    totals.set(bounds.currency, total);
  }
  return { totals, unknown };
}

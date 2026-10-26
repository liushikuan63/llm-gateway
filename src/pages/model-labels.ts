import type { ModelRef, ProviderView } from "../api";

export function modelDisplayLabel(provider: ProviderView, model: ModelRef): string {
  const name = provider.model_display_names?.[model.alias];
  if (typeof name !== "string" || !name.trim() || name.length > 256 || /[\u0000-\u001f\u007f]/.test(name)) return model.alias;
  return model.alias !== model.upstream && model.alias !== name ? `${name}（${model.alias}）` : name;
}

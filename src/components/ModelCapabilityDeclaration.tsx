import { ModelDeclarationView, ModelRef, ModelType } from "../api";

// Presentation follows existing ingress consumers, not a new model score/vector.
// Typed passthrough uses model_type and default chat requirements. It does not
// establish model-specific tool/reasoning/media/stream availability.
const ENTRY: Record<ModelType, string> = {
  chat: "聊天端点 /v1/chat/completions",
  embedding: "Embedding 端点 /v1/embeddings",
  image: "图像生成端点 /v1/images/generations",
  speech: "语音合成端点 /v1/audio/speech",
};
export function usesChatDeclaration(model: ModelRef) {
  // Match the backend's legacy missing model_type default, not unknown future types.
  return model.model_type === "chat" || model.model_type == null;
}
export default function ModelCapabilityDeclaration({ model, declaration }: { model: ModelRef; declaration?: ModelDeclarationView }) {
  const flags = [[model.supports_tools, "工具"], [model.supports_thinking, "思考"], [model.supports_vision, "图像输入"], [model.supports_audio, "音频输入"], [model.supports_video, "视频输入"], [model.supports_stream, "流式"]] as const;
  const supported = flags.filter(([value]) => value === true).map(([, label]) => label);
  const unconfirmed = flags.filter(([value]) => value !== true).map(([, label]) => label);
  const raw = <><span>配置声明：{supported.join("、") || "暂无支持声明"}</span>
    {unconfirmed.length > 0 && <small>未确认支持：{unconfirmed.join("、")}</small>}
    <small>旧布尔 false 未区分缺失和否定来源，不等于已实测不支持。</small></>;
  // The view and booleans come from the same provider/alias read snapshot.
  // Never infer a local declaration from a legacy false or a score factor.
  const fields = ["tools", "thinking", "vision", "audio", "video", "stream"] as const;
  const malformed = declaration?.status === "invalid" || (declaration?.status === "declared" && (
    declaration.read_only || !/^[0-9a-f]{64}$/.test(declaration.binding) || !/^[0-9a-f]{64}$/.test(declaration.revision)
    || !declaration.fields || typeof declaration.fields !== "object" || Array.isArray(declaration.fields)
    || Object.keys(declaration.fields).length === 0
    || Object.entries(declaration.fields).some(([field, record]) => {
      const index = fields.indexOf(field as typeof fields[number]);
      return index < 0 || !record || !["supported", "unsupported", "unknown"].includes(record.state)
        || !Number.isSafeInteger(record.recorded_at_ms) || record.recorded_at_ms <= 0
        || (record.state === "supported") !== (flags[index][0] === true);
    })
  ));
  const declared = declaration?.status === "declared" && !malformed;
  const declaredStates = declared ? flags.map(([value, label], index) => ({
    value, label, record: declaration.fields[fields[index]],
  })) : [];
  const current = declared ? <>
    {([['supported', '本地配置声明支持'], ['unsupported', '本地配置声明不支持'], ['unknown', '本地配置未确认']] as const).map(([state, label]) => {
      const names = declaredStates.filter(item => item.record?.state === state).map(item => item.label);
      return names.length > 0 && <small key={state}>{label}：{names.join("、")}</small>;
    })}
    {declaredStates.some(item => !item.record) && <small>其余字段仍是旧布尔参考：{declaredStates.filter(item => !item.record).map(item => `${item.label}${item.value === true ? "支持（未核验）" : "未确认"}`).join("、")}</small>}
    <small>仅本地配置，未实测；修订仅用于配置并发校验，不代表模型版本。</small>
  </> : <>{malformed && <small>本地声明资料不可确认（保留旧布尔参考）。</small>}{declaration?.status === "not_applicable" && <small>本地三态声明不适用；保留原配置参考。</small>}{raw}</>;
  return <div data-testid="model-capability-declaration">
    {usesChatDeclaration(model) ? current : <>
      <span>{ENTRY[model.model_type] ?? "未识别类型端点"}</span>
      <small>聊天控制维度：本类型路径未评价，不记为不支持。</small>
      <small>类型入口不是当前供应商／协议／账号可用保证。</small>
      <details><summary>查看旧配置声明</summary>{raw}</details>
    </>}
    <small>仅配置资料，未自动探测，不作为额外路由权重。</small>
  </div>;
}

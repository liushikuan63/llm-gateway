import type { ModelDeclarationField, ModelDeclarationState, ModelDeclarationView, ModelRef } from "../api";

export const DECLARATION_FIELDS = [
  ["tools", "supports_tools", "工具调用"], ["thinking", "supports_thinking", "思考 / 推理"],
  ["vision", "supports_vision", "图像输入"], ["audio", "supports_audio", "音频输入"],
  ["video", "supports_video", "视频输入"], ["stream", "supports_stream", "流式响应"],
  ["parallel_tools", null, "并行工具"], ["structured_output", null, "结构化输出"], ["audio_output", null, "音频输出"],
] as const;
export type DeclarationFlags = Pick<ModelRef, NonNullable<typeof DECLARATION_FIELDS[number][1]>>;
export type DeclarationDraft = { fields: Partial<Record<ModelDeclarationField, ModelDeclarationState>>; originalFlags: DeclarationFlags };
export const declarationFlags = (model: ModelRef): DeclarationFlags => ({
  supports_tools: model.supports_tools, supports_thinking: model.supports_thinking,
  supports_vision: model.supports_vision, supports_audio: model.supports_audio,
  supports_video: model.supports_video, supports_stream: model.supports_stream,
});

/** A captured view must authorize the same target and revision for controls and saves. */
export const isEditableDeclarationView = (view: ModelDeclarationView) =>
  !view.read_only && ["legacy", "declared"].includes(view.status)
  && /^[0-9a-f]{64}$/.test(view.binding) && /^[0-9a-f]{64}$/.test(view.revision);

/** Older or malformed advertisements cannot authorize an extension write. */
export function isExtensionAdvertised(view: ModelDeclarationView | undefined, field: ModelDeclarationField) {
  const advertised = view?.supported_fields;
  return Array.isArray(advertised) && advertised.every(item => typeof item === "string") && advertised.includes(field);
}

/** A configuration declaration is neither measured capability nor fee proof. */
export function ModelDeclarationFields({ model, view, draft, disabled, blockedReason, onRecord, onUndo }: {
  model: ModelRef; view?: ModelDeclarationView; draft?: DeclarationDraft; disabled: boolean; blockedReason?: string;
  onRecord: (field: ModelDeclarationField, state: ModelDeclarationState) => void; onUndo: () => void;
}) {
  const blocked = blockedReason || (view && !isEditableDeclarationView(view) ? "声明记录不可用（只读）；原布尔参数保留，不制造来源。" : "");
  return <section aria-label={`${model.alias || "新模型"} 能力声明`} className="model-declarations">
    <p className="muted">仅本地配置声明，未经实测，不证明免费资格或费用。旧字段显式“未确认”也保留本地目录覆盖；扩展项没有旧布尔投影。</p>
    {blocked && <p role="status">{blocked}</p>}
    {DECLARATION_FIELDS.some(([field, property]) => property === null && !isExtensionAdvertised(view, field)) && <p role="status">未获当前服务明确支持的扩展项不可编辑。新模型请先保存基础配置并重新打开。</p>}
    <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(210px, 1fr))", gap: 10 }}>
      {DECLARATION_FIELDS.map(([field, property, label]) => {
        const serverUnsupported = property === null && !isExtensionAdvertised(view, field);
        const legacy = property !== null && model[property] === true;
        const record = view?.fields?.[field];
        const stored = record && ["supported", "unsupported", "unknown"].includes(record.state) ? record.state : undefined;
        const state = draft?.fields[field] ?? (blocked ? "unknown" : stored ?? (legacy ? "supported" : "unknown"));
        const source = draft?.fields[field] ? "待保存的本地配置" : stored ? "已保存的本地配置 · 未实测" : legacy ? "旧 / 默认配置支持 · 未核验" : property === null ? "扩展项未确认" : "旧字段未确认";
        return <div key={field}>
          <label style={{ display: "block" }}>{label}声明<select style={{ width: "100%", minWidth: 0 }} aria-label={`${label}声明`} value={state} disabled={disabled || !!blocked || serverUnsupported}
            onChange={event => onRecord(field, event.target.value as ModelDeclarationState)}>
            <option value="unknown">未确认</option><option value="supported">配置声明支持</option><option value="unsupported">配置声明不支持</option>
          </select></label>
          <small className="muted" style={{ display: "block" }}>{blocked ? "声明未确认" : source}</small>
          {stored && record && <small className="muted" style={{ display: "block" }}>配置记录：{new Date(record.recorded_at_ms).toLocaleString()}</small>}
          <button type="button" className="ghost" disabled={disabled || !!blocked || serverUnsupported} aria-label={`记录${label}当前声明`} onClick={() => onRecord(field, state)}>记录当前声明</button>
        </div>;
      })}
    </div>
    {draft && <button type="button" className="ghost" disabled={disabled} onClick={onUndo}>撤销本行声明改动</button>}
  </section>;
}

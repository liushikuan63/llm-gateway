// Only fixed codes emitted by the current native catalog read path may reach UI.
export const knownNativeCatalogErrorCodes = [
  "native_auth_unavailable", "native_login_status_unavailable", "native_login_required", "native_login_identity_unavailable",
  "native_source_function_unsupported", "native_client_unavailable", "native_ipc_arguments_invalid", "native_model_authorization_invalid",
  "native_model_login_pending", "native_device_bootstrap_unavailable", "native_device_profile_unverified", "native_model_proxy_invalid",
  "native_model_auth_instance_changed", "native_model_session_changed", "native_profile_invalid", "native_catalog_request_invalid",
  "native_catalog_transport_failed", "native_catalog_deadline_exceeded", "native_catalog_http_status_rejected", "native_catalog_body_limit",
  "native_catalog_body_read_failed", "native_catalog_sensitive_reflection", "native_catalog_nesting_invalid", "native_catalog_json_invalid",
  "native_catalog_shape_invalid", "native_catalog_service_error", "native_catalog_function_mismatch", "native_catalog_entry_limit", "native_catalog_selector_conflict",
] as const;
type ProducerCode = typeof knownNativeCatalogErrorCodes[number];
type SafeCode = ProducerCode | "native_catalog_account_unavailable" | "native_catalog_rejected";
export type NativeCatalogDiagnostic = { code: SafeCode; text: string };
const codes: ReadonlySet<string> = new Set(knownNativeCatalogErrorCodes);
const fallback = "native_catalog_rejected";

export function classifyNativeCatalogFailure(value: unknown): SafeCode {
  try {
    // An own data property avoids custom message getters. Reflection itself may
    // throw (for example a revoked Proxy); that is also an unknown failure.
    const message: unknown = typeof value === "string" ? value
      : value instanceof Error ? Object.getOwnPropertyDescriptor(value, "message")?.value : undefined;
    if (typeof message !== "string") return fallback;
    const prefix = "page.evaluate: ";
    const exact = message.startsWith(prefix) ? message.slice(prefix.length) : message;
    if (exact === "Trae模型目录需要已启用的国内权益账号") return "native_catalog_account_unavailable";
    return codes.has(exact) ? exact as ProducerCode : fallback;
  } catch { return fallback; }
}

const login = "无法读取可接入模型：当前独立登录或身份信息不可用。请手动检查当前账号的独立登录状态，确认有效后再手动读取。";
const device = "无法读取可接入模型：当前设备登记或设备配置尚未就绪。请检查独立登录中的设备登记状态及已保存的设备配置，确认后再手动读取。";
const authorization = "无法读取可接入模型：本次目录授权无效或读取期间授权状态已变化。请手动检查当前账号与独立登录状态，确认后重新手动读取目录。";
const configuration = "无法读取可接入模型：当前目录接入配置或请求参数不可用。请检查应用版本和独立模型接入配置，保留诊断码并反馈问题，确认后再手动读取。";
const network = "无法读取可接入模型：目录网络传输未完成。请检查网络与应用代理设置，确认连接正常后再手动读取。";
const response = "无法读取可接入模型：目录响应未通过安全、大小、JSON 或结构校验。请保留诊断码并反馈问题，检查应用版本及服务状态，确认后再手动读取。";
const guidance: Record<SafeCode, string> = {
  native_auth_unavailable: login,
  native_login_status_unavailable: login,
  native_login_required: login,
  native_login_identity_unavailable: login,
  native_catalog_account_unavailable: "无法读取可接入模型：当前账号不满足已启用的国内权益账号条件。请检查当前选择的 Trae CN 账号及账号、权益开关，确认后再手动读取。",
  native_source_function_unsupported: configuration,
  native_client_unavailable: configuration,
  native_ipc_arguments_invalid: configuration,
  native_model_authorization_invalid: authorization,
  native_model_login_pending: "无法读取可接入模型：当前独立登录尚在进行。请等待并手动检查登录状态，确认登录完成后再手动读取。",
  native_device_bootstrap_unavailable: device,
  native_device_profile_unverified: device,
  native_model_proxy_invalid: "无法读取可接入模型：应用代理配置无效。请检查应用代理地址及配置，确认有效后再手动读取。",
  native_model_auth_instance_changed: authorization,
  native_model_session_changed: authorization,
  native_profile_invalid: configuration,
  native_catalog_request_invalid: configuration,
  native_catalog_transport_failed: network,
  native_catalog_deadline_exceeded: "无法读取可接入模型：目录读取超过等待时限。请检查网络、应用代理及服务状态，确认后再手动读取。",
  native_catalog_http_status_rejected: "无法读取可接入模型：目录服务返回了非成功的 HTTP 状态；当前诊断未提供具体状态码及原因。请检查服务状态与网络、代理设置，保留诊断码反馈问题，确认后再手动读取。",
  native_catalog_body_limit: response,
  native_catalog_body_read_failed: "无法读取可接入模型：目录响应正文未能完整读取。请检查网络、应用代理及服务状态，确认后再手动读取。",
  native_catalog_sensitive_reflection: response,
  native_catalog_nesting_invalid: response,
  native_catalog_json_invalid: response,
  native_catalog_shape_invalid: response,
  native_catalog_service_error: "无法读取可接入模型：目录服务报告业务失败；当前诊断未提供具体业务原因。请检查服务状态，保留诊断码反馈问题，确认后再手动读取。",
  native_catalog_function_mismatch: response,
  native_catalog_entry_limit: response,
  native_catalog_selector_conflict: response,
  native_catalog_rejected: "无法读取可接入模型，暂无法确定原因。请手动检查当前账号状态、网络与应用配置，确认后再手动读取；如仍失败，请提供诊断码反馈问题。",
};

export function nativeCatalogFailure(value: unknown): NativeCatalogDiagnostic {
  const code = classifyNativeCatalogFailure(value);
  return { code, text: guidance[code] };
}

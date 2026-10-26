// Tauri's Qoder Result error producer is a string. Never inspect or stringify
// arbitrary objects, URLs, credential-shaped values or unknown message suffixes.
export type QoderPollFailureCode =
  | "unknown" | "request_failed_or_timeout" | "response_unavailable"
  | "poll_http_rejected" | "identity_mismatch" | "identity_verification_failed"
  | "local_persistence_failed" | "previous_poll_unknown"
  | "expired_or_cancelled" | "invalid_local_session";

export type QoderPollFailure = { code: QoderPollFailureCode; text: string; httpStatus?: number };

const unknownFailure: QoderPollFailure = {
  code: "unknown",
  text: "本次授权检查失败或结果不明，已停止检查。请取消后重新登录，不会自动重放。",
};
const requestFailure: QoderPollFailure = {
  code: "request_failed_or_timeout",
  text: "授权检查请求失败或超时，已停止检查；远端结果未确认，不会自动重试。",
};
const responseFailure: QoderPollFailure = {
  code: "response_unavailable",
  text: "授权检查响应无法确认，已停止检查；不会据此判断远端未签发。",
};
const identityMismatch: QoderPollFailure = {
  code: "identity_mismatch",
  text: "授权身份与原账号或组织不一致，已停止检查；请核对原账号和组织。",
};
const persistenceFailure: QoderPollFailure = {
  code: "local_persistence_failed",
  text: "本地登录资料保存未确认，已停止检查；请保留原状态，不要重复提交。",
};
const expired: QoderPollFailure = {
  code: "expired_or_cancelled",
  text: "本次授权已过期或取消，已停止检查；不会自动重试。",
};
const known: ReadonlyMap<string, QoderPollFailure> = new Map([
  ["Qoder认证请求失败或超时", requestFailure],
  ["Qoder认证响应读取失败", responseFailure],
  ["Qoder认证响应不是有效JSON", responseFailure],
  ["Qoder认证响应过大", responseFailure],
  ["Qoder登录与原账号或组织不一致", identityMismatch],
  ["Qoder令牌与账号身份不一致", identityMismatch],
  ["Qoder账号核验失败，未保存认证", {
    code: "identity_verification_failed", text: "官方账号核验未通过，已停止检查；本次登录未确认。",
  }],
  ["无法保存Qoder登录", persistenceFailure],
  ["无法保存Qoder认证", persistenceFailure],
  ["无法结束Qoder授权", persistenceFailure],
  ["该授权已发起轮询且结果不明，请取消后重新登录", {
    code: "previous_poll_unknown", text: "原授权检查结果不明，已停止检查；请先取消等待并核对本地状态。",
  }],
  ["Qoder授权等待已过期", expired],
  ["Qoder授权已取消、过期或不存在", expired],
  ["Qoder授权已取消或过期，未保存认证", expired],
  ["Qoder授权会话绑定无效", {
    code: "invalid_local_session", text: "本次授权的本地绑定无法确认，已停止检查；不会自动重试。",
  }],
]);

export function qoderPollFailure(error: unknown): QoderPollFailure {
  if (typeof error !== "string" || error.length > 512) return unknownFailure;
  const fixed = known.get(error);
  if (fixed) return fixed;
  // Only the exact local producer envelope can expose a public HTTP status.
  const status = /^Qoder授权轮询被拒绝（HTTP ([1-5][0-9]{2})）$/.exec(error);
  if (status) return {
    code: "poll_http_rejected", httpStatus: Number(status[1]),
    text: "授权检查收到非成功响应，已停止检查；不会自动重试。",
  };
  return unknownFailure;
}

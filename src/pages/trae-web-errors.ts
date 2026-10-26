// Unknown auth failures may contain credentials or callback details. Only exact
// locally known producer messages select fixed recovery guidance; never echo input.
const unknownFailure = "网页登录操作结果无法确认，请使用当前可用的恢复操作；未自动重试。";
const knownFailures: ReadonlyMap<string, string> = new Map([
  ["无法确认官方网页登录链接，未打开浏览器。", "无法确认官方网页登录链接，未打开浏览器。"],
  ["无法确认网页登录状态，请重新读取本地状态。", "无法确认网页登录状态，请重新读取本地状态。"],
  ["取消尚未确认，请读取本地状态或重试取消。", "取消尚未确认，请读取本地状态或重试取消。"],
  ["无法确认网页刷新成功，请读取本地状态；未自动重试。", "无法确认网页刷新成功，请读取本地状态；未自动重试。"],
  ["Trae网页会话地区不一致", "网页登录地区或账号归属无法确认，请检查当前区域和本机账号后读取状态。"],
  ["Trae网页等待归属不一致", "网页登录地区或账号归属无法确认，请检查当前区域和本机账号后读取状态。"],
  ["Trae网页认证主机与所选地区不匹配，未发送凭据", "网页登录地区或账号归属无法确认，请检查当前区域和本机账号后读取状态。"],
  ["该Trae网页账号已有进行中的授权，请先完成或取消", "当前账号已有网页授权等待，请先读取本地状态并完成或取消原等待。"],
  ["Trae网页登录需要有效本机账号名称及明确授权，未发起登录", "请核对本机账号名称和保存网页登录会话的同意状态；未发起登录。"],
]);

export function webLoginFailureMessage(error: unknown): string {
  try {
    // Do not invoke custom message getters or stringify arbitrary/revoked objects.
    const text: unknown = typeof error === "string" ? error
      : error instanceof Error ? Object.getOwnPropertyDescriptor(error, "message")?.value : undefined;
    return typeof text === "string" && text.length <= 512 ? knownFailures.get(text) ?? unknownFailure : unknownFailure;
  } catch { return unknownFailure; }
}

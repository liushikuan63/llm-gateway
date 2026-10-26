import type { DesktopChatStreamEvent } from "../api";

export interface StreamIdentity { requestId: string; sessionId: string; model: string }
export interface StreamPreview {
  sequence: number; route: string | null; content: string; reasoning: string; refusal: string;
}
export const emptyStreamPreview = (): StreamPreview => ({ sequence: 0, route: null, content: "", reasoning: "", refusal: "" });
export function appendStreamEvent(state: StreamPreview, identity: StreamIdentity, event: DesktopChatStreamEvent): StreamPreview {
  // No transcript, route selection or entitlement is reconstructed from a chunk.
  // An old channel callback may outlive its invoke; its identities never change.
  if (event.request_id !== identity.requestId || event.session_id !== identity.sessionId || event.requested_model !== identity.model ||
    !Number.isSafeInteger(event.sequence) || event.sequence !== state.sequence + 1 || typeof event.text !== "string" ||
    !["content", "reasoning", "refusal"].includes(event.kind) ||
    (event.routed_via !== null && typeof event.routed_via !== "string") ||
    (state.sequence > 0 && event.routed_via !== state.route) ||
    state.content.length + state.reasoning.length + state.refusal.length + event.text.length > 8 * 1024 * 1024) return state;
  return { ...state, sequence: event.sequence, route: event.routed_via, [event.kind]: state[event.kind] + event.text };
}

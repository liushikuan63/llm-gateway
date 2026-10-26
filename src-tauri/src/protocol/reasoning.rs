//! OpenAI's two effort field names share a value, not a token-budget conversion.
//! Keep the Responses object in the ordinary IR extras so cloning/compaction and
//! cache identity retain the actual request. These are controls, never audit data.
use serde_json::{json, Value};

use crate::domain::{ChatRequest, Dialect};
use crate::error::{GatewayError, Result};

pub(crate) fn validate(req: &ChatRequest, dialect: Dialect, supported: bool) -> Result<()> {
    // Preserve the existing explicit capability rule: false/unknown strips these
    // controls at the final body boundary, including model extra-body overrides.
    if !supported {
        return Ok(());
    }
    let object = req.extra.get("reasoning");
    let flat_effort = req.extra.get("reasoning_effort");
    if object.is_none() && flat_effort.is_none() {
        return Ok(());
    }
    let reject = |reason| GatewayError::RequestControlUnavailable { reason };
    if let (Some(object), Some(effort)) = (object, flat_effort) {
        if !object.is_object() || object.get("effort").is_some_and(|v| v != effort) {
            return Err(reject("reasoning.effort 与 reasoning_effort 冲突"));
        }
    }
    match dialect {
        Dialect::Responses => Ok(()),
        Dialect::OpenAI => {
            if object.is_some_and(|value| {
                value.as_object().map_or(true, |fields| fields.keys().any(|key| key != "effort"))
            }) {
                return Err(reject("Chat Completions 无法表达该 reasoning 对象；请使用 Responses 上游，或仅请求 effort"));
            }
            Ok(())
        }
        _ => Err(reject("所选上游方言没有已验证的 OpenAI reasoning 映射；请使用 Responses 或 Chat Completions 上游")),
    }
}

pub(crate) fn responses_value(req: &ChatRequest) -> Option<Value> {
    if let Some(mut object) = req.extra.get("reasoning").cloned() {
        if let (Some(fields), Some(effort)) =
            (object.as_object_mut(), req.extra.get("reasoning_effort"))
        {
            fields.entry("effort").or_insert_with(|| effort.clone());
        }
        Some(object)
    } else {
        req.extra
            .get("reasoning_effort")
            .map(|effort| json!({"effort": effort}))
    }
}

pub(crate) fn chat_effort(req: &ChatRequest) -> Option<&Value> {
    req.extra
        .get("reasoning")
        .and_then(|value| value.get("effort"))
        .or_else(|| req.extra.get("reasoning_effort"))
}

/// Message-only agent bridges have no control mapping. Decline before invoking
/// an adapter instead of silently dropping the newly retained public controls.
pub(crate) fn validate_message_only(req: &ChatRequest) -> Result<()> {
    if req.extra.contains_key("reasoning") || req.extra.contains_key("reasoning_effort") {
        return Err(GatewayError::RequestControlUnavailable {
            reason: "当前代理运行时仅接收消息，未定义 reasoning 控制映射",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn responses_reasoning_message_only_adapter_declines_without_mutating_request() {
        let mut req: ChatRequest =
            serde_json::from_value(json!({"model":"owned","messages":[],"stream":false})).unwrap();
        assert!(validate_message_only(&req).is_ok());
        req.extra
            .insert("reasoning".into(), json!({"effort":"high"}));
        let before = serde_json::to_value(&req).unwrap();
        assert!(matches!(
            validate_message_only(&req),
            Err(GatewayError::RequestControlUnavailable { .. })
        ));
        assert_eq!(serde_json::to_value(&req).unwrap(), before);
    }
}

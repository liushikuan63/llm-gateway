//! Fixed control tags, never content or proof of vendor acceptance.
//! Caller tags precede protocol conversion; normalized_ingress follows it.
//! effective_request may include gateway enrichment. No stage stores raw content.
use crate::domain::{ChatRequest, Dialect};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningMode {
    Absent,
    Disabled,
    Enabled,
    Adaptive,
    Mixed,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolsMode {
    Absent,
    Empty,
    Configured,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoiceMode {
    Absent,
    None,
    Auto,
    Required,
    Named,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlSet {
    pub reasoning: ReasoningMode,
    pub tools: ToolsMode,
    pub tool_choice: ToolChoiceMode,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireDialect {
    OpenAI,
    Anthropic,
    Gemini,
    Ollama,
    Responses,
}
impl From<Dialect> for WireDialect {
    fn from(value: Dialect) -> Self {
        match value {
            Dialect::OpenAI => Self::OpenAI,
            Dialect::Anthropic => Self::Anthropic,
            Dialect::Gemini => Self::Gemini,
            Dialect::Ollama => Self::Ollama,
            Dialect::Responses => Self::Responses,
        }
    }
}
/// Original caller tags are extracted before protocol normalization. Native
/// dialect and fixed tags only: no caller-controlled metadata object is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallerControls {
    pub dialect: WireDialect,
    pub modes: ControlSet,
}
impl CallerControls {
    pub(crate) fn capture(body: &Value, dialect: Dialect) -> Self {
        Self {
            dialect: dialect.into(),
            modes: ControlSet::from_wire(body, dialect),
        }
    }
}
/// Independent of the fixed numeric TimingSnapshot v1/v2. A missing ingress
/// means no trustworthy ingress boundary, including direct/internal callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "Value")]
pub struct WireModeSnapshot {
    pub schema_version: u8,
    pub dialect: WireDialect,
    pub declared_thinking: Option<bool>,
    pub caller: Option<CallerControls>,
    pub normalized_ingress: Option<ControlSet>,
    pub effective_request: ControlSet,
    pub wire: ControlSet,
}
impl WireModeSnapshot {
    pub(crate) fn observe(
        dialect: Dialect,
        declared_thinking: Option<bool>,
        caller: Option<CallerControls>,
        ingress: Option<ControlSet>,
        request: &ChatRequest,
        wire: &Value,
    ) -> Self {
        Self {
            schema_version: 1,
            dialect: dialect.into(),
            declared_thinking,
            caller,
            normalized_ingress: ingress,
            effective_request: ControlSet::from_request(request),
            wire: ControlSet::from_wire(wire, dialect),
        }
    }
    pub fn project(value: &Value) -> Option<Self> {
        const FIELDS: [&str; 7] = [
            "schema_version",
            "dialect",
            "declared_thinking",
            "caller",
            "normalized_ingress",
            "effective_request",
            "wire",
        ];
        let object = value.as_object()?;
        if object.len() != FIELDS.len() || FIELDS.iter().any(|key| !object.contains_key(*key)) {
            return None;
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            schema_version: u8,
            dialect: WireDialect,
            declared_thinking: Option<bool>,
            caller: Option<CallerControls>,
            normalized_ingress: Option<ControlSet>,
            effective_request: ControlSet,
            wire: ControlSet,
        }
        let raw: Raw = Raw::deserialize(value).ok()?;
        (raw.schema_version == 1).then_some(Self {
            schema_version: 1,
            dialect: raw.dialect,
            declared_thinking: raw.declared_thinking,
            caller: raw.caller,
            normalized_ingress: raw.normalized_ingress,
            effective_request: raw.effective_request,
            wire: raw.wire,
        })
    }
}
impl TryFrom<Value> for WireModeSnapshot {
    type Error = &'static str;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        Self::project(&value).ok_or("invalid bounded wire-mode observation")
    }
}
fn merge(left: ReasoningMode, right: ReasoningMode) -> ReasoningMode {
    use ReasoningMode::*;
    match (left, right) {
        (Absent, x) | (x, Absent) => x,
        (Unknown, _) | (_, Unknown) => Unknown,
        (x, y) if x == y => x,
        _ => Mixed,
    }
}
fn effort(value: Option<&Value>) -> ReasoningMode {
    use ReasoningMode::*;
    match value {
        None => Absent,
        Some(Value::String(s)) => match s.as_str() {
            "none" => Disabled,
            "minimal" | "low" | "medium" | "high" | "xhigh" => Enabled,
            "auto" => Adaptive,
            _ => Unknown,
        },
        _ => Unknown,
    }
}
fn reasoning(value: Option<&Value>) -> ReasoningMode {
    use ReasoningMode::*;
    match value {
        None => Absent,
        Some(Value::Bool(false)) => Disabled,
        Some(Value::Bool(true)) => Enabled,
        Some(Value::Object(object)) => {
            let by_type = match object.get("type") {
                None => Absent,
                Some(Value::String(s)) => match s.as_str() {
                    "disabled" => Disabled,
                    "enabled" => Enabled,
                    "adaptive" => Adaptive,
                    _ => Unknown,
                },
                _ => Unknown,
            };
            let combined = merge(by_type, effort(object.get("effort")));
            if combined == Absent {
                Unknown
            } else {
                combined
            }
        }
        _ => Unknown,
    }
}
fn tools(value: Option<&Value>) -> ToolsMode {
    match value {
        None => ToolsMode::Absent,
        Some(Value::Array(a)) if a.is_empty() => ToolsMode::Empty,
        Some(Value::Array(_)) => ToolsMode::Configured,
        _ => ToolsMode::Unknown,
    }
}
fn choice(value: Option<&Value>) -> ToolChoiceMode {
    use ToolChoiceMode::*;
    match value {
        std::option::Option::None => Absent,
        Some(Value::String(s)) => match s.as_str() {
            "none" => None,
            "auto" => Auto,
            "required" => Required,
            _ => Unknown,
        },
        Some(Value::Object(o)) => match o.get("type").and_then(Value::as_str) {
            Some("none") => None,
            Some("auto") => Auto,
            Some("any") => Required,
            Some("function")
                if o.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                    || o.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty()) =>
            {
                Named
            }
            Some("tool")
                if o.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty()) =>
            {
                Named
            }
            _ => Unknown,
        },
        _ => Unknown,
    }
}
impl ControlSet {
    pub(crate) fn from_request(req: &ChatRequest) -> Self {
        Self {
            reasoning: merge(
                merge(
                    reasoning(req.thinking.as_ref()),
                    reasoning(req.extra.get("reasoning")),
                ),
                effort(req.extra.get("reasoning_effort")),
            ),
            tools: tools(req.tools.as_ref()),
            tool_choice: choice(req.tool_choice.as_ref()),
        }
    }
    fn from_wire(body: &Value, dialect: Dialect) -> Self {
        let mut mode = merge(
            merge(
                reasoning(body.get("thinking")),
                reasoning(body.get("reasoning")),
            ),
            effort(body.get("reasoning_effort")),
        );
        if dialect == Dialect::Ollama {
            mode = merge(mode, reasoning(body.get("think")));
        }
        if dialect == Dialect::Gemini
            && body
                .get("generationConfig")
                .and_then(|v| v.get("thinkingConfig"))
                .is_some()
        {
            // Presence is known; do not guess budget/level semantics.
            mode = merge(mode, ReasoningMode::Unknown);
        }
        let tool_choice = if dialect == Dialect::Gemini {
            match body.get("toolConfig") {
                None => ToolChoiceMode::Absent,
                Some(v) => match v
                    .get("functionCallingConfig")
                    .and_then(|v| v.get("mode"))
                    .and_then(Value::as_str)
                {
                    Some("AUTO") => ToolChoiceMode::Auto,
                    Some("NONE") => ToolChoiceMode::None,
                    Some("ANY") => {
                        match v
                            .get("functionCallingConfig")
                            .and_then(|v| v.get("allowedFunctionNames"))
                        {
                            None => ToolChoiceMode::Required,
                            Some(Value::Array(names))
                                if !names.is_empty()
                                    && names.iter().all(|n| {
                                        n.as_str().is_some_and(|n| !n.trim().is_empty())
                                    }) =>
                            {
                                ToolChoiceMode::Named
                            }
                            Some(_) => ToolChoiceMode::Unknown,
                        }
                    }
                    _ => ToolChoiceMode::Unknown,
                },
            }
        } else {
            choice(body.get("tool_choice"))
        };
        Self {
            reasoning: mode,
            tools: tools(body.get("tools")),
            tool_choice,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn gemini_named_choice_requires_recognizable_nonempty_names() {
        for names in [
            Value::Null,
            json!(42),
            json!([]),
            json!([""]),
            json!([15]),
            json!(["owned", ""]),
        ] {
            let controls = ControlSet::from_wire(
                &json!({"toolConfig":{"functionCallingConfig":{"mode":"ANY","allowedFunctionNames":names}}}),
                Dialect::Gemini,
            );
            assert_eq!(
                controls.tool_choice,
                ToolChoiceMode::Unknown,
                "no valid named-tool evidence: {names}"
            );
        }
        let named = ControlSet::from_wire(
            &json!({"toolConfig":{"functionCallingConfig":{"mode":"ANY","allowedFunctionNames":["owned"]}}}),
            Dialect::Gemini,
        );
        assert_eq!(named.tool_choice, ToolChoiceMode::Named);
    }
    #[test]
    fn absent_disabled_unknown_and_conflicting_controls_are_distinct() {
        for (body, expected) in [
            (json!({}), ReasoningMode::Absent),
            (
                json!({"thinking":{"type":"disabled"}}),
                ReasoningMode::Disabled,
            ),
            (json!({"reasoning_effort":"high"}), ReasoningMode::Enabled),
            (
                json!({"thinking":{"type":"adaptive"}}),
                ReasoningMode::Adaptive,
            ),
            (json!({"thinking":null}), ReasoningMode::Unknown),
            (
                json!({"thinking":{"budget_tokens":0}}),
                ReasoningMode::Unknown,
            ),
            (
                json!({"thinking":{"type":"disabled"},"reasoning_effort":"high"}),
                ReasoningMode::Mixed,
            ),
        ] {
            assert_eq!(
                ControlSet::from_wire(&body, Dialect::OpenAI).reasoning,
                expected
            );
        }
    }
    #[test]
    fn tools_and_choices_record_only_fixed_tags() {
        let body = json!({"tools":[{"private-name":"private-schema"}],"tool_choice":{"type":"function","function":{"name":"private-tool"}}});
        let value = ControlSet::from_wire(&body, Dialect::OpenAI);
        assert_eq!(value.tools, ToolsMode::Configured);
        assert_eq!(value.tool_choice, ToolChoiceMode::Named);
        assert!(!serde_json::to_string(&value).unwrap().contains("private"));
        assert_eq!(tools(Some(&json!([]))), ToolsMode::Empty);
        assert_eq!(tools(Some(&Value::Null)), ToolsMode::Unknown);
    }
    #[test]
    fn strict_projection_and_direct_deserialize_reject_unknown_or_private_fields() {
        let controls = json!({"reasoning":"absent","tools":"empty","tool_choice":"none"});
        let good = json!({"schema_version":1,"dialect":"openai","declared_thinking":null,"caller":null,"normalized_ingress":null,"effective_request":controls,"wire":controls});
        assert_eq!(
            serde_json::to_value(WireModeSnapshot::project(&good).unwrap()).unwrap(),
            good
        );
        for patch in [
            json!({"schema_version":2}),
            json!({"private_body":"secret"}),
            json!({"dialect":"future"}),
            json!({"declared_thinking":0}),
            json!({"wire":{"reasoning":"absent","tools":"configured","tool_choice":"auto","private_name":"secret"}}),
        ] {
            let mut bad = good.clone();
            bad.as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(WireModeSnapshot::project(&bad).is_none());
            assert!(serde_json::from_value::<WireModeSnapshot>(bad).is_err());
        }
        let mut missing = good;
        missing
            .as_object_mut()
            .unwrap()
            .remove("normalized_ingress");
        assert!(WireModeSnapshot::project(&missing).is_none());
    }
    #[test]
    fn dialect_specific_presence_does_not_guess_thinking_budgets() {
        let gemini = ControlSet::from_wire(
            &json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":0}},"toolConfig":{"functionCallingConfig":{"mode":"ANY"}},"tools":[]}),
            Dialect::Gemini,
        );
        assert_eq!(gemini.reasoning, ReasoningMode::Unknown);
        assert_eq!(gemini.tool_choice, ToolChoiceMode::Required);
        let ollama = ControlSet::from_wire(&json!({"think":false}), Dialect::Ollama);
        assert_eq!(ollama.reasoning, ReasoningMode::Disabled);
    }
}

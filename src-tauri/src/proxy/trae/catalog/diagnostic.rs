//! Closed, value-free projection diagnostics. Never retain an upstream Value.
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Location {
    RootType,
    ListType,
    EntryType,
    ConfigName,
    EntryPromptMaxTokens,
    EntryMaxTokens,
    DetailsType,
    DetailsCount,
    DetailType,
    DetailPromptMaxTokens,
    DetailMaxTokens,
    DisplayType,
    DisplayName,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Reason {
    ExpectedObject,
    ExpectedArray,
    InvalidName,
    InvalidLimit,
    TooManyDetails,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Missing,
    Null,
    Boolean,
    Number,
    String,
    Array,
    Object,
}
impl Kind {
    fn of(raw: Option<&Value>) -> Self {
        match raw {
            None => Self::Missing,
            Some(Value::Null) => Self::Null,
            Some(Value::Bool(_)) => Self::Boolean,
            Some(Value::Number(_)) => Self::Number,
            Some(Value::String(_)) => Self::String,
            Some(Value::Array(_)) => Self::Array,
            Some(Value::Object(_)) => Self::Object,
        }
    }
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Class {
    Missing,
    Null,
    WrongType,
    Empty,
    Oversize,
    Control,
    Unsafe,
    Zero,
    Negative,
    NotUint,
    OverLimit,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SelectorConflictReason {
    ViewMismatch,
    PreviousNotSelectable,
    CurrentExcluded,
    CurrentInvalid,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SelectorField {
    DisplayName,
    PromptMaxTokens,
    MaxTokens,
    DeclaredVision,
    DeclaredTools,
    DeclaredThinking,
    ReasoningEffortOptions,
    ProtocolCarriers,
    AccountModelEvidence,
}
pub(super) struct Failure {
    location: Location,
    observed_kind: Kind,
    value_class: Class,
}
pub(super) struct SelectorConflictFailure {
    reason: SelectorConflictReason,
    fields: Vec<SelectorField>,
}
pub(super) enum Error {
    Shape(Failure),
    Public(&'static str),
    SelectorConflict(SelectorConflictFailure),
}
impl From<&'static str> for Error {
    fn from(value: &'static str) -> Self {
        Self::Public(value)
    }
}
impl Error {
    pub(super) fn shape(location: Location, raw: Option<&Value>, class: Class) -> Self {
        Self::Shape(Failure {
            location,
            observed_kind: Kind::of(raw),
            value_class: class,
        })
    }
    pub(super) fn wrong_type(location: Location, raw: Option<&Value>) -> Self {
        Self::shape(
            location,
            raw,
            match raw {
                None => Class::Missing,
                Some(Value::Null) => Class::Null,
                _ => Class::WrongType,
            },
        )
    }
    pub(super) fn selector_conflict(
        reason: SelectorConflictReason,
        fields: Vec<SelectorField>,
    ) -> Self {
        Self::SelectorConflict(SelectorConflictFailure { reason, fields })
    }
    pub(super) fn into_public(self, packet: &Value) -> String {
        match self {
            Self::Public(code) => code.into(),
            Self::Shape(failure) => {
                failure.emit(packet);
                super::INVALID.into()
            }
            Self::SelectorConflict(failure) => {
                failure.emit();
                "native_catalog_selector_conflict".into()
            }
        }
    }
}
#[derive(Serialize)]
struct Summary {
    version: u8,
    reason: Reason,
    location: Location,
    observed_kind: Kind,
    value_class: Class,
    root_kind: Kind,
    root_list_kind: Kind,
    data_kind: Kind,
    data_list_kind: Kind,
    result_kind: Kind,
    result_list_kind: Kind,
}
impl Failure {
    fn emit(self, packet: &Value) {
        let reason = match self.location {
            Location::RootType
            | Location::EntryType
            | Location::DetailType
            | Location::DisplayType => Reason::ExpectedObject,
            Location::ListType | Location::DetailsType => Reason::ExpectedArray,
            Location::ConfigName | Location::DisplayName => Reason::InvalidName,
            Location::EntryPromptMaxTokens
            | Location::EntryMaxTokens
            | Location::DetailPromptMaxTokens
            | Location::DetailMaxTokens => Reason::InvalidLimit,
            Location::DetailsCount => Reason::TooManyDetails,
        };
        let summary = Summary {
            version: 1,
            reason,
            location: self.location,
            observed_kind: self.observed_kind,
            value_class: self.value_class,
            root_kind: Kind::of(Some(packet)),
            root_list_kind: Kind::of(packet.get("config_info_list")),
            data_kind: Kind::of(packet.get("data")),
            data_list_kind: Kind::of(packet.get("data").and_then(|v| v.get("config_info_list"))),
            result_kind: Kind::of(packet.get("Result")),
            result_list_kind: Kind::of(
                packet.get("Result").and_then(|v| v.get("config_info_list")),
            ),
        };
        // Exactly eleven fixed fields; all strings are enum literals. No raw
        // value, dynamic path, identifier, length, Debug output or parent span.
        if let Ok(summary) = serde_json::to_string(&summary) {
            tracing::info!(target: "llm_gateway::native_catalog_projection", parent: None,
                marker = "native_catalog_projection_v1", summary = %summary);
        }
    }
}
#[derive(Serialize)]
struct SelectorConflictSummary {
    version: u8,
    reason: SelectorConflictReason,
    fields: Vec<SelectorField>,
}
impl SelectorConflictFailure {
    fn emit(self) {
        let summary = SelectorConflictSummary {
            version: 1,
            reason: self.reason,
            fields: self.fields,
        };
        // The summary is deliberately independent of the upstream packet and
        // contains only fixed enum literals. Never add a config name, hash,
        // value, length, URL, or dynamic JSON path here.
        if let Ok(summary) = serde_json::to_string(&summary) {
            tracing::info!(target: "llm_gateway::native_catalog_projection", parent: None,
                marker = "native_catalog_selector_conflict_v1", summary = %summary);
        }
    }
}

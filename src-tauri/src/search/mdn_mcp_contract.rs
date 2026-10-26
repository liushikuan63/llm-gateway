//! Fixed-purpose, non-networking MDN MCP message contract.
//!
//! This is not a transport or a registered search source. Request descriptions
//! are plans, not dispatch receipts; response status/session inputs must be
//! supplied by a future transport. This JSON-only profile does not claim general
//! Streamable HTTP/SSE support or a verified deployed service version.
//! Public contract and privacy boundary: docs/wiki/search.md (MDN MCP section).
//! The experimental service's analytics opt-out is not a no-retention promise.
//! Tool text is opaque untrusted reference material: its concatenated document
//! format cannot establish record boundaries, titles, paths or document counts.
use serde::{
    de::{value::MapAccessDeserializer, DeserializeOwned, IgnoredAny, MapAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{json, value::RawValue, Value};
use std::fmt;
use std::marker::PhantomData;

pub const ENDPOINT: &str = "https://mcp.mdn.mozilla.net/";
pub const PROTOCOL_VERSION: &str = "2025-03-26";
// Local acceptance bounds, not supplier limits or cost/quota guarantees.
pub const MAX_QUERY_CHARS: usize = 256;
pub const MAX_QUERY_BYTES: usize = 1024;
pub const MAX_REPLY_BYTES: usize = 256 * 1024;
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
const HEADERS: [(&str, &str); 4] = [
    ("Content-Type", "application/json"),
    ("Accept", "application/json, text/event-stream"),
    ("MCP-Protocol-Version", PROTOCOL_VERSION),
    ("X-Moz-1st-Party-Data-Opt-Out", "1"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Created,
    AwaitInitialize,
    ReadyNotify,
    AwaitNotify,
    ReadySearch,
    AwaitSearch,
    Complete,
    Blocked,
}

/// Fixed categories only. Never attach upstream error text, query or session.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    #[error("MDN query contract input invalid")]
    InvalidQuery,
    #[error("MDN contract sequence invalid")]
    InvalidSequence,
    #[error("MDN JSON message envelope invalid or unsupported")]
    InvalidEnvelope,
    #[error("MDN protocol capability or version unsupported")]
    UnsupportedProtocol,
    #[error("MDN session-bearing response unsupported by stateless profile")]
    UnsupportedSession,
    #[error("MDN supplied response exceeds local size bound")]
    ResponseTooLarge,
    #[error("MDN supplied initialized notification status rejected")]
    NotificationRejected,
    #[error("MDN remote tool returned an error")]
    RemoteToolError,
    #[error("MDN unstructured text violates local contract")]
    InvalidText,
}

/// No I/O, credential, executable, arbitrary host, method or tool configuration.
pub struct RequestDescription {
    body: Value,
}

impl RequestDescription {
    pub fn endpoint(&self) -> &'static str {
        ENDPOINT
    }
    pub fn http_method(&self) -> &'static str {
        "POST"
    }
    pub fn headers(&self) -> &'static [(&'static str, &'static str)] {
        &HEADERS
    }
    pub fn body(&self) -> &Value {
        &self.body
    }
}

impl fmt::Debug for RequestDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MdnRequestDescription")
            .field("endpoint", &ENDPOINT)
            .field("body", &"<redacted>")
            .finish()
    }
}

/// Not a list of normalized hits; no inferred URLs, quality scores or counts.
pub struct TextReference {
    text: String,
}

impl TextReference {
    pub fn untrusted_text(&self) -> &str {
        &self.text
    }
    pub fn document_count(&self) -> Option<u32> {
        None
    }
}

impl fmt::Debug for TextReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MdnTextReference")
            .field("text", &"<redacted>")
            .field("document_count", &None::<u32>)
            .finish()
    }
}

pub struct Contract {
    query: String,
    phase: Phase,
}

impl fmt::Debug for Contract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MdnContract")
            .field("phase", &self.phase)
            .finish()
    }
}

impl Contract {
    /// Validates an already-parsed value. Original duplicate input keys cannot
    /// be recovered from Value and must not be claimed as checked here.
    pub fn new(input: &Value) -> Result<Self, Error> {
        let object = input.as_object().ok_or(Error::InvalidQuery)?;
        let query = object
            .get("query")
            .and_then(Value::as_str)
            .ok_or(Error::InvalidQuery)?;
        if object.len() != 1
            || query.trim().is_empty()
            || query.len() > MAX_QUERY_BYTES
            || query.chars().count() > MAX_QUERY_CHARS
            || query.chars().any(char::is_control)
        {
            return Err(Error::InvalidQuery);
        }
        Ok(Self {
            query: query.to_owned(),
            phase: Phase::Created,
        })
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    fn blocked<T>(&mut self, error: Error) -> Result<T, Error> {
        self.phase = Phase::Blocked;
        Err(error)
    }

    fn require(&mut self, expected: Phase) -> Result<(), Error> {
        if self.phase == expected {
            Ok(())
        } else {
            self.blocked(Error::InvalidSequence)
        }
    }

    pub fn initialize_request(&mut self) -> Result<RequestDescription, Error> {
        self.require(Phase::Created)?;
        self.phase = Phase::AwaitInitialize;
        Ok(RequestDescription {
            body: json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":PROTOCOL_VERSION,"capabilities":{},
                "clientInfo":{"name":"LLM-Gateway-MDN-Contract","version":env!("CARGO_PKG_VERSION")}
            }}),
        })
    }

    /// JSON-profile bytes and an externally observed session header, if any.
    /// This codec does not inspect a connection or verify an HTTP response.
    pub fn accept_initialize_json(
        &mut self,
        bytes: &[u8],
        supplied_session: Option<&str>,
    ) -> Result<(), Error> {
        self.require(Phase::AwaitInitialize)?;
        if supplied_session.is_some() {
            return self.blocked(Error::UnsupportedSession);
        }
        let init: InitializeResult = match decode_result(bytes, 1) {
            Ok(value) => value,
            Err(error) => return self.blocked(error),
        };
        if init.protocol_version != PROTOCOL_VERSION
            || !bounded_identity(&init.server_info.0.name)
            || !bounded_identity(&init.server_info.0.version)
        {
            return self.blocked(Error::UnsupportedProtocol);
        }
        // Instructions and other advertised capabilities are never executed.
        self.phase = Phase::ReadyNotify;
        Ok(())
    }

    pub fn initialized_notification(&mut self) -> Result<RequestDescription, Error> {
        self.require(Phase::ReadyNotify)?;
        self.phase = Phase::AwaitNotify;
        Ok(RequestDescription {
            body: json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        })
    }

    pub fn accept_initialized_status(&mut self, status: u16, body: &[u8]) -> Result<(), Error> {
        self.require(Phase::AwaitNotify)?;
        if status != 202 || !body.is_empty() {
            return self.blocked(Error::NotificationRejected);
        }
        self.phase = Phase::ReadySearch;
        Ok(())
    }

    pub fn search_request(&mut self) -> Result<RequestDescription, Error> {
        self.require(Phase::ReadySearch)?;
        self.phase = Phase::AwaitSearch;
        Ok(RequestDescription {
            body: json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"search","arguments":{"query":self.query}}}),
        })
    }

    pub fn accept_search_json(
        &mut self,
        bytes: &[u8],
        supplied_session: Option<&str>,
    ) -> Result<TextReference, Error> {
        self.require(Phase::AwaitSearch)?;
        if supplied_session.is_some() {
            return self.blocked(Error::UnsupportedSession);
        }
        let result: ToolResult = match decode_result(bytes, 2) {
            Ok(value) => value,
            Err(error) => return self.blocked(error),
        };
        if matches!(result.is_error, Present::Value(true)) {
            return self.blocked(Error::RemoteToolError);
        }
        if result.content.len() != 1 {
            return self.blocked(Error::InvalidText);
        }
        let Some(Object(block)) = result.content.into_iter().next() else {
            return self.blocked(Error::InvalidText);
        };
        if block.kind != "text"
            || block.text.trim().is_empty()
            || block.text.len() > MAX_TEXT_BYTES
            || block
                .text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return self.blocked(Error::InvalidText);
        }
        self.phase = Phase::Complete;
        Ok(TextReference { text: block.text })
    }
}

fn bounded_identity(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

// Preserve presence, including explicit null. RawValue prevents silently
// overwriting duplicate named tool-result keys before typed deserialization.
#[derive(Default)]
enum Present<T> {
    #[default]
    Missing,
    Value(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Present<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::Value)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    jsonrpc: String,
    id: u64,
    #[serde(default)]
    result: Present<Box<RawValue>>,
    #[serde(default)]
    error: Present<Box<RawValue>>,
}

fn decode_result<T: DeserializeOwned>(bytes: &[u8], id: u64) -> Result<T, Error> {
    if bytes.len() > MAX_REPLY_BYTES {
        return Err(Error::ResponseTooLarge);
    }
    let Object(value): Object<Envelope> =
        serde_json::from_slice(bytes).map_err(|_| Error::InvalidEnvelope)?;
    if value.jsonrpc != "2.0" || value.id != id {
        return Err(Error::InvalidEnvelope);
    }
    match (value.result, value.error) {
        (Present::Value(result), Present::Missing) => {
            serde_json::from_str::<Object<T>>(result.get())
                .map(|value| value.0)
                .map_err(|_| Error::InvalidEnvelope)
        }
        (Present::Missing, Present::Value(_)) => Err(Error::RemoteToolError),
        _ => Err(Error::InvalidEnvelope),
    }
}

// serde-derived structs accept positional sequences. JSON-RPC requires maps.
// Retain the original MapAccess so named-key duplication is rejected by the
// nested typed deserializer before any conversion into a generic Value.
struct Object<T>(T);
struct ObjectVisitor<T>(PhantomData<T>);
impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
    type Value = Object<T>;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        T::deserialize(MapAccessDeserializer::new(map)).map(Object)
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ObjectVisitor::<T>(PhantomData))
    }
}

// Only the tools capability is consumed; reject duplicate/non-object tools
// before losing duplicate information. Unknown capabilities grant no actions.
struct Capabilities;
struct CapabilitiesVisitor;
impl<'de> Visitor<'de> for CapabilitiesVisitor {
    type Value = Capabilities;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a capabilities object with one tools object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut seen_tools = false;
        while let Some(key) = map.next_key::<String>()? {
            if key == "tools" {
                if seen_tools {
                    return Err(serde::de::Error::duplicate_field("tools"));
                }
                map.next_value::<Object<IgnoredAny>>()?;
                seen_tools = true;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        if !seen_tools {
            return Err(serde::de::Error::missing_field("tools"));
        }
        Ok(Capabilities)
    }
}
impl<'de> Deserialize<'de> for Capabilities {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(CapabilitiesVisitor)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    #[serde(rename = "capabilities")]
    _capabilities: Capabilities,
    #[serde(rename = "serverInfo")]
    server_info: Object<ServerInfo>,
    #[serde(default, rename = "instructions")]
    _instructions: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerInfo {
    name: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolResult {
    content: Vec<Object<TextBlock>>,
    #[serde(default, rename = "isError")]
    is_error: Present<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextBlock {
    #[serde(rename = "type")]
    kind: String,
    text: String,
}

#[cfg(test)]
mod tests {
    include!("mdn_mcp_contract_tests.rs");
}

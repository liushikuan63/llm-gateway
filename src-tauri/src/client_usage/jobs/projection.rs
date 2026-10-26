//! Streaming whitelist extraction. Unknown JSON values use serde's IgnoredAny,
//! so even a multi-gigabyte message string is never materialized.
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::{self, BufRead, Read},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy)]
enum Node {
    Root,
    Payload,
    Info,
    Usage,
    Details,
    Limits,
    Settings,
    Source,
    Subagent,
    Spawn,
    Data,
    Ledger,
    Days,
    Providers,
    Models,
    LedgerUsage,
    Scalar,
    RequestId,
    Message,
    QoderUsage,
    Boolean,
    MessageModel,
}
struct Seed(Node);
impl<'de> DeserializeSeed<'de> for Seed {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("usage projection")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Value, M::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if let Some(node) = child(self.0, &key) {
                let value = map.next_value_seed(Seed(node))?;
                // Identifiers and source labels are bounded and sanitized at this
                // boundary. Untrusted labels never become scratch conversation text.
                let value = if matches!(node, Node::Scalar) {
                    scalar(&key, value)
                } else {
                    value
                };
                let key = if matches!(self.0, Node::Providers | Node::Models)
                    && super::super::name(Some(&key)) != key
                {
                    format!("!{:x}", Sha256::digest(key.as_bytes()))
                } else {
                    key
                };
                out.insert(key, value);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Value::Object(out))
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Value, S::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(if matches!(self.0, Node::Boolean) {
            Value::Null
        } else {
            Value::Bool(true)
        })
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(match self.0 {
            Node::Boolean => Value::Null,
            Node::MessageModel => Value::String(if super::super::name(Some(v)) == v {
                v.into()
            } else {
                format!("!{:x}", Sha256::digest(v.as_bytes()))
            }),
            // Empty identifiers retain their absence semantics and allow the
            // scanner's message fallback; hashing them would invent an identity.
            Node::RequestId if v.is_empty() => Value::String(String::new()),
            Node::RequestId => Value::String(format!("id-{:x}", Sha256::digest(v.as_bytes()))),
            Node::Scalar if v.len() <= 512 => Value::String(v.into()),
            Node::Scalar => Value::String("!oversized!".into()),
            _ => Value::Bool(true),
        })
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Value, E> {
        self.visit_str(&v)
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Value, E> {
        Ok(serde_json::Number::from_f64(v)
            .map(Value::Number)
            .unwrap_or(Value::Null))
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
}
fn scalar(key: &str, value: Value) -> Value {
    if let Value::String(ref raw) = value {
        if matches!(key, "timestamp" | "ts") {
            return if chrono::DateTime::parse_from_rfc3339(raw).is_ok() {
                value
            } else {
                Value::Bool(true)
            };
        }
        if matches!(key, "model" | "model_name" | "model_provider") {
            return Value::String(super::super::name(Some(raw)));
        }
        if key == "type" {
            return if matches!(
                raw.as_str(),
                "session_meta"
                    | "turn_context"
                    | "event_msg"
                    | "token_usage_record"
                    | "token_count"
                    | "thread_settings_applied"
                    | "model.response.completed"
                    | "assistant"
            ) {
                value
            } else {
                Value::Null
            };
        }
        if matches!(
            key,
            "id" | "thread_id"
                | "turn_id"
                | "response_id"
                | "limit_id"
                | "forked_from_id"
                | "parent_thread_id"
        ) {
            return if super::super::replay::identity(&value).is_some() {
                value
            } else {
                Value::Bool(true)
            };
        }
        return Value::Bool(true);
    }
    value
}
fn child(node: Node, key: &str) -> Option<Node> {
    use Node::*;
    Some(match (node, key) {
        (Root, "type" | "timestamp" | "ts" | "seq") => Scalar,
        (Root, "request_id") => RequestId,
        (Root, "payload") => Payload,
        (Root, "data") => Data,
        (Root, "message") => Message,
        (Message, "id") => RequestId,
        (Message, "model") => MessageModel,
        (Message, "usage") => QoderUsage,
        (QoderUsage, "request_id") => RequestId,
        (QoderUsage, "billable") => Boolean,
        (
            QoderUsage,
            "input_tokens"
            | "output_tokens"
            | "cache_read_input_tokens"
            | "cache_creation_input_tokens"
            | "credits",
        ) => Scalar,
        // Preserve only a digest for unsafe source labels, so display sanitization
        // cannot turn two contradictory Codex model/provider labels into agreement.
        (Payload, "model_provider" | "model" | "model_name") => MessageModel,
        (
            Payload,
            "id" | "forked_from_id" | "timestamp" | "thread_id" | "turn_id" | "response_id"
            | "type",
        ) => Scalar,
        (Payload, "source") => Source,
        (Payload, "info") => Info,
        (Payload, "usage" | "thread_token_usage") => Usage,
        (Payload, "rate_limits") => Limits,
        (Payload, "thread_settings") => Settings,
        (Info, "model" | "model_name") => MessageModel,
        (Info, "total_token_usage" | "last_token_usage") => Usage,
        (
            Usage,
            "input_tokens"
            | "output_tokens"
            | "cached_input_tokens"
            | "cache_write_input_tokens"
            | "reasoning_output_tokens"
            | "total_tokens",
        ) => Scalar,
        (Usage, "input_tokens_details") => Details,
        (Details, "cached_tokens" | "cache_write_tokens") => Scalar,
        (Limits, "limit_id") => Scalar,
        (Settings, "model") => MessageModel,
        (Source, "subagent") => Subagent,
        (Subagent, "thread_spawn") => Spawn,
        (Spawn, "parent_thread_id") => Scalar,
        (Data, "model" | "output_tokens") => Scalar,
        (Ledger, "version") => Scalar,
        (Ledger, "days") => Days,
        (Days, day) if chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok() => Providers,
        (Providers, _) => Models,
        (Models, _) => LedgerUsage,
        (
            LedgerUsage,
            "inputTokens" | "outputTokens" | "cacheReadTokens" | "cacheWriteTokens" | "calls",
        ) => Scalar,
        _ => return None,
    })
}
pub(super) fn record(reader: impl Read) -> Result<Value, serde_json::Error> {
    let mut d = serde_json::Deserializer::from_reader(io::BufReader::new(reader));
    let value = Seed(Node::Root).deserialize(&mut d)?;
    d.end()?;
    Ok(value)
}
pub(super) fn ledger(reader: impl Read) -> Result<Value, serde_json::Error> {
    let mut d = serde_json::Deserializer::from_reader(io::BufReader::new(reader));
    let value = Seed(Node::Ledger).deserialize(&mut d)?;
    d.end()?;
    Ok(value)
}

// serde_json's streaming reader asks for individual bytes. Hash them in sizeable
// blocks instead of issuing a digest update per byte; the backing file is buffered.
pub(super) struct HashReader<'a, R> {
    inner: &'a mut R,
    digest: &'a mut Sha256,
    pending: Vec<u8>,
    count: u64,
    cancel: &'a AtomicBool,
    validator: super::validation::Validator,
}
impl<'a, R> HashReader<'a, R> {
    pub(super) fn new(inner: &'a mut R, digest: &'a mut Sha256, cancel: &'a AtomicBool) -> Self {
        Self {
            inner,
            digest,
            pending: Vec::with_capacity(65536),
            count: 0,
            cancel,
            validator: super::validation::Validator::default(),
        }
    }
    pub(super) fn count(&self) -> u64 {
        self.count
    }
    pub(super) fn valid(&mut self) -> bool {
        self.validator.valid()
    }
    fn add(&mut self, bytes: &[u8]) {
        self.validator.feed(bytes);
        self.count += bytes.len() as u64;
        self.pending.extend_from_slice(bytes);
        if self.pending.len() >= 65536 {
            self.digest.update(&self.pending);
            self.pending.clear();
        }
    }
}
impl<R: Read> Read for HashReader<'_, R> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(io::Error::other("cancelled"));
        }
        let n = self.inner.read(b)?;
        self.add(&b[..n]);
        Ok(n)
    }
}
impl<R> Drop for HashReader<'_, R> {
    fn drop(&mut self) {
        self.digest.update(&self.pending);
    }
}
pub(super) struct LineReader<'a, R> {
    inner: HashReader<'a, R>,
    ended: bool,
    complete: bool,
}
impl<'a, R> LineReader<'a, R> {
    pub(super) fn new(inner: &'a mut R, digest: &'a mut Sha256, cancel: &'a AtomicBool) -> Self {
        Self {
            inner: HashReader::new(inner, digest, cancel),
            ended: false,
            complete: false,
        }
    }
    pub(super) fn count(&self) -> u64 {
        self.inner.count()
    }
    pub(super) fn complete(&self) -> bool {
        self.complete
    }
    pub(super) fn valid(&mut self) -> bool {
        self.inner.valid()
    }
}
impl<R: BufRead> Read for LineReader<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.ended || out.is_empty() {
            return Ok(0);
        }
        if self.inner.cancel.load(Ordering::Acquire) {
            return Err(io::Error::other("cancelled"));
        }
        let buffer = self.inner.inner.fill_buf()?;
        if buffer.is_empty() {
            self.ended = true;
            return Ok(0);
        }
        let boundary = buffer.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let n = boundary.unwrap_or(buffer.len()).min(out.len());
        out[..n].copy_from_slice(&buffer[..n]);
        self.inner.inner.consume(n);
        self.inner.add(&out[..n]);
        if boundary == Some(n) {
            self.complete = true;
            self.ended = true;
        }
        Ok(n)
    }
}

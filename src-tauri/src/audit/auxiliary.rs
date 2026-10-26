//! Bounded local invocation observations. No URLs, bodies, prices or credentials.
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Instant};

const MAX_CALLS: usize = 2;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Purpose {
    Classify,
    Refine,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Terminal {
    Started,
    Returned,
    InterruptedUnknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Counts {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Observation {
    ordinal: usize,
    purpose: Purpose,
    provider_id: Option<String>,
    model: Option<String>,
    target_unavailable: bool,
    started_at_unix_ms: i64,
    terminal: Terminal,
    elapsed_ms: Option<u64>,
    http_status: Option<u16>,
    primary_usage: Option<Counts>,
    result_ok: Option<bool>,
    applied: Option<bool>,
    wire_post_verified: bool,
}
impl Observation {
    fn valid(&self, index: usize) -> bool {
        self.ordinal == index
            && self.started_at_unix_ms >= 0
            && !self.wire_post_verified
            && self
                .provider_id
                .as_deref()
                .map_or(true, super::route_decision::identifier)
            && self
                .model
                .as_deref()
                .map_or(true, super::route_decision::identifier)
            && (self.target_unavailable
                || self.model.is_some()
                    && (self.purpose == Purpose::Classify || self.provider_id.is_some()))
            && self.http_status.map_or(true, |n| (100..=599).contains(&n))
            && self.primary_usage.as_ref().map_or(true, |u| {
                u.prompt_tokens.checked_add(u.completion_tokens) == Some(u.total_tokens)
            })
            && (self.purpose != Purpose::Classify
                || self.provider_id.is_none()
                    && self.primary_usage.is_none()
                    && self.applied.is_none())
            && (self.purpose != Purpose::Refine || self.result_ok.is_none())
            && match self.terminal {
                Terminal::Started => {
                    self.elapsed_ms.is_none() && self.result_ok.is_none() && self.applied.is_none()
                }
                Terminal::Returned => self.elapsed_ms.is_some(),
                Terminal::InterruptedUnknown => {
                    self.elapsed_ms.is_some() && self.result_ok.is_none() && self.applied.is_none()
                }
            }
    }
}
pub(crate) fn valid(calls: &[Observation], omitted: u32) -> bool {
    calls.len() <= MAX_CALLS
        && (omitted == 0 || calls.len() == MAX_CALLS)
        && calls.iter().enumerate().all(|(i, c)| c.valid(i))
}
#[derive(Debug, Default)]
struct Entries {
    calls: Vec<Observation>,
    omitted: u32,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct Journal(Arc<Mutex<Entries>>);
impl Journal {
    pub(crate) fn begin(
        &self,
        purpose: Purpose,
        provider: Option<&str>,
        model: &str,
    ) -> Option<Call> {
        let mut entries = self.0.lock();
        if entries.calls.len() >= MAX_CALLS {
            entries.omitted = entries.omitted.saturating_add(1);
            return None;
        }
        let model = Some(model)
            .filter(|v| super::route_decision::identifier(v))
            .map(str::to_owned);
        let provider_id = provider
            .filter(|v| super::route_decision::identifier(v))
            .map(str::to_owned);
        let ordinal = entries.calls.len();
        entries.calls.push(Observation {
            ordinal,
            purpose,
            target_unavailable: model.is_none()
                || purpose == Purpose::Refine && provider_id.is_none(),
            provider_id,
            model,
            started_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            terminal: Terminal::Started,
            elapsed_ms: None,
            http_status: None,
            primary_usage: None,
            result_ok: None,
            applied: None,
            wire_post_verified: false,
        });
        Some(Call {
            journal: self.clone(),
            index: ordinal,
            started: Instant::now(),
            finished: false,
        })
    }
    pub(crate) fn snapshot(&self) -> (Vec<Observation>, u32) {
        let v = self.0.lock();
        (v.calls.clone(), v.omitted)
    }
}
pub(crate) struct Call {
    journal: Journal,
    index: usize,
    started: Instant,
    finished: bool,
}
impl Call {
    pub(crate) fn headers(&mut self, status: u16) {
        self.journal.0.lock().calls[self.index].http_status = Some(status);
    }
    pub(crate) fn usage(&mut self, usage: Option<&crate::domain::Usage>) {
        self.journal.0.lock().calls[self.index].primary_usage = usage
            .filter(|u| u.prompt_tokens.checked_add(u.completion_tokens) == Some(u.total_tokens))
            .map(|u| Counts {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            });
    }
    pub(crate) fn returned(&mut self, result_ok: Option<bool>, applied: Option<bool>) {
        let mut entries = self.journal.0.lock();
        let call = &mut entries.calls[self.index];
        call.terminal = Terminal::Returned;
        call.result_ok = result_ok;
        call.applied = applied;
        call.elapsed_ms = Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        self.finished = true;
    }
}
impl Drop for Call {
    fn drop(&mut self) {
        if !self.finished {
            let mut entries = self.journal.0.lock();
            let call = &mut entries.calls[self.index];
            call.terminal = Terminal::InterruptedUnknown;
            call.elapsed_ms = Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_invocations_preserve_return_interruption_and_unknown_usage() {
        let journal = Journal::default();
        let mut a = journal
            .begin(Purpose::Classify, None, "owned-classifier")
            .unwrap();
        a.headers(200);
        a.returned(Some(true), None);
        let mut b = journal
            .begin(Purpose::Refine, Some("owned"), "physical")
            .unwrap();
        b.headers(200);
        drop(b);
        assert!(journal
            .begin(Purpose::Refine, Some("owned"), "extra")
            .is_none());
        let (calls, omitted) = journal.snapshot();
        assert!(valid(&calls, omitted));
        assert_eq!(omitted, 1);
        assert_eq!(calls[0].terminal, Terminal::Returned);
        assert_eq!(calls[1].terminal, Terminal::InterruptedUnknown);
        assert!(calls[1].primary_usage.is_none());
        assert!(!calls[0].wire_post_verified);
        let known = Journal::default();
        let mut call = known
            .begin(Purpose::Refine, Some("owned"), "physical")
            .unwrap();
        call.usage(Some(&crate::domain::Usage::default()));
        drop(call);
        let (calls, omitted) = known.snapshot();
        assert!(valid(&calls, omitted));
        assert_eq!(calls[0].primary_usage.as_ref().unwrap().total_tokens, 0);
        assert_eq!(calls[0].terminal, Terminal::InterruptedUnknown);
    }
}

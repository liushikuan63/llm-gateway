//! WorkBuddy CN personal consumption wire projection, not money or an entitlement receipt.
//! Authentication is intentionally separate: the public website proves Cookie auth, not
//! the native VSCode token audience. No network, login, persistence or budget side effects.
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;

const INVALID: &str = "个人积分明细响应缺失、无效或超出本次查询边界";
const PAGE_SIZE: usize = 10;
const BODY_LIMIT: usize = 512 * 1024;

pub const PERSONAL_USAGE_ORIGIN: &str = "https://www.codebuddy.cn";
pub const PERSONAL_USAGE_PATH: &str = "/billing/meter/get-user-request-usage";

#[derive(Debug, Clone)]
pub struct PersonalUsageQuery {
    start: String,
    end: String,
    page: u32,
}

#[derive(Debug, Serialize)]
pub struct PersonalUsageRecord {
    pub request_id: String,
    pub credit: Option<String>,
    pub model: Option<String>,
    pub client: Option<String>,
    pub request_time: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PersonalUsagePage {
    pub records: Vec<PersonalUsageRecord>,
    pub total: u64,
    pub page: u32,
    pub has_more: bool,
    pub duplicate_request_ids: Vec<String>,
    pub unit: &'static str,
    pub timezone: Option<&'static str>,
    pub final_settlement_verified: bool,
}

// Deserialize only the allowlist. In particular, input/inputTrunc are skipped by
// serde rather than retained in a Value or echoed through an error/Debug message.
#[derive(Deserialize)]
struct Envelope {
    code: Option<i64>,
    data: PageWire,
}

#[derive(Deserialize)]
struct PageWire {
    data: Vec<RecordWire>,
    total: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordWire {
    request_id: String,
    credit: Option<Box<RawValue>>,
    model: Option<String>,
    client: Option<String>,
    request_time: Option<String>,
}

fn bounded_text(text: &str, max: usize) -> bool {
    text.len() <= max && !text.chars().any(char::is_control)
}

fn decimal_literal(text: &str) -> bool {
    // Keep exact supplier lexical precision (including negative observations).
    // A negative record is not automatically a verified refund or final revision.
    if text.is_empty() || text.len() > 96 {
        return false;
    }
    let text = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => (&text[..index], Some(&text[index + 1..])),
        None => (text, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (mantissa, None),
    };
    digits(whole)
        && fraction.map_or(true, digits)
        && exponent.map_or(true, |e| digits(e.strip_prefix(['+', '-']).unwrap_or(e)))
}

fn credit_literal(raw: Option<Box<RawValue>>) -> Result<Option<String>, &'static str> {
    let Some(raw) = raw else { return Ok(None) };
    let text = if raw.get().starts_with('"') {
        serde_json::from_str::<String>(raw.get()).map_err(|_| INVALID)?
    } else {
        raw.get().to_owned()
    };
    if !decimal_literal(&text) {
        return Err(INVALID);
    }
    Ok(Some(text))
}

impl PersonalUsageQuery {
    pub fn new(start: &str, end: &str, page: u32) -> Result<Self, &'static str> {
        let parse = |s: &str| {
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .filter(|date| date.format("%Y-%m-%d").to_string() == s)
        };
        let a = parse(start).ok_or("个人明细日期无效")?;
        let b = parse(end).ok_or("个人明细日期无效")?;
        // These are conservative local limits (at most 31 inclusive dates),
        // not a claim about undocumented server limits or the server timezone.
        if start < "2025-12-01" || !(0..31).contains(&(b - a).num_days()) || page == 0 {
            return Err("个人明细须使用2025-12-01起最多31天的日期范围及正页码");
        }
        Ok(Self {
            start: start.into(),
            end: end.into(),
            page,
        })
    }

    pub fn request_body(&self) -> Value {
        serde_json::json!({"startTime":format!("{} 00:00:00", self.start),"endTime":format!("{} 23:59:59", self.end),"pageNum":self.page,"pageSize":PAGE_SIZE})
    }

    pub fn project_page(&self, body: &[u8]) -> Result<PersonalUsagePage, &'static str> {
        if body.len() > BODY_LIMIT {
            return Err(INVALID);
        }
        let wire: Envelope = serde_json::from_slice(body).map_err(|_| INVALID)?;
        // The observed consumer requires this exact data shape. A known error
        // code cannot become an empty success; absence is not invented code=0.
        // Projection alone does not establish HTTP/auth/business authenticity.
        if wire.code.is_some_and(|code| code != 0) || wire.data.data.len() > PAGE_SIZE {
            return Err(INVALID);
        }
        let end = u64::from(self.page - 1) * PAGE_SIZE as u64 + wire.data.data.len() as u64;
        if !wire.data.data.is_empty() && wire.data.total < end {
            return Err(INVALID);
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut duplicates = std::collections::BTreeSet::new();
        let mut records = Vec::with_capacity(wire.data.data.len());
        for row in wire.data.data {
            if row.request_id.is_empty()
                || !bounded_text(&row.request_id, 256)
                || [&row.model, &row.client, &row.request_time]
                    .iter()
                    .any(|text| text.as_ref().is_some_and(|s| !bounded_text(s, 512)))
            {
                return Err(INVALID);
            }
            if !seen.insert(row.request_id.clone()) {
                duplicates.insert(row.request_id.clone());
            }
            records.push(PersonalUsageRecord {
                request_id: row.request_id,
                credit: credit_literal(row.credit)?,
                model: row.model,
                client: row.client,
                request_time: row.request_time,
            });
        }
        Ok(PersonalUsagePage {
            records,
            total: wire.data.total,
            page: self.page,
            has_more: end < wire.data.total,
            duplicate_request_ids: duplicates.into_iter().collect(),
            unit: "credits",
            timezone: None,
            final_settlement_verified: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn personal_query_uses_v1_and_exact_inclusive_date_bounds() {
        let query = PersonalUsageQuery::new("2026-10-01", "2026-10-31", 2).unwrap();
        assert_eq!(
            query.request_body(),
            json!({"startTime":"2026-10-01 00:00:00","endTime":"2026-10-31 23:59:59","pageNum":2,"pageSize":10})
        );
        for (a, b, p) in [
            ("2026-10-01", "2026-11-01", 1),
            ("2026-10-02", "2026-10-01", 1),
            ("2026-02-30", "2026-03-01", 1),
            ("2026-10-1", "2026-10-02", 1),
            ("2025-11-30", "2025-12-01", 1),
            ("2026-10-01", "2026-10-01", 0),
        ] {
            assert!(PersonalUsageQuery::new(a, b, p).is_err(), "{a} {b} {p}");
        }
    }

    #[test]
    fn exact_credits_unknown_and_prompt_projection_are_separate_from_cash() {
        let q = PersonalUsageQuery::new("2026-10-21", "2026-10-21", 1).unwrap();
        let p = q.project_page(br#"{"code":0,"data":{"data":[{"requestId":"owned-1","credit":0,"model":"glm-fixture","client":"VSCode","requestTime":"2026-10-21 12:00:00","input":"private-owned-prompt","inputTrunc":"private-owned-fragment"},{"requestId":"owned-2","credit":1.23000000000000000001},{"requestId":"owned-3","credit":"-0.0100"},{"requestId":"owned-4"}],"total":14}}"#).unwrap();
        assert_eq!(p.records[0].credit.as_deref(), Some("0"));
        assert_eq!(
            p.records[1].credit.as_deref(),
            Some("1.23000000000000000001")
        );
        assert_eq!(p.records[2].credit.as_deref(), Some("-0.0100"));
        assert!(p.records[3].credit.is_none());
        assert_eq!(p.unit, "credits");
        assert!(p.timezone.is_none());
        assert!(!p.final_settlement_verified);
        assert!(p.has_more);
        let wire = serde_json::to_string(&p).unwrap();
        assert!(!wire.contains("private-owned"));
        assert!(!wire.contains("input"));
        assert!(!wire.contains("currency"));
        assert!(!format!("{p:?}").contains("private-owned"));
    }

    #[test]
    fn explicit_empty_differs_from_malformed_and_duplicates_are_not_deduplicated() {
        let q = PersonalUsageQuery::new("2026-10-21", "2026-10-21", 1).unwrap();
        let p = q
            .project_page(br#"{"data":{"data":[],"total":0}}"#)
            .unwrap();
        assert!(p.records.is_empty());
        assert!(!p.has_more);
        let p = q.project_page(br#"{"data":{"data":[{"requestId":"same","credit":1},{"requestId":"same","credit":2}],"total":2}}"#).unwrap();
        assert_eq!(p.records.len(), 2);
        assert_eq!(p.duplicate_request_ids, vec!["same"]);
        for raw in [
            r#"{}"#,
            r#"{"code":401,"data":{"data":[],"total":0}}"#,
            r#"{"data":{"data":[]}}"#,
            r#"{"data":{"data":null,"total":0}}"#,
            r#"{"data":{"data":[{"requestId":"x","credit":"NaN"}],"total":1}}"#,
            r#"{"data":{"data":[{"requestId":"x","credit":true}],"total":1}}"#,
            r#"{"data":{"data":[{}],"total":1}}"#,
            r#"{"data":{"data":[{"requestId":"x"}],"total":0}}"#,
        ] {
            assert!(q.project_page(raw.as_bytes()).is_err(), "{raw}");
        }
    }

    #[test]
    fn response_limits_and_page_accounting_do_not_trigger_automatic_pagination() {
        let q = PersonalUsageQuery::new("2026-10-21", "2026-10-21", 2).unwrap();
        let p = q
            .project_page(br#"{"data":{"data":[],"total":30}}"#)
            .unwrap();
        assert_eq!(p.page, 2);
        assert!(p.has_more);
        assert!(q.project_page(&vec![b' '; 512 * 1024 + 1]).is_err());
        let rows: Vec<_> = (0..11)
            .map(|n| json!({"requestId":format!("owned-{n}")}))
            .collect();
        assert!(q
            .project_page(&serde_json::to_vec(&json!({"data":{"data":rows,"total":30}})).unwrap())
            .is_err());
        let raw =
            br#"{"data":{"data":[{"requestId":"owned","credit":"secret-invalid"}],"total":11}}"#;
        assert!(!q.project_page(raw).unwrap_err().contains("secret-invalid"));
    }
}

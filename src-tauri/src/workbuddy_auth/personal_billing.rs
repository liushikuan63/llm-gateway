//! Pure projection of the cached shared Intl personal billing response. No I/O.
//! Website rounding and default USD are not a monetary-unit contract. Never sum
//! these observations into budgets or infer payment/entitlement settlement.
use serde::{de::IgnoredAny, Deserialize, Deserializer, Serialize};
use serde_json::{value::RawValue, Value};

const INVALID: &str = "个人订单／发票响应无效或超出本地解析边界";
const BODY_LIMIT: usize = 512 * 1024;
const PAGE_SIZE: usize = 10;
const SUBORDER_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalBillingKind {
    Orders,
    Invoices,
}

pub struct PersonalBillingQuery {
    kind: PersonalBillingKind,
    page: u32,
}

#[derive(Debug, Serialize)]
pub struct PersonalBillingRecord {
    pub record_id: String,
    /// Response-local Data array index, not a stable supplier group ID.
    pub order_group: Option<usize>,
    pub amount_raw: Option<String>,
    /// Supplier label only. Missing currency stays unknown, including invoices.
    pub currency_declared: Option<String>,
    pub status_raw: Option<String>,
    pub time_raw: Option<String>,
    pub order_action_raw: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PersonalBillingPage {
    pub kind: PersonalBillingKind,
    pub page: u32,
    /// Supplier count; its relation to suborders is not established.
    pub server_total: u64,
    pub records: Vec<PersonalBillingRecord>,
    pub amount_unit_verified: bool,
    pub settlement_verified: bool,
}

// Local conservative rejection, not an invented server Error DTO. Even null
// error members reject. Known-field duplicates reject; unknown PII/URLs skip.
fn reject_error<'de, D: Deserializer<'de>>(d: D) -> Result<(), D::Error> {
    let _ = IgnoredAny::deserialize(d)?;
    Err(serde::de::Error::custom("unexpected error member"))
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "Response")]
    response: Option<Box<RawValue>>,
    data: Option<Nested>,
    code: Option<i64>,
    #[serde(
        rename = "Error",
        alias = "error",
        default,
        deserialize_with = "reject_error"
    )]
    _error: (),
}
#[derive(Deserialize)]
struct Nested {
    #[serde(rename = "Response")]
    response: Box<RawValue>,
    code: Option<i64>,
    #[serde(
        rename = "Error",
        alias = "error",
        default,
        deserialize_with = "reject_error"
    )]
    _error: (),
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Orders {
    data: Vec<OrderGroup>,
    total_count: Box<RawValue>,
    #[serde(
        rename = "Error",
        alias = "error",
        default,
        deserialize_with = "reject_error"
    )]
    _error: (),
}
#[derive(Deserialize)]
struct OrderGroup {
    #[serde(rename = "OrderInfos")]
    orders: Vec<Order>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Order {
    order_id: Box<RawValue>,
    real_total_cost: Option<Box<RawValue>>,
    currency: Option<String>,
    status: Option<Box<RawValue>>,
    create_time: Option<Box<RawValue>>,
    order_action: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Invoices {
    invoice_data: Vec<Invoice>,
    total_records: Box<RawValue>,
    #[serde(
        rename = "Error",
        alias = "error",
        default,
        deserialize_with = "reject_error"
    )]
    _error: (),
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Invoice {
    #[serde(rename = "IMSApplyId")]
    id: Box<RawValue>,
    amount: Option<Box<RawValue>>,
    status: Option<Box<RawValue>>,
    apply_time: Option<Box<RawValue>>,
}

fn bounded(text: &str, limit: usize) -> bool {
    text.len() <= limit && !text.chars().any(char::is_control)
}
fn decimal(text: &str) -> bool {
    if text.is_empty() || text.len() > 96 {
        return false;
    }
    let text = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = text
        .find(['e', 'E'])
        .map_or((text, None), |i| (&text[..i], Some(&text[i + 1..])));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (whole, fraction) = mantissa
        .split_once('.')
        .map_or((mantissa, None), |(a, b)| (a, Some(b)));
    digits(whole)
        && fraction.map_or(true, digits)
        && exponent.map_or(true, |e| digits(e.strip_prefix(['+', '-']).unwrap_or(e)))
}
fn scalar(raw: &RawValue) -> Result<String, &'static str> {
    if raw.get().starts_with('"') {
        let text: String = serde_json::from_str(raw.get()).map_err(|_| INVALID)?;
        if !bounded(&text, 256) {
            return Err(INVALID);
        }
        Ok(text)
    } else if decimal(raw.get()) {
        Ok(raw.get().to_owned())
    } else {
        Err(INVALID)
    }
}
fn optional_scalar(raw: Option<Box<RawValue>>) -> Result<Option<String>, &'static str> {
    raw.map(|v| scalar(&v)).transpose()
}
fn amount(raw: Option<Box<RawValue>>) -> Result<Option<String>, &'static str> {
    let text = optional_scalar(raw)?;
    if text.as_ref().is_some_and(|s| !decimal(s)) {
        return Err(INVALID);
    }
    Ok(text)
}
fn total(raw: &RawValue) -> Result<u64, &'static str> {
    let text = scalar(raw)?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(INVALID);
    }
    text.parse().map_err(|_| INVALID)
}
fn id(raw: &RawValue) -> Result<String, &'static str> {
    let text = scalar(raw)?;
    if text.trim().is_empty() {
        return Err(INVALID);
    }
    Ok(text)
}

impl PersonalBillingQuery {
    pub fn orders(page: u32) -> Result<Self, &'static str> {
        Self::new(PersonalBillingKind::Orders, page)
    }
    pub fn invoices(page: u32) -> Result<Self, &'static str> {
        Self::new(PersonalBillingKind::Invoices, page)
    }
    fn new(kind: PersonalBillingKind, page: u32) -> Result<Self, &'static str> {
        if page == 0 {
            return Err(INVALID);
        }
        Ok(Self { kind, page })
    }
    /// Relative path only: no current host, cookie audience or live-call authority.
    pub fn path(&self) -> &'static str {
        match self.kind {
            PersonalBillingKind::Orders => "/billing/pay/get-order-list",
            PersonalBillingKind::Invoices => "/billing/pay/get-intl-invoice",
        }
    }
    pub fn request_body(&self) -> Value {
        match self.kind {
            // Exact observed Offset=page-1. Do not silently multiply by Limit.
            PersonalBillingKind::Orders => {
                serde_json::json!({"Limit":PAGE_SIZE,"Offset":self.page-1,"PayMode":"prePay","Status":["processing","delivered","finished","canceled","delete"],"OrderType":"desc"})
            }
            PersonalBillingKind::Invoices => {
                serde_json::json!({"PageSize":PAGE_SIZE,"PageNumber":self.page})
            }
        }
    }
    pub fn project_page(&self, body: &[u8]) -> Result<PersonalBillingPage, &'static str> {
        if body.len() > BODY_LIMIT {
            return Err(INVALID);
        }
        let envelope: Envelope = serde_json::from_slice(body).map_err(|_| INVALID)?;
        if envelope.code.is_some_and(|c| c != 0) {
            return Err(INVALID);
        }
        let response = match (envelope.response, envelope.data) {
            (Some(v), None) => v,
            (None, Some(v)) if !v.code.is_some_and(|c| c != 0) => v.response,
            _ => return Err(INVALID),
        };
        let mut records = Vec::new();
        let server_total = match self.kind {
            PersonalBillingKind::Orders => {
                let wire: Orders = serde_json::from_str(response.get()).map_err(|_| INVALID)?;
                if wire.data.len() > PAGE_SIZE {
                    return Err(INVALID);
                }
                for (group, row) in wire.data.into_iter().enumerate() {
                    if row.orders.len() > SUBORDER_LIMIT {
                        return Err(INVALID);
                    }
                    // Preserve every suborder/refund and duplicate. Website product
                    // hiding and first-suborder selection do not establish completeness.
                    for order in row.orders {
                        if order.currency.as_ref().is_some_and(|s| {
                            s.len() != 3 || !s.bytes().all(|b| b.is_ascii_uppercase())
                        }) || order
                            .order_action
                            .as_ref()
                            .is_some_and(|s| !bounded(s, 256))
                        {
                            return Err(INVALID);
                        }
                        records.push(PersonalBillingRecord {
                            record_id: id(&order.order_id)?,
                            order_group: Some(group),
                            amount_raw: amount(order.real_total_cost)?,
                            currency_declared: order.currency,
                            status_raw: optional_scalar(order.status)?,
                            time_raw: optional_scalar(order.create_time)?,
                            order_action_raw: order.order_action,
                        });
                    }
                }
                total(&wire.total_count)?
            }
            PersonalBillingKind::Invoices => {
                let wire: Invoices = serde_json::from_str(response.get()).map_err(|_| INVALID)?;
                if wire.invoice_data.len() > PAGE_SIZE {
                    return Err(INVALID);
                }
                for invoice in wire.invoice_data {
                    records.push(PersonalBillingRecord {
                        record_id: id(&invoice.id)?,
                        order_group: None,
                        amount_raw: amount(invoice.amount)?,
                        currency_declared: None,
                        status_raw: optional_scalar(invoice.status)?,
                        time_raw: optional_scalar(invoice.apply_time)?,
                        order_action_raw: None,
                    });
                }
                total(&wire.total_records)?
            }
        };
        Ok(PersonalBillingPage {
            kind: self.kind,
            page: self.page,
            server_total,
            records,
            amount_unit_verified: false,
            settlement_verified: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn owned_order_positive_is_not_an_empty_or_zero_cash_default() {
        let q = PersonalBillingQuery::orders(1).unwrap();
        let p = q.project_page(br#"{"data":{"Response":{"Data":[{"OrderInfos":[{"OrderId":"owned-order","RealTotalCost":123456789012345678901,"Currency":"CNY","Status":"finished","CreateTime":"2026-10-21"}]}],"TotalCount":1}}}"#).unwrap();
        assert_eq!(p.records.len(), 1);
        assert_eq!(
            p.records[0].amount_raw.as_deref(),
            Some("123456789012345678901")
        );
        assert_eq!(p.records[0].currency_declared.as_deref(), Some("CNY"));
        assert_eq!(p.records[0].record_id, "owned-order");
        assert_eq!(p.records[0].status_raw.as_deref(), Some("finished"));
        assert_eq!(p.records[0].time_raw.as_deref(), Some("2026-10-21"));
        assert!(!p.amount_unit_verified && !p.settlement_verified);
    }
    #[test]
    fn exact_queries_have_no_implicit_region_or_extra_pages() {
        let o = PersonalBillingQuery::orders(3).unwrap();
        assert_eq!(o.path(), "/billing/pay/get-order-list");
        assert_eq!(
            o.request_body(),
            json!({"Limit":10,"Offset":2,"PayMode":"prePay","Status":["processing","delivered","finished","canceled","delete"],"OrderType":"desc"})
        );
        let i = PersonalBillingQuery::invoices(3).unwrap();
        assert_eq!(i.path(), "/billing/pay/get-intl-invoice");
        assert_eq!(i.request_body(), json!({"PageSize":10,"PageNumber":3}));
        assert!(PersonalBillingQuery::orders(0).is_err());
        assert!(PersonalBillingQuery::invoices(0).is_err());
    }
    #[test]
    fn suborders_duplicates_and_refund_observations_are_not_summed_or_filtered() {
        let p=PersonalBillingQuery::orders(2).unwrap().project_page(br#"{"Response":{"Data":[{"OrderInfos":[{"OrderId":"same","RealTotalCost":"-0.0100","OrderAction":"preRefund","Currency":"CNY"},{"OrderId":"same","RealTotalCost":"1.23000000000000000001","Currency":"USD"}]},{"OrderInfos":[{"OrderId":"other","RealTotalCost":0,"Status":"unknown_future"}]}],"TotalCount":"7"}}"#).unwrap();
        assert_eq!(p.records.len(), 3);
        assert_eq!(p.server_total, 7);
        assert_eq!(p.page, 2);
        assert_eq!(
            p.records.iter().map(|r| r.order_group).collect::<Vec<_>>(),
            vec![Some(0), Some(0), Some(1)]
        );
        assert_eq!(p.records[0].record_id, p.records[1].record_id);
        assert_eq!(p.records[0].amount_raw.as_deref(), Some("-0.0100"));
        assert_eq!(p.records[0].order_action_raw.as_deref(), Some("preRefund"));
        assert_eq!(
            p.records[1].amount_raw.as_deref(),
            Some("1.23000000000000000001")
        );
        assert_eq!(p.records[2].amount_raw.as_deref(), Some("0"));
        assert_eq!(p.records[2].currency_declared, None);
        assert_eq!(p.records[2].status_raw.as_deref(), Some("unknown_future"));
    }
    #[test]
    fn invoices_keep_unknown_currency_and_discard_download_and_identity_fields() {
        let p=PersonalBillingQuery::invoices(1).unwrap().project_page(br#"{"Response":{"InvoiceData":[{"IMSApplyId":123456789012345678901,"Amount":"1.2000e-3","ApplyTime":1790000000000,"Status":"1","InvoiceUrl":"https://owned.invalid/private-download","email":"private-owned-email","Currency":"USD"},{"IMSApplyId":"unknown","Amount":null}],"TotalRecords":2}}"#).unwrap();
        assert_eq!(p.records[0].record_id, "123456789012345678901");
        assert_eq!(p.records[0].amount_raw.as_deref(), Some("1.2000e-3"));
        assert_eq!(p.records[0].time_raw.as_deref(), Some("1790000000000"));
        assert_eq!(p.records[0].currency_declared, None);
        assert_eq!(p.records[0].status_raw.as_deref(), Some("1"));
        assert_eq!(p.records[1].amount_raw, None);
        assert!(!p.settlement_verified);
        let exported = serde_json::to_string(&p).unwrap();
        for x in [
            "private-download",
            "private-owned-email",
            "InvoiceUrl",
            "USD",
        ] {
            assert!(!exported.contains(x));
        }
    }
    #[test]
    fn explicit_empty_and_missing_amount_do_not_invent_zero_or_usd() {
        let q = PersonalBillingQuery::orders(1).unwrap();
        assert!(q
            .project_page(br#"{"Response":{"Data":[],"TotalCount":0}}"#)
            .unwrap()
            .records
            .is_empty());
        let p = q
            .project_page(
                br#"{"Response":{"Data":[{"OrderInfos":[{"OrderId":"missing"}]}],"TotalCount":1}}"#,
            )
            .unwrap();
        assert_eq!(p.records[0].amount_raw, None);
        assert_eq!(p.records[0].currency_declared, None);
        assert_eq!(p.records[0].status_raw, None);
        assert!(!p.amount_unit_verified && !p.settlement_verified);
    }
    #[test]
    fn errors_ambiguous_missing_and_duplicate_contracts_fail_closed() {
        let q = PersonalBillingQuery::orders(1).unwrap();
        for raw in [
            "{}",
            r#"{"Response":{}}"#,
            r#"{"Response":{"Data":[]}}"#,
            r#"{"code":7,"Response":{"Data":[],"TotalCount":0}}"#,
            r#"{"data":{"code":7,"Response":{"Data":[],"TotalCount":0}}}"#,
            r#"{"Response":{"Data":[],"TotalCount":0},"data":{"Response":{"Data":[],"TotalCount":0}}}"#,
            r#"{"Response":{"Data":[],"TotalCount":0,"Error":null}}"#,
            r#"{"Error":{"Message":"owned-private-error"},"Response":{"Data":[],"TotalCount":0}}"#,
            r#"{"Response":{"Data":[],"Data":[],"TotalCount":0}}"#,
            r#"{"Response":{"Data":[],"TotalCount":-1}}"#,
            r#"{"Response":{"Data":[],"TotalCount":"1.0"}}"#,
            r#"{"Response":{"Data":[],"TotalCount":18446744073709551616}}"#,
            r#"{"Response":{"Data":[{}],"TotalCount":1}}"#,
        ] {
            assert_eq!(
                q.project_page(raw.as_bytes()).unwrap_err(),
                INVALID,
                "{raw}"
            );
        }
        assert!(PersonalBillingQuery::invoices(1)
            .unwrap()
            .project_page(br#"{"Response":{"Data":[],"TotalCount":0}}"#)
            .is_err());
    }
    #[test]
    fn invalid_amount_currency_identity_and_scalar_types_are_not_defaults() {
        let q = PersonalBillingQuery::orders(1).unwrap();
        for value in [
            json!("NaN"),
            json!(""),
            json!("+1"),
            json!("1e"),
            json!(true),
            json!({}),
            json!([]),
        ] {
            let raw = json!({"Response":{"Data":[{"OrderInfos":[{"OrderId":"owned","RealTotalCost":value}]}],"TotalCount":1}});
            assert_eq!(
                q.project_page(&serde_json::to_vec(&raw).unwrap())
                    .unwrap_err(),
                INVALID
            );
        }
        for record in [
            json!({"OrderId":""}),
            json!({"OrderId":"  "}),
            json!({"OrderId":true}),
            json!({"OrderId":"owned","Currency":"usd"}),
            json!({"OrderId":"owned","Currency":"USD\n"}),
            json!({"OrderId":"owned","Status":{}}),
            json!({"OrderId":"owned","CreateTime":true}),
            json!({"OrderId":"owned","OrderAction":"bad\n"}),
        ] {
            let raw = json!({"Response":{"Data":[{"OrderInfos":[record]}],"TotalCount":1}});
            assert_eq!(
                q.project_page(&serde_json::to_vec(&raw).unwrap())
                    .unwrap_err(),
                INVALID
            );
        }
    }
    #[test]
    fn local_body_record_and_text_bounds_fail_without_body_echo() {
        let q = PersonalBillingQuery::orders(1).unwrap();
        assert_eq!(
            q.project_page(&vec![b' '; BODY_LIMIT + 1]).unwrap_err(),
            INVALID
        );
        for groups in [
            vec![json!({"OrderInfos":[]}); 11],
            vec![json!({"OrderInfos":vec![json!({"OrderId":"owned"});101]})],
        ] {
            let raw = json!({"Response":{"Data":groups,"TotalCount":1}});
            assert_eq!(
                q.project_page(&serde_json::to_vec(&raw).unwrap())
                    .unwrap_err(),
                INVALID
            );
        }
        let inv = json!({"Response":{"InvoiceData":vec![json!({"IMSApplyId":"owned"});11],"TotalRecords":11}});
        assert_eq!(
            PersonalBillingQuery::invoices(1)
                .unwrap()
                .project_page(&serde_json::to_vec(&inv).unwrap())
                .unwrap_err(),
            INVALID
        );
        for field in ["OrderId", "Status", "CreateTime", "OrderAction"] {
            let mut row = json!({"OrderId":"owned"});
            row[field] = json!("x".repeat(257));
            let raw = json!({"Response":{"Data":[{"OrderInfos":[row]}],"TotalCount":1}});
            assert_eq!(
                q.project_page(&serde_json::to_vec(&raw).unwrap())
                    .unwrap_err(),
                INVALID
            );
        }
    }
}

//! Native account credit observations, separate from activity rewards and model permissions.
//! Source freshness and quota semantics: [request flow](../../../docs/wiki/request-flows.md).
use super::*;

const INVALID: &str = "账户额度响应缺失、无效或存在冲突；未展示不完整额度";
const MAX_CREDIT: f64 = 9_007_199_254_740_991.0;
const PAGE_SIZE: usize = 200;
const MAX_RESOURCES: usize = 2000;

#[derive(Debug, Serialize)]
pub struct WorkBuddyQuota {
    pub account_id: String,
    pub region: WorkBuddyRegion,
    pub scope: &'static str,
    pub total: Option<f64>,
    pub used: f64,
    pub remaining: Option<f64>,
    pub unlimited: bool,
    pub is_paid_user: Option<bool>,
    pub resources: Vec<WorkBuddyQuotaResource>,
    pub resource_details_complete: bool,
    pub checked_at: String,
}

#[derive(Debug, Serialize)]
pub struct WorkBuddyQuotaResource {
    pub resource_key: String,
    pub source: &'static str,
    pub package_code: String,
    pub name: Option<String>,
    pub total: f64,
    pub used: f64,
    pub remaining: f64,
    pub daily_slice: bool,
}

fn number(value: &Value) -> Result<f64, String> {
    let result = if let Some(text) = value.as_str() {
        if text.is_empty()
            || text.len() > 64
            || !text
                .bytes()
                .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'-'))
        {
            return Err(INVALID.into());
        }
        text.parse::<f64>().map_err(|_| INVALID)?
    } else {
        value.as_f64().ok_or(INVALID)?
    };
    if result.is_finite() && result.abs() <= MAX_CREDIT {
        Ok(result)
    } else {
        Err(INVALID.into())
    }
}

fn credit(value: &Value) -> Result<f64, String> {
    let result = number(value)?;
    if result < 0.0 {
        Err(INVALID.into())
    } else {
        Ok(result)
    }
}

fn personal_totals(value: &Value) -> Result<(f64, f64, f64), String> {
    let rows = value["Packages"]
        .as_array()
        .filter(|rows| rows.len() <= 2000)
        .ok_or(INVALID)?;
    let mut sums = (0.0, 0.0, 0.0);
    for row in rows {
        sums.0 += credit(&row["CycleTotalCapacity"])?;
        sums.1 += credit(&row["CycleUsedCapacity"])?;
        sums.2 += credit(&row["CycleRemainCapacity"])?;
        if [sums.0, sums.1, sums.2]
            .iter()
            .any(|n| !n.is_finite() || *n > MAX_CREDIT)
        {
            return Err(INVALID.into());
        }
    }
    Ok(sums)
}

type EnterpriseTotals = (Option<f64>, f64, Option<f64>, bool);

fn enterprise_totals(value: &Value) -> Result<EnterpriseTotals, String> {
    let mut found = None;
    for candidate in [value, &value["data"], &value["data"]["data"]] {
        if let Some(code) = candidate.get("code") {
            if code.as_i64() != Some(0) {
                return Err(INVALID.into());
            }
        }
        if ["limitNum", "limit_num", "credit", "used_num", "usedNum"]
            .iter()
            .any(|key| candidate.get(*key).is_some())
        {
            let parsed = enterprise_direct(candidate)?;
            if found.is_some_and(|previous| previous != parsed) {
                return Err(INVALID.into());
            }
            found = Some(parsed);
        }
    }
    found.ok_or(INVALID.into())
}

fn enterprise_direct(value: &Value) -> Result<EnterpriseTotals, String> {
    let alias = |keys: &[&str]| -> Result<f64, String> {
        let mut found = None;
        for key in keys {
            if let Some(raw) = value.get(*key).filter(|raw| !raw.is_null()) {
                let n = number(raw)?;
                if found.is_some_and(|old| old != n) {
                    return Err(INVALID.into());
                }
                found = Some(n);
            }
        }
        found.ok_or(INVALID.into())
    };
    let total = alias(&["limitNum", "limit_num"])?;
    let used = alias(&["credit", "used_num", "usedNum"])?;
    if used < 0.0 || total < 0.0 && total != -1.0 {
        return Err(INVALID.into());
    }
    Ok(if total == -1.0 {
        (None, used, None, true)
    } else {
        (Some(total), used, Some((total - used).max(0.0)), false)
    })
}

impl WorkBuddyAuth {
    /// Ordinary use prepares authentication once; the observation itself never retries.
    pub async fn quota_for_use(&self, account: &str) -> Result<WorkBuddyQuota, String> {
        self.prepare_model_authorization(account).await?;
        self.quota_once(account).await
    }

    /// Explicit one-shot observation does not refresh, log in, generate or claim.
    pub async fn quota_once(&self, account: &str) -> Result<WorkBuddyQuota, String> {
        if !valid_id(account) {
            return Err("账号 ID 无效".into());
        }
        let key = self.session_key(account);
        let cipher = self.read(&key).await?.ok_or("请先登录 WorkBuddy")?;
        let credential: Credential = decode(&cipher)?;
        validate_credential(&credential, self.region)?;
        super::daily_claim::principal(&credential, self.region)?;
        let day = beijing_day();
        self.quota_current(&key, &cipher, &credential, &day).await?;
        tokio::time::timeout(
            Duration::from_secs(60),
            self.quota_query(account, &key, &cipher, &credential, &day),
        )
        .await
        .map_err(|_| "账户额度查询超时；未自动重试".to_owned())?
    }

    async fn quota_current(
        &self,
        key: &str,
        cipher: &str,
        credential: &Credential,
        day: &str,
    ) -> Result<(), String> {
        self.quota_current_at(key, cipher, credential, day, chrono::Utc::now)
            .await
    }

    async fn quota_current_at(
        &self,
        key: &str,
        cipher: &str,
        credential: &Credential,
        day: &str,
        now: impl Fn() -> chrono::DateTime<chrono::Utc>,
    ) -> Result<(), String> {
        let current = self.read(key).await?;
        let checked_at = now();
        if credential
            .access_expires_at_ms
            .is_some_and(|at| at <= checked_at.timestamp_millis())
        {
            return Err("访问认证已过期，请重新登录；未发送额度查询".into());
        }
        if current.as_deref() != Some(cipher) || beijing_day_at(checked_at) != day {
            return Err("登录状态或查询日期已变化，请重新检查；未展示迟到结果".into());
        }
        Ok(())
    }

    async fn quota_post(
        &self,
        key: &str,
        cipher: &str,
        credential: &Credential,
        day: &str,
        path: &str,
        body: &Value,
    ) -> Result<Value, String> {
        self.quota_current(key, cipher, credential, day).await?;
        #[cfg(test)]
        let origin = self.base.as_str();
        #[cfg(not(test))]
        let origin = self.region.profile().origin;
        let mut headers = reqwest::header::HeaderMap::new();
        let mut pairs = vec![
            (
                "authorization",
                format!("Bearer {}", credential.access_token),
            ),
            ("x-user-id", credential.uid.clone().ok_or(INVALID)?),
            ("x-domain", credential.domain.clone()),
        ];
        if let Some(enterprise) = &credential.enterprise_id {
            pairs.push(("x-enterprise-id", enterprise.clone()));
            pairs.push(("x-tenant-id", enterprise.clone()));
        }
        for (name, value) in pairs {
            let mut value = HeaderValue::from_str(&value).map_err(|_| "额度查询认证头无效")?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        let response = self
            .client
            .post(format!("{origin}{path}"))
            .headers(headers)
            .header("Accept", "application/json")
            .header(
                "Accept-Language",
                if self.region == WorkBuddyRegion::Cn {
                    "zh"
                } else {
                    "en"
                },
            )
            .header("X-Client-Platform", "web")
            .header("Origin", self.region.profile().origin)
            .header("User-Agent", self.region.profile().user_agent)
            .json(body)
            .send()
            .await
            .map_err(|_| "额度查询失败或超时；未自动重试")?;
        let http_status = response.status().as_u16();
        if http_status != 200 {
            return Err(format!("额度查询被拒绝（HTTP {http_status}）；未自动重试"));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "额度响应读取失败；未自动重试")?;
            if bytes.len() + chunk.len() > BODY_LIMIT {
                return Err("额度响应过大".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
        let mut secrets = vec![credential.access_token.as_str()];
        secrets.extend(credential.refresh_token.as_deref());
        if reflects(&value, &secrets) {
            return Err("额度响应包含认证资料，已拒绝展示".into());
        }
        if value["code"].as_i64() != Some(0) {
            return Err("平台未确认额度查询；未自动重试".into());
        }
        let data = value
            .get("data")
            .filter(|v| v.is_object())
            .ok_or(INVALID)?
            .clone();
        self.quota_current(key, cipher, credential, day).await?;
        Ok(data)
    }

    async fn quota_query(
        &self,
        account: &str,
        key: &str,
        cipher: &str,
        credential: &Credential,
        day: &str,
    ) -> Result<WorkBuddyQuota, String> {
        let mut result = WorkBuddyQuota {
            account_id: account.into(),
            region: self.region,
            scope: "personal",
            total: None,
            used: 0.0,
            remaining: None,
            unlimited: false,
            is_paid_user: None,
            resources: Vec::new(),
            resource_details_complete: true,
            checked_at: String::new(),
        };
        if credential
            .enterprise_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
        {
            let data = self
                .quota_post(
                    key,
                    cipher,
                    credential,
                    day,
                    "/v2/billing/meter/get-enterprise-user-usage",
                    &json!({}),
                )
                .await?;
            let (total, used, remaining, unlimited) = enterprise_totals(&data)?;
            result.scope = "enterprise";
            result.total = total;
            result.used = used;
            result.remaining = remaining;
            result.unlimited = unlimited;
        } else {
            let data = self
                .quota_post(
                    key,
                    cipher,
                    credential,
                    day,
                    "/billing/meter/get-user-resource-summary",
                    &json!({}),
                )
                .await?;
            let (total, used, remaining) = personal_totals(&data)?;
            result.total = Some(total);
            result.used = used;
            result.remaining = Some(remaining);
            result.is_paid_user = match data.get("IsPaidUser") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_bool().ok_or(INVALID)?),
            };
            let codes: Value =
                serde_json::from_str(include_str!("quota-package-codes-20261018.json"))
                    .map_err(|_| INVALID)?;
            let mut seen = std::collections::BTreeMap::<String, Value>::new();
            for source in ["paid", "free"] {
                let mut fetched = 0usize;
                let mut expected = None;
                let mut source_ids = std::collections::BTreeSet::new();
                for page in 1..=10 {
                    let mut body = json!({"PageNumber":page,"PageSize":PAGE_SIZE,"Status":[0,3],"PackageCodes":codes[source]});
                    if source == "paid" {
                        body["NeedRenewInfo"] = json!(true);
                    } else {
                        body["SlicePeriodStartTime"] = json!(format!("{day} 00:00:00"));
                        body["SlicePeriodEndTime"] = json!(format!("{day} 23:59:59"));
                    }
                    let page = self
                        .quota_post(
                            key,
                            cipher,
                            credential,
                            day,
                            &format!("/billing/meter/get-user-resource-{source}-packages"),
                            &body,
                        )
                        .await?;
                    let rows = page["Accounts"]
                        .as_array()
                        .filter(|v| v.len() <= PAGE_SIZE)
                        .ok_or(INVALID)?;
                    let total = credit(&page["TotalCount"])?;
                    if total.fract() != 0.0 || total > MAX_RESOURCES as f64 {
                        return Err(INVALID.into());
                    }
                    let total = total as usize;
                    if expected.is_some_and(|n| n != total) {
                        return Err("资源包分页已变化，请重新查询；未展示部分额度".into());
                    }
                    expected = Some(total);
                    fetched += rows.len();
                    if fetched > total || fetched < total && rows.is_empty() {
                        return Err(INVALID.into());
                    }
                    for row in rows {
                        let id = safe_text(&row["ResourceId"], 256)?;
                        if !source_ids.insert(id.clone()) {
                            return Err("同一来源重复返回资源包；未展示不完整额度".into());
                        }
                        if let Some(old) = seen.get(&id) {
                            if old != row {
                                return Err("资源包重复项存在冲突；未展示不完整额度".into());
                            }
                            continue;
                        }
                        if seen.len() >= MAX_RESOURCES {
                            return Err("资源包过多，未展示截断额度".into());
                        }
                        let resource = resource(row, source, &id)?;
                        seen.insert(id, row.clone());
                        result.resources.push(resource);
                    }
                    if fetched == total {
                        break;
                    }
                }
                if expected != Some(fetched) {
                    return Err("资源包分页未完成，未展示截断额度".into());
                }
            }
            // Summary is authoritative. A dated detail-code list must not silently claim
            // complete coverage for new package codes or resources absent from both lists.
            for package in data["Packages"].as_array().ok_or(INVALID)? {
                let code = safe_text(&package["PackageCode"], 128)?;
                if !result.resources.iter().any(|r| r.package_code == code) {
                    result.resource_details_complete = false;
                }
            }
        }
        self.quota_current(key, cipher, credential, day).await?;
        result.checked_at = chrono::Utc::now().to_rfc3339();
        Ok(result)
    }
}

fn beijing_day() -> String {
    beijing_day_at(chrono::Utc::now())
}

fn beijing_day_at(now: chrono::DateTime<chrono::Utc>) -> String {
    (now + chrono::Duration::hours(8))
        .format("%Y-%m-%d")
        .to_string()
}

fn safe_text(value: &Value, max: usize) -> Result<String, String> {
    value
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or(INVALID.into())
}

fn resource(
    value: &Value,
    source: &'static str,
    id: &str,
) -> Result<WorkBuddyQuotaResource, String> {
    let kind = value["CapacityType"]
        .as_i64()
        .filter(|n| (1..=4).contains(n))
        .ok_or(INVALID)?;
    let (total, remaining) = if kind == 4 {
        let first = value["SlicePeriodUsageDetails"]
            .as_array()
            .filter(|v| v.len() == 1)
            .and_then(|v| v.first())
            .ok_or(INVALID)?;
        (
            credit(&first["SlicePeriodCapacitySizePrecise"])?,
            credit(&first["SlicePeriodCapacityRemainPrecise"])?,
        )
    } else {
        (
            credit(&value["CycleCapacitySizePrecise"])?,
            credit(&value["CycleCapacityRemainPrecise"])?,
        )
    };
    if remaining > total {
        return Err(INVALID.into());
    }
    let name = match value.get("PackageName") {
        None | Some(Value::Null) => None,
        Some(v) => Some(safe_text(v, 256)?),
    };
    Ok(WorkBuddyQuotaResource {
        resource_key: format!("{:x}", Sha256::digest(id.as_bytes())),
        source,
        package_code: safe_text(&value["PackageCode"], 128)?,
        name,
        total,
        remaining,
        used: (total - remaining).max(0.0),
        daily_slice: kind == 4,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Path, State},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
        Json, Router,
    };
    use std::collections::BTreeMap;

    fn summary() -> Value {
        json!({"Packages":[{"PackageCode":"TCACA_code_002_AkiJS3ZHF5", "CycleTotalCapacity":"120.5","CycleUsedCapacity":"20.25","CycleRemainCapacity":"100.25"}],"IsPaidUser":true})
    }

    fn resource_row(id: &str) -> Value {
        json!({"ResourceId":id,"PackageCode":"TCACA_code_002_AkiJS3ZHF5","CapacityType":1,"CycleCapacitySizePrecise":"120.5","CycleCapacityRemainPrecise":"100.25","PackageName":"Fixture subscription"})
    }

    type Calls = Arc<Mutex<Vec<(String, Value)>>>;
    #[derive(Clone)]
    struct FixtureState {
        calls: Calls,
        mode: &'static str,
        pool: SqlitePool,
        region: WorkBuddyRegion,
    }

    async fn fixture(
        region: WorkBuddyRegion,
        enterprise: bool,
        mode: &'static str,
    ) -> (WorkBuddyAuth, Calls, tokio::task::JoinHandle<()>) {
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = calls.clone();
        let app = Router::new()
            .route(
                "/*path",
                post(
                    |Path(path): Path<String>,
                     State(FixtureState{calls,mode,pool,region}): State<FixtureState>,
                     headers: HeaderMap,
                     Json(body): Json<Value>| async move {
                        assert_eq!(headers["authorization"], "Bearer fixture-quota-access");
                        assert_eq!(headers["x-user-id"], "fixture-quota-uid");
                        assert!(!headers.contains_key("x-refresh-token"));
                        let page=body["PageNumber"].as_u64().unwrap_or(0);
                        calls.lock().await.push((path.clone(), body));
                        if mode=="changed" && path.ends_with("get-user-resource-summary") {
                            let key=WorkBuddyAuth::benefit_session_key(region,"fixture").unwrap();let cipher=repo::get_secret(&pool,&key).await.unwrap().unwrap();let mut saved:Credential=decode(&cipher).unwrap();saved.access_token="fixture-quota-other-access".into();repo::set_secret(&pool,&key,&encode(&saved).unwrap()).await.unwrap();
                        }
                        if mode=="timeout" {tokio::time::sleep(Duration::from_millis(150)).await;}
                        if mode=="http" {return (StatusCode::BAD_REQUEST,Json(json!({"code":0,"data":summary()}))).into_response();}
                        if mode=="redirect" {return (StatusCode::FOUND,[("location","/must-not-follow")],"private redirect body").into_response();}
                        if mode=="large" {return Json(json!({"code":0,"data":{"x":"x".repeat(BODY_LIMIT+1)}})).into_response();}
                        let data = if mode=="same-source-duplicate" && path.ends_with("get-user-resource-paid-packages") {
                            let row=json!({"ResourceId":"fixture-duplicate","PackageCode":"TCACA_code_002_AkiJS3ZHF5","CapacityType":1,"CycleCapacitySizePrecise":"120.5","CycleCapacityRemainPrecise":"100.25"});
                            json!({"Accounts":[row.clone(),row],"TotalCount":2})
                        } else if matches!(mode,"pages"|"cross-page-duplicate"|"changing-total") && path.ends_with("get-user-resource-paid-packages") {
                            let rows:Vec<_>=if page==1 {(0..200).map(|n|resource_row(&format!("fixture-{n}"))).collect()} else {vec![resource_row(if mode=="cross-page-duplicate" {"fixture-0"}else{"fixture-200"})]};
                            json!({"Accounts":rows,"TotalCount":if mode=="changing-total"&&page==2 {202}else{201}})
                        } else if matches!(mode,"cross-source-duplicate"|"cross-source-conflict") && (path.ends_with("get-user-resource-paid-packages") || path.ends_with("get-user-resource-free-packages")) {
                            let mut row=resource_row("fixture-shared");if mode=="cross-source-conflict"&&path.ends_with("get-user-resource-free-packages") {row["CycleCapacityRemainPrecise"]=json!(1);}
                            json!({"Accounts":[row],"TotalCount":1})
                        } else if mode=="empty-page" && path.ends_with("get-user-resource-paid-packages") {
                            json!({"Accounts":[],"TotalCount":2})
                        } else if path.ends_with("get-user-resource-summary") {
                            summary()
                        } else if path.ends_with("get-enterprise-user-usage") {
                            json!({"limitNum":100,"credit":30})
                        } else {
                            json!({"Accounts":[],"TotalCount":0})
                        };
                        if mode == "reflect" {
                            return Json(
                                json!({"code":0,"data":data,"debug":"fixture-quota-access"}),
                            ).into_response();
                        }
                        if mode == "fail" && path.ends_with("get-user-resource-free-packages") {
                            return Json(json!({"code":11128,"msg":"must not escape"})).into_response();
                        }
                        Json(json!({"code":0,"data":data})).into_response()
                    },
                ),
            )
            .with_state(FixtureState{calls:captured,mode,pool:db.pool().clone(),region});
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let auth = WorkBuddyAuth::with_test_origin_for_region(db.pool().clone(), origin, region);
        auth.test_seed_account(
            "fixture",
            "fixture-quota-uid",
            "fixture-quota-access",
            "fixture-quota-refresh",
        )
        .await;
        if enterprise {
            let key = auth.session_key("fixture");
            let mut saved: Credential = decode(&auth.read(&key).await.unwrap().unwrap()).unwrap();
            saved.enterprise_id = Some("fixture-company".into());
            repo::set_secret(&auth.pool, &key, &encode(&saved).unwrap())
                .await
                .unwrap();
        }
        (auth, calls, server)
    }

    #[tokio::test]
    async fn review_regression_expiry_after_blocked_pool_read() {
        use futures_util::FutureExt;
        use std::sync::atomic::{AtomicI64, Ordering};
        let (auth, calls, server) = fixture(WorkBuddyRegion::Global, false, "ok").await;
        let key = auth.session_key("fixture");
        let mut credential: Credential = decode(&auth.read(&key).await.unwrap().unwrap()).unwrap();
        let before = chrono::Utc::now();
        credential.access_expires_at_ms = Some(before.timestamp_millis() + 70_000);
        let cipher = encode(&credential).unwrap();
        repo::set_secret(&auth.pool, &key, &cipher).await.unwrap();
        for expired in [false, true] {
            let now = AtomicI64::new(before.timestamp_millis());
            let day = beijing_day_at(before);
            let held = auth.pool.acquire().await.unwrap();
            let pending = auth.quota_current_at(&key, &cipher, &credential, &day, || {
                chrono::DateTime::from_timestamp_millis(now.load(Ordering::SeqCst)).unwrap()
            });
            tokio::pin!(pending);
            assert!(pending.as_mut().now_or_never().is_none());
            now.store(
                before.timestamp_millis() + if expired { 70_001 } else { 1 },
                Ordering::SeqCst,
            );
            drop(held);
            assert_eq!(
                pending.await.is_err(),
                expired,
                "expiry must be evaluated after pool wait"
            );
        }
        assert!(calls.lock().await.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn review_regression_same_source_duplicates_cannot_satisfy_total_count() {
        let (auth, calls, server) =
            fixture(WorkBuddyRegion::Global, false, "same-source-duplicate").await;
        assert!(
            auth.quota_once("fixture").await.is_err(),
            "two identical rows are not two resources"
        );
        assert_eq!(calls.lock().await.len(), 2);
        server.abort();
    }

    #[test]
    fn review_regression_official_enterprise_envelopes_and_conflicts() {
        for value in [
            json!({"limitNum":100,"credit":30}),
            json!({"data":{"limitNum":100,"credit":30}}),
            json!({"data":{"data":{"limitNum":100,"credit":30}}}),
        ] {
            assert_eq!(
                enterprise_totals(&value).unwrap(),
                (Some(100.0), 30.0, Some(70.0), false)
            );
        }
        assert!(enterprise_totals(
            &json!({"limitNum":100,"credit":30,"data":{"limitNum":200,"credit":30}})
        )
        .is_err());
        assert!(enterprise_totals(
            &json!({"data":{"code":11128,"data":{"limitNum":100,"credit":30}}})
        )
        .is_err());
    }

    #[tokio::test]
    async fn pages_complete_but_duplicate_conflict_changed_total_and_empty_pages_fail_closed() {
        let (auth, calls, server) = fixture(WorkBuddyRegion::Global, false, "pages").await;
        let result = auth.quota_once("fixture").await.unwrap();
        assert_eq!(result.resources.len(), 201);
        assert!(result.resource_details_complete);
        assert_eq!(
            calls
                .lock()
                .await
                .iter()
                .filter(|(p, _)| p.ends_with("paid-packages"))
                .count(),
            2
        );
        server.abort();
        let (auth, calls, server) =
            fixture(WorkBuddyRegion::Global, false, "cross-source-duplicate").await;
        assert_eq!(auth.quota_once("fixture").await.unwrap().resources.len(), 1);
        assert_eq!(calls.lock().await.len(), 3);
        server.abort();
        for mode in [
            "cross-page-duplicate",
            "changing-total",
            "cross-source-conflict",
            "empty-page",
        ] {
            let (auth, calls, server) = fixture(WorkBuddyRegion::Global, false, mode).await;
            assert!(auth.quota_once("fixture").await.is_err(), "{mode}");
            assert!(calls.lock().await.len() <= 3);
            server.abort();
        }
    }

    #[tokio::test]
    async fn changed_source_http_redirect_large_body_and_timeout_never_replay() {
        for mode in ["changed", "http", "redirect", "large", "timeout"] {
            let (mut auth, calls, server) = fixture(WorkBuddyRegion::Global, false, mode).await;
            if mode == "timeout" {
                auth.client = Client::builder()
                    .retry(reqwest::retry::never())
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_millis(35))
                    .build()
                    .unwrap();
            }
            let error = auth.quota_once("fixture").await.unwrap_err();
            assert!(!error.contains("private redirect body"));
            assert_eq!(calls.lock().await.len(), 1, "{mode}");
            server.abort();
        }
    }

    #[test]
    fn precise_resource_and_daily_slice_capacities_are_not_interchanged() {
        let row = resource_row("fixture-resource");
        let parsed = resource(&row, "paid", "fixture-resource").unwrap();
        assert_eq!(parsed.remaining, 100.25);
        assert_eq!(parsed.used, 20.25);
        assert!(!parsed.daily_slice);
        let mut sliced = row.clone();
        sliced["CapacityType"] = json!(4);
        sliced["SlicePeriodUsageDetails"] =
            json!([{"SlicePeriodCapacitySizePrecise":"10","SlicePeriodCapacityRemainPrecise":"3"}]);
        let parsed = resource(&sliced, "free", "fixture-resource").unwrap();
        assert_eq!(parsed.total, 10.0);
        assert_eq!(parsed.remaining, 3.0);
        assert_eq!(parsed.used, 7.0);
        assert!(parsed.daily_slice);
        for bad in [json!([]), json!([{}]), json!(null)] {
            sliced["SlicePeriodUsageDetails"] = bad;
            assert!(resource(&sliced, "free", "fixture-resource").is_err());
        }
        let mut invalid = row;
        invalid["CycleCapacityRemainPrecise"] = json!(999);
        assert!(resource(&invalid, "paid", "fixture-resource").is_err());
    }

    #[tokio::test]
    async fn both_regions_query_all_personal_sources_once_without_auth_or_claim_replay() {
        for region in [WorkBuddyRegion::Global, WorkBuddyRegion::Cn] {
            let (auth, calls, server) = fixture(region, false, "ok").await;
            let before = auth.read(&auth.session_key("fixture")).await.unwrap();
            let quota = auth.quota_once("fixture").await.unwrap();
            assert_eq!(quota.region, region);
            assert_eq!(quota.scope, "personal");
            assert_eq!(quota.remaining, Some(100.25));
            assert!(!quota.unlimited);
            let got = calls.lock().await.clone();
            assert_eq!(got.len(), 3);
            assert!(got
                .iter()
                .any(|(p, _)| p == "billing/meter/get-user-resource-summary"));
            for kind in ["paid", "free"] {
                let (_, body) = got
                    .iter()
                    .find(|(p, _)| p.ends_with(&format!("get-user-resource-{kind}-packages")))
                    .unwrap();
                assert_eq!(body["PageNumber"], 1);
                assert_eq!(body["PageSize"], 200);
                assert_eq!(body["Status"], json!([0, 3]));
                assert!(body["PackageCodes"].as_array().unwrap().len() > 5);
            }
            assert_eq!(
                auth.read(&auth.session_key("fixture")).await.unwrap(),
                before
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn enterprise_uses_only_its_allocation_endpoint() {
        let (auth, calls, server) = fixture(WorkBuddyRegion::Cn, true, "ok").await;
        let q = auth.quota_once("fixture").await.unwrap();
        assert_eq!(q.scope, "enterprise");
        assert_eq!(q.used, 30.0);
        assert_eq!(q.remaining, Some(70.0));
        assert_eq!(q.is_paid_user, None);
        assert_eq!(
            calls.lock().await.as_slice(),
            &[(
                "v2/billing/meter/get-enterprise-user-usage".into(),
                json!({})
            )]
        );
        server.abort();
    }

    #[tokio::test]
    async fn incomplete_sources_and_reflection_are_not_presented_or_retried() {
        for mode in ["fail", "reflect"] {
            let (auth, calls, server) = fixture(WorkBuddyRegion::Global, false, mode).await;
            let error = auth.quota_once("fixture").await.unwrap_err();
            assert!(!error.contains("fixture-quota-access"));
            assert!(!error.contains("must not escape"));
            let mut counts = BTreeMap::new();
            for (path, _) in calls.lock().await.iter() {
                *counts.entry(path.clone()).or_insert(0) += 1;
            }
            assert!(!counts.is_empty());
            assert!(counts.values().all(|n| *n == 1));
            server.abort();
        }
    }

    #[tokio::test]
    async fn expired_or_missing_auth_does_not_send_or_refresh_a_one_shot_query() {
        let (auth, calls, server) = fixture(WorkBuddyRegion::Global, false, "ok").await;
        assert!(auth.quota_once("absent").await.is_err());
        assert!(auth.quota_once("invalid.name").await.is_err());
        let key = auth.session_key("fixture");
        let mut saved: Credential = decode(&auth.read(&key).await.unwrap().unwrap()).unwrap();
        saved.access_expires_at_ms = Some(now_ms() - 1);
        let cipher = encode(&saved).unwrap();
        repo::set_secret(&auth.pool, &key, &cipher).await.unwrap();
        assert!(auth.quota_once("fixture").await.is_err());
        assert!(calls.lock().await.is_empty());
        assert_eq!(auth.read(&key).await.unwrap(), Some(cipher));
        server.abort();
    }

    #[test]
    fn summary_requires_real_capacities_and_preserves_explicit_zero() {
        let data = json!({"Packages":[{"PackageCode":"fixture", "CycleTotalCapacity":"120.5","CycleUsedCapacity":"20.25","CycleRemainCapacity":"100.25"}],"IsPaidUser":true});
        assert_eq!(personal_totals(&data).unwrap(), (120.5, 20.25, 100.25));
        assert_eq!(
            personal_totals(&json!({"Packages":[]})).unwrap(),
            (0.0, 0.0, 0.0)
        );
        for bad in [
            json!({}),
            json!({"Packages":null}),
            json!({"Packages":[{}]}),
            json!({"Packages":[{"CycleTotalCapacity":"NaN","CycleUsedCapacity":"0","CycleRemainCapacity":"0"}]}),
        ] {
            assert!(personal_totals(&bad).is_err());
        }
        for bad in [
            json!(null),
            json!(true),
            json!(-1),
            json!(""),
            json!("Infinity"),
            json!("-0.1"),
            json!("1e999"),
            json!(" 10 "),
        ] {
            let mut value = data.clone();
            value["Packages"][0]["CycleTotalCapacity"] = bad;
            assert!(personal_totals(&value).is_err());
        }
    }

    #[test]
    fn enterprise_used_is_not_remaining_and_unlimited_is_not_zero() {
        assert_eq!(
            enterprise_totals(&json!({"limitNum":100,"credit":30})).unwrap(),
            (Some(100.0), 30.0, Some(70.0), false)
        );
        assert_eq!(
            enterprise_totals(&json!({"limitNum":-1,"credit":30})).unwrap(),
            (None, 30.0, None, true)
        );
        assert_eq!(
            enterprise_totals(&json!({"limitNum":0,"credit":0})).unwrap(),
            (Some(0.0), 0.0, Some(0.0), false)
        );
        assert_eq!(
            enterprise_totals(&json!({"limit_num":"100","used_num":"120"})).unwrap(),
            (Some(100.0), 120.0, Some(0.0), false)
        );
        for bad in [
            json!({}),
            json!({"limitNum":10}),
            json!({"limitNum":null,"credit":0}),
            json!({"limitNum":-2,"credit":0}),
            json!({"limitNum":10,"credit":-1}),
            json!({"limitNum":100,"limit_num":200,"credit":1}),
            json!({"limitNum":100,"credit":1,"used_num":2}),
        ] {
            assert!(enterprise_totals(&bad).is_err());
        }
    }
}

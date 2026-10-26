//! Trae CN native check-in contract; observation is POST, mutation is a separate POST.
//! Fixed production host, bounded responses, no retry or invented device identity.
use crate::{benefits::ClaimPlatform, client_session::SessionCredential};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use futures_util::StreamExt;
use serde_json::{json, Value};

pub(crate) const DEVICE_CONTEXT_REQUIRED: &str = "Trae 会话缺少可直连设备上下文，领取已暂停；请到客户端接入页关联本网关 Trae 独立登录，或在账号权益中显式重新复用客户端登录；已有旧预约时仍需等新活动窗口；未启动客户端";

fn validate_status(value: &Value) -> Result<(), String> {
    if value.get("code").and_then(Value::as_i64) != Some(0)
        || !value.get("enable").is_some_and(Value::is_boolean)
        || !value.get("checked_in").is_some_and(Value::is_boolean)
    {
        return Err("Trae 签到状态未通过协议校验".into());
    }
    Ok(())
}

fn credits(value: &Value) -> Option<f64> {
    value
        .get("credits")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n > 0.0)
}

fn daily_window(now: DateTime<Utc>) -> (String, i64, i64) {
    let zone = FixedOffset::east_opt(28800).expect("valid Beijing offset");
    let day = now.with_timezone(&zone).date_naive();
    let start = zone
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("valid midnight"))
        .single()
        .expect("fixed timezone")
        .timestamp();
    (
        format!("daily_{}", day.format("%Y-%m-%d")),
        start,
        start + 86400,
    )
}

pub fn normalize_status(value: &Value, now: DateTime<Utc>) -> Result<Value, String> {
    validate_status(value)?;
    let (id, start, end) = daily_window(now);
    let enabled = value["enable"] == true;
    let checked = value["checked_in"] == true;
    Ok(
        json!({"claimable":enabled && !checked,"campaigns":if enabled {vec![json!({"campaignId":id,"campaignKey":"trae_daily_checkin","claimStatus":if checked{"CLAIMED"}else{"CLAIMABLE"},"actionType":"CLAIM_BENEFIT","benefit":{"amount":credits(value),"kind":"CREDITS"},"startAt":start,"endAt":end})]}else{vec![]}}),
    )
}

async fn post(
    client: &reqwest::Client,
    base: &str,
    route: &str,
    token: &str,
    source: u8,
    guard: Option<&crate::benefits::ClaimDispatchGuard>,
) -> Result<Value, String> {
    let request = client
        .post(format!("{base}/trae/api/v2/ug/checkin_credits/{route}"))
        .headers(crate::client_session::request_headers(
            token,
            ClaimPlatform::TraeCn,
        )?)
        .json(&json!({"req_source":source}));
    if let Some(guard) = guard {
        guard.check_for(token).await?;
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Trae 签到连接失败或超时；未自动重发领取".to_owned())?;
    let status = response.status();
    if response.content_length().is_some_and(|n| n > 512 * 1024) {
        return Err("平台权益响应超过 512 KiB 上限".into());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "读取平台权益响应失败")?;
        if bytes.len().saturating_add(chunk.len()) > 512 * 1024 {
            return Err("平台权益响应超过 512 KiB 上限".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if crate::client_session::contains_credential(&bytes, token, ClaimPlatform::TraeCn) {
        return Err("平台权益响应包含敏感凭据，已拒绝展示".into());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "平台权益响应不是合法 JSON")?;
    if !status.is_success() {
        return Err(crate::benefits::http_error(status.as_u16(), &value));
    }
    Ok(value)
}

pub async fn request(
    token: &str,
    proxy: Option<&str>,
    campaign_id: Option<&str>,
) -> Result<Value, String> {
    request_at(token, proxy, campaign_id, "https://api.trae.cn").await
}

/// Every saved source carries an epoch guard, not only verified native sources.
/// The old client executor cannot recheck this application's source after its
/// internal status await. Missing device context therefore needs explicit
/// migration; it must never silently enter that unguarded mutation path.
pub(crate) async fn request_checked(
    token: &str,
    proxy: Option<&str>,
    campaign_id: &str,
    guard: &crate::benefits::ClaimDispatchGuard,
) -> Result<Value, String> {
    request_with_native_guarded(
        token,
        proxy,
        Some(campaign_id),
        "https://api.trae.cn",
        Some(guard),
        |_, _| async { Err(DEVICE_CONTEXT_REQUIRED.into()) },
    )
    .await
}

#[cfg(test)]
pub(crate) async fn request_at_for_test(
    token: &str,
    campaign_id: Option<&str>,
    base: &str,
) -> Result<Value, String> {
    request_at(token, None, campaign_id, base).await
}

#[cfg(test)]
pub(crate) async fn request_checked_at_for_test(
    token: &str,
    campaign_id: &str,
    base: &str,
    guard: &crate::benefits::ClaimDispatchGuard,
) -> Result<Value, String> {
    request_with_native_guarded(
        token,
        None,
        Some(campaign_id),
        base,
        Some(guard),
        |_, _| async {
            panic!("owned guarded native HTTP test must not start a client");
            #[allow(unreachable_code)]
            Ok(Value::Null)
        },
    )
    .await
}

async fn request_at(
    token: &str,
    proxy: Option<&str>,
    campaign_id: Option<&str>,
    base: &str,
) -> Result<Value, String> {
    request_with_native(token, proxy, campaign_id, base, |session, id| async move {
        crate::benefit_login::native_trae_claim(&session, &id, false).await
    })
    .await
}

async fn request_with_native<F, Fut>(
    token: &str,
    proxy: Option<&str>,
    campaign_id: Option<&str>,
    base: &str,
    native: F,
) -> Result<Value, String>
where
    F: FnOnce(SessionCredential, String) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    request_with_native_guarded(token, proxy, campaign_id, base, None, native).await
}

async fn request_with_native_guarded<F, Fut>(
    token: &str,
    proxy: Option<&str>,
    campaign_id: Option<&str>,
    base: &str,
    guard: Option<&crate::benefits::ClaimDispatchGuard>,
    native: F,
) -> Result<Value, String>
where
    F: FnOnce(SessionCredential, String) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let session = SessionCredential::parse(token, ClaimPlatform::TraeCn)?;
    if let Some(guard) = guard {
        guard.check_for(token).await?;
        if !session.has_trae_device_context() {
            return Err(DEVICE_CONTEXT_REQUIRED.into());
        }
    }
    let source = session.request_source.ok_or("Trae 客户端来源无效")?;
    let client = crate::outbound::apply_proxy(
        reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(6))
            .timeout(std::time::Duration::from_secs(20)),
        proxy,
    )
    .map_err(|_| "权益代理配置无效")?
    .build()
    .map_err(|_| "无法初始化权益请求")?;
    let before = post(&client, base, "status", token, source, guard).await?;
    if let Some(guard) = guard {
        guard.check_for(token).await?;
    }
    validate_status(&before)?;
    let now = Utc::now();
    let Some(id) = campaign_id else {
        return normalize_status(&before, now);
    };
    let (expected_id, _, _) = daily_window(now);
    if id != expected_id || before["enable"] != true {
        return Err("Trae 签到活动已过期或未启用；未发起领取".into());
    }
    if before["checked_in"] != true {
        // Select the transport before mutation. A failed/ambiguous direct claim
        // must never trigger a second claim through the client.
        if !session.has_trae_device_context() {
            return native(session, expected_id).await;
        }
        let claim = post(&client, base, "claim", token, source, guard).await?;
        if !claim.is_object() || (claim.get("code").is_some() && claim["code"].as_i64() != Some(0))
        {
            return Err("Trae 直连领取被平台拒绝；未自动重发".into());
        }
        let after = post(&client, base, "status", token, source, guard).await?;
        validate_status(&after)?;
        if after["checked_in"] != true || daily_window(Utc::now()).0 != expected_id {
            return Err("Trae 直连领取未获同日状态确认；未自动重发".into());
        }
        let amount = credits(&after).ok_or("Trae 直连领取数量未获确认；未自动重发")?;
        return Ok(
            json!({"campaignId":id,"campaignKey":"trae_daily_checkin","status":"CLAIMED","replayed":false,"benefit":{"amount":amount,"kind":"CREDITS"},"claimedAt":Utc::now().to_rfc3339(),"expiresAt":null}),
        );
    }
    // No expiration is invented when the platform does not provide one.
    Ok(
        json!({"campaignId":id,"campaignKey":"trae_daily_checkin","status":"CLAIMED","replayed":true,"benefit":{"amount":credits(&before),"kind":"CREDITS"},"claimedAt":null,"expiresAt":null}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    #[tokio::test]
    async fn full_device_context_claims_directly_once_and_never_falls_back_after_failure() {
        use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
        #[derive(Clone, Default)]
        struct StateData {
            claims: Arc<AtomicUsize>,
            observations: Arc<AtomicUsize>,
            failure: Arc<AtomicUsize>,
        }
        fn verify(headers: &HeaderMap, body: &Value) {
            assert_eq!(
                headers["authorization"],
                "Cloud-IDE-JWT synthetic-context-secret"
            );
            assert_eq!(headers["x-device-id"], "1111222233334444");
            assert_eq!(body["req_source"], 1);
        }
        async fn status(
            State(s): State<StateData>,
            h: HeaderMap,
            Json(b): Json<Value>,
        ) -> Json<Value> {
            verify(&h, &b);
            s.observations.fetch_add(1, Ordering::SeqCst);
            Json(
                json!({"code":0,"enable":true,"checked_in":s.claims.load(Ordering::SeqCst)>0&&s.failure.load(Ordering::SeqCst)==0,"credits":100}),
            )
        }
        async fn claim(
            State(s): State<StateData>,
            h: HeaderMap,
            Json(b): Json<Value>,
        ) -> Json<Value> {
            verify(&h, &b);
            s.claims.fetch_add(1, Ordering::SeqCst);
            Json(if s.failure.load(Ordering::SeqCst) == 1 {
                json!({"code":9074})
            } else {
                json!({"code":0})
            })
        }
        let state = StateData::default();
        let router = Router::new()
            .route("/trae/api/v2/ug/checkin_credits/status", post(status))
            .route("/trae/api/v2/ug/checkin_credits/claim", post(claim))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let token=json!({"schema_version":1,"platform":"trae_cn","request_source":1,"headers":{"authorization":"Cloud-IDE-JWT synthetic-context-secret","x-device-id":"1111222233334444"}}).to_string();
        let id = daily_window(Utc::now()).0;
        let native = |_: SessionCredential, _: String| async {
            panic!("full device context must not start the client");
            #[allow(unreachable_code)]
            Ok(Value::Null)
        };
        assert_eq!(
            request_with_native(&token, None, None, &base, native)
                .await
                .unwrap()["claimable"],
            true
        );
        assert_eq!(state.claims.load(Ordering::SeqCst), 0);
        let before = state.observations.load(Ordering::SeqCst);
        let result = request_with_native(&token, None, Some(&id), &base, native)
            .await
            .unwrap();
        assert_eq!(result["replayed"], false);
        assert_eq!(result["benefit"]["amount"], 100.0);
        assert_eq!(state.claims.load(Ordering::SeqCst), 1);
        assert_eq!(state.observations.load(Ordering::SeqCst), before + 2);
        assert_eq!(
            request_with_native(&token, None, Some(&id), &base, native)
                .await
                .unwrap()["replayed"],
            true
        );
        assert_eq!(state.claims.load(Ordering::SeqCst), 1);
        for mode in [1, 2] {
            state.claims.store(0, Ordering::SeqCst);
            state.failure.store(mode, Ordering::SeqCst);
            assert!(request_with_native(&token, None, Some(&id), &base, native)
                .await
                .is_err());
            assert_eq!(state.claims.load(Ordering::SeqCst), 1);
        }
        state.claims.store(0, Ordering::SeqCst);
        assert!(
            request_with_native(&token, None, Some("daily_2000-01-01"), &base, native)
                .await
                .is_err()
        );
        assert_eq!(state.claims.load(Ordering::SeqCst), 0);
        server.abort();
    }
    #[tokio::test]
    async fn native_post_contract_verifies_mutation_and_never_retries_claim() {
        use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
        #[derive(Clone, Default)]
        struct TestState {
            claims: Arc<AtomicUsize>,
            observations: Arc<AtomicUsize>,
            failure: Arc<AtomicUsize>,
        }
        async fn status(
            State(s): State<TestState>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            assert_eq!(
                headers["authorization"],
                "Cloud-IDE-JWT synthetic-native-secret"
            );
            assert_eq!(body["req_source"], 1);
            s.observations.fetch_add(1, Ordering::SeqCst);
            Json(
                json!({"code":0,"enable":true,"checked_in":s.claims.load(Ordering::SeqCst)>0&&s.failure.load(Ordering::SeqCst)==0,"credits":100}),
            )
        }
        async fn claim() -> Json<Value> {
            panic!("production must not make an unverified JWT-only HTTP claim");
        }
        let state = TestState::default();
        let router = Router::new()
            .route("/trae/api/v2/ug/checkin_credits/status", post(status))
            .route("/trae/api/v2/ug/checkin_credits/claim", post(claim))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let token=json!({"schema_version":1,"platform":"trae_cn","request_source":1,"headers":{"authorization":"Cloud-IDE-JWT synthetic-native-secret"}}).to_string();
        let base = format!("http://{address}");
        let id = daily_window(Utc::now()).0;
        let native = |session: SessionCredential, id: String| {
            let state = state.clone();
            async move {
                assert_eq!(session.request_source, Some(1));
                state.claims.fetch_add(1, Ordering::SeqCst);
                if state.failure.load(Ordering::SeqCst) != 0 {
                    return Err("native rejected".into());
                }
                Ok(
                    json!({"campaignId":id,"campaignKey":"trae_daily_checkin","status":"CLAIMED","replayed":false,"benefit":{"amount":100.0,"kind":"CREDITS"}}),
                )
            }
        };
        let status = request_with_native(&token, None, None, &base, native)
            .await
            .unwrap();
        assert_eq!(status["claimable"], true);
        assert_eq!(state.claims.load(Ordering::SeqCst), 0);
        let value = request_with_native(&token, None, Some(&id), &base, native)
            .await
            .unwrap();
        assert_eq!(value["replayed"], false);
        assert_eq!(value["benefit"]["amount"], 100.0);
        assert_eq!(state.claims.load(Ordering::SeqCst), 1);
        assert_eq!(state.observations.load(Ordering::SeqCst), 2);
        let replay = request_with_native(&token, None, Some(&id), &base, native)
            .await
            .unwrap();
        assert_eq!(replay["replayed"], true);
        assert_eq!(state.claims.load(Ordering::SeqCst), 1);
        state.claims.store(0, Ordering::SeqCst);
        state.failure.store(1, Ordering::SeqCst);
        let count = state.observations.load(Ordering::SeqCst);
        assert!(request_with_native(&token, None, Some(&id), &base, native)
            .await
            .is_err());
        assert_eq!(state.claims.load(Ordering::SeqCst), 1);
        assert_eq!(state.observations.load(Ordering::SeqCst), count + 1);
        // A stale day is rejected before any claim even when the server says claimable.
        assert!(
            request_with_native(&token, None, Some("daily_2000-01-01"), &base, native)
                .await
                .is_err()
        );
        assert_eq!(state.claims.load(Ordering::SeqCst), 1);
        server.abort();
    }
    #[test]
    fn status_uses_beijing_day_and_preserves_disabled_unknown_and_claimed() {
        let now = DateTime::parse_from_rfc3339("2026-10-14T00:05:00+08:00")
            .unwrap()
            .with_timezone(&Utc);
        let value = normalize_status(
            &json!({"code":0,"enable":true,"checked_in":false,"credits":100}),
            now,
        )
        .unwrap();
        assert_eq!(value["campaigns"][0]["campaignId"], "daily_2026-10-14");
        assert_eq!(value["campaigns"][0]["startAt"], 1791907200_i64);
        assert_eq!(value["claimable"], true);
        assert_eq!(
            normalize_status(&json!({"code":0,"enable":true,"checked_in":true}), now).unwrap()
                ["claimable"],
            false
        );
        assert!(
            normalize_status(&json!({"code":0,"enable":false,"checked_in":false}), now).unwrap()
                ["campaigns"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            normalize_status(&json!({"code":0,"enable":true,"checked_in":false}), now).unwrap()
                ["campaigns"][0]["benefit"]["amount"],
            Value::Null
        );
        for bad in [
            json!({"code":401,"enable":true,"checked_in":false}),
            json!({"code":0,"enable":true}),
            json!({"code":0,"enable":"true","checked_in":false}),
        ] {
            assert!(normalize_status(&bad, now).is_err());
        }
    }
}

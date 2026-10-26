//! Explicit web refresh; a durable source-generation reservation survives lost
//! invokes. No model/free authority or native-device credentials are produced.
use super::*;
use std::{future::Future, sync::atomic::Ordering};

pub(super) fn eligible(session: &WebSession) -> bool {
    session
        .pair
        .refresh
        .as_deref()
        .is_some_and(contract::valid_token)
        && session
            .pair
            .refresh_expires_at_ms
            .is_some_and(|v| v > now())
}

struct Captured {
    region: WebRegion,
    account: String,
    raw: String,
    session: WebSession,
    client: reqwest::Client,
    origin: Url,
    epoch: u64,
}

async fn no_login(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    region: WebRegion,
    account: &str,
) -> Result<(), String> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name LIKE ? LIMIT 65")
            .bind(web_key(region, "pending", "%"))
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| "无法核验Trae网页等待")?;
    if rows.len() > 64 {
        return Err("Trae网页预约超过核验上限".into());
    }
    for raw in rows {
        let p: WebPending = decode(&raw)?;
        if p.region == region
            && p.account == account
            && p.deadline > now()
            && ["waiting", "exchanging"].contains(&p.state.as_str())
        {
            return Err("该Trae网页账号正在登录，请先完成或取消等待".into());
        }
    }
    Ok(())
}

impl TraeAuth {
    pub async fn web_refresh(
        &self,
        region: WebRegion,
        account: &str,
        consent: bool,
    ) -> Result<WebLoginStatus, String> {
        if !id_valid(account) || !consent {
            return Err("刷新Trae网页登录需要有效本机账号名称及明确授权，未发起刷新".into());
        }
        let (captured, reservation) = self.core.reserve_web_refresh(region, account).await?;
        // A dropped future or timeout retains the already committed unknown
        // reservation; it cannot silently become a safe-to-replay refresh.
        tokio::time::timeout(
            std::time::Duration::from_secs(40),
            self.core.finish_web_refresh(&captured, &reservation),
        )
        .await
        .map_err(|_| "Trae网页刷新结果未知，未自动重试")??;
        self.web_status(region, account).await
    }
}

impl Core {
    async fn reserve_web_refresh(
        &self,
        region: WebRegion,
        account: &str,
    ) -> Result<(Captured, String), String> {
        let (client, epoch) = {
            let client = self.http.read();
            (client.clone()?, self.model_epoch.load(Ordering::SeqCst))
        };
        let _gate = self.gate.lock().await;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "无法预约Trae网页刷新")?;
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(web_key(region, "session", account))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法锁定Trae网页刷新")?;
        let raw: String = sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
            .bind(web_key(region, "session", account))
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| "无法读取Trae网页会话")?
            .ok_or("请先完成该区Trae网页登录")?;
        let session: WebSession = decode(&raw)?;
        session.validate()?;
        if session.region != region || !eligible(&session) {
            return Err("Trae网页刷新凭据缺失、到期或期限未知，未发起刷新".into());
        }
        let origin = self.web_transport_origin(WebOrigin::parse(region, &session.issuer)?)?;
        no_login(&mut tx, region, account).await?;
        let previous: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(web_key(region, "refresh", account))
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "无法核验Trae网页刷新预约")?;
        if let Some(previous) = previous {
            let p: RefreshAttempt = decode(&previous)?;
            if p.generation == generation(&raw) {
                return Err(
                    "本次Trae网页会话已有刷新结果或未知占用，未重复发送；请读取状态或重新网页登录"
                        .into(),
                );
            }
        }
        let reservation = encode(&RefreshAttempt {
            generation: generation(&raw),
            id: uuid::Uuid::new_v4().to_string(),
            state: "pending_unknown".into(),
        })?;
        let at = chrono::Utc::now();
        sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO UPDATE SET value_enc=excluded.value_enc,updated_at=excluded.updated_at")
            .bind(web_key(region,"refresh",account)).bind(&reservation).bind(at).bind(at)
            .execute(&mut *tx).await.map_err(|_| "无法保存Trae网页刷新预约")?;
        tx.commit().await.map_err(|_| "无法提交Trae网页刷新预约")?;
        Ok((
            Captured {
                region,
                account: account.into(),
                raw,
                session,
                client,
                origin,
                epoch,
            },
            reservation,
        ))
    }

    async fn web_refresh_current(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        source: &Captured,
        expected: &str,
    ) -> Result<(), String> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name,value_enc FROM app_secrets WHERE name IN (?,?)")
                .bind(web_key(source.region, "session", &source.account))
                .bind(web_key(source.region, "refresh", &source.account))
                .fetch_all(&mut **tx)
                .await
                .map_err(|_| "无法核验Trae网页刷新代次")?;
        for (name, raw) in [
            (
                web_key(source.region, "session", &source.account),
                source.raw.as_str(),
            ),
            (web_key(source.region, "refresh", &source.account), expected),
        ] {
            if rows.iter().find(|v| v.0 == name).map(|v| v.1.as_str()) != Some(raw) {
                return Err("Trae网页会话或刷新预约已改变，未覆盖迟到结果".into());
            }
        }
        if !eligible(&source.session) || self.model_epoch.load(Ordering::SeqCst) != source.epoch {
            return Err("Trae网页刷新来源已失效，未自动重试".into());
        }
        no_login(tx, source.region, &source.account).await
    }

    async fn web_refresh_post(
        &self,
        source: &Captured,
        expected: &str,
        path: &str,
        token: &str,
        body: &Value,
    ) -> Result<Value, String> {
        let mut operation = Box::pin(http::post(
            &source.client,
            &source.origin,
            path,
            token,
            body,
        ));
        let waker = std::future::poll_fn(|cx| std::task::Poll::Ready(cx.waker().clone())).await;
        let first = {
            let _gate = self.gate.lock().await;
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|_| "无法核验Trae网页刷新发送")?;
            sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
                .bind(web_key(source.region, "session", &source.account))
                .execute(&mut *tx)
                .await
                .map_err(|_| "无法锁定Trae网页刷新发送")?;
            self.web_refresh_current(&mut tx, source, expected).await?;
            let first = {
                let _proxy = self.http.read();
                if self.model_epoch.load(Ordering::SeqCst) != source.epoch {
                    return Err("Trae网页刷新代理代次已改变".into());
                }
                operation
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(&waker))
            };
            tx.rollback()
                .await
                .map_err(|_| "无法释放Trae网页刷新本地检查")?;
            first
        };
        match first {
            std::task::Poll::Ready(v) => v,
            std::task::Poll::Pending => operation.await,
        }
    }

    async fn finish_web_refresh(&self, source: &Captured, expected: &str) -> Result<(), String> {
        let body = json!({"ClientID":contract::CLIENT_ID,"RefreshToken":source.session.pair.refresh,"ClientSecret":"-","UserID":""});
        let packet = self
            .web_refresh_post(source, expected, EXCHANGE_WEB, "", &body)
            .await?;
        let pair = contract::parse_pair(&packet, Some(&source.session.pair), now())?;
        if pair.access_expires_at_ms.is_some_and(|v| v <= now()) {
            return Err("Trae网页刷新返回已到期令牌，未保存".into());
        }
        let body = json!({"ReqSource":"IDE","IDEVersion":contract::VERSION});
        let principal = self
            .web_refresh_post(source, expected, contract::PRINCIPAL, &pair.access, &body)
            .await?;
        let mut secrets = vec![source.session.pair.access.as_str(), pair.access.as_str()];
        secrets.extend(source.session.pair.refresh.as_deref());
        secrets.extend(pair.refresh.as_deref());
        let mut identity = contract::identity(&principal, &secrets)?;
        if identity.uid != source.session.uid {
            return Err("Trae网页刷新主体改变，未替换原账号".into());
        }
        identity.snapshot.identity_hash = Some(source.region.principal_hash(&identity.uid));
        let session = WebSession {
            region: source.region,
            issuer: source.session.issuer.clone(),
            client_id: contract::CLIENT_ID.into(),
            uid: identity.uid,
            pair,
            snapshot: identity.snapshot,
            // Missing new metadata clears the prior observation; it must not
            // inherit an old country's apparent freshness after refresh.
            store_country: principal_country(&principal),
        };
        session.validate()?;
        let replacement = encode(&session)?;
        let _gate = self.gate.lock().await;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "无法保存Trae网页刷新")?;
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(web_key(source.region, "session", &source.account))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法锁定Trae网页刷新保存")?;
        self.web_refresh_current(&mut tx, source, expected).await?;
        if session
            .pair
            .access_expires_at_ms
            .is_some_and(|v| v <= now())
        {
            return Err("Trae网页刷新令牌在保存前已到期".into());
        }
        let mut done: RefreshAttempt = decode(expected)?;
        done.state = "completed".into();
        for (name, value) in [
            (
                web_key(source.region, "session", &source.account),
                replacement,
            ),
            (
                web_key(source.region, "refresh", &source.account),
                encode(&done)?,
            ),
        ] {
            sqlx::query("UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=?")
                .bind(value)
                .bind(chrono::Utc::now())
                .bind(name)
                .execute(&mut *tx)
                .await
                .map_err(|_| "无法原子保存Trae网页刷新")?;
        }
        tx.commit()
            .await
            .map_err(|_| "无法提交Trae网页刷新，未自动重试".into())
    }
}

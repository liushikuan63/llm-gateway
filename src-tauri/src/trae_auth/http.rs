use super::contract::{self, Pair};
use futures_util::StreamExt;
use reqwest::{Client, Url};
use serde_json::{json, Value};
use std::time::Duration;

pub(super) fn client(proxy: Option<&str>) -> Result<Client, String> {
    crate::outbound::apply_proxy(
        Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20)),
        proxy,
    )?
    .build()
    .map_err(|_| "Trae认证连接初始化失败".into())
}
pub(super) async fn post(
    client: &Client,
    origin: &Url,
    path: &str,
    token: &str,
    body: &Value,
) -> Result<Value, String> {
    if ![contract::EXCHANGE, contract::PRINCIPAL].contains(&path) {
        return Err("Trae认证操作尚未验证".into());
    }
    let url = origin.join(path).map_err(|_| "Trae认证目标无效")?;
    let response = client
        .post(url)
        .header("x-cloudide-token", token)
        .json(body)
        .send()
        .await
        .map_err(|_| "Trae认证连接失败或超时，未自动重试")?;
    if response.status().as_u16() != 200 {
        return Err(format!(
            "Trae认证返回HTTP{}，未自动重试",
            response.status().as_u16()
        ));
    }
    if response.content_length().is_some_and(|v| v > 512 * 1024) {
        return Err("Trae认证响应超过上限".into());
    }
    let mut bytes = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| "Trae认证响应读取失败")?;
        if bytes.len().saturating_add(chunk.len()) > 512 * 1024 {
            return Err("Trae认证响应超过上限".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Trae认证响应格式无效".into())
}
pub(super) async fn principal(
    client: &Client,
    origin: &Url,
    pair: &Pair,
    secrets: &[&str],
) -> Result<contract::Identity, String> {
    let value = post(
        client,
        origin,
        contract::PRINCIPAL,
        &pair.access,
        &json!({"ReqSource":"IDE","IDEVersion":contract::VERSION}),
    )
    .await?;
    contract::identity(&value, secrets)
}

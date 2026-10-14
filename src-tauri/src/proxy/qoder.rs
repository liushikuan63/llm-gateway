//! Independent, account-scoped Qoder HTTP transport. Production origins are fixed.
use crate::{
    domain::{ChatRequest, ChatResponse},
    error::{GatewayError, Result},
    proxy::{upstream::UpstreamEvent, workbuddy::ToolAssembler},
    qoder_auth::{CatalogModel, ModelAuthorization, QoderAuth, Region},
};
use futures_util::{Stream, StreamExt};
use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

type EventStream = Pin<Box<dyn Stream<Item = Result<UpstreamEvent>> + Send>>;
type Slot = Arc<Mutex<Option<Cached>>>;
struct Cached {
    at: Instant,
    models: Option<Vec<CatalogModel>>,
}
pub(crate) struct QoderModelClient {
    auth: parking_lot::RwLock<QoderAuth>,
    http: parking_lot::RwLock<std::result::Result<reqwest::Client, String>>,
    catalogs: parking_lot::Mutex<BTreeMap<String, Slot>>,
    #[cfg(test)]
    test_origin: Option<reqwest::Url>,
}
fn protocol(message: &str) -> GatewayError {
    GatewayError::Protocol(format!("Qoder {message}"))
}
fn account_auth(_: String) -> GatewayError {
    GatewayError::Upstream {
        provider: "Qoder".into(),
        model: "authentication".into(),
        status: 401,
        body: "账号登录或模型认证已失效，请检查账号认证状态；未自动重放".into(),
    }
}
fn network(error: reqwest::Error) -> GatewayError {
    if error.is_timeout() {
        GatewayError::Timeout("Qoder模型网络超时，未自动重试".into())
    } else {
        GatewayError::Upstream {
            provider: "Qoder".into(),
            model: "request".into(),
            status: 502,
            body: "模型连接或响应读取失败，未自动重试".into(),
        }
    }
}
impl QoderModelClient {
    pub(crate) async fn discover_models(
        &self,
        region: Region,
        account: &str,
    ) -> std::result::Result<crate::model_catalog::DiscoveryResponse, String> {
        use crate::model_catalog::{
            ContextSource, DiscoveredModel, DiscoveryResponse, DEFAULT_CONTEXT_WINDOW,
        };
        let service = self.auth.read().clone();
        let authorization = service.prepare_model_authorization(region, account).await?;
        let catalog = self
            .catalog(&service, &authorization)
            .await
            .map_err(|e| e.public_message())?;
        let models = catalog
            .into_iter()
            .filter(|v| v.enabled == Some(true) && v.format == "openai")
            .map(|v| {
                let declared = v
                    .context_window
                    .and_then(|v| i32::try_from(v).ok())
                    .filter(|v| *v > 0);
                DiscoveredModel {
                    id: v.id,
                    name: v.name,
                    context_window: declared.unwrap_or(DEFAULT_CONTEXT_WINDOW),
                    context_source: if declared.is_some() {
                        ContextSource::Provider
                    } else {
                        ContextSource::Default
                    },
                    supports_tools: None,
                    supports_vision: v.vision,
                    supports_thinking: v.thinking,
                    supports_audio: None,
                    supports_video: None,
                    supports_stream: Some(true),
                    model_type: Some(crate::domain::ModelType::Chat),
                    is_free: v.free,
                    price: None,
                    capabilities: None,
                }
            })
            .collect();
        Ok(DiscoveryResponse{base_url:region.model_origin().into(),models,warnings:vec!["Qoder积分倍率不代表现金单价；缺失的上下文窗口使用明确标记的保守默认值，工具及媒体真实能力需单独核验。".into()]})
    }
    pub(crate) fn new(auth: QoderAuth) -> Self {
        Self {
            auth: parking_lot::RwLock::new(auth),
            http: parking_lot::RwLock::new(Self::build_http(None)),
            catalogs: parking_lot::Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            test_origin: None,
        }
    }
    fn build_http(proxy: Option<&str>) -> std::result::Result<reqwest::Client, String> {
        crate::outbound::apply_proxy(
            reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .pool_max_idle_per_host(16),
            proxy,
        )?
        .build()
        .map_err(|_| "Qoder模型连接初始化失败".into())
    }
    pub(crate) fn set_proxy(&self, proxy: Option<&str>) {
        let auth = self.auth.read().with_proxy(proxy);
        let http = Self::build_http(proxy);
        match auth {
            Ok(auth) => {
                *self.auth.write() = auth;
                *self.http.write() = http;
            }
            Err(error) => *self.http.write() = Err(error),
        }
    }
    #[cfg(test)]
    pub(crate) fn with_test_origin(auth: QoderAuth, origin: reqwest::Url) -> Self {
        let mut client = Self::new(auth);
        client.test_origin = Some(origin);
        client
    }
    async fn catalog(
        &self,
        service: &QoderAuth,
        auth: &ModelAuthorization,
    ) -> Result<Vec<CatalogModel>> {
        let key = format!("{:?}\0{}\0{}", auth.region, auth.account, auth.fingerprint);
        let slot = {
            let mut slots = self.catalogs.lock();
            if !slots.contains_key(&key) {
                let prefix = format!("{:?}\0{}\0", auth.region, auth.account);
                slots.retain(|old, _| !old.starts_with(&prefix));
                if slots.len() >= 64 {
                    if let Some(old) = slots.keys().next().cloned() {
                        slots.remove(&old);
                    }
                }
            }
            slots
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        let mut cached = slot.lock().await;
        if let Some(entry) = cached.as_ref().filter(|v| {
            v.at.elapsed()
                < if v.models.is_some() {
                    Duration::from_secs(300)
                } else {
                    Duration::from_secs(1)
                }
        }) {
            service
                .model_authorization_current(auth)
                .await
                .map_err(account_auth)?;
            return entry
                .models
                .clone()
                .ok_or_else(|| protocol("近期目录读取失败，未发送模型请求"));
        }
        let result = service
            .model_catalog(auth.region, &auth.account)
            .await
            .map_err(|_| protocol("目录读取失败，未发送模型请求"));
        // A directory loaded using another generation must not authorize this POST.
        service
            .model_authorization_current(auth)
            .await
            .map_err(account_auth)?;
        *cached = Some(Cached {
            at: Instant::now(),
            models: result.as_ref().ok().cloned(),
        });
        result
    }
    pub(crate) async fn call_stream(
        &self,
        region: Region,
        account: &str,
        request: &ChatRequest,
        model: &str,
        timeout: Duration,
    ) -> Result<EventStream> {
        let deadline = tokio::time::Instant::now() + timeout;
        let service = self.auth.read().clone();
        let http = self.http.read().clone().map_err(GatewayError::Protocol)?;
        let prepare = async {
            let mut auth = service
                .prepare_model_authorization(region, account)
                .await
                .map_err(account_auth)?;
            let catalog = self.catalog(&service, &auth).await?;
            let metadata = catalog.iter().find(|v| v.id == model).ok_or_else(|| {
                GatewayError::ModelNotFound("Qoder当前账号和区域目录没有该模型".into())
            })?;
            let body = crate::qoder_auth::request::build(
                request,
                metadata,
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                chrono::Utc::now().timestamp_millis(),
            )?;
            let (headers, encoded) = service
                .prepare_model_wire_request(&mut auth, &body, model)
                .await
                .map_err(account_auth)?;
            let url=reqwest::Url::parse(&format!("{}/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1",region.model_origin()))
                .map_err(|_|protocol("模型区域地址无效"))?;
            #[cfg(test)]
            let url = if let Some(origin) = &self.test_origin {
                let mut url = origin.clone();
                url.set_path(&format!(
                    "/{}/algo/api/v2/service/pro/sse/agent_chat_generation",
                    if region == Region::Global {
                        "global"
                    } else {
                        "cn"
                    }
                ));
                url.set_query(Some(
                    "FetchKeys=llm_model_result&AgentId=agent_common&Encode=1",
                ));
                url
            } else {
                url
            };
            service
                .model_authorization_current(&auth)
                .await
                .map_err(account_auth)?;
            // The wire body is already encoded. JSON-quoting it again invalidates
            // both the request and its body-bound signature.
            let response = http
                .post(url)
                .headers(headers)
                .header(reqwest::header::USER_AGENT, "LLM-Gateway/0.2.1")
                .timeout(timeout)
                .body(encoded)
                .send()
                .await
                .map_err(network)?;
            service
                .model_authorization_current(&auth)
                .await
                .map_err(account_auth)?;
            let status = response.status().as_u16();
            if status != 200 {
                return Err(GatewayError::Upstream {
                    provider: "Qoder".into(),
                    model: model.into(),
                    status,
                    body: "模型请求被拒绝，未读取或回显认证响应，未自动重试".into(),
                });
            }
            if !response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| {
                    v.split(';')
                        .next()
                        .is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream"))
                })
            {
                return Err(GatewayError::Upstream {
                    provider: "Qoder".into(),
                    model: model.into(),
                    status: 502,
                    body: "模型响应不是SSE，未自动重试".into(),
                });
            }
            Ok((response, auth))
        };
        let (response, auth) = tokio::time::timeout_at(deadline, prepare)
            .await
            .map_err(|_| {
                GatewayError::Timeout("Qoder模型准备或发送超时，未自动重试".into()).no_replay()
            })?
            .map_err(GatewayError::no_replay)?;
        let events = async_stream::try_stream! {
            let mut bytes=response.bytes_stream();
            let mut decoder=crate::qoder_auth::stream::QoderSseDecoder::default();
            let secrets=auth.secrets();
            loop {
                let chunk=tokio::time::timeout_at(deadline,async {
                    service.model_authorization_current(&auth).await.map_err(account_auth)?;
                    let next=bytes.next().await.transpose().map_err(network)?;
                    service.model_authorization_current(&auth).await.map_err(account_auth)?;
                    Ok::<_,GatewayError>(next)
                }).await.map_err(|_|GatewayError::Timeout("Qoder模型响应超时，未自动重试".into()))??;
                match chunk {
                    Some(chunk)=>for event in decoder.feed(&chunk,false,&secrets)? {yield event;},
                    None=>{for event in decoder.feed(&[],true,&secrets)? {yield event;}break;}
                }
                if decoder.is_done(){break;}
            }
        };
        Ok(Box::pin(events.map(|result: Result<UpstreamEvent>| {
            result.map_err(GatewayError::no_replay)
        })))
    }
    pub(crate) async fn call(
        &self,
        region: Region,
        account: &str,
        request: &ChatRequest,
        model: &str,
        timeout: Duration,
    ) -> Result<ChatResponse> {
        collect(
            self.call_stream(region, account, request, model, timeout)
                .await?,
            model,
        )
        .await
    }
}
async fn collect(mut events: EventStream, model: &str) -> Result<ChatResponse> {
    let mut response = ChatResponse {
        id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
        model: model.into(),
        transport: Some("qoder-http".into()),
        ..Default::default()
    };
    let mut tools = ToolAssembler::default();
    let mut done = false;
    while let Some(event) = events.next().await {
        match event? {
            UpstreamEvent::Delta(text) => response.content.push_str(&text),
            UpstreamEvent::ReasoningDelta(text) => response
                .reasoning_content
                .get_or_insert_with(String::new)
                .push_str(&text),
            UpstreamEvent::RefusalDelta(text) => response
                .refusal
                .get_or_insert_with(String::new)
                .push_str(&text),
            UpstreamEvent::ToolCalls(calls) => {
                tools.append(&calls)?;
            }
            UpstreamEvent::Finish { finish_reason } => response.finish_reason = finish_reason,
            UpstreamEvent::Usage(usage) => response.usage = Some(usage),
            UpstreamEvent::Done {
                finish_reason,
                usage,
            } => {
                response.finish_reason = finish_reason;
                response.usage = usage;
                done = true;
            }
            UpstreamEvent::Ping => {}
        }
    }
    if !done {
        return Err(protocol("流没有权威成功结束"));
    }
    let calls = tools.complete()?;
    if !calls.is_empty() {
        response.tool_calls = Some(calls);
    }
    Ok(response)
}

#[cfg(test)]
#[path = "qoder_tests.rs"]
mod tests;

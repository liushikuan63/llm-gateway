//! One evidenced native profile. No caller-selected endpoints or headers.
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) const ORIGIN: &str = "https://trae-api-cn.mchost.guru";
pub(crate) const CATALOG_PATH: &str = "/api/ide/v1/get_detail_param";
pub(crate) const MODEL_PATH: &str = "/api/agent/v3/llm_utils_chat";
pub(crate) const APP_ID: &str = "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8";
pub(crate) const VERSION_CODE: &str = "20260212";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TraeRegion {
    Cn,
    Global,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum TraeProfileId {
    #[serde(rename = "cn-ordinary-3.4.3-aicc20260212-v1")]
    CnOrdinary343Aicc20260212V1,
    #[serde(rename = "cn-web-raw-v1")]
    CnWebRawV1,
    #[serde(rename = "global-web-raw-sg-v1")]
    GlobalWebRawSgV1,
}
impl TraeProfileId {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CnOrdinary343Aicc20260212V1 => "cn-ordinary-3.4.3-aicc20260212-v1",
            Self::CnWebRawV1 => "cn-web-raw-v1",
            Self::GlobalWebRawSgV1 => "global-web-raw-sg-v1",
        }
    }
    pub(crate) fn is_web(self) -> bool {
        self != Self::CnOrdinary343Aicc20260212V1
    }
    pub(crate) fn region(self) -> TraeRegion {
        match self {
            Self::GlobalWebRawSgV1 => TraeRegion::Global,
            _ => TraeRegion::Cn,
        }
    }
    pub(crate) fn origin(self) -> &'static str {
        match self {
            Self::GlobalWebRawSgV1 => "https://coresg-normal.trae.ai",
            _ => ORIGIN,
        }
    }
    pub(crate) fn function(self) -> SourceFunction {
        if self.is_web() {
            SourceFunction::ChatV3
        } else {
            SourceFunction::SoloWorkRemote
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceFunction {
    SoloWorkRemote,
    ChatV3,
}
impl SourceFunction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SoloWorkRemote => "solo_work_remote",
            Self::ChatV3 => "chat_v3",
        }
    }
    pub(crate) fn catalog_body(self) -> Value {
        json!({"function":self.as_str(),"config_names":null,"need_prompt":false,
            "current_config_info":null,"poly_prompt":true,"mode_type":null,"agent_type":null})
    }
}
#[derive(Clone, Copy)]
pub(crate) enum NativeOperation {
    Catalog,
    Model,
}
/// Exact raw-directory identity, never inferred from a display name or pool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawModelIdentity {
    pub model_name: String,
    pub config_source: u32,
}
impl RawModelIdentity {
    pub(crate) fn valid(&self) -> bool {
        self.config_source == 1
            && !self.model_name.is_empty()
            && self.model_name.len() <= 256
            && !self.model_name.chars().any(char::is_control)
            && !self.model_name.contains(['<', '>'])
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct TraeModelBinding {
    pub region: TraeRegion,
    pub account_id: String,
    pub principal_hash: String,
    pub profile_id: TraeProfileId,
}
pub(super) fn catalog_url(test_origin: Option<&Url>) -> Result<Url, String> {
    #[cfg(test)]
    if let Some(origin) = test_origin {
        if origin.scheme() != "http"
            || origin.host_str() != Some("127.0.0.1")
            || !origin.port().is_some_and(|v| v != 0)
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
        {
            return Err("native_catalog_test_transport_invalid".into());
        }
        return origin
            .join(CATALOG_PATH)
            .map_err(|_| "native_profile_invalid".into());
    }
    #[cfg(not(test))]
    let _ = test_origin;
    Url::parse(ORIGIN)
        .and_then(|url| url.join(CATALOG_PATH))
        .map_err(|_| "native_profile_invalid".into())
}
#[cfg(test)]
pub(crate) fn is_catalog_url(url: &Url) -> bool {
    if url.as_str() == format!("{ORIGIN}{CATALOG_PATH}") {
        return true;
    }
    #[cfg(test)]
    {
        let mut origin = url.clone();
        origin.set_path("/");
        catalog_url(Some(&origin)).is_ok_and(|expected| expected == *url)
    }
    #[cfg(not(test))]
    false
}
#[cfg(test)]
pub(super) fn model_url(test_origin: Option<&Url>) -> Result<Url, String> {
    let mut url = catalog_url(test_origin)?;
    url.set_path(MODEL_PATH);
    Ok(url)
}
#[cfg(test)]
pub(crate) fn is_model_url(url: &Url) -> bool {
    let mut catalog = url.clone();
    catalog.set_path(CATALOG_PATH);
    url.path() == MODEL_PATH && is_catalog_url(&catalog)
}
pub(crate) fn operation_url(
    profile: TraeProfileId,
    operation: NativeOperation,
    test_origin: Option<&Url>,
) -> Result<Url, String> {
    let mut url = catalog_url(test_origin)?;
    if !cfg!(test) || test_origin.is_none() {
        url = Url::parse(profile.origin()).map_err(|_| "native_profile_invalid")?;
    }
    url.set_path(match operation {
        NativeOperation::Catalog => CATALOG_PATH,
        NativeOperation::Model if profile.is_web() => "/api/ide/v2/llm_raw_chat",
        NativeOperation::Model => MODEL_PATH,
    });
    if profile.is_web() && matches!(operation, NativeOperation::Catalog) {
        url.query_pairs_mut().extend_pairs([
            ("function", "chat_v3"),
            ("need_prompt", "false"),
            ("poly_prompt", "true"),
        ]);
    }
    Ok(url)
}
pub(crate) fn operation_url_matches(
    profile: TraeProfileId,
    operation: NativeOperation,
    url: &Url,
) -> bool {
    if operation_url(profile, operation, None).is_ok_and(|expected| expected == *url) {
        return true;
    }
    #[cfg(test)]
    {
        let mut origin = url.clone();
        origin.set_path("/");
        origin.set_query(None);
        operation_url(profile, operation, Some(&origin)).is_ok_and(|expected| expected == *url)
    }
    #[cfg(not(test))]
    false
}
/// JS encodeURIComponent's exact UTF-8 byte allowlist, including !'()*.
pub(crate) fn encode_component(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            result.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}

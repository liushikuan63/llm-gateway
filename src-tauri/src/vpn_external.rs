//! 外部 VPN（魔戒 mojie）只读探测。
//!
//! 与 `vpn` 模块**完全独立**：`vpn` 管的是本应用自己启动的 Mihomo 内核；
//! 这里只做「读」——读用户自己的 mojie 客户端，不启动、不停止、不修改它的任何配置。
//!
//! 数据来源都是 mojie 内置 Mihomo 内核的 RESTful API：
//!   GET /version                 → 内核版本
//!   GET /proxies                 → 节点列表；mojie 把「剩余流量：X GB」「套餐到期：…」
//!                                  伪装成节点塞在里面，额度与到期就在这里
//!   GET /proxies/<name>/delay    → 主动测某节点延迟
//!   GET /traffic                 → 实时上下行（流式，取前两帧）
//!
//! API 地址不硬编码：从 mojie 自己的 config.yaml 读 `external-controller`。
//! 注意本文件**不使用 regex**（Cargo.toml 没有该依赖），文本解析全部手写。

use futures_util::StreamExt;
use reqwest::{redirect::Policy, Client, Url};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// mojie 配置目录相对 `%APPDATA%` 的位置（Windows 上 dirs::config_dir() 即 Roaming）。
const MOJIE_DIR: &str = "mojie";
const MOJIE_CONFIG: &str = "config.yaml";
const MOJIE_APP_CONFIG: &str = "config.json";

/// 订阅流量用量（标准 `Subscription-Userinfo` 响应头，FlClash / v2rayN 同款做法）。
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct SubscriptionUsage {
    /// 已用上行（字节）。
    pub upload_bytes: Option<u64>,
    /// 已用下行（字节）。
    pub download_bytes: Option<u64>,
    /// 总流量（字节）。
    pub total_bytes: Option<u64>,
    /// 到期时间戳（秒）。mojie 实测为空，故为 Option。
    pub expire_at: Option<u64>,
}

impl SubscriptionUsage {
    /// 剩余字节数 = total - (upload + download)，三项齐全才给值。
    pub fn remaining_bytes(&self) -> Option<u64> {
        match (self.total_bytes, self.upload_bytes, self.download_bytes) {
            (Some(total), Some(up), Some(down)) => total.checked_sub(up.saturating_add(down)),
            _ => None,
        }
    }
}

/// 解析 `Subscription-Userinfo` 头的值，例如
/// `upload=21606895115; download=22775131682; total=805306368000; expire=`。
///
/// 按 `;` 分段、`=` 切键值；未知键忽略，解析不了的数字也忽略（宁可少给，不给错的）。
pub fn parse_subscription_userinfo(value: &str) -> SubscriptionUsage {
    let mut usage = SubscriptionUsage::default();
    for part in value.split(';') {
        let Some((key, raw)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let number = raw.trim().parse::<u64>().ok();
        match key.as_str() {
            "upload" => usage.upload_bytes = number,
            "download" => usage.download_bytes = number,
            "total" => usage.total_bytes = number,
            "expire" => usage.expire_at = number,
            _ => {}
        }
    }
    usage
}

/// 从 mojie 的 `config.json` 文本里取订阅地址。
///
/// 该文件**不是严格 JSON**（实测 `serde_json` 解析失败，含尾部冗余），
/// 所以这里手写提取：找到 `"subLink"` 后取紧随其后的字符串字面量。
/// 返回值属于凭据，只在本函数内用于发请求，**不得进入日志或前端结构体**。
pub fn parse_sub_link(app_config: &str) -> Option<String> {
    let idx = app_config.find("\"subLink\"")?;
    let rest = &app_config[idx + "\"subLink\"".len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let url = serde_json::Deserializer::from_str(after)
        .into_iter::<String>()
        .next()?
        .ok()?;
    let parsed = Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    Some(parsed.to_string())
}

/// 单个外部节点（只保留界面需要的字段）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ExternalVpnNode {
    pub name: String,
    pub kind: String,
    /// 最近一次延迟（毫秒）。`None` 表示还没有历史数据。
    pub delay_ms: Option<i64>,
}

/// 外部 VPN 的只读快照。
///
/// 注意**不能派生 `Eq`**：`up_kbps` / `down_kbps` 是 `f64`，浮点数没有 `Eq`。
/// 单测里比较用 `PartialEq`。
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ExternalVpnStatus {
    /// 探测是否成功（能读到配置且能连上控制器）。
    pub available: bool,
    /// 配置文件路径（界面上说明数据来源用）。
    pub config_path: String,
    /// 解析到的 external-controller，形如 127.0.0.1:18606。
    pub controller: Option<String>,
    /// 内核版本。
    pub kernel: Option<String>,
    /// 剩余额度（优先用订阅头算出的剩余；没有订阅头时退回伪节点名）。
    pub quota: Option<String>,
    /// 订阅头给的结构化用量（FlClash / v2rayN 同款来源）。
    pub subscription: Option<SubscriptionUsage>,
    /// 套餐到期（订阅头没有时间戳时退回伪节点名的「长期有效」之类）。
    pub expire: Option<String>,
    /// 当前选中的节点名。
    pub current: Option<String>,
    /// 当前节点的最近延迟。
    pub delay_ms: Option<i64>,
    /// 各节点延迟（按延迟升序，仅含有历史数据的）。
    pub nodes: Vec<ExternalVpnNode>,
    /// 上行 KB/s（一次采样）。
    pub up_kbps: Option<f64>,
    /// 下行 KB/s（一次采样）。
    pub down_kbps: Option<f64>,
    /// 失败原因（available=false 时给出，成功时为 None）。
    pub error: Option<String>,
}

impl ExternalVpnStatus {
    fn unavailable(path: &Path, reason: impl Into<String>) -> Self {
        Self {
            available: false,
            config_path: path.display().to_string(),
            controller: None,
            kernel: None,
            quota: None,
            subscription: None,
            expire: None,
            current: None,
            delay_ms: None,
            nodes: Vec::new(),
            up_kbps: None,
            down_kbps: None,
            error: Some(reason.into()),
        }
    }
}

/// mojie 的配置文件路径。找不到时返回候选路径（供报错信息展示）。
pub fn mojie_config_path() -> PathBuf {
    dirs::config_dir()
        .map(|dir| dir.join(MOJIE_DIR).join(MOJIE_CONFIG))
        .unwrap_or_else(|| PathBuf::from(MOJIE_DIR).join(MOJIE_CONFIG))
}

/// 从 config.yaml 文本里取 `external-controller` 的值。
///
/// 手写解析（无 regex 依赖）：逐行找以 `external-controller:` 开头的行，
/// 去掉冒号后的空白与可选引号。空值或形如 `0.0.0.0:` 之外的危险地址一律不接受——
/// 本模块只允许连回环地址。
pub fn parse_external_controller(yaml: &str) -> Option<String> {
    for line in yaml.lines() {
        let trimmed = line.trim();
        let rest = match trimmed.strip_prefix("external-controller:") {
            Some(rest) => rest.trim(),
            None => continue,
        };
        let value = rest
            .split('#')
            .next()?
            .trim()
            .trim_matches(['"', '\''])
            .trim();
        if value.is_empty() {
            continue;
        }
        if !is_loopback_authority(value) {
            // 只接受回环地址，避免把用户配置里的对外监听地址拿来做请求。
            return None;
        }
        return Some(value.to_string());
    }
    None
}

/// 判断 `host:port` 是否为回环地址（单测覆盖两侧）。
pub fn is_loopback_authority(value: &str) -> bool {
    let host = match value.rsplit_once(':') {
        Some((host, port)) => {
            if !port.chars().all(|c| c.is_ascii_digit())
                || !matches!(port.parse::<u16>(), Ok(1..=65535))
            {
                return false;
            }
            host
        }
        None => return false,
    };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// 从伪装节点名里提取额度，例如「剩余流量：712.08 GB」→ `712.08 GB`。
pub fn extract_quota(name: &str) -> Option<String> {
    let idx = name.find("剩余流量")?;
    let rest = name[idx + "剩余流量".len()..].trim_start_matches(['：', ':', ' ', '\u{3000}']);
    let value = rest.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// 从伪装节点名里提取到期时间，例如「套餐到期：长期有效」→ `长期有效`。
pub fn extract_expire(name: &str) -> Option<String> {
    for marker in ["套餐到期", "到期时间", "过期时间"] {
        if let Some(idx) = name.find(marker) {
            let rest = name[idx + marker.len()..].trim_start_matches(['：', ':', ' ', '\u{3000}']);
            let value = rest.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// 读取 `%APPDATA%\mojie\config.yaml` 的文本内容。
fn read_mojie_config(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|_| format!("读不到 mojie 配置：{}", path.display()))
}

/// 为读外部控制器构造的客户端：不走代理、不跟随重定向、短超时。
///
/// 超时取 3 秒：`/traffic` 是流式接口，2 秒（内置内核用的值）在慢机器上会切断首帧；
/// 3 秒的取值依据是实测本机首帧约 1 秒内到达，留 3 倍余量。
fn probe_client() -> Result<Client, String> {
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| "无法初始化外部 VPN 探测客户端".to_string())
}

async fn get_json(
    client: &Client,
    base: &Url,
    segments: &[&str],
) -> Result<serde_json::Value, String> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| "外部 VPN 地址无效".to_string())?
        .clear()
        .extend(segments.iter().copied());
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "连不上外部 VPN 控制器".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "外部 VPN 控制器返回 HTTP {}",
            response.status().as_u16()
        ));
    }
    const LIMIT: usize = 4 * 1024 * 1024;
    if response.content_length().is_some_and(|n| n > LIMIT as u64) {
        return Err("外部 VPN 响应过大".to_string());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "读外部 VPN 响应失败".to_owned())?;
        if chunk.len() > LIMIT.saturating_sub(bytes.len()) {
            return Err("外部 VPN 响应过大".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "外部 VPN 返回了无效 JSON".to_string())
}

/// 读一帧 `/traffic`，返回 (上行 KB/s, 下行 KB/s)。
///
/// 该接口持续推流，这里读够一帧完整 JSON 行就断开；超时或格式不符都返回 None
/// （网速采样失败不该让整个状态查询失败）。
async fn sample_traffic(client: &Client, base: &Url) -> Option<(f64, f64)> {
    let mut url = base.clone();
    url.path_segments_mut().ok()?.clear().push("traffic");
    let response = tokio::time::timeout(Duration::from_secs(4), client.get(url).send())
        .await
        .ok()?
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut buffer = String::new();
    let mut stream = response.bytes_stream();
    // At most 64 KiB and one absolute four-second read window; chunk boundaries are arbitrary.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        // timeout 后是 Result<Option<Result<Bytes, Error>>, Elapsed>：三层都要拆开。
        // 超时、流结束、传输出错三种情况都直接返回 None（网速采样失败不该让整个查询失败）。
        let chunk = match tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_secs(2)),
            stream.next(),
        )
        .await
        {
            Ok(Some(Ok(chunk))) => chunk,
            _ => return None,
        };
        if chunk.len() > (64 * 1024usize).saturating_sub(buffer.len()) {
            return None;
        }
        let text = String::from_utf8_lossy(&chunk);
        buffer.push_str(&text);
        while let Some(pos) = buffer.find('\n') {
            let line: String = buffer.drain(..=pos).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                let up = value.get("up").and_then(serde_json::Value::as_f64);
                let down = value.get("down").and_then(serde_json::Value::as_f64);
                if let (Some(up), Some(down)) = (up, down) {
                    return Some((up / 1024.0, down / 1024.0));
                }
            }
        }
    }
    None
}

/// mojie 的应用配置路径（config.json，含 subLink）。
pub fn mojie_app_config_path() -> PathBuf {
    dirs::config_dir()
        .map(|dir| dir.join(MOJIE_DIR).join(MOJIE_APP_CONFIG))
        .unwrap_or_else(|| PathBuf::from(MOJIE_DIR).join(MOJIE_APP_CONFIG))
}

/// 读订阅用量。
///
/// 订阅地址从 mojie 的 config.json 取，**只在本函数内使用**：它是凭据，
/// 不进日志、不进返回结构体、不出现在任何错误信息里。
///
/// 单独用一个超时更长的客户端：订阅站点在境外时首包可能慢（实测同一台机器上
/// 读控制器是毫秒级，而拉订阅要几百毫秒到数秒），3 秒会误判成"没有额度"。
async fn fetch_subscription_usage(app_config_path: &Path) -> Option<SubscriptionUsage> {
    let text = std::fs::read_to_string(app_config_path).ok()?;
    let link = parse_sub_link(&text)?;
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(12))
        .build()
        .ok()?;
    let response = client.get(&link).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let header = response
        .headers()
        .get("subscription-userinfo")
        .and_then(|value| value.to_str().ok())?;
    let usage = parse_subscription_userinfo(header);
    if usage.total_bytes.is_none() && usage.upload_bytes.is_none() {
        return None;
    }
    Some(usage)
}

/// 把字节数格式化成人类可读的 GB（保留两位），用于在没有前端格式化时也能直接显示。
pub fn format_gb(bytes: u64) -> String {
    format!("{:.2} GB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

/// 探一次外部 VPN 状态（用 mojie 的默认路径）。**只读**。
pub async fn probe() -> ExternalVpnStatus {
    probe_with(mojie_config_path(), mojie_app_config_path()).await
}

/// 与 [`probe`] 相同，但配置文件路径可指定 —— 供测试指向 fixture，避免测试依赖本机 mojie。
///
/// 失败也返回结构体（带 error），不向上抛错：界面在 mojie 没开时同样能显示说明而不是报错。
pub async fn probe_with(config_path: PathBuf, app_config_path: PathBuf) -> ExternalVpnStatus {
    let path = config_path;
    let yaml = match read_mojie_config(&path) {
        Ok(text) => text,
        Err(reason) => return ExternalVpnStatus::unavailable(&path, reason),
    };
    let Some(controller) = parse_external_controller(&yaml) else {
        return ExternalVpnStatus::unavailable(&path, "mojie 配置里没有可用的 external-controller");
    };
    let client = match probe_client() {
        Ok(client) => client,
        Err(reason) => return ExternalVpnStatus::unavailable(&path, reason),
    };
    let base = match Url::parse(&format!("http://{controller}/")) {
        Ok(url) => url,
        Err(_) => return ExternalVpnStatus::unavailable(&path, "external-controller 格式无效"),
    };

    let mut status = ExternalVpnStatus {
        available: false,
        config_path: path.display().to_string(),
        controller: Some(controller),
        kernel: None,
        quota: None,
        subscription: None,
        expire: None,
        current: None,
        delay_ms: None,
        nodes: Vec::new(),
        up_kbps: None,
        down_kbps: None,
        error: None,
    };

    // 节点表：额度/到期/延迟都从这里出。
    match get_json(&client, &base, &["proxies"]).await {
        Ok(value) => {
            let Some(entries) = value.get("proxies").and_then(serde_json::Value::as_object) else {
                status.error = Some("外部 VPN 返回的节点表格式不符".to_string());
                return status;
            };
            let mut nodes = Vec::new();
            // Prefer mojie's selector; GLOBAL often points to another group.
            let mut selected = entries
                .get("MoJie")
                .or_else(|| entries.get("GLOBAL"))
                .and_then(|entry| entry.get("now"))
                .and_then(serde_json::Value::as_str);
            let mut visited = std::collections::HashSet::new();
            while let Some(name) = selected {
                if !visited.insert(name) {
                    selected = None;
                    break;
                }
                let Some(entry) = entries.get(name) else {
                    selected = None;
                    break;
                };
                match entry.get("now").and_then(serde_json::Value::as_str) {
                    Some(next) => selected = Some(next),
                    None => break,
                }
            }
            status.current = selected.map(str::to_owned);
            for (name, entry) in entries {
                if status.quota.is_none() {
                    status.quota = extract_quota(name);
                }
                if status.expire.is_none() {
                    status.expire = extract_expire(name);
                }
                let kind = entry
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                // 伪装节点（额度/到期）不作为可测节点展示。
                if extract_quota(name).is_some()
                    || extract_expire(name).is_some()
                    || entry.get("now").is_some()
                    || matches!(kind.as_str(), "Direct" | "Reject" | "RejectDrop")
                {
                    continue;
                }
                let delay_ms = entry
                    .get("history")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|history| history.last())
                    .and_then(|last| last.get("delay"))
                    .and_then(serde_json::Value::as_i64)
                    .filter(|delay| *delay > 0);
                nodes.push(ExternalVpnNode {
                    name: name.clone(),
                    kind,
                    delay_ms,
                });
            }
            if let Some(current) = &status.current {
                status.delay_ms = nodes
                    .iter()
                    .find(|node| &node.name == current)
                    .and_then(|node| node.delay_ms);
            }
            // 按延迟升序，延迟未知的排后面；同延迟按名字保证稳定顺序。
            nodes.sort_by(|a, b| match (a.delay_ms, b.delay_ms) {
                (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.name.cmp(&b.name)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.cmp(&b.name),
            });
            nodes.truncate(32);
            status.nodes = nodes;
            status.available = true;
        }
        Err(reason) => {
            status.error = Some(reason);
            return status;
        }
    }

    if let Ok(value) = get_json(&client, &base, &["version"]).await {
        status.kernel = value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
    }
    // 额度优先用订阅头（FlClash / v2rayN 的做法）：比 mojie 伪装节点名里的快照新。
    // 剩余值必须同时扣除 upload 和 download，不能只减 download。
    if let Some(usage) = fetch_subscription_usage(&app_config_path).await {
        if let Some(remaining) = usage.remaining_bytes() {
            status.quota = Some(format_gb(remaining));
        }
        status.subscription = Some(usage);
    }
    if let Some((up, down)) = sample_traffic(&client, &base).await {
        status.up_kbps = Some(up);
        status.down_kbps = Some(down);
    }
    status
}

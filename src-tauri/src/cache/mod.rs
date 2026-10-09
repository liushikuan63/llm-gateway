//! 精确响应缓存。**默认关闭。**
//!
//! ## 「精确」是什么意思
//!
//! 只有**逐字段完全相同**的请求才命中。不做语义相似、不做归一化改写、
//! 不做「意思差不多就算命中」——那类缓存在 LLM 场景里会把不同问题的答案串味，
//! 而且串味时客户端完全看不出来。
//!
//! 关闭时（默认）本模块的所有入口都提前返回，路径与改动前**逐位等价**：
//! 不做哈希、不查表、不写表、不加任何 `X-Cache-*` 头。
//!
//! ## 缓存键由什么组成
//!
//! 少一项就可能串味，所以逐项列全：
//!
//! | 组成 | 漏了会怎样 |
//! | --- | --- |
//! | `provider_id` | 同一模型在两家供应商上限流策略不同，答案会串 |
//! | `routed_model` | 同名模型的不同上游版本会串 |
//! | 全部 messages（含 system） | 换了系统提示词却命中旧答案 |
//! | temperature / top_p / max_tokens / seed … | 同问题不同采样参数拿到同一份答案 |
//! | tools 序列化 | 带工具与不带工具是两种请求 |
//! | tool_choice | `auto` 与 `required` 行为不同 |
//! | response_format | JSON 模式与纯文本模式输出不同 |
//! | 会话 id（若有） | 跨会话串联，把 A 的上下文答给 B |
//!
//! ## 规范化 JSON 不能带 Map 迭代顺序
//!
//! `serde_json::Value::Object` 底层是 `BTreeMap`，序列化天然有序；
//! 但**不能依赖这一点**——一旦哪天换成 `preserve_order` 特性，
//! 同一请求两次会算出不同 key，表现为「缓存永远不命中」，
//! 而功能看起来「没生效」，最难查。所以 [`canonical_json`] 显式递归排序。
//!
//! ## 明确不缓存的场景
//!
//! 每条都有理由，**不要自行放宽**：
//!
//! - **流式（SSE）**：本批不做。要做必须整段缓冲再重放，首包延迟从
//!   百毫秒级涨到一次完整上游耗时，与「网关该更快」的前提冲突。
//! - **响应含 `tool_calls`**：工具调用的正确性依赖当轮上下文，重放会执行过期动作。
//! - **触发过搜索预取**（`X-Route-Search` 非空）：把当时的检索结果固化成
//!   「模型的记忆」，是错误的信息来源——今天查到的和昨天不一样。
//! - **含 image_url / audio / video**：多模态内容哈希成本高、缓存体积大。
//! - **上游返回错误**（5xx / 429 / 401）：错误不该被缓存，更不该被重放。

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 命中的三种结果。会写进响应头 `X-Cache`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum CacheOutcome {
    /// 命中，直接返回缓存体
    Hit,
    /// 未命中，正常走上游
    Miss,
    /// 命中过键，但该场景明确不缓存
    Bypass,
}

impl CacheOutcome {
    pub fn code(self) -> &'static str {
        match self {
            CacheOutcome::Hit => "HIT",
            CacheOutcome::Miss => "MISS",
            CacheOutcome::Bypass => "BYPASS",
        }
    }
}

/// 为什么绕过。写进 `X-Cache-Reason` 便于排查 ——
/// 只说 BYPASS 不说原因，等于让人去读源码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassReason {
    /// 请求本身是流式的
    Streaming,
    /// 请求带多模态内容
    Multimodal,
    /// 请求带 tools（带工具的请求不缓存，理由见模块头）
    HasTools,
    /// 触发过搜索预取
    SearchInjected,
    /// 上游返回了错误
    UpstreamError,
    /// 响应体里含 tool_calls
    ResponseHasToolCalls,
    /// 缓存总开关关着
    Disabled,
    /// 请求使用的配置快照已失效，不能读取或填充新代次的缓存。
    ConfigurationChanged,
}

impl BypassReason {
    pub fn code(self) -> &'static str {
        match self {
            BypassReason::Streaming => "streaming",
            BypassReason::Multimodal => "multimodal",
            BypassReason::HasTools => "has_tools",
            BypassReason::SearchInjected => "search_injected",
            BypassReason::UpstreamError => "upstream_error",
            BypassReason::ResponseHasToolCalls => "response_has_tool_calls",
            BypassReason::Disabled => "disabled",
            BypassReason::ConfigurationChanged => "configuration_changed",
        }
    }
}

/// 缓存配置。`enabled` 默认 **false** —— 默认关是卡片写死的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 容量，按**条目数**而不是字节数。
    ///
    /// 理由：单条响应体积不可控（一次长回答可能几百 KB），
    /// 按字节数需要先知道体积再决定淘汰，而体积只有写完才知道；
    /// 条目数则是确定的、可预测的，不会因为一条超大响应把整表清空。
    #[serde(default = "default_capacity")]
    pub capacity: usize,
    /// 有效期（秒）；0 保留历史行为，不自动过期。命中不延长有效期。
    #[serde(default)]
    pub ttl_secs: u64,
}

fn default_capacity() -> usize {
    200
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            capacity: default_capacity(),
            ttl_secs: 0,
        }
    }
}

/// 一次缓存查询的结果。
pub enum Lookup {
    /// 命中
    Hit(Value),
    /// 未命中，继续走上游（键已经算好，回填时复用）
    Miss(CacheMiss),
    /// 明确绕过，不走缓存
    Bypass(BypassReason),
}

/// MISS 的回填凭据。键和代次只能由 lookup 产生，防止在清空后复活旧响应。
#[derive(Debug, Clone)]
pub struct CacheMiss {
    key: String,
    generation: u64,
}

impl CacheMiss {
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// 精确响应缓存。
///
/// 用 `Mutex<BTreeMap>` + 手工 LRU 顺序，没引 `lru` crate：
/// 依赖表里只有 `dashmap` 与 `sha2` 可用，而 CLAUDE.md 第 4 条禁止加依赖。
/// 条目数量级是几百，`Mutex` 的争用可以忽略。
pub struct ResponseCache {
    inner: std::sync::Mutex<Inner>,
    /// 命中/未命中的累计计数。**只增不减**，配置清空后仍保留 ——
    /// 它是观测「缓存到底有没有在工作」的唯一证据，被一次 invalidate 抹掉
    /// 就再也说不清「刚才那次为什么没命中」。
    hits: AtomicU64,
    misses: AtomicU64,
    bypasses: AtomicU64,
    invalidations: AtomicU64,
}

struct Inner {
    map: BTreeMap<String, Entry>,
    /// LRU 顺序：队尾最新。用 Vec 而不是链表——容量是几百，
    /// `position` + `remove` 的 O(n) 完全够，换来的是可读性。
    order: Vec<String>,
    config: CacheConfig,
    generation: u64,
}

struct Entry {
    value: Value,
    inserted_at: Instant,
}

impl Inner {
    fn clear(&mut self) -> usize {
        let removed = self.map.len();
        self.map.clear();
        self.order.clear();
        self.generation = self.generation.wrapping_add(1);
        removed
    }

    fn purge_expired(&mut self, now: Instant) {
        if self.config.ttl_secs == 0 {
            return;
        }
        let ttl = Duration::from_secs(self.config.ttl_secs);
        self.map
            .retain(|_, entry| now.saturating_duration_since(entry.inserted_at) < ttl);
        self.order.retain(|key| self.map.contains_key(key));
    }
}

impl Default for ResponseCache {
    fn default() -> Self {
        Self::new(CacheConfig::default())
    }
}

impl ResponseCache {
    pub fn new(cfg: CacheConfig) -> Self {
        Self {
            inner: std::sync::Mutex::new(Inner {
                map: BTreeMap::new(),
                order: Vec::new(),
                config: cfg,
                generation: 0,
            }),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            bypasses: AtomicU64::new(0),
            invalidations: AtomicU64::new(0),
        }
    }

    /// Read counters and stored entries without expiring or otherwise changing the cache.
    /// Expired entries remain included until a normal cache operation purges them.
    pub fn snapshot_stats(&self) -> CacheStats {
        CacheStats {
            entries: self
                .inner
                .lock()
                .map(|guard| guard.map.len() as u64)
                .unwrap_or_default(),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            bypasses: self.bypasses.load(Ordering::Relaxed),
            invalidations: self.invalidations.load(Ordering::Relaxed),
        }
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self
                .inner
                .lock()
                .map(|mut guard| {
                    guard.purge_expired(Instant::now());
                    guard.map.len() as u64
                })
                .unwrap_or_default(),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            bypasses: self.bypasses.load(Ordering::Relaxed),
            invalidations: self.invalidations.load(Ordering::Relaxed),
        }
    }

    /// 清空。**配置变更、供应商启停、模型增删改、价格刷新都必须调它。**
    ///
    /// 漏一个就是「改了配置却不生效」的经典症状：界面显示保存成功，
    /// 运行时却还在返回旧答案。所以在那几个入口显式调用，不靠猜。
    pub fn invalidate_all(&self) -> usize {
        let removed = self
            .inner
            .lock()
            .map(|mut guard| guard.clear())
            .unwrap_or(0);
        self.invalidations.fetch_add(1, Ordering::Relaxed);
        removed
    }

    /// 更新实例配置与代次在同一把锁内完成，旧 MISS 不能越过更新回填。
    pub fn reconfigure(&self, cfg: &CacheConfig) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.config = cfg.clone();
            guard.clear();
        }
        self.invalidations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn generation(&self) -> u64 {
        self.inner.lock().map(|guard| guard.generation).unwrap_or(0)
    }

    /// 查一次。`cfg` 关着时**立刻返回 Bypass(Disabled)**，
    /// 不做任何哈希与查表——这是「关闭时路径逐位等价」的实现方式。
    pub fn lookup(&self, cfg: &CacheConfig, input: &CacheKeyInput<'_>) -> Lookup {
        if !cfg.enabled {
            return Lookup::Bypass(BypassReason::Disabled);
        }
        self.lookup_for_generation(cfg, input, self.generation())
    }

    /// 带请求配置快照的代次检查，覆盖「旧请求直到清空后才开始 lookup」的窗口。
    pub fn lookup_for_generation(
        &self,
        cfg: &CacheConfig,
        input: &CacheKeyInput<'_>,
        generation: u64,
    ) -> Lookup {
        self.lookup_at(cfg, input, generation, Instant::now())
    }

    fn lookup_at(
        &self,
        cfg: &CacheConfig,
        input: &CacheKeyInput<'_>,
        generation: u64,
        now: Instant,
    ) -> Lookup {
        if !cfg.enabled {
            return Lookup::Bypass(BypassReason::Disabled);
        }
        if let Some(reason) = input.bypass_reason_code() {
            self.bypasses.fetch_add(1, Ordering::Relaxed);
            return Lookup::Bypass(reason);
        }

        let key = cache_key(input);
        let Ok(mut guard) = self.inner.lock() else {
            return Lookup::Bypass(BypassReason::ConfigurationChanged);
        };
        if generation != guard.generation || cfg != &guard.config {
            self.bypasses.fetch_add(1, Ordering::Relaxed);
            return Lookup::Bypass(BypassReason::ConfigurationChanged);
        }
        guard.purge_expired(now);
        let hit = guard.map.get(&key).map(|entry| entry.value.clone());
        if hit.is_some() {
            // 命中即提到队尾（最近使用）
            if let Some(pos) = guard.order.iter().position(|k| k == &key) {
                let k = guard.order.remove(pos);
                guard.order.push(k);
            }
        }

        match hit {
            Some(value) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Lookup::Hit(value)
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                Lookup::Miss(CacheMiss {
                    key,
                    generation: guard.generation,
                })
            }
        }
    }

    /// 回填。`key` 来自 [`Lookup::Miss`]，避免重算一次哈希。
    pub fn store(&self, miss: &CacheMiss, value: Value) -> bool {
        self.store_at(miss, value, Instant::now())
    }

    fn store_at(&self, miss: &CacheMiss, value: Value, now: Instant) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        if miss.generation != guard.generation || !guard.config.enabled {
            return false;
        }
        guard.purge_expired(now);
        guard.map.insert(
            miss.key.clone(),
            Entry {
                value,
                inserted_at: now,
            },
        );
        // 同一键的并发 MISS 最后一次回填也应成为最近使用。
        guard.order.retain(|key| key != &miss.key);
        guard.order.push(miss.key.clone());
        while guard.order.len() > guard.config.capacity.max(1) {
            let oldest = guard.order.remove(0);
            guard.map.remove(&oldest);
        }
        true
    }
}

/// 缓存计数快照。用于界面与排查。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CacheStats {
    pub entries: u64,
    pub hits: u64,
    pub misses: u64,
    pub bypasses: u64,
    pub invalidations: u64,
}

/// 算缓存键需要的全部输入。
///
/// 每一个字段都对应模块头表格里的一行。**加字段时先问「漏了它会串味吗」**，
/// 答是的就必须加。
pub struct CacheKeyInput<'a> {
    pub provider_id: &'a str,
    pub routed_model: &'a str,
    /// 完整请求体。这里取 messages / 采样参数 / tools / tool_choice /
    /// response_format 都在它里面，逐个摘出来反而容易漏。
    pub request: &'a Value,
    /// 会话 id。远程隔离后的内部键，不是回显给客户端的那个。
    pub session_id: &'a str,
    /// 请求是否流式。
    pub stream: bool,
    /// 请求是否带多模态内容。
    pub multimodal: bool,
    /// 是否触发过搜索预取。
    pub search_injected: bool,
    /// 上游返回的错误状态（成功传 `None`）。
    pub upstream_status: Option<u16>,
    /// 响应体是否含 tool_calls。
    pub response_has_tool_calls: bool,
}

impl CacheKeyInput<'_> {
    /// 该不该绕过。顺序即优先级，**第一条命中就返回**。
    /// 公开的理由：dispatch 在「缓存开着但这次请求要绕过的场景」下需要知道原因，
    /// 才能写出 `X-Cache: BYPASS` + `X-Cache-Reason`。
    /// 只说 BYPASS 不说原因，等于让人去读源码。
    pub fn bypass_reason_code(&self) -> Option<BypassReason> {
        if self.stream {
            return Some(BypassReason::Streaming);
        }
        if self.multimodal {
            return Some(BypassReason::Multimodal);
        }
        if self.search_injected {
            return Some(BypassReason::SearchInjected);
        }
        if self.upstream_status.is_some_and(|s| s >= 400) {
            return Some(BypassReason::UpstreamError);
        }
        if self.response_has_tool_calls {
            return Some(BypassReason::ResponseHasToolCalls);
        }
        if self.request.get("tools").is_some_and(|t| !t.is_null()) {
            return Some(BypassReason::HasTools);
        }
        None
    }
}

/// 命中时 `lookup` 会走的前置检查，回填前也要走一遍。
///
/// 为什么回填要单独查：上游可能**返回** tool_calls，而请求里没有 `tools`
/// 字段 —— 那种情况只有拿到响应才知道，`lookup` 时无从判断。
pub fn response_has_tool_calls(body: &Value) -> bool {
    body.get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| {
            choices.iter().any(|c| {
                c.get("message")
                    .and_then(|m| m.get("tool_calls"))
                    .is_some_and(|t| !t.is_null())
            })
        })
}

/// 算缓存键。`sha256(canonical_json(全部输入))` 的前 32 位十六进制。
///
/// 取 32 位而不是全 64 位：这是**缓存键不是安全边界**，碰撞的后果是
/// 返回一份错误的缓存而不是泄露数据；128 bit 的碰撞概率在
/// 「几百条 / 分钟」的量级下可以忽略，而短键在日志里可读。
pub fn cache_key(input: &CacheKeyInput<'_>) -> String {
    let mut hasher = Sha256::new();
    // 逐段喂进去而不是拼一个大字符串：拼接需要分隔符，而分隔符本身
    // 可能与内容冲突（`a` + `|` + `b` 与 `a|` + `b` 同键）。
    // 每段先写长度再写内容，天然无歧义。
    let mut feed = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    feed(b"llm-gateway-response-cache-v1");
    feed(input.provider_id.as_bytes());
    feed(input.routed_model.as_bytes());
    feed(input.session_id.as_bytes());
    feed(canonical_json(input.request).as_bytes());
    let digest = hasher.finalize();
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// 规范化 JSON：递归按 key 排序后序列化。
///
/// 为什么要显式排序而不是信任 `serde_json`：当前 `Value::Object` 底层是
/// `BTreeMap`，本来就按 key 有序。但那是**实现细节**——一旦启用
/// `preserve_order` 特性（很常见的需求，为了回显时保持字段顺序），
/// 同一份逻辑请求两次会算出不同 key，表现为「缓存永远不命中」，
/// 而功能看起来只是「没生效」。显式排序把这个假设变成代码。
///
/// 数组**不排序**：messages 的顺序是语义的一部分，
/// 把 `[user, assistant]` 排成 `[assistant, user]` 会算出同键。
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            // BTreeMap 已有序，但显式排一次，让「有序」成为本函数的承诺
            // 而不是底层容器的偶然性质。
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(&map[*k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        // 标量与 null 直接用 serde_json 的紧凑表示（它是确定性的）
        other => out.push_str(&other.to_string()),
    }
}

/// 缓存键的前 16 位，写进 `X-Cache-Key` 便于排查。
///
/// 只给前 16 位：完整键有 32 位，日志里占地方且没人会逐位比对；
/// 16 位足够在「同一时间窗口内区分两个请求」这个用途上不撞。
pub fn key_prefix(key: &str) -> String {
    key.chars().take(16).collect()
}

/// 容量 0 的配置会被 [`ResponseCache::new`] 抬到 1。
///
/// 单独写出来的理由：`NonZeroUsize` 只在这里用一次，
/// 与其引一个类型，不如把这个边界写成可测的函数。
pub fn normalize_capacity(raw: usize) -> NonZeroUsize {
    NonZeroUsize::new(raw).unwrap_or(NonZeroUsize::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input<'a>(req: &'a Value, session: &'a str) -> CacheKeyInput<'a> {
        CacheKeyInput {
            provider_id: "p1",
            routed_model: "m1",
            request: req,
            session_id: session,
            stream: false,
            multimodal: false,
            search_injected: false,
            upstream_status: None,
            response_has_tool_calls: false,
        }
    }

    #[test]
    fn 关闭时立刻绕过且不做任何哈希() {
        let cache = ResponseCache::default();
        let cfg = CacheConfig::default();
        assert!(!cfg.enabled, "缓存必须默认关闭");
        let req = json!({"model": "auto", "messages": []});
        match cache.lookup(&cfg, &input(&req, "s1")) {
            Lookup::Bypass(BypassReason::Disabled) => {}
            _ => panic!("关闭时必须返回 Bypass(Disabled)"),
        }
        // 关着的时候连计数都不该动，否则统计会虚高
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.bypasses, 0);
    }

    #[test]
    fn map_顺序不同但内容相同算同一个键() {
        // 这条挡的是「规范化漏了排序」：一旦漏了，缓存永远不命中，
        // 而功能看起来只是「没生效」，最难查。
        let a = json!({"model": "auto", "messages": [{"role": "user"}], "temperature": 0.7});
        let b = json!({"temperature": 0.7, "messages": [{"role": "user"}], "model": "auto"});
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(cache_key(&input(&a, "s1")), cache_key(&input(&b, "s1")));
    }

    #[test]
    fn 嵌套的_map_也要排序() {
        let a = json!({"outer": {"b": 1, "a": 2}});
        let b = json!({"outer": {"a": 2, "b": 1}});
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn 数组顺序不能排序() {
        // messages 的顺序是语义的一部分。排了序会把
        // [user, assistant] 与 [assistant, user] 算成同键。
        let a = json!({"messages": [{"role": "user"}, {"role": "assistant"}]});
        let b = json!({"messages": [{"role": "assistant"}, {"role": "user"}]});
        assert_ne!(cache_key(&input(&a, "s1")), cache_key(&input(&b, "s1")));
    }

    #[test]
    fn 不同采样参数算出不同键() {
        let base = json!({"messages": [], "temperature": 0.7});
        let other = json!({"messages": [], "temperature": 0.2});
        assert_ne!(
            cache_key(&input(&base, "s1")),
            cache_key(&input(&other, "s1"))
        );
    }

    #[test]
    fn 不同会话算出不同键() {
        let req = json!({"messages": []});
        assert_ne!(
            cache_key(&input(&req, "session-a")),
            cache_key(&input(&req, "session-b"))
        );
    }

    #[test]
    fn 分隔符冲突不会撞键() {
        // 长度前缀的意义：a="ab", b="c" 与 a="a", b="bc" 拼接后都是 "abc"。
        let req = json!({});
        let one = CacheKeyInput {
            provider_id: "ab",
            routed_model: "c",
            ..input(&req, "s")
        };
        let two = CacheKeyInput {
            provider_id: "a",
            routed_model: "bc",
            ..input(&req, "s")
        };
        assert_ne!(cache_key(&one), cache_key(&two));
    }

    #[test]
    fn 流式请求走_bypass() {
        let cache = ResponseCache::default();
        let cfg = CacheConfig {
            enabled: true,
            ..Default::default()
        };
        let req = json!({});
        let mut i = input(&req, "s1");
        i.stream = true;
        match cache.lookup(&cfg, &i) {
            Lookup::Bypass(BypassReason::Streaming) => {}
            _ => panic!("流式必须 bypass"),
        }
    }

    #[test]
    fn 带工具的请求走_bypass() {
        let cache = ResponseCache::default();
        let cfg = CacheConfig {
            enabled: true,
            ..Default::default()
        };
        let req = json!({"tools": [{"type": "function"}]});
        match cache.lookup(&cfg, &input(&req, "s1")) {
            Lookup::Bypass(BypassReason::HasTools) => {}
            _ => panic!("带 tools 必须 bypass"),
        }
    }

    #[test]
    fn 多模态与搜索注入都走_bypass() {
        let cache = ResponseCache::default();
        let cfg = CacheConfig {
            enabled: true,
            ..Default::default()
        };
        let req = json!({});
        let mut mm = input(&req, "s1");
        mm.multimodal = true;
        assert!(matches!(
            cache.lookup(&cfg, &mm),
            Lookup::Bypass(BypassReason::Multimodal)
        ));

        let mut si = input(&req, "s1");
        si.search_injected = true;
        assert!(matches!(
            cache.lookup(&cfg, &si),
            Lookup::Bypass(BypassReason::SearchInjected)
        ));
    }

    #[test]
    fn 上游错误状态走_bypass() {
        // 这条直接查 `bypass_reason_code()`，不经过 lookup ——
        // 上游状态只有拿到响应才知道，属于「回填前」的判定。
        let req = json!({});
        for status in [401u16, 429, 500, 502] {
            let mut i = input(&req, "s1");
            i.upstream_status = Some(status);
            assert!(
                matches!(i.bypass_reason_code(), Some(BypassReason::UpstreamError)),
                "上游 {status} 必须 bypass"
            );
        }
        // 2xx 不该 bypass
        let mut ok = input(&req, "s1");
        ok.upstream_status = Some(200);
        assert!(ok.bypass_reason_code().is_none());
    }

    #[test]
    fn 响应含_tool_calls_时回填被拒() {
        let body = json!({
            "choices": [{"message": {"role": "assistant", "tool_calls": [{"id": "c1"}]}}]
        });
        assert!(response_has_tool_calls(&body));
        let plain = json!({"choices": [{"message": {"role": "assistant", "content": "ok"}}]});
        assert!(!response_has_tool_calls(&plain));
        // 有 tool_calls 键但是 null 不算
        let nulled = json!({"choices": [{"message": {"tool_calls": null}}]});
        assert!(!response_has_tool_calls(&nulled));
    }

    #[test]
    fn 命中后再查同一键会走_lru_且命中() {
        let cfg = CacheConfig {
            enabled: true,
            capacity: 2,
            ..Default::default()
        };
        let cache = ResponseCache::new(cfg.clone());
        let req = json!({"messages": []});
        let key = match cache.lookup(&cfg, &input(&req, "s1")) {
            Lookup::Miss(k) => k,
            _ => panic!("首次必须未命中"),
        };
        cache.store(&key, json!({"ok": true}));
        match cache.lookup(&cfg, &input(&req, "s1")) {
            Lookup::Hit(v) => assert_eq!(v, json!({"ok": true})),
            _ => panic!("回填后必须命中"),
        }
        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn 超过容量时最老的被淘汰() {
        let cfg = CacheConfig {
            enabled: true,
            capacity: 2,
            ..Default::default()
        };
        let cache = ResponseCache::new(cfg.clone());
        let mut keys = Vec::new();
        for n in 0..3 {
            let req = json!({"messages": [], "n": n});
            if let Lookup::Miss(k) = cache.lookup(&cfg, &input(&req, "s1")) {
                cache.store(&k, json!({"n": n}));
                keys.push((k, req));
            }
        }
        assert_eq!(cache.stats().entries, 2, "容量 2 只能留 2 条");
        // 第一条被淘汰
        assert!(matches!(
            cache.lookup(&cfg, &input(&keys[0].1, "s1")),
            Lookup::Miss(_)
        ));
        // 最后一条还在
        assert!(matches!(
            cache.lookup(&cfg, &input(&keys[2].1, "s1")),
            Lookup::Hit(_)
        ));
    }

    #[test]
    fn 命中会把条目提到最近使用() {
        let cfg = CacheConfig {
            enabled: true,
            capacity: 2,
            ..Default::default()
        };
        let cache = ResponseCache::new(cfg.clone());
        let r0 = json!({"messages": [], "n": 0});
        let r1 = json!({"messages": [], "n": 1});
        let r2 = json!({"messages": [], "n": 2});
        let k0 = match cache.lookup(&cfg, &input(&r0, "s")) {
            Lookup::Miss(k) => k,
            _ => unreachable!(),
        };
        cache.store(&k0, json!(0));
        let k1 = match cache.lookup(&cfg, &input(&r1, "s")) {
            Lookup::Miss(k) => k,
            _ => unreachable!(),
        };
        cache.store(&k1, json!(1));
        // 摸一下 k0，它变成最近使用
        assert!(matches!(
            cache.lookup(&cfg, &input(&r0, "s")),
            Lookup::Hit(_)
        ));
        // 再插一条，应该淘汰 k1 而不是 k0
        let k2 = match cache.lookup(&cfg, &input(&r2, "s")) {
            Lookup::Miss(k) => k,
            _ => unreachable!(),
        };
        cache.store(&k2, json!(2));
        assert!(
            matches!(cache.lookup(&cfg, &input(&r0, "s")), Lookup::Hit(_)),
            "被摸过的 k0 不该被淘汰"
        );
        assert!(
            matches!(cache.lookup(&cfg, &input(&r1, "s")), Lookup::Miss(_)),
            "k1 才是最久未用的"
        );
        let _ = (k1, k2);
    }

    #[test]
    fn 清空后条目归零但计数保留() {
        let cfg = CacheConfig {
            enabled: true,
            capacity: 8,
            ..Default::default()
        };
        let cache = ResponseCache::new(cfg.clone());
        let req = json!({"messages": []});
        if let Lookup::Miss(k) = cache.lookup(&cfg, &input(&req, "s")) {
            cache.store(&k, json!({}));
        }
        let _ = cache.lookup(&cfg, &input(&req, "s"));
        assert_eq!(cache.stats().entries, 1);
        assert_eq!(cache.stats().hits, 1);

        assert_eq!(cache.invalidate_all(), 1);
        let s = cache.stats();
        assert_eq!(s.entries, 0, "清空后不该还有条目");
        assert_eq!(s.invalidations, 1);
        // 计数保留：它是「缓存有没有在工作」的证据，不该被清空抹掉
        assert_eq!(s.hits, 1, "命中计数不该被 invalidate 抹掉");
    }

    #[test]
    fn 容量为零时抬到一() {
        assert_eq!(normalize_capacity(0).get(), 1);
        assert_eq!(normalize_capacity(7).get(), 7);
    }

    #[test]
    fn 键前缀取十六位() {
        let k = cache_key(&input(&json!({}), "s"));
        assert_eq!(k.len(), 32, "完整键应是 32 位十六进制");
        assert_eq!(key_prefix(&k).len(), 16);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn 相同请求算出稳定键() {
        // 同一次输入算两次必须一样，否则缓存永远不命中。
        let req = json!({"a": 1, "b": [1, 2, {"c": 3}]});
        let first = cache_key(&input(&req, "s"));
        for _ in 0..5 {
            assert_eq!(cache_key(&input(&req, "s")), first);
        }
    }

    #[test]
    fn 清空后旧_miss_不能重新回填() {
        let cfg = CacheConfig {
            enabled: true,
            ..Default::default()
        };
        let cache = ResponseCache::new(cfg.clone());
        let req = json!({"messages": []});
        let key = match cache.lookup(&cfg, &input(&req, "s")) {
            Lookup::Miss(key) => key,
            _ => panic!("首次必须 MISS"),
        };
        cache.invalidate_all();
        cache.store(&key, json!({"obsolete": true}));
        assert_eq!(cache.stats().entries, 0, "旧 MISS 不能在清空后回填");
        assert!(matches!(
            cache.lookup(&cfg, &input(&req, "s")),
            Lookup::Miss(_)
        ));
    }

    #[test]
    fn 旧请求在清空后才查询也必须绕过() {
        let cfg = CacheConfig {
            enabled: true,
            ..Default::default()
        };
        let cache = ResponseCache::new(cfg.clone());
        let old_generation = cache.generation();
        cache.invalidate_all();
        let req = json!({"messages": []});
        assert!(matches!(
            cache.lookup_for_generation(&cfg, &input(&req, "s"), old_generation),
            Lookup::Bypass(BypassReason::ConfigurationChanged)
        ));
        assert!(matches!(
            cache.lookup(&cfg, &input(&req, "s")),
            Lookup::Miss(_)
        ));
    }

    #[test]
    fn 热更新实例容量且拒绝旧配置回填() {
        let mut cfg = CacheConfig {
            enabled: true,
            capacity: 3,
            ..Default::default()
        };
        let cache = ResponseCache::new(cfg.clone());
        let req = json!({"n": 0});
        let old = match cache.lookup(&cfg, &input(&req, "s")) {
            Lookup::Miss(miss) => miss,
            _ => panic!(),
        };
        cfg.capacity = 1;
        cache.reconfigure(&cfg);
        assert!(!cache.store(&old, json!("obsolete")));
        for n in 1..=2 {
            let request = json!({"n": n});
            let miss = match cache.lookup(&cfg, &input(&request, "s")) {
                Lookup::Miss(miss) => miss,
                _ => panic!(),
            };
            assert!(cache.store(&miss, json!(n)));
        }
        assert_eq!(cache.stats().entries, 1);
        assert!(matches!(
            cache.lookup(&cfg, &input(&json!({"n": 1}), "s")),
            Lookup::Miss(_)
        ));
        assert!(matches!(
            cache.lookup(&cfg, &input(&json!({"n": 2}), "s")),
            Lookup::Hit(_)
        ));
    }

    #[test]
    fn ttl_使用写入时间且命中不续期() {
        let cfg = CacheConfig {
            enabled: true,
            ttl_secs: 1,
            ..Default::default()
        };
        let cache = ResponseCache::new(cfg.clone());
        let req = json!({});
        let now = Instant::now();
        let miss = match cache.lookup_at(&cfg, &input(&req, "s"), 0, now) {
            Lookup::Miss(miss) => miss,
            _ => panic!(),
        };
        assert!(cache.store_at(&miss, json!("answer"), now));
        assert!(matches!(
            cache.lookup_at(&cfg, &input(&req, "s"), 0, now + Duration::from_millis(999)),
            Lookup::Hit(_)
        ));
        assert!(matches!(
            cache.lookup_at(&cfg, &input(&req, "s"), 0, now + Duration::from_secs(1)),
            Lookup::Miss(_)
        ));
        assert_eq!(cache.stats().entries, 0);
    }

    #[test]
    fn 默认零_ttl_保留不过期行为() {
        let cfg: CacheConfig =
            serde_json::from_value(json!({"enabled": true, "capacity": 2})).unwrap();
        assert_eq!(cfg.ttl_secs, 0);
        let cache = ResponseCache::new(cfg.clone());
        let req = json!({});
        let now = Instant::now();
        let miss = match cache.lookup_at(&cfg, &input(&req, "s"), 0, now) {
            Lookup::Miss(miss) => miss,
            _ => panic!(),
        };
        cache.store_at(&miss, json!("answer"), now);
        assert!(matches!(
            cache.lookup_at(&cfg, &input(&req, "s"), 0, now + Duration::from_secs(86400)),
            Lookup::Hit(_)
        ));
    }

    #[test]
    fn 热更新_ttl_以新配置为准且不接受旧快照() {
        let old_cfg = CacheConfig {
            enabled: true,
            ..Default::default()
        };
        let cache = ResponseCache::new(old_cfg.clone());
        let cfg = CacheConfig {
            ttl_secs: 1,
            ..old_cfg.clone()
        };
        cache.reconfigure(&cfg);
        let req = json!({});
        assert!(matches!(
            cache.lookup(&old_cfg, &input(&req, "s")),
            Lookup::Bypass(BypassReason::ConfigurationChanged)
        ));
        let now = Instant::now();
        let generation = cache.generation();
        let miss = match cache.lookup_at(&cfg, &input(&req, "s"), generation, now) {
            Lookup::Miss(miss) => miss,
            _ => panic!(),
        };
        cache.store_at(&miss, json!("answer"), now);
        assert!(matches!(
            cache.lookup_at(
                &cfg,
                &input(&req, "s"),
                generation,
                now + Duration::from_secs(1)
            ),
            Lookup::Miss(_)
        ));
    }
}

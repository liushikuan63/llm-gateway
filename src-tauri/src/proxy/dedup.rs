//! 重复请求抑制。**默认只观察，不改变任何响应。**
//!
//! ## 它解决什么
//!
//! 症状：用户只问了一次，却收到两份回答；或同一条弹窗 / tool 调用被执行两次。
//!
//! 根因不是模型，是**同一个请求被执行了两次**。三种常见触发方式：
//!
//! | 触发方式 | 观测到的间隔 | 客户端表现 |
//! | --- | --- | --- |
//! | 客户端超时重试（网关只是在慢慢跑） | 5–20 秒 | 两份**不同**的回答 |
//! | 自动接续 / 重放整轮 | 秒级到分钟级 | 整轮被重做，弹窗重复 |
//! | 用户或 UI 重复点击发送 | 毫秒级 | 两条请求同时在飞 |
//!
//! 三种的表象不同（「答两次」是前两种，「问两次」是第三种），但判据同一条：
//! **同一份请求内容在短窗口内出现了两次**。
//!
//! ## 怎么判、为什么这么判
//!
//! ### 只对「内容完全相同」的请求生效，不做语义近似
//!
//! 判据是**请求指纹完全相同**：逐字节等价的请求体 + 同一个会话 + 同一个客户端。
//! 不做「意思差不多就算重复」—— 那会把「帮我改代码」与「帮我改配置」合并成一条，
//! 用户会拿到一个张冠李戴的答案，而界面上看不出任何异常。这与 [`crate::cache`]
//! 拒绝语义相似的理由是同一条。
//!
//! ### 只有 Owner 真正执行，且在途不会被取消
//!
//! 第一个到达的请求拿到该指纹的专用许可（Owner），后续请求（Follower）排队。
//! **许可只用来定义「谁先到」与「什么时候可以复用结果」，不用来取消在途请求**。
//!
//! Owner 执行完把**可复用的结果**留在条目里；Follower 拿到许可后先查结果：
//!
//! | 情况 | 处置 |
//! | --- | --- |
//! | 上次执行成功且响应可缓冲 | 直接复用（`Shared`），一份回答只发一次 |
//! | 上次执行成功但响应是流式 | 无载荷可用：显式拒绝，而不是重放双份 |
//! | 上次执行失败 | 当作没发生过，照常执行（失败不拦） |
//! | 上一次的窗口已过期 | 当作新请求执行（真实重复提问不该被吸收） |
//!
//! ### 明确不做的五件事（每条都有代价，不要自行放宽）
//!
//! - **不取消在途请求**：用户等着的连接突然断掉，比多花一份钱差得多。
//! - **不合并「历史不同、本轮相同」的请求**：历史不同就是不同的问题。
//! - **不拦带 `tools` 的请求**：工具调用的正确性依赖当轮上下文，合并会执行过期动作。
//! - **不拦多模态请求**：同样的文字配不同的图是两回事，而图的内容不参与指纹比对。
//!   （这条由调用方在算指纹前排除，见 `proxy::server` 的调用点。）
//! - **不拦窗口外的重复**：用户真的问了两遍，就该答两遍 —— 吸收它是错的。
//!
//! ### 为什么默认是「只观察」
//!
//! 抑制会改变**用户可见的行为**：本该跑第二次的请求不跑了。在拿到真实重复率之前
//! 默认开抑制，等于拿用户的账单和行为做实验。观察模式只计数、只记日志，
//! 一个字节的响应都不动 —— 而它给出的重复计数，正是决定要不要开抑制的前提。
//!
//! ## 与 `X-Request-Id` 的关系（稳健幂等键的解释）
//!
//! 规范做法是客户端带一个幂等键回来。但这条路在本项目当前的调用方上走不通：
//! `agent_upstream::plugin` 里的 `request_id` 由**网关自己生成**、只用于插件往返配对；
//! 主流 SDK 客户端也不保证回传。所以这里用「网关侧派生键」——
//! 指纹即键，不依赖任何调用方配合。
//!
//! 两个键的用途要分清，别互相替代：
//!
//! - **指纹**：同一次请求的两次投递算出同一个值 ⇒ 用于**抑制**。
//! - **`request_id`**：每次投递唯一 ⇒ 用于**日志与审计里区分「哪一次」**。
//!
//! 所以抑制必须把被抑制那次的原样请求记进日志（落盘见 `proxy::server` 的调用点），
//! 否则「这次为什么没有响应」就没有任何可查的痕迹。
//!
//! ## 关闭时逐位等价
//!
//! `mode == Off` 时所有入口提前返回，不做哈希、不查表、不建信号量。
//! 判定所需的指纹**由调用方按需现算**（[`should_compute_fingerprint`]），
//! 所以关闭时不产生任何开销 —— 这是「模式隔离」的实现方式，不是靠注释保证。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// 判定的三种模式。序列化成小写字符串，写进配置文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DedupMode {
    /// 不做任何判定（默认）。路径与改动前逐位等价。
    #[default]
    Off,
    /// 只计数、只记日志，响应一个字节都不改。
    Observe,
    /// 判定为重复时复用上一次结果，或明确拒绝。见模块头表格。
    Enforce,
}

impl DedupMode {
    pub fn code(self) -> &'static str {
        match self {
            DedupMode::Off => "off",
            DedupMode::Observe => "observe",
            DedupMode::Enforce => "enforce",
        }
    }

    /// 该模式是否要真的拦一下。观察模式判出重复也不拦。
    pub fn is_enforcing(self) -> bool {
        matches!(self, DedupMode::Enforce)
    }
}

/// 去重配置。`mode` 默认 `off` —— 默认关是刻意的，理由见模块头。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DedupConfig {
    pub mode: DedupMode,
    /// 判定窗口（秒）。0 表示不判定。
    ///
    /// **默认 15 秒**：窗口要短。窗口越大，越容易把「用户真的问了两遍」
    /// 判成重复 —— 连问两遍本来就是常见行为，而它必须被正常回答。
    /// 同时它要能盖住客户端的重试间隔（观测到的是 5–20 秒）。
    pub window_secs: u64,
    /// Follower 最多等 Owner 多久（秒）。等不到就自己执行。
    ///
    /// **宁可多跑一次，也不要给用户一个失败**：等待超时只是让这次请求自己执行。
    /// 它不该被设得很大 —— 用户是在等一个回答。
    pub wait_secs: u64,
    /// 条目容量上限。超过就淘汰最老的（按 `inserted_at`）。
    pub capacity: usize,
}

impl Default for DedupConfig {
    fn default() -> Self {
        Self {
            mode: DedupMode::Off,
            window_secs: 15,
            wait_secs: 10,
            capacity: 512,
        }
    }
}

/// 该不该为这次请求算指纹。关闭时不算 —— 一个字节的哈希开销都不花。
pub fn should_compute_fingerprint(cfg: &DedupConfig) -> bool {
    cfg.mode != DedupMode::Off && cfg.window_secs > 0
}

/// 计算指纹所需的输入。
///
/// **加字段前先问：漏了它，两次不同的请求会算出同一个值吗？**
/// 答「是」就必须加。漏掉的后果是吸收了不该吸收的请求 —— 用户拿不到自己那份回答。
///
/// ## 为什么**没有**会话 ID
///
/// 因为它在当前调用方上不是稳定值：`context::derive_session_id` 对没回传
/// `X-Session-Id` 的客户端**每次生成随机 ID**（那是有意设计，为了不让相同首问的
/// 独立会话共享上下文）。把随机值并入指纹 ⇒ 同一次请求的两次投递算出两个不同的键
/// ⇒ 去重永远不命中。而「网关侧派生键」的全部意义就是不依赖调用方配合。
///
/// 代价要写清楚：不同会话里**逐字节相同**的请求会被判成重复。实测这个代价很小 ——
/// 客户端重发同一句话时，上一次的回答已经进了历史，两轮的请求体并不相同；
/// 真正逐字节相同的两次投递，就是同一次请求被发了两遍。
#[derive(Debug, Clone, Copy)]
pub struct FingerprintInput<'a> {
    /// 请求体（内部结构的序列化）。采样参数、tools、模型名都在它里面，
    /// 逐个摘出来反而容易漏。
    pub request_json: &'a str,
    /// 远端 Key 的内部标识。**不能省**：远程模式下不同租户可以问同一句话。
    pub client: Option<&'a str>,
}

/// 请求指纹：`sha256(client | request_json)` 的前 16 字节十六进制。
///
/// 与 [`crate::cache::cache_key`] 取同样的长度，理由也同：它是**判定键不是安全边界**；
/// 截断到 128 bit 在「几百条 / 分钟」的量级下碰撞可以忽略，而短键在日志里可读。
///
/// 前缀 `dedup-v1` 把它与缓存键分开：两者恰好都在算同一个请求的哈希，
/// 但**用途不同、生命周期不同**，共用一份值会让一次改动同时影响两个功能。
pub fn request_fingerprint(input: &FingerprintInput<'_>) -> String {
    let mut hasher = Sha256::new();
    // 逐段喂进去而不是拼一个大字符串：拼接需要分隔符，而分隔符本身可能与内容冲突
    // （`a` + `|` + `b` 与 `a|` + `b` 会算出同一个键）。每段先写长度再写内容，天然无歧义。
    let mut feed = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    feed(b"llm-gateway-dedup-v1");
    feed(input.client.unwrap_or("").as_bytes());
    feed(input.request_json.as_bytes());
    let digest = hasher.finalize();
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// 一次判定的结论。写进 `X-Dedup` 头。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 未启用：什么都没做。
    Off,
    /// 第一次见到：正常执行。
    New,
    /// 窗口外或上次失败：正常执行，但这是「重复出现了」的证据。
    Expired,
    /// 等 Owner 超时：正常执行，不报错。
    WaitTimeout,
    /// 判为重复，且上一次已成功交付：明确拒绝，不重复执行。
    ///
    /// 没有「复用上一次响应」这一档 —— 见 [`DedupOutcome`] 的说明：
    /// 本项目的重复投递都发生在流式请求上，而流式响应无法重放。
    /// 留一个不可达的 `Replay` 分支，就是留一条只有测试会走的路。
    Conflict,
    /// 判为重复，但**不拦**（观察模式）。仍然正常执行。
    ObservedDuplicate,
}

impl Verdict {
    /// 调用方是否应当真的执行这次请求。
    pub fn should_execute(self) -> bool {
        matches!(
            self,
            Verdict::Off | Verdict::New | Verdict::ObservedDuplicate
        )
    }

    /// 这次判定是否构成「重复」。
    pub fn is_duplicate(self) -> bool {
        matches!(
            self,
            Verdict::Conflict | Verdict::ObservedDuplicate
        )
    }

    /// 写进 `X-Dedup` 头或日志的码。**只有真做过判定才给头**：关闭时一个头都不加。
    pub fn code(self) -> &'static str {
        match self {
            Verdict::Off => "off",
            Verdict::New => "new",
            Verdict::Expired => "expired",
            Verdict::WaitTimeout => "wait-timeout",
            Verdict::Conflict => "conflict",
            Verdict::ObservedDuplicate => "duplicate-observed",
        }
    }
}

/// 上一次执行的结局。**只记结局，不存内容。**
///
/// 为什么不存响应体：本项目的重复投递都发生在流式请求上，而流式响应无法在
/// 流已经开始之后重放；缓冲非流式响应则要处理体积上限、超大响应、缓冲失败
/// 退回一整套边界，换来的只是「同窗口内第二次**非流式**请求复用上一次结果」
/// 这一个当前用不到的场景。登记「有没有成功交付过」这一个 bit 足够做判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupOutcome {
    /// 上一次成功交付了一份响应。判为重复时不能重放，只能明确拒绝。
    Delivered,
    /// 上一次执行失败。**失败不留痕** —— 这正是最该让客户端重试的时候。
    Failed,
}

/// 一次执行的凭据：持有它就是这个指纹的 Owner。
///
/// 用 `OwnedSemaphorePermit` 而不是 `MutexGuard`：Owner 要跨多个 `await`
/// 执行上游请求，而 `MutexGuard` 既不能跨 `await` 也无法自持所有权。
/// 丢弃凭据即释放 —— 调用方在任何提前 return 的分支上都不会把指纹锁死。
pub struct DedupPermit {
    fingerprint: String,
    /// 本次投递的唯一标识。指纹相同、`id` 不同 —— 这正是
    /// 「同一次请求的两次投递」在日志里的区分方式。
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    permit: OwnedSemaphorePermit,
}

impl DedupPermit {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// 本次投递的唯一标识。指纹相同、`id` 不同 —— 这正是
    /// 「同一次请求的两次投递」在日志里的区分方式。
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// [`DedupRegistry::begin`] 的结果。
pub enum DedupDecision {
    /// 未启用，或等待超时：照常执行，不做任何登记。
    Off,
    /// 可以执行；执行完用 `permit` 调 [`DedupRegistry::finish`]。
    Take(DedupPermit),
    /// 判为重复且上一次已成功交付：不要执行，明确拒绝。
    Share { verdict: Verdict },
}

/// 已登记的重复计数快照。用于界面、`/gw/stats` 与排查。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DedupStats {
    pub entries: u64,
    /// 判为重复的次数（含只观察的）。
    pub duplicates: u64,
    /// 判为重复、且上一次已成功交付因而被明确拒绝的次数。
    pub conflicts: u64,
    /// 观察模式下被判为重复、但仍然执行了的次数。
    pub observed: u64,
}

impl DedupStats {
    /// 没有真实重复时，抑制的价值为零 —— 这个判断要能一眼做出来。
    pub fn has_evidence(&self) -> bool {
        self.duplicates > 0
    }
}

struct Entry {
    inserted_at: Instant,
    last_seen_at: Instant,
    count: u64,
    /// 上一次执行的结局。`None` 表示还在跑，或上次失败了。
    outcome: Option<DedupOutcome>,
    /// 该指纹的专用许可。与条目同生命周期。
    gate: Arc<Semaphore>,
}

struct Inner {
    map: BTreeMap<String, Entry>,
    config: DedupConfig,
}

/// 重复请求抑制表。
///
/// 用 `std::sync::Mutex<BTreeMap>` 与手工容量淘汰，理由与 [`crate::cache`] 相同：
/// CLAUDE.md 第 4 条禁止新增依赖，而这里没有 `lru` 可用；
/// 条目数量级是几百，锁争用可以忽略。**内层锁只用于几次 map 操作，绝不跨 `await`。**
pub struct DedupRegistry {
    inner: std::sync::Mutex<Inner>,
    /// 计数**只增不减**：它是「到底有没有重复」的唯一证据，
    /// 被一次清空抹掉就再也说不清「刚才那次为什么算了两次」。
    duplicates: AtomicU64,
    /// 判为重复、且上一次已成功交付因而被明确拒绝的次数。
    conflicts: AtomicU64,
    observed: AtomicU64,
}

impl Default for DedupRegistry {
    fn default() -> Self {
        Self::new(DedupConfig::default())
    }
}

impl DedupRegistry {
    pub fn new(cfg: DedupConfig) -> Self {
        Self {
            inner: std::sync::Mutex::new(Inner {
                map: BTreeMap::new(),
                config: cfg,
            }),
            duplicates: AtomicU64::new(0),
            conflicts: AtomicU64::new(0),
            observed: AtomicU64::new(0),
        }
    }

    /// 判定入口。
    ///
    /// `fingerprint` 为 `None` 表示调用方判定这次请求不参与去重
    /// （带 tools、多模态、或开关关着）—— 这里的返回必定是 [`DedupDecision::Off`]。
    pub async fn begin(&self, fingerprint: Option<&str>) -> DedupDecision {
        let Some(fingerprint) = fingerprint else {
            return DedupDecision::Off;
        };
        let cfg = self.config();
        if !should_compute_fingerprint(&cfg) {
            return DedupDecision::Off;
        }

        let now = Instant::now();
        let window = Duration::from_secs(cfg.window_secs);
        let wait = Duration::from_secs(cfg.wait_secs);

        // 建条目并把许可克隆出来 —— 这一段不跨 await。
        //
        // **不用 `entry()` API**：它的 `or_insert_with` 返回值只能说明「调用时不存在」，
        // 而本函数后面要拿到的判断是「本次调用是不是第一个」，两者在并发下不是同一件事。
        // 用显式的 `contains_key` + `insert` 把顺序写成代码。
        let mut is_first_touch = false;
        let gate = {
            let Ok(mut inner) = self.inner.lock() else {
                // 中毒的锁不该让请求变成 500：退回「不判定」，请求照常执行。
                return DedupDecision::Off;
            };
            inner.prune(now);
            if inner.map.contains_key(fingerprint) {
                inner.map.get(fingerprint).map(|entry| entry.gate.clone())
            } else {
                is_first_touch = true;
                let gate = Arc::new(Semaphore::new(1));
                inner.map.insert(
                    fingerprint.to_string(),
                    Entry {
                        inserted_at: now,
                        last_seen_at: now,
                        count: 1,
                        outcome: None,
                        gate: gate.clone(),
                    },
                );
                Some(gate)
            }
        };
        let Some(gate) = gate else {
            return DedupDecision::Off;
        };

        // 在锁外等：`acquire_owned` 返回的是 future，交给 `timeout` 计时，
        // 不轮询、不忙等。
        //
        // 两层 `Result` 各有含义，别合成一个：
        // 外层 `Err` = 等超时（Owner 还在跑）⇒ 本次自行执行；
        // 内层 `Err` = 信号量已关闭（只可能是本表被销毁过程中）⇒ 同样自行执行。
        let permit = match tokio::time::timeout(wait, gate.clone().acquire_owned()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return DedupDecision::Off,
            Err(_) => {
                // 等不到 Owner：**不报错**，让这次请求自己执行。
                // 宁可多跑一次，也不要给用户一个失败。
                tracing::warn!(
                    fingerprint = %fingerprint,
                    wait_secs = cfg.wait_secs,
                    "去重：等待同指纹的上一次执行超时，本次自行执行"
                );
                return DedupDecision::Off;
            }
        };

        let id = uuid::Uuid::new_v4().simple().to_string();
        let permit = DedupPermit {
            fingerprint: fingerprint.to_string(),
            id,
            permit,
        };

        // 拿到许可后再看条目状态：只有仔细判定才作数。
        enum Existing {
            Fresh,
            Duplicate { count: u64, outcome: Option<DedupOutcome> },
            Expired,
        }
        let existing = {
            let Ok(mut inner) = self.inner.lock() else {
                return DedupDecision::Take(permit);
            };
            if is_first_touch {
                Existing::Fresh
            } else {
                match inner.map.get_mut(fingerprint) {
                    None => Existing::Fresh,
                    Some(entry) => {
                        if now.saturating_duration_since(entry.last_seen_at) >= window {
                            entry.inserted_at = now;
                            entry.last_seen_at = now;
                            entry.count = 1;
                            entry.outcome = None;
                            Existing::Expired
                        } else {
                            entry.count += 1;
                            entry.last_seen_at = now;
                            Existing::Duplicate {
                                count: entry.count,
                                outcome: entry.outcome,
                            }
                        }
                    }
                }
            }
        };

        match existing {
            Existing::Fresh | Existing::Expired => DedupDecision::Take(permit),
            Existing::Duplicate { count, outcome } => {
                self.duplicates.fetch_add(1, Ordering::Relaxed);
                if !cfg.mode.is_enforcing() {
                    self.observed.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(
                        fingerprint = %fingerprint,
                        count,
                        permit_id = %permit.id(),
                        mode = cfg.mode.code(),
                        "去重：同一请求在窗口内重复出现（只观察，不拦截）"
                    );
                    return DedupDecision::Take(permit);
                }
                match outcome {
                    // 上一次成功交付过：**不能重放**，只能明确拒绝。
                    // 这正是「答两次」被挡住的地方 —— 而不是让它再答一遍。
                    Some(DedupOutcome::Delivered) => {
                        self.conflicts.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            fingerprint = %fingerprint,
                            count,
                            permit_id = %permit.id(),
                            "去重：判为重复且上一次已成功交付，本次明确拒绝（响应无法重放）"
                        );
                        DedupDecision::Share {
                            verdict: Verdict::Conflict,
                        }
                    }
                    // **上一次执行失败了**：当作没发生过，这次照常执行。
                    //
                    // 与上面那条分开写是必须的：把「失败」也吸收掉的话，
                    // 客户端一旦遇到一次上游错误，后续重试就永远是 4xx ——
                    // 而失败恰恰是最该让它重试的时候。
                    _ => DedupDecision::Take(permit),
                }
            }
        }
    }

    /// Owner 执行完登记结局。传 [`DedupOutcome::Failed`] 时**不留痕** ——
    /// 后面的重复请求照常执行（把失败也吸收掉会让客户端拿着一次错误重试到天亮）。
    pub fn finish(&self, permit: &DedupPermit, outcome: DedupOutcome) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if let Some(entry) = inner.map.get_mut(permit.fingerprint()) {
            entry.outcome = Some(outcome);
            entry.last_seen_at = Instant::now();
        }
    }

    pub fn stats(&self) -> DedupStats {
        let entries = self
            .inner
            .lock()
            .map(|guard| guard.map.len() as u64)
            .unwrap_or_default();
        DedupStats {
            entries,
            duplicates: self.duplicates.load(Ordering::Relaxed),
            conflicts: self.conflicts.load(Ordering::Relaxed),
            observed: self.observed.load(Ordering::Relaxed),
        }
    }

    /// 清空条目，**保留计数**（理由见 `duplicates` 字段的注释）。
    pub fn clear(&self) -> usize {
        self.inner
            .lock()
            .map(|mut guard| {
                let removed = guard.map.len();
                guard.map.clear();
                removed
            })
            .unwrap_or(0)
    }

    /// 配置热更新：换判定口径，并把旧条目清掉。
    ///
    /// 为什么必须清：条目里存的是「上一次的执行结果」，
    /// 换了模式（观察 ↔ 抑制）或窗口之后，它们与新口径不再可比。
    /// 计数**不清** —— 见 `duplicates` 字段的注释。
    pub fn rewire(&self, cfg: &DedupConfig) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.config = cfg.clone();
            guard.map.clear();
        }
    }

    fn config(&self) -> DedupConfig {
        self.inner
            .lock()
            .map(|guard| guard.config.clone())
            .unwrap_or_default()
    }
}

impl Inner {
    /// 淘汰两类条目：窗口外的、以及超出容量的最老的。
    ///
    /// **正在被持有的许可不会被这里打断**：许可持有者拿到的是 `Arc<Semaphore>` 的克隆，
    /// 条目被移除只是不再复用它的结果，不会取消在途请求。
    fn prune(&mut self, now: Instant) {
        let window = Duration::from_secs(self.config.window_secs);
        self.map
            .retain(|_, entry| now.saturating_duration_since(entry.last_seen_at) < window);
        while self.map.len() > self.config.capacity.max(1) {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, entry)| entry.inserted_at)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    self.map.remove(&key);
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mode: DedupMode, window: u64) -> DedupConfig {
        DedupConfig {
            mode,
            window_secs: window,
            wait_secs: 2,
            capacity: 8,
        }
    }

    /// 关闭时：不算指纹、不留条目、不计数。这是铁律 2 的最小判据。
    #[test]
    fn 关闭时完全不做判定() {
        let registry = DedupRegistry::default();
        assert!(!should_compute_fingerprint(&registry.config()));
        let stats = registry.stats();
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.duplicates, 0);
    }

    /// 观察模式：判出重复，但仍然放行执行 —— 且计数必须真的动。
    ///
    /// 计数不动的话，「只观察」就等于什么都没做，而用户会以为功能坏了。
    #[tokio::test]
    async fn 观察模式下重复被计数但照常执行() {
        let registry = DedupRegistry::new(cfg(DedupMode::Observe, 60));
        let first = registry.begin(Some("fp-1")).await;
        let DedupDecision::Take(permit) = first else {
            panic!("第一次必须是 Take");
        };
        registry.finish(&permit, DedupOutcome::Delivered);
        drop(permit);

        let second = registry.begin(Some("fp-1")).await;
        let DedupDecision::Take(_permit) = second else {
            panic!("观察模式必须仍然放行执行");
        };
        let stats = registry.stats();
        assert_eq!(stats.duplicates, 1, "重复次数必须被记下来");
        assert_eq!(stats.observed, 1);
        assert_eq!(stats.conflicts, 0, "观察模式不许拒绝");
    }

    /// 抑制模式 + 上一次已交付：**不允许重放**，只能明确拒绝。
    ///
    /// 这条是「答两次」的核心判据：两个投递都在飞的时候，第二个必须被挡住，
    /// 而不是再产出第二份回答。
    #[tokio::test]
    async fn 抑制模式下重复请求被明确拒绝() {
        let registry = DedupRegistry::new(cfg(DedupMode::Enforce, 60));
        let DedupDecision::Take(permit) = registry.begin(Some("fp-2")).await else {
            panic!("第一次必须是 Take");
        };
        registry.finish(&permit, DedupOutcome::Delivered);
        drop(permit);

        match registry.begin(Some("fp-2")).await {
            DedupDecision::Share { verdict } => {
                assert_eq!(verdict, Verdict::Conflict);
            }
            _ => panic!("第二次必须是 Share/Conflict"),
        }
        assert_eq!(registry.stats().conflicts, 1);
    }

    /// 上一次失败（没有任何已交付结果）时，重复请求必须照常执行。
    ///
    /// 分开测的理由：把「失败」也吸收掉的话，客户端遇到一次上游错误之后就
    /// 永远是 4xx —— 而失败恰恰是最该让它重试的时候。
    #[tokio::test]
    async fn 上一次失败时重复请求照常执行() {
        let registry = DedupRegistry::new(cfg(DedupMode::Enforce, 60));
        let DedupDecision::Take(permit) = registry.begin(Some("fp-4")).await else {
            panic!("第一次必须是 Take");
        };
        registry.finish(&permit, DedupOutcome::Failed);
        drop(permit);

        let second = registry.begin(Some("fp-4")).await;
        assert!(
            matches!(second, DedupDecision::Take(_)),
            "上次失败不该被吸收成载荷"
        );
        let stats = registry.stats();
        assert_eq!(stats.duplicates, 1, "重复仍然要计数");
        assert_eq!(stats.conflicts, 0, "失败不是冲突，不该记成拒绝");
    }

    /// 窗口外不拦：用户真的问了两遍，就该答两遍。
    #[tokio::test]
    async fn 窗口外的重复不被拦截() {
        let registry = DedupRegistry::new(cfg(DedupMode::Enforce, 0));
        // window_secs = 0 ⇒ `should_compute_fingerprint` 为 false，判定整个关闭。
        assert!(!should_compute_fingerprint(&registry.config()));
        assert!(matches!(registry.begin(Some("fp-5")).await, DedupDecision::Off));
        assert_eq!(registry.stats().duplicates, 0);
    }

    /// 指纹：内容不同、客户端不同 —— 两者都必须区分开。
    ///
    /// 漏掉客户端会让 A 的问题被 B 的答案顶掉，而这在界面上看不出来。
    ///
    /// **会话不在指纹里**，这是刻意的：无状态客户端每次拿到的会话 ID 都是新的随机值，
    /// 把它并进去会让同一次请求的两次投递算出两个键 —— 去重永远不命中。
    /// 理由与实测见 [`FingerprintInput`] 的文档。
    #[test]
    fn 指纹区分内容与客户端() {
        let base = FingerprintInput {
            request_json: "{\"messages\":[]}",
            client: Some("key-1"),
        };
        let same = FingerprintInput { ..base };
        assert_eq!(request_fingerprint(&base), request_fingerprint(&same));

        let other_body = FingerprintInput {
            request_json: "{\"messages\":[1]}",
            ..base
        };
        assert_ne!(request_fingerprint(&base), request_fingerprint(&other_body));

        let other_client = FingerprintInput {
            client: Some("key-2"),
            ..base
        };
        assert_ne!(request_fingerprint(&base), request_fingerprint(&other_client));

        let anonymous = FingerprintInput {
            client: None,
            ..base
        };
        assert_ne!(request_fingerprint(&base), request_fingerprint(&anonymous));
    }

    /// 指纹长度与字符集固定：它是键，不是安全边界（与 cache 同口径）。
    #[test]
    fn 指纹是_32_位十六进制() {
        let input = FingerprintInput {
            request_json: "{}",
            client: None,
        };
        let fingerprint = request_fingerprint(&input);
        assert_eq!(fingerprint.len(), 32);
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// 计数只增不减：清空条目后，「重复发生过」这件事仍然可查。
    #[tokio::test]
    async fn 清空条目后计数保留() {
        let registry = DedupRegistry::new(cfg(DedupMode::Observe, 60));
        let DedupDecision::Take(permit) = registry.begin(Some("fp-6")).await else {
            panic!("第一次必须是 Take");
        };
        drop(permit);
        let _ = registry.begin(Some("fp-6")).await;
        assert_eq!(registry.stats().entries, 1);
        assert_eq!(registry.stats().duplicates, 1);

        assert_eq!(registry.clear(), 1);
        let stats = registry.stats();
        assert_eq!(stats.entries, 0, "清空后不该还有条目");
        assert_eq!(stats.duplicates, 1, "重复计数不该被 clear 抹掉");
    }

    /// 并发下的核心判据：同一指纹的两次投递只会执行一次。
    ///
    /// 这条同时覆盖「两个请求同时在飞」这个最典型的触发方式 ——
    /// 计数型断言先列操作：两次 begin、一次 finish、再等第二次判定。
    #[tokio::test]
    async fn 并发两次投递只执行一次() {
        let registry = Arc::new(DedupRegistry::new(cfg(DedupMode::Enforce, 60)));
        let first = registry.begin(Some("fp-7")).await;
        let DedupDecision::Take(permit) = first else {
            panic!("第一次必须是 Take");
        };

        // Owner 还在执行（许可未释放），第二次投递应当排队等它。
        let waiting = {
            let registry = registry.clone();
            tokio::spawn(async move { registry.begin(Some("fp-7")).await })
        };

        tokio::time::sleep(Duration::from_millis(80)).await;
        registry.finish(&permit, DedupOutcome::Delivered);
        drop(permit);

        // 判据：第二次拿到的不是「再执行一次」，而是明确的 Share（拒绝）。
        let expected_verdict = Verdict::Conflict;
        let decision = waiting.await.unwrap();
        let got_verdict = match decision {
            DedupDecision::Share { verdict, .. } => Some(verdict),
            DedupDecision::Take(_) => None,
            DedupDecision::Off => None,
        };
        assert_eq!(
            got_verdict,
            Some(expected_verdict),
            "第二次投递必须被判为重复并拒绝，而不是再执行一次"
        );
        let stats = registry.stats();
        assert_eq!(stats.duplicates, 1);
        assert_eq!(stats.conflicts, 1);
    }

    /// 不参与去重的请求（带 tools、多模态、开关关着）走 `None` 分支，
    /// 必须是「什么都不做」而不是「建条目」。
    #[tokio::test]
    async fn 不参与去重的请求不建条目() {
        let registry = DedupRegistry::new(cfg(DedupMode::Enforce, 60));
        assert!(matches!(registry.begin(None).await, DedupDecision::Off));
        assert_eq!(registry.stats().entries, 0);
    }
}

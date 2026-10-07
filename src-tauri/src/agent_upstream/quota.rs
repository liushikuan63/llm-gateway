//! 任务卡二 B5 判据 5：**网关自己的**配额器（RPM + 每日次数）。
//!
//! ## 卡片原文
//!
//! 「**RPM + 每日次数**（裁决 4）：把日上限配成 2，第 3 次必须**被网关拒绝**，
//! 断言里的错误来源是**网关自己的配额器**，而不是上游返回的 429。」
//!
//! 「错误来源」这四个字是本模块的设计目标：网关拒绝与上游 429
//! **必须可以区分**。对用户是「谁在拦我」，对排查是「该看哪一边的日志」。
//! 所以拒绝是一个**独立类型**，不是把上游错误包装一层。
//!
//! ## 时钟必须可注入
//!
//! 用 `Utc::now()` 的话，测「跨分钟窗口重置」只能 `sleep` 等着 ——
//! 那既慢又不稳（`CLAUDE.md` 纪律 11 的同款教训）。
//! 所以每个判定都收一个 `now`。

use std::collections::HashMap;

use chrono::{DateTime, Utc};

/// 一个运行时的配额。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AgentQuota {
    /// 每分钟请求上限。**0 = 不限**。
    pub rpm: u32,
    /// 每日请求上限。**0 = 不限**。
    ///
    /// 「日」按 **UTC 自然日**切 —— 不按「最近 24 小时」。
    /// 自然日的好处是用户能预期（「今天用完了，明天零点恢复」），
    /// 而滑动窗口的「什么时候恢复」说不清。
    pub daily_limit: u32,
}

/// 拒绝的原因。**独立类型**，不与上游错误混。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaRejection {
    /// 撞到每分钟上限。
    RateLimited {
        limit: u32,
        /// 当前这一分钟已经用掉多少。
        used: u32,
    },
    /// 撞到当日上限。
    DailyLimit {
        limit: u32,
        used: u32,
        /// 下一个 UTC 自然日的零点 —— 告诉用户**什么时候能再用**。
        resets_at: DateTime<Utc>,
    },
}

impl QuotaRejection {
    /// 给用户看的文本。
    ///
    /// **必须自报家门**：说清是「网关的配额」，不是上游的。
    /// 否则用户会去查上游的额度页，而那里一切正常。
    pub fn message(&self) -> String {
        match self {
            Self::RateLimited { limit, used } => format!(
                "网关配额：每分钟上限 {limit} 次，本分钟已用 {used} 次。\
                 这是**本地网关**的限制，与上游无关；请降低请求频率后重试。"
            ),
            Self::DailyLimit {
                limit,
                used,
                resets_at,
            } => format!(
                "网关配额：每日上限 {limit} 次，今天已用 {used} 次。\
                 这是**本地网关**的限制，与上游无关；\
                 额度将在 {}（UTC）重置。",
                resets_at.format("%Y-%m-%d %H:%M")
            ),
        }
    }

    /// 是不是「上游的 429」？
    ///
    /// **永远返回 `false`** —— 这个类型只表示**网关自己**的拒绝。
    /// 它的存在是为了让调用方与用例能**明确地**断言来源，
    /// 而不是靠字符串里有没有某个词去猜。
    pub fn is_upstream_rate_limit(&self) -> bool {
        false
    }
}

/// 任务卡二 B5 判据 4：**并发上限**，「第 N+1 次并发请求返回 429
/// 且**不排队**」。
///
/// ## 「不排队」这件事由**签名**保证
///
/// [`try_enter`](ConcurrencyGate::try_enter) 是**同步**函数
/// （不是 `async`、不返回 future）。想要「排队」，它就必须是 async
/// 或者返回一个 future —— 而它两者都不是。
///
/// 这比「实现里写了 `try_acquire` 而不是 `acquire`」更强：
/// **类型层面就没有等待的可能**。用例里那条
/// `不排队的判据是签名本身` 就是在钉这一点。
///
/// ## 已知缺口：跨 `try_enter` / `leave` 的 panic 会漏掉一个槽位
///
/// 严格的做法是 RAII guard（`Drop` 时自动 `leave`），
/// 但那要求把这个结构放在 `Arc<Mutex<…>>` 后面并让 guard 持有它 ——
/// 是另一层设计。当前调用点在 `run_agent_request` 里不 panic
/// （失败都以 `Result` 返回），所以这个缺口**在实践中不触发**。
/// 写在这里免得以后当 bug 查。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConcurrencyRejection {
    pub limit: usize,
    pub active: usize,
}

impl ConcurrencyRejection {
    pub fn message(&self) -> String {
        format!(
            "网关并发上限：最多 {} 个 Agent 同时执行，当前已有 {} 个在跑。\
             这是**本地网关**的限制，与上游无关；\
             本次请求**未被排队**（排队会让它在上限恢复后突然开始跑，\
             而客户端早已超时）。请稍后重试。",
            self.limit, self.active
        )
    }
}

/// 并发闸门。**不做队列** —— 满了就拒。
#[derive(Debug, Default)]
pub struct ConcurrencyGate {
    active: usize,
}

impl ConcurrencyGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 尝试进入。满了就**立刻**返回错误（不等待、不排队）。
    ///
    /// `limit == 0` 表示**不限**（与项目里其它上限的口径一致）。
    pub fn try_enter(&mut self, limit: usize) -> Result<(), ConcurrencyRejection> {
        if limit == 0 {
            // 不限时也要计数：`active` 会被 `peek` 用来显示「当前在跑几个」。
            self.active += 1;
            return Ok(());
        }
        if self.active >= limit {
            return Err(ConcurrencyRejection {
                limit,
                active: self.active,
            });
        }
        self.active += 1;
        Ok(())
    }

    /// 退出。**必须与 `try_enter` 配对** —— 漏调用会让槽位永久占住，
    /// 表现为「跑过几次之后再也进不来」。
    pub fn leave(&mut self) {
        self.active = self.active.saturating_sub(1);
    }

    pub fn peek(&self) -> usize {
        self.active
    }
}

/// 一个运行时的用量计数。
#[derive(Debug, Clone)]
struct Usage {
    /// 当前分钟窗口的起始分钟（UTC 时间戳 / 60）。
    minute_bucket: i64,
    used_in_minute: u32,
    /// 当前 UTC 自然日（`YYYY-MM-DD` 的序号，用 `num_days_from_ce`）。
    day: i32,
    used_in_day: u32,
}

/// 配额账本。每个 `runtime_id` 一本。
#[derive(Debug, Default)]
pub struct QuotaBook {
    entries: HashMap<String, Usage>,
}

impl QuotaBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// 判定这次请求能不能过；**能过就顺手记账**。
    ///
    /// 「判定 + 记账」合成一个动作是刻意的：分成两个函数的话，
    /// 调用方可能只判不记（于是上限永不生效）或只记不判（于是上限不拦）。
    /// 那种缺陷在单测里看不出来，只会在真跑时表现为「配额形同虚设」。
    pub fn check_and_record(
        &mut self,
        runtime_id: &str,
        quota: AgentQuota,
        now: DateTime<Utc>,
    ) -> Result<(), QuotaRejection> {
        let minute_bucket = now.timestamp() / 60;
        let day = chrono::Datelike::num_days_from_ce(&now.date_naive());

        let entry = self
            .entries
            .entry(runtime_id.to_string())
            .or_insert_with(|| Usage {
                minute_bucket,
                used_in_minute: 0,
                day,
                used_in_day: 0,
            });

        // 跨窗口就重置 —— 先重置再判定，否则「上一分钟的用量」会一直压着。
        if entry.minute_bucket != minute_bucket {
            entry.minute_bucket = minute_bucket;
            entry.used_in_minute = 0;
        }
        if entry.day != day {
            entry.day = day;
            entry.used_in_day = 0;
        }

        // 先判 RPM 再判日上限：RPM 是更细的约束，
        // 且它的重置最近（用户更容易等到）。
        if quota.rpm > 0 && entry.used_in_minute >= quota.rpm {
            return Err(QuotaRejection::RateLimited {
                limit: quota.rpm,
                used: entry.used_in_minute,
            });
        }
        if quota.daily_limit > 0 && entry.used_in_day >= quota.daily_limit {
            return Err(QuotaRejection::DailyLimit {
                limit: quota.daily_limit,
                used: entry.used_in_day,
                resets_at: next_utc_midnight(now),
            });
        }

        entry.used_in_minute += 1;
        entry.used_in_day += 1;
        Ok(())
    }

    /// 查当前用量（不记账）。供界面显示「今天用了 2/10」。
    pub fn peek(&self, runtime_id: &str) -> (u32, u32) {
        match self.entries.get(runtime_id) {
            Some(u) => (u.used_in_minute, u.used_in_day),
            None => (0, 0),
        }
    }
}

/// 下一个 UTC 自然日的零点。
fn next_utc_midnight(now: DateTime<Utc>) -> DateTime<Utc> {
    let tomorrow = now.date_naive().succ_opt().unwrap_or(now.date_naive());
    DateTime::<Utc>::from_naive_utc_and_offset(
        tomorrow.and_hms_opt(0, 0, 0).unwrap_or_default(),
        Utc,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    /// **卡片判据 5 的原文场景**：日上限配成 2，第 3 次必须被网关拒绝。
    #[test]
    fn 日上限为二时第三次被拒() {
        let mut book = QuotaBook::new();
        let quota = AgentQuota {
            rpm: 0,
            daily_limit: 2,
        };
        let t = at(2026, 10, 7, 10, 0);

        assert!(
            book.check_and_record("codex", quota, t).is_ok(),
            "第 1 次应过"
        );
        assert!(
            book.check_and_record("codex", quota, t).is_ok(),
            "第 2 次应过（上限是 2）"
        );
        let err = book
            .check_and_record("codex", quota, t)
            .expect_err("第 3 次必须被拒");
        assert_eq!(
            err,
            QuotaRejection::DailyLimit {
                limit: 2,
                used: 2,
                resets_at: at(2026, 10, 8, 0, 0),
            }
        );
    }

    /// **「错误来源是网关自己的配额器」** —— 这条断言的是**来源**，
    /// 不是「有没有报错」。
    #[test]
    fn 拒绝来源是网关而不是上游的_429() {
        let mut book = QuotaBook::new();
        let quota = AgentQuota {
            rpm: 0,
            daily_limit: 1,
        };
        let t = at(2026, 10, 7, 10, 0);
        book.check_and_record("codex", quota, t).unwrap();
        let err = book.check_and_record("codex", quota, t).unwrap_err();

        // ① 类型层面：它是网关的拒绝类型，不是上游错误的包装
        assert!(
            !err.is_upstream_rate_limit(),
            "这个类型只表示网关自己的拒绝"
        );
        // ② 文本层面：**自报家门**，用户不会跑去查上游额度
        let msg = err.message();
        assert!(msg.contains("网关配额"), "要自报家门：{msg}");
        assert!(msg.contains("与上游无关"), "要明确排除上游：{msg}");
        // ③ 带上「什么时候恢复」—— 否则用户只能反复试
        assert!(msg.contains("2026-10-08"), "要告诉什么时候重置：{msg}");
    }

    #[test]
    fn 跨日会重置() {
        let mut book = QuotaBook::new();
        let quota = AgentQuota {
            rpm: 0,
            daily_limit: 1,
        };
        assert!(book
            .check_and_record("codex", quota, at(2026, 10, 7, 23, 59))
            .is_ok());
        assert!(
            book.check_and_record("codex", quota, at(2026, 10, 7, 23, 59))
                .is_err(),
            "同一天第二次必须被拒"
        );
        assert!(
            book.check_and_record("codex", quota, at(2026, 10, 8, 0, 0))
                .is_ok(),
            "跨到第二天零点必须重置"
        );
    }

    #[test]
    fn 每分钟上限会拦且跨分钟重置() {
        let mut book = QuotaBook::new();
        let quota = AgentQuota {
            rpm: 2,
            daily_limit: 0,
        };
        let t = at(2026, 10, 7, 10, 0);
        assert!(book.check_and_record("codex", quota, t).is_ok());
        assert!(book.check_and_record("codex", quota, t).is_ok());
        let err = book.check_and_record("codex", quota, t).unwrap_err();
        assert_eq!(err, QuotaRejection::RateLimited { limit: 2, used: 2 });
        // 同一分钟的第 30 秒仍被拒
        assert!(book
            .check_and_record(
                "codex",
                quota,
                at(2026, 10, 7, 10, 0) + chrono::Duration::seconds(30)
            )
            .is_err());
        // 下一分钟放行
        assert!(
            book.check_and_record("codex", quota, at(2026, 10, 7, 10, 1))
                .is_ok(),
            "跨分钟必须重置"
        );
    }

    #[test]
    fn 上限为零表示不限() {
        let mut book = QuotaBook::new();
        let quota = AgentQuota {
            rpm: 0,
            daily_limit: 0,
        };
        let t = at(2026, 10, 7, 10, 0);
        for i in 0..100 {
            assert!(
                book.check_and_record("codex", quota, t + chrono::Duration::seconds(i))
                    .is_ok(),
                "0 = 不限，第 {} 次不该被拒",
                i + 1
            );
        }
    }

    /// **不同运行时各有一本账** —— 共用一个计数器会让
    /// 「A 用完了 B 也不能用」，那不是配额，是连坐。
    #[test]
    fn 不同运行时互不影响() {
        let mut book = QuotaBook::new();
        let quota = AgentQuota {
            rpm: 0,
            daily_limit: 1,
        };
        let t = at(2026, 10, 7, 10, 0);
        assert!(book.check_and_record("a", quota, t).is_ok());
        assert!(
            book.check_and_record("b", quota, t).is_ok(),
            "a 用掉了不该影响 b"
        );
        assert!(book.check_and_record("a", quota, t).is_err());
        assert!(book.check_and_record("b", quota, t).is_err());
    }

    /// **被拒的那次不记账** —— 否则一个被拒的请求会继续推高计数，
    /// 用户看到「今天已用 50 次」而实际上只成功了 2 次。
    #[test]
    fn 被拒的请求不推高计数() {
        let mut book = QuotaBook::new();
        let quota = AgentQuota {
            rpm: 0,
            daily_limit: 1,
        };
        let t = at(2026, 10, 7, 10, 0);
        book.check_and_record("codex", quota, t).unwrap();
        for _ in 0..5 {
            let _ = book.check_and_record("codex", quota, t);
        }
        assert_eq!(
            book.peek("codex"),
            (1, 1),
            "被拒 5 次之后用量仍应是 1 —— 被拒的不该记账"
        );
    }

    // ---------------- B5 判据 4：并发上限 ----------------

    /// **卡片原文场景**：上限 2，第 3 个并发必须被拒。
    #[test]
    fn 并发上限为二时第三个被拒() {
        let mut gate = ConcurrencyGate::new();
        assert!(gate.try_enter(2).is_ok(), "第 1 个应进");
        assert!(gate.try_enter(2).is_ok(), "第 2 个应进");
        let err = gate.try_enter(2).expect_err("第 3 个并发必须被拒");
        assert_eq!(
            err,
            ConcurrencyRejection {
                limit: 2,
                active: 2
            }
        );
    }

    /// **「不排队」的判据是签名本身。**
    ///
    /// `try_enter` 是**同步**函数（不是 async、不返回 future）——
    /// 类型层面就没有等待的可能。阻塞版必然得是 async 才能挂起调用方。
    ///
    /// 这条用例在**编译期**成立：把 `try_enter` 改成 async 的话，
    /// 本文件里所有同步调用点都会编译失败。所以它不需要运行时断言 ——
    /// 写出来是为了让这个论证**留在代码里**，而不是只在某个提交信息里。
    #[test]
    fn 不排队的判据是签名本身() {
        // 这一行**没有 .await** —— 它就是证据。
        let mut gate = ConcurrencyGate::new();
        let _: Result<(), ConcurrencyRejection> = gate.try_enter(1);
        assert_eq!(gate.peek(), 1);
    }

    #[test]
    fn 退出后槽位会被放回() {
        let mut gate = ConcurrencyGate::new();
        gate.try_enter(1).unwrap();
        assert!(gate.try_enter(1).is_err(), "满了");
        gate.leave();
        assert_eq!(gate.peek(), 0);
        assert!(gate.try_enter(1).is_ok(), "退出后必须能再进");
    }

    /// **漏调用 `leave` 的后果可见** —— 这是「必须配对」的证据。
    #[test]
    fn 漏掉_leave_会让槽位永久占住() {
        let mut gate = ConcurrencyGate::new();
        for _ in 0..3 {
            gate.try_enter(3).unwrap();
            // 故意不 leave
        }
        assert_eq!(gate.peek(), 3);
        assert!(
            gate.try_enter(3).is_err(),
            "三次都没 leave ⇒ 槽位占满 ⇒ 之后再也进不来"
        );
    }

    #[test]
    fn 并发上限为零表示不限但仍计数() {
        let mut gate = ConcurrencyGate::new();
        for _ in 0..50 {
            assert!(gate.try_enter(0).is_ok(), "0 = 不限");
        }
        // 不限时**仍然计数** —— `peek` 要用来显示「当前在跑几个」
        assert_eq!(gate.peek(), 50);
    }

    /// 多出来的 `leave` 不该把计数搞成负数（那会让上限永久失效）。
    #[test]
    fn 多余的_leave_不会让计数变负() {
        let mut gate = ConcurrencyGate::new();
        gate.leave();
        gate.leave();
        assert_eq!(gate.peek(), 0, "saturating_sub 守着它");
        // 计数没变负 ⇒ 上限仍然有效
        gate.try_enter(1).unwrap();
        assert!(gate.try_enter(1).is_err());
    }

    /// 错误文本要自报家门 + 说清「没排队」。
    #[test]
    fn 并发拒绝的文本自报家门且说明未排队() {
        let msg = ConcurrencyRejection {
            limit: 3,
            active: 3,
        }
        .message();
        assert!(msg.contains("网关并发上限"), "要自报家门：{msg}");
        assert!(msg.contains("与上游无关"), "要排除上游：{msg}");
        assert!(
            msg.contains("未被排队"),
            "要说清没排队 —— 否则用户以为等一等就能跑：{msg}"
        );
        assert!(msg.contains("稍后重试"), "要给下一步：{msg}");
    }

    #[test]
    fn peek_对未知运行时装返回零() {
        let book = QuotaBook::new();
        assert_eq!(book.peek("从没见过的"), (0, 0));
    }

    /// 两种上限同时配着时，先用完的那个拦 —— 且**原因要准**。
    #[test]
    fn 两种上限同时配着时原因要准() {
        let mut book = QuotaBook::new();
        let quota = AgentQuota {
            rpm: 1,
            daily_limit: 100,
        };
        let t = at(2026, 10, 7, 10, 0);
        assert!(book.check_and_record("codex", quota, t).is_ok());
        let err = book.check_and_record("codex", quota, t).unwrap_err();
        assert!(
            matches!(err, QuotaRejection::RateLimited { .. }),
            "该报 RPM 而不是日上限（RPM 更细、重置更近），实际：{err:?}"
        );
    }
}

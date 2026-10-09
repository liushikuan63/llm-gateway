//! B2 预算闸门与 per-key 模型白名单。**判定逻辑与 I/O 分离。**
//!
//! 本模块只有纯函数与类型，不碰数据库、不碰 HTTP。理由：闸门判错的后果
//! 是「该拦的没拦（钱花超了）」或「不该拦的拦了（正常请求被拒）」，
//! 两种都要能单测打穿。接线部分（限额从哪来、花掉多少从哪查）在
//! `db::repo` 与 `proxy::server`。
//!
//! ## 金额为什么用 micros（整数）而不是浮点
//!
//! `requests.cost` 是 `REAL`，因为它来自上游的计费口径。但**预算比对不能用浮点**：
//! `0.1 + 0.2 != 0.3` 这类误差累积到「已花 9.999999 对上限 10.0」时，
//! 判定会在边界上随机翻转，而这种 bug 只在特定金额组合下偶发。
//! 所以累计值先转成整数 micros（1e-6）再比。
//!
//! ## 多币种不相加
//!
//! 判定是**按币种分组**做的：USD 100 + CNY 100 不等于 200，
//! 它们是两组互不相干的累计值。Key 的 `budget_currency` 只对它自己那一组生效；
//! 若该币种一分钱都没花过，视为 0。

use serde::{Deserialize, Serialize};

/// 一次闸门判定的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GateDecision {
    /// 放行
    Allow,
    /// 超预算
    BudgetExceeded {
        /// 已花（micros）
        used: i64,
        /// 上限（micros）
        limit: i64,
        currency: String,
    },
    /// 模型不在白名单
    ModelNotAllowed { model: String },
}

/// 闸门配置。`enabled` 是**总开关**：关掉时两条闸门都完全不生效。
///
/// 默认 **true**：Key 上设了预算就该生效，否则用户填了数字却不拦，
/// 那是「开着没反应的开关」——CLAUDE.md 第 9 条点名的反面模式。
/// 真正让默认路径零成本的是「`monthly_budget_micros == 0` 直接跳过」，
/// 而不是把总开关关掉。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// 金额换算：1 货币单位 = 1_000_000 micros。
pub const MICROS_PER_UNIT: i64 = 1_000_000;

/// Match saved budgets to the same canonical currency codes used by model prices.
/// Zero keeps the existing unlimited semantics; invalid limits must not become unlimited.
pub fn normalize_budget_currency(limit_micros: i64, currency: &str) -> Result<String, String> {
    if limit_micros < 0 {
        return Err("月度预算不能为负数".into());
    }
    if limit_micros == 0 {
        return Ok(String::new());
    }
    crate::domain::Currency::parse(currency)
        .map(|currency| currency.code().to_owned())
        .ok_or_else(|| "设置月度预算时，币种必须为 USD 或 CNY".into())
}

/// 把上游计费口径的浮点金额转成整数 micros。
///
/// 用 `round` 而不是 `trunc`：`0.1 + 0.2` 算出来的 `0.30000000000000004`
/// 截断成 300000 会把三分之一 micro 抹掉，累计多次后判定会偏松。
/// 四舍五入到最近的 micro 是这里能做到的最接近的口径。
pub fn units_to_micros(units: f64) -> i64 {
    if !units.is_finite() || units <= 0.0 {
        return 0;
    }
    (units * MICROS_PER_UNIT as f64).round() as i64
}

/// 判定预算。
///
/// `limit_micros == 0` 表示**不限**（不是「上限为 0」）。这是卡片写死的口径：
/// 让「不填预算」与「预算为零」用同一个值表达，用户不用去区分
/// 「我没设」和「我设了 0 元」——后者对一个 Key 来说没有意义。
pub fn check_budget(used_micros: i64, limit_micros: i64, currency: &str) -> GateDecision {
    if limit_micros <= 0 {
        return GateDecision::Allow;
    }
    if used_micros >= limit_micros {
        return GateDecision::BudgetExceeded {
            used: used_micros,
            limit: limit_micros,
            currency: currency.to_string(),
        };
    }
    GateDecision::Allow
}

/// 在「按币种分组的累计值」里取出目标币种的那一份。
///
/// 组内求和而不是跨币种求和 —— 见模块头。找不到该币种时返回 0，
/// 语义是「这个币种一分钱都还没花」，而不是「数据缺失」。
pub fn spent_in_currency(rows: &[(String, f64)], currency: &str) -> i64 {
    let mut total_units = 0.0f64;
    for (row_currency, cost) in rows {
        if row_currency.trim().eq_ignore_ascii_case(currency.trim()) {
            total_units += cost;
        }
    }
    units_to_micros(total_units)
}

/// 解析 `allowed_models`（JSON 数组字符串）。
///
/// **空串与解析失败都返回空列表**，语义都是「不限」。理由：
/// 这个字段是手改数据库或旧版本留下的可能性最大的地方，
/// 解析失败时如果当成「一个都不许」，用户会看到所有请求突然 403
/// 而不知道为什么；当成「不限」则是「白名单没生效」，一样可见但不会阻断服务。
/// 两种都可选，选后者是因为**闸门失效比闸门误伤更容易被发现**。
pub fn parse_allowed_models(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<String>>(trimmed).unwrap_or_default()
}

/// 序列化白名单。空列表存空串（而不是 `[]`），让「不限」只有一种存储形态。
pub fn serialize_allowed_models(models: &[String]) -> String {
    if models.is_empty() {
        return String::new();
    }
    serde_json::to_string(models).unwrap_or_default()
}

/// 模型是否被允许。
///
/// 白名单为空 ⇒ 不限（卡片写死的口径）。
/// 支持 `*` 通配（复用路由那边已有的 `model_name_matches` 口径：
/// `*` 匹配任意长度含空串）。不带 `*` 时是精确比较，不做前缀匹配 ——
/// 前缀匹配会让 `gpt-4` 这个条目意外放行 `gpt-4o`，那是两类不同能力的模型。
pub fn model_allowed(whitelist: &[String], requested: &str) -> bool {
    if whitelist.is_empty() {
        return true;
    }
    let requested = requested.trim();
    whitelist.iter().any(|pattern| {
        let pattern = pattern.trim();
        if !pattern.contains('*') {
            return pattern == requested;
        }
        // 只允许 `*`，避免正则带来的回溯与转义歧义。与路由器同口径。
        let parts: Vec<&str> = pattern.split('*').collect();
        let mut rest = requested;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            if i == 0 {
                let Some(stripped) = rest.strip_prefix(part) else {
                    return false;
                };
                rest = stripped;
            } else if i == parts.len() - 1 {
                return rest.ends_with(part);
            } else {
                let Some(pos) = rest.find(part) else {
                    return false;
                };
                rest = &rest[pos + part.len()..];
            }
        }
        true
    })
}

/// 判断两条闸门，按固定顺序。
///
/// 顺序是**预算优先于白名单**：预算超了是对这个 Key 的整体判断，
/// 白名单是针对本次请求的判断。先报整体原因，用户更容易知道该去改什么
/// （充值 vs 换模型）。
pub fn gate(
    cfg: &BudgetConfig,
    used_micros: i64,
    limit_micros: i64,
    currency: &str,
    whitelist: &[String],
    requested_model: &str,
) -> GateDecision {
    if !cfg.enabled {
        return GateDecision::Allow;
    }
    match check_budget(used_micros, limit_micros, currency) {
        GateDecision::Allow => {}
        denied => return denied,
    }
    if !model_allowed(whitelist, requested_model) {
        return GateDecision::ModelNotAllowed {
            model: requested_model.to_string(),
        };
    }
    GateDecision::Allow
}

/// 自然月的起点（Unix 秒，UTC）。用于「当月累计」。
///
/// 用 UTC 而不是本地时区：`requests.ts` 存的就是 Unix 秒，
/// 按本地时区切月会在有夏令时的地区产生 23/25 小时的首日，
/// 而账单口径上的「一个月」不该随观察者位置变化。
pub fn month_start_secs(now_secs: i64) -> i64 {
    use chrono::{TimeZone, Utc};
    let Some(dt) = Utc.timestamp_opt(now_secs, 0).single() else {
        return 0;
    };
    use chrono::Datelike;
    let first = dt
        .date_naive()
        .with_day(1)
        .unwrap_or_else(|| dt.date_naive());
    let naive = first.and_hms_opt(0, 0, 0).unwrap_or_default();
    Utc.from_utc_datetime(&naive).timestamp()
}

/// 把 `remote-key:<id>` 拆出 Key id。不是这个前缀时返回 `None`。
///
/// 鉴权中间件就是按这个格式写的 `client`（见 `proxy::server::auth`），
/// 所以这里与它是同一份约定的两端。写成函数而不是各处 `strip_prefix`，
/// 是为了让「格式变了要改哪里」只有一个答案。
pub fn access_key_id_of(client: Option<&str>) -> Option<&str> {
    client?.strip_prefix("remote-key:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 预算为零表示不限() {
        assert_eq!(check_budget(999_999_999, 0, "USD"), GateDecision::Allow);
        assert_eq!(check_budget(0, 0, "USD"), GateDecision::Allow);
    }

    #[test]
    fn 未超预算时放行() {
        assert_eq!(check_budget(500_000, 1_000_000, "USD"), GateDecision::Allow);
    }

    #[test]
    fn 刚好等于上限时拒绝() {
        // 边界口径：`>=`。取「花完就不许再花」，
        // 否则上限会实际变成「上限 + 最后一笔」。
        assert_eq!(
            check_budget(1_000_000, 1_000_000, "USD"),
            GateDecision::BudgetExceeded {
                used: 1_000_000,
                limit: 1_000_000,
                currency: "USD".into()
            }
        );
    }

    #[test]
    fn 超预算时带上已用与上限() {
        match check_budget(2_500_000, 1_000_000, "CNY") {
            GateDecision::BudgetExceeded {
                used,
                limit,
                currency,
            } => {
                assert_eq!(used, 2_500_000);
                assert_eq!(limit, 1_000_000);
                assert_eq!(currency, "CNY");
            }
            other => panic!("应超预算，实际 {other:?}"),
        }
    }

    #[test]
    fn 多币种不相加() {
        // USD 100 + CNY 100 ≠ 200。按 USD 看只花了 100。
        let rows = vec![("USD".to_string(), 100.0), ("CNY".to_string(), 100.0)];
        assert_eq!(spent_in_currency(&rows, "USD"), 100 * MICROS_PER_UNIT);
        assert_eq!(spent_in_currency(&rows, "CNY"), 100 * MICROS_PER_UNIT);
        // 没花过的币种是 0，不是「把别的币种加起来」
        assert_eq!(spent_in_currency(&rows, "JPY"), 0);
    }

    #[test]
    fn 同币种多行会累加() {
        let rows = vec![
            ("USD".to_string(), 1.5),
            ("USD".to_string(), 2.25),
            ("CNY".to_string(), 999.0),
        ];
        assert_eq!(spent_in_currency(&rows, "USD"), 3_750_000);
    }

    #[test]
    fn 预算币种忽略大小写且不同币种仍分别累计() {
        let rows = vec![
            ("usd".to_string(), 1.5),
            ("USD".to_string(), 2.25),
            ("cny".to_string(), 10.0),
            ("CNY".to_string(), 20.0),
        ];
        for currency in ["USD", "usd", " UsD "] {
            assert_eq!(spent_in_currency(&rows, currency), 3_750_000);
        }
        for currency in ["CNY", "cny", " CnY "] {
            assert_eq!(spent_in_currency(&rows, currency), 30_000_000);
        }
        assert_eq!(spent_in_currency(&rows, "JPY"), 0);
    }

    #[test]
    fn 保存预算时规范币种并拒绝会让限额失效的输入() {
        for (input, canonical) in [
            ("USD", "usd"),
            ("usd", "usd"),
            (" CNY ", "cny"),
            ("cny", "cny"),
        ] {
            assert_eq!(
                normalize_budget_currency(1_000_000, input).unwrap(),
                canonical
            );
        }
        assert_eq!(normalize_budget_currency(0, "USD").unwrap(), "");
        assert_eq!(normalize_budget_currency(0, "").unwrap(), "");
        for currency in ["", " ", "JPY", "US D"] {
            assert!(normalize_budget_currency(1_000_000, currency).is_err());
        }
        assert!(normalize_budget_currency(-1, "USD").is_err());
    }

    #[test]
    fn 浮点误差不会让边界判定乱翻() {
        // 0.1 + 0.2 = 0.30000000000000004。若直接比浮点，
        // 「已花 0.3 对上限 0.3」会判成未超（因为是 0.30000000000000004 > 0.3
        // 反而超了），取决于累加顺序。转 micros 之后是确定的。
        let rows = vec![("USD".to_string(), 0.1), ("USD".to_string(), 0.2)];
        let used = spent_in_currency(&rows, "USD");
        assert_eq!(used, 300_000, "0.1 + 0.2 必须精确落在 300000 micros");
        assert!(matches!(
            check_budget(used, 300_000, "USD"),
            GateDecision::BudgetExceeded { .. }
        ));
    }

    #[test]
    fn 负数与非法金额当零() {
        assert_eq!(units_to_micros(-5.0), 0);
        assert_eq!(units_to_micros(f64::NAN), 0);
        assert_eq!(units_to_micros(f64::INFINITY), 0);
    }

    #[test]
    fn 白名单为空表示不限() {
        assert!(model_allowed(&[], "任意模型"));
        assert!(model_allowed(&[], ""));
    }

    #[test]
    fn 白名单精确匹配() {
        let wl = vec!["gpt-4".to_string(), "claude-3".to_string()];
        assert!(model_allowed(&wl, "gpt-4"));
        assert!(model_allowed(&wl, "claude-3"));
        // 不做前缀匹配：`gpt-4` 不该放行 `gpt-4o`，那是两类不同能力的模型
        assert!(!model_allowed(&wl, "gpt-4o"));
        assert!(!model_allowed(&wl, "gpt-4-mini"));
        assert!(!model_allowed(&wl, "gpt"));
    }

    #[test]
    fn 白名单支持星号通配() {
        let wl = vec!["gpt-*".to_string()];
        assert!(model_allowed(&wl, "gpt-4"));
        assert!(model_allowed(&wl, "gpt-4o-mini"));
        assert!(!model_allowed(&wl, "claude-3"));

        // 前后都有通配
        let wl2 = vec!["*-turbo".to_string()];
        assert!(model_allowed(&wl2, "gpt-4-turbo"));
        assert!(!model_allowed(&wl2, "gpt-4"));

        // 中间通配
        let wl3 = vec!["claude-*-sonnet".to_string()];
        assert!(model_allowed(&wl3, "claude-3-5-sonnet"));
        assert!(!model_allowed(&wl3, "claude-3-5-haiku"));

        // 单个 `*` 放行一切
        assert!(model_allowed(&["*".to_string()], "随便什么"));
    }

    #[test]
    fn 解析失败与空串都当成不限() {
        assert!(parse_allowed_models("").is_empty());
        assert!(parse_allowed_models("   ").is_empty());
        // 坏 JSON 也当不限：闸门失效比闸门误伤更容易被发现
        assert!(parse_allowed_models("{不是数组}").is_empty());
        assert!(parse_allowed_models("[\"a\",").is_empty());
        // 合法输入正常解析
        assert_eq!(
            parse_allowed_models(r#"["a","b"]"#),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn 序列化空列表是空串() {
        assert_eq!(serialize_allowed_models(&[]), "");
        assert_eq!(serialize_allowed_models(&["a".to_string()]), r#"["a"]"#);
        // 往返一致
        let round = parse_allowed_models(&serialize_allowed_models(&["x".into(), "y".into()]));
        assert_eq!(round, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn 总开关关掉时两条闸门都不生效() {
        let off = BudgetConfig { enabled: false };
        // 预算早就超了、模型也不在白名单，仍然放行
        assert_eq!(
            gate(&off, 99_999_999, 1, "USD", &["别的模型".into()], "gpt-4"),
            GateDecision::Allow
        );
        // 对照组：同样的输入，开关打开时必须拒绝 ——
        let on = BudgetConfig { enabled: true };
        assert!(matches!(
            gate(&on, 99_999_999, 1, "USD", &["别的模型".into()], "gpt-4"),
            GateDecision::BudgetExceeded { .. }
        ));
    }

    #[test]
    fn 预算优先于白名单() {
        // 两者都命中时先报预算：整体原因比单次请求原因更容易指导用户动作
        let on = BudgetConfig { enabled: true };
        match gate(&on, 2_000_000, 1_000_000, "USD", &["别的".into()], "gpt-4") {
            GateDecision::BudgetExceeded { .. } => {}
            other => panic!("两者都命中时应先报预算，实际 {other:?}"),
        }
    }

    #[test]
    fn 白名单在预算未超时生效() {
        let on = BudgetConfig { enabled: true };
        assert_eq!(
            gate(&on, 0, 1_000_000, "USD", &["别的".into()], "gpt-4"),
            GateDecision::ModelNotAllowed {
                model: "gpt-4".into()
            }
        );
        // 在白名单里就放行
        assert_eq!(
            gate(&on, 0, 1_000_000, "USD", &["gpt-4".into()], "gpt-4"),
            GateDecision::Allow
        );
    }

    #[test]
    fn 月份起点是当月一日零点() {
        // 2026-10-06T12:34:56Z
        let now = 1_791_286_496i64;
        let start = month_start_secs(now);
        use chrono::{TimeZone, Utc};
        let dt = Utc.timestamp_opt(start, 0).single().unwrap();
        use chrono::{Datelike, Timelike};
        assert_eq!(dt.day(), 1);
        assert_eq!(dt.hour(), 0);
        assert_eq!(dt.minute(), 0);
        assert_eq!(dt.second(), 0);
        assert_eq!(dt.month(), 10);
        assert!(start <= now);
        // 同日不同时刻算出的月初相同
        assert_eq!(month_start_secs(now + 3600), start);
    }

    #[test]
    fn 解析_access_key_id() {
        assert_eq!(access_key_id_of(Some("remote-key:abc")), Some("abc"));
        assert_eq!(access_key_id_of(Some("local-unified-key")), None);
        assert_eq!(access_key_id_of(None), None);
        // id 里含冒号时只切第一个
        assert_eq!(access_key_id_of(Some("remote-key:a:b")), Some("a:b"));
    }
}

//! 远程 HTTPS 反代模式使用的客户端访问 Key 元数据。
//!
//! 数据库只保存 `key_hash`，原始 Key 只在创建命令的返回值中出现一次。

#[derive(Debug, Clone)]
pub struct RemoteAccessKey {
    pub id: String,
    pub label: String,
    pub key_hash: String,
    pub enabled: bool,
    pub rpm_limit: u32,
    /// 月度预算，单位 micros（1e-6 货币单位）。**0 = 不限。**
    ///
    /// 用整数而不是浮点：预算比对在边界上不能有浮点误差，
    /// `0.1 + 0.2 != 0.3` 会让「刚好花完」的判定随机翻转。
    pub monthly_budget_micros: i64,
    /// 预算币种。空串表示这个 Key 没设预算。
    ///
    /// 必须有币种才能比：USD 100 + CNY 100 相加是没有意义的。
    pub budget_currency: String,
    /// 模型白名单。**空 = 不限。** 支持 `*` 通配，不带通配时精确匹配。
    pub allowed_models: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

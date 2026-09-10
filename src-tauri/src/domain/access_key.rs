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
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

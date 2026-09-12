//! 配置包读写（多设备同步用）。
//!
//! 与 CC Switch 的「配置目录同步」等价：导出把 `config.toml` 与 `gateway.db`
//! 复制到目标目录；导入从包中读取这两项并返回给调用方应用到本机。真正的落库
//! 与热更新由 commands 层负责，本模块只处理文件与解析，便于在集成测试中直接
//! 覆盖「密钥无法解密」这类跨设备场景。

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::db::repo;
use crate::domain::Provider;

/// 配置包里使用的两个文件名。
pub const BUNDLE_CONFIG: &str = "config.toml";
pub const BUNDLE_DB: &str = "gateway.db";

#[derive(Debug, Clone)]
pub struct BundleContents {
    /// 包内的原始 TOML 文本；缺失时为 `None`。
    pub config_toml: Option<String>,
    /// 包内读出的 Provider（含模型）；没有数据库文件时为空。
    pub providers: Vec<Provider>,
    /// 凭据无法用本机主密钥解密的 Provider 名称。
    pub providers_missing_key: Vec<String>,
}

/// 校验目录里至少有一个包文件，避免把任意目录当成导出包。
pub fn ensure_bundle_dir(dir: &Path) -> Result<(), String> {
    let has_config = dir.join(BUNDLE_CONFIG).is_file();
    let has_db = dir.join(BUNDLE_DB).is_file();
    if !has_config && !has_db {
        return Err(format!(
            "该目录里没有 {BUNDLE_CONFIG} 或 {BUNDLE_DB}，不是导出包"
        ));
    }
    Ok(())
}

/// 读取配置包。凭据解密失败不视为整体失败：该 Provider 的配置保留，但凭据被
/// 清空并记入 `providers_missing_key`，界面据此提示重新填写。
pub async fn read_bundle(dir: &Path) -> Result<BundleContents, String> {
    ensure_bundle_dir(dir)?;

    let config_toml = {
        let path = dir.join(BUNDLE_CONFIG);
        if path.is_file() {
            Some(
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("读取 {BUNDLE_CONFIG} 失败：{e}"))?,
            )
        } else {
            None
        }
    };

    let db_path = dir.join(BUNDLE_DB);
    let raw_providers = if db_path.is_file() {
        read_providers(&db_path).await?
    } else {
        Vec::new()
    };

    let mut providers_missing_key = Vec::new();
    let mut providers = Vec::with_capacity(raw_providers.len());
    for mut provider in raw_providers {
        if !provider.api_key_enc.trim().is_empty()
            && crate::crypto::decrypt(&provider.api_key_enc).is_err()
        {
            providers_missing_key.push(provider.name.clone());
            provider.api_key_enc.clear();
        }
        providers.push(provider);
    }

    Ok(BundleContents {
        config_toml,
        providers,
        providers_missing_key,
    })
}

/// 只读打开包内数据库并读出 Provider。绝不修改导出包。
async fn read_providers(db_path: &Path) -> Result<Vec<Provider>, String> {
    let pool = open_bundle_pool(db_path, true).await?;
    let result = repo::list_providers(&pool)
        .await
        .map_err(|e| format!("读取包内 Provider 失败：{e}"));
    pool.close().await;
    result
}

/// 导出：把源数据目录里的配置与数据库复制到目标目录。
///
/// 数据库处于 WAL 模式：直接复制 `gateway.db` 会丢掉仍在 `-wal` 里的已提交数据
/// （实测会把刚写入的 Provider 导出成空库）。因此这里通过 SQLite 的
/// `VACUUM INTO` 生成一致性副本，它包含了当前 WAL 中的所有已提交内容。
pub async fn export_bundle(source: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let config = source.join(BUNDLE_CONFIG);
    if config.is_file() {
        std::fs::copy(&config, dest.join(BUNDLE_CONFIG)).map_err(|e| e.to_string())?;
    }
    let db = source.join(BUNDLE_DB);
    if db.is_file() {
        vacuum_into(&db, &dest.join(BUNDLE_DB)).await?;
    }
    Ok(())
}

/// 用 `VACUUM INTO` 把库文件（含 WAL 中的已提交事务）写成一个独立的单文件副本。
/// 目标文件已存在时 SQLite 会报错，这里先删除以保证重复导出可用。
async fn vacuum_into(source_db: &Path, dest_db: &Path) -> Result<(), String> {
    if dest_db.exists() {
        std::fs::remove_file(dest_db).map_err(|e| e.to_string())?;
    }
    let pool = open_bundle_pool(source_db, false).await?;
    let destination = dest_db
        .to_str()
        .ok_or_else(|| "目标路径无法用于数据库导出".to_string())?
        .to_owned();
    let result = sqlx::query("VACUUM INTO ?")
        .bind(destination)
        .execute(&pool)
        .await
        .map_err(|e| format!("导出 {BUNDLE_DB} 失败：{e}"));
    pool.close().await;
    result.map(|_| ())
}

/// 以固定方式打开包相关数据库：导出/备份需要写权限，读取包内数据库一律只读。
async fn open_bundle_pool(path: &Path, read_only: bool) -> Result<SqlitePool, String> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(read_only)
        .busy_timeout(Duration::from_secs(5));
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| format!("打开数据库 {} 失败：{e}", path.display()))
}

/// 导入前把当前数据目录整体复制到带时间戳的新目录。目标目录已存在时直接失败，
/// 绝不覆盖上一次的备份。
///
/// 数据库备份同样经过 `VACUUM INTO`：直接复制 WAL 模式下的主库文件会把「导入前
/// 的完整状态」退化成不完整快照，让回退手段本身失去意义。
pub async fn backup_data_dir(source: &Path, timestamp: &str) -> Result<PathBuf, String> {
    let backup = source.join(format!("backup-{timestamp}"));
    if backup.exists() {
        return Err(format!("备份目录已存在：{}", backup.display()));
    }
    std::fs::create_dir_all(&backup).map_err(|e| e.to_string())?;
    let config = source.join(BUNDLE_CONFIG);
    if config.is_file() {
        std::fs::copy(&config, backup.join(BUNDLE_CONFIG)).map_err(|e| e.to_string())?;
    }
    let db = source.join(BUNDLE_DB);
    if db.is_file() {
        vacuum_into(&db, &backup.join(BUNDLE_DB)).await?;
    }
    Ok(backup)
}

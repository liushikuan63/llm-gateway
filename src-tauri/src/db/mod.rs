//! SQLite 存储层。
//! 选 SQLite 的理由：单文件、零运维、便于快照整体拷贝（这是 CC Switch 与
//! FreeLLMAPI 的共同选择），并通过 WAL + 原子写入避免配置写坏。

pub mod migrations;
pub mod repo;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use std::time::Duration;

use crate::config;

#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    pub async fn connect(_cfg: &config::AppConfig) -> anyhow::Result<Self> {
        Self::connect_path(config::db_path()).await
    }

    /// 打开位于指定路径的数据库。
    ///
    /// 生产路径与测试路径共用这一初始化过程，确保所有持久化库都启用 WAL、
    /// 外键校验和相同的忙等待策略。
    pub async fn connect_path(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }

        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // WAL：读写不互斥，代理高并发写日志时不会被 UI 读阻塞
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true);

        Self::connect_with_options(opts, 8).await
    }

    /// 创建隔离的内存数据库，供集成测试和嵌入式调用使用。
    ///
    /// SQLite 的 `:memory:` 数据库按连接隔离，因此连接池必须限制为一个连接，
    /// 否则迁移和后续查询会落在不同的空数据库中。
    pub async fn connect_in_memory() -> anyhow::Result<Self> {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true);

        Self::connect_with_options(opts, 1).await
    }

    async fn connect_with_options(
        opts: SqliteConnectOptions,
        max_connections: u32,
    ) -> anyhow::Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(opts)
            .await?;

        migrations::run(&pool).await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

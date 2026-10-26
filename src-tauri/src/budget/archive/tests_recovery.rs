use super::*;
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
fn root() -> PathBuf {
    std::env::var_os("LLMGW_ARCHIVE_RECOVERY_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("llmgw-owned-archive-{}", uuid::Uuid::new_v4()))
        })
}
fn guard(p: &std::path::Path) {
    assert!(p.is_absolute());
    let n = p.file_name().unwrap().to_string_lossy();
    assert!(n.starts_with("owned-recovery-") || n.starts_with("llmgw-owned-archive-"));
}
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[tokio::test]
async fn owned_actual_process_crash_before_and_after_commit_recovers_without_money_loss() {
    if let Ok(role) = std::env::var("LLMGW_ARCHIVE_CHILD_ROLE") {
        assert!(role == "before_commit" || role == "after_commit");
        let r = root();
        guard(&r);
        assert_eq!(
            std::fs::read_to_string(r.join("owner.txt")).unwrap(),
            "owned-financial-archive-test-v1"
        );
        let path = PathBuf::from(std::env::var_os("LLMGW_ARCHIVE_CHILD_DB").unwrap());
        assert_eq!(path.parent().unwrap().parent().unwrap(), r);
        let ready = path.parent().unwrap().join("ready.json");
        let db = Db::connect_path(&path).await.unwrap();
        let m = crate::budget::month_start_secs(at().timestamp());
        if role == "before_commit" {
            let arrived = Arc::new(tokio::sync::Notify::new());
            let proceed = Arc::new(tokio::sync::Notify::new());
            let signal = arrived.clone();
            tokio::spawn(async move {
                signal.notified().await;
                std::fs::write(ready, "before_commit").unwrap();
            });
            crate::budget::archive::AFTER_DELETE
                .scope((arrived, proceed), purge(db.pool(), m + 2))
                .await
                .unwrap();
            panic!("owned child unexpectedly committed before parent termination");
        } else {
            purge(db.pool(), m + 2).await.unwrap();
            std::fs::write(ready, "after_commit").unwrap();
            std::future::pending::<()>().await;
        }
    } else {
        let r = root();
        guard(&r);
        std::fs::create_dir(&r).unwrap();
        std::fs::write(r.join("owner.txt"), "owned-financial-archive-test-v1").unwrap();
        for role in ["before_commit", "after_commit"] {
            let d = r.join(role);
            std::fs::create_dir(&d).unwrap();
            let path = d.join("owned.sqlite3");
            let db = Db::connect_path(&path).await.unwrap();
            key(&db, "k").await;
            let m = crate::budget::month_start_secs(at().timestamp());
            row(&db, "k", m + 1, Some(1.0), Some("usd")).await;
            db.pool().close().await;
            drop(db);
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(d.join("child.log"))
                .unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.arg("--exact").arg("budget::archive::tests::recovery::owned_actual_process_crash_before_and_after_commit_recovers_without_money_loss").arg("--nocapture")
                .env("LLMGW_ARCHIVE_CHILD_ROLE",role).env("LLMGW_ARCHIVE_RECOVERY_ROOT",&r).env("LLMGW_ARCHIVE_CHILD_DB",&path).stdout(Stdio::from(file.try_clone().unwrap())).stderr(Stdio::from(file));
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let mut child = ChildGuard(command.spawn().unwrap());
            let pid = child.0.id();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !d.join("ready.json").exists() {
                    assert!(
                        child.0.try_wait().unwrap().is_none(),
                        "owned child exited before ready; inspect preserved child.log"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("owned child did not reach exact transaction checkpoint");
            assert_eq!(std::fs::read_to_string(d.join("ready.json")).unwrap(), role);
            child.0.kill().unwrap();
            let exit = child.0.wait().unwrap();
            assert!(!exit.success());
            let reopened = Db::connect_path(&path).await.unwrap();
            let expected = if role == "before_commit" {
                (1, 0)
            } else {
                (0, 1)
            };
            assert_eq!(
                (
                    count(&reopened, "requests").await,
                    count(&reopened, "budget_legacy_archive").await
                ),
                expected
            );
            assert_eq!(used(&reopened, "k", m, "usd").await.unwrap(), 1_000_000);
            purge(reopened.pool(), m + 2).await.unwrap();
            purge(reopened.pool(), m + 2).await.unwrap();
            assert_eq!(count(&reopened, "requests").await, 0);
            assert_eq!(count(&reopened, "budget_legacy_archive").await, 1);
            assert_eq!(used(&reopened, "k", m, "usd").await.unwrap(), 1_000_000);
            let check: String = sqlx::query_scalar("PRAGMA quick_check")
                .fetch_one(reopened.pool())
                .await
                .unwrap();
            assert_eq!(check, "ok");
            reopened.pool().close().await;
            eprintln!(
                "OWNED_ARCHIVE_CRASH {}",
                serde_json::json!({"role":role,"pid":pid,"exit":exit.code(),"database":path,"parent_terminated_only_owned_child":true,"recovered":true,"power_loss_tested":false})
            );
        }
    }
}

#[tokio::test]
async fn concurrent_cleanup_and_auth_readers_keep_one_complete_financial_snapshot() {
    let r = std::env::temp_dir().join(format!("llmgw-owned-archive-{}", uuid::Uuid::new_v4()));
    guard(&r);
    std::fs::create_dir(&r).unwrap();
    let db = Db::connect_path(r.join("concurrency.sqlite3"))
        .await
        .unwrap();
    key(&db, "k").await;
    let m = crate::budget::month_start_secs(at().timestamp());
    let mut tx = db.pool().begin().await.unwrap();
    for _ in 0..1003 {
        sqlx::query("INSERT INTO requests(ts,requested_model,access_key_id,cost,currency,refined_prompt) VALUES(?,'owned','k',0.0000004,'usd','owned concurrent prompt')").bind(m+1).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    let before = used(&db, "k", m, "usd").await.unwrap();
    assert_eq!(before, 401);
    let reads =
        async {
            futures_util::future::join_all((0..12).map(|_| async {
                crate::budget::holds::used_for_auth(db.pool(), "k", m, "usd").await
            }))
            .await
        };
    let (a, b, results) = tokio::join!(purge(db.pool(), m + 2), purge(db.pool(), m + 2), reads);
    a.unwrap();
    b.unwrap();
    for value in results {
        assert_eq!(value.unwrap(), before);
    }
    assert_eq!(count(&db, "requests").await, 0);
    assert_eq!(count(&db, "budget_legacy_archive").await, 1003);
    assert_eq!(used(&db, "k", m, "usd").await.unwrap(), before);
    db.pool().close().await;
    eprintln!(
        "OWNED_ARCHIVE_CONCURRENCY {}",
        serde_json::json!({"directory":r,"rows":1003,"readers":12,"purges":2,"micro_total":before,"pool_closed":true})
    );
}

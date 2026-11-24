//! Native, one-record consumer of the private journal CAS snapshot.
//! Dialog waits hold neither a SQLite transaction nor the client-command lock.
use std::future::Future;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crate::db::{repo::client_usage_journal as journal, Db};
use serde::Serialize;

#[derive(Default)]
pub struct Controls {
    active: parking_lot::Mutex<Option<Active>>,
}

struct Active {
    owner: String,
    request_id: String,
    cancelled: Arc<AtomicBool>,
    committing: bool,
}

struct Lease<'a> {
    controls: &'a Controls,
    cancelled: Arc<AtomicBool>,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.controls.active.lock().take();
    }
}

impl Controls {
    fn begin(&self, owner: &str, request_id: &str) -> Result<Lease<'_>, String> {
        if uuid::Uuid::parse_str(request_id).is_err() || request_id.len() != 36 {
            return Err("client_usage_review_invalid_request".into());
        }
        let mut active = self.active.lock();
        if active.is_some() {
            return Err("已有原生核阅窗口，请先关闭它后重试".into());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        *active = Some(Active {
            owner: owner.into(),
            request_id: request_id.into(),
            cancelled: cancelled.clone(),
            committing: false,
        });
        Ok(Lease {
            controls: self,
            cancelled,
        })
    }

    pub fn cancel(&self, owner: &str, request_id: &str) -> bool {
        let active = self.active.lock();
        let Some(active) = active
            .as_ref()
            .filter(|job| job.owner == owner && job.request_id == request_id && !job.committing)
        else {
            return false;
        };
        active.cancelled.store(true, Ordering::SeqCst);
        true
    }

    pub fn cancel_owner(&self, owner: Option<&str>) {
        let active = self.active.lock();
        if let Some(active) = active
            .as_ref()
            .filter(|job| !job.committing && owner.map_or(true, |owner| job.owner == owner))
        {
            active.cancelled.store(true, Ordering::SeqCst);
        }
    }

    fn start_commit(&self, lease: &Lease<'_>) -> bool {
        let mut active = self.active.lock();
        let Some(active) = active
            .as_mut()
            .filter(|job| Arc::ptr_eq(&job.cancelled, &lease.cancelled))
        else {
            return false;
        };
        if active.cancelled.load(Ordering::SeqCst) {
            return false;
        }
        // The same mutex linearizes cancellation and the one-shot DAO call.
        // Cancellation reports false once commit has started, even while SQL
        // waits for a writer. Never disguise a known commit as cancellation.
        active.committing = true;
        true
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReviewResult {
    Reviewed { reviewed_count: usize },
    Cancelled,
}

#[derive(Clone, Serialize)]
pub struct ReviewReady {
    pub request_id: String,
}

/// Registration is acknowledged before native presentation. A frontend that
/// unmounted before the ready callback sends cancellation again after ready.
/// Keep the busy lease until a shown dialog actually closes, even if cancelled.
async fn execute<F, Fut>(
    db: &Db,
    controls: &Controls,
    owner: &str,
    request_id: &str,
    attempt_id: &str,
    ready: impl FnOnce() -> Result<(), String>,
    show: F,
) -> Result<ReviewResult, String>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    let lease = controls.begin(owner, request_id)?;
    ready()?;
    if lease.cancelled.load(Ordering::SeqCst) {
        return Ok(ReviewResult::Cancelled);
    }
    let pending = journal::prepare_review(db, &[attempt_id.to_owned()])
        .await
        .map_err(|e| e.to_string())?;
    if lease.cancelled.load(Ordering::SeqCst) {
        return Ok(ReviewResult::Cancelled);
    }
    let accepted = show(pending.native_confirmation_details()).await?;
    if !accepted || !controls.start_commit(&lease) {
        return Ok(ReviewResult::Cancelled);
    }
    // Move this snapshot once. No retry on conflict or unknown commit, and no
    // post-commit cancellation is allowed to turn a known success into failure.
    let reviewed_count = journal::confirm_review(db, pending)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ReviewResult::Reviewed { reviewed_count })
}

pub async fn review(
    window: tauri::WebviewWindow,
    db: &Db,
    controls: &Controls,
    request_id: String,
    attempt_id: String,
    on_ready: tauri::ipc::Channel<ReviewReady>,
) -> Result<ReviewResult, String> {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogResult};
    let owner = window.label().to_owned();
    let ready_id = request_id.clone();
    execute(
        db,
        controls,
        &owner,
        &request_id,
        &attempt_id,
        move || {
            on_ready
                .send(ReviewReady {
                    request_id: ready_id,
                })
                .map_err(|_| "原生核阅就绪通道不可用；请重新读取后重试".into())
        },
        move |details| async move {
            let (send, receive) = tokio::sync::oneshot::channel();
            window
                .dialog()
                .message(details)
                .title("核阅已知未发送记录")
                .parent(&window)
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "确认核阅这1条".into(),
                    "取消".into(),
                ))
                .show_with_result(move |result| {
                    let _ = send.send(result);
                });
            let result = receive
                .await
                .map_err(|_| "原生核阅通道中断；未自动重试，请重新读取".to_owned())?;
            let visible = window
                .is_visible()
                .map_err(|_| "原生窗口已不可用；请重新读取".to_owned())?;
            Ok(visible
                && matches!(result, MessageDialogResult::Custom(label) if label == "确认核阅这1条"))
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_native_cancellation_has_a_commit_linearization_point() {
        let controls = Controls::default();
        let request = uuid::Uuid::new_v4().to_string();
        let lease = controls.begin("owned", &request).unwrap();
        assert!(controls.start_commit(&lease));
        assert!(!controls.cancel("owned", &request));
        controls.cancel_owner(Some("owned"));
        assert!(!lease.cancelled.load(Ordering::SeqCst));
        drop(lease);
        let lease = controls.begin("owned", &request).unwrap();
        controls.cancel_owner(Some("foreign"));
        assert!(!lease.cancelled.load(Ordering::SeqCst));
        controls.cancel_owner(Some("owned"));
        assert!(!controls.start_commit(&lease));
    }
    async fn gate(db: &Db) -> String {
        let id = journal::record_intent(
            db,
            &uuid::Uuid::new_v4().to_string(),
            journal::AttemptKind::AgentIntent,
        )
        .await
        .unwrap();
        journal::mark_agent_gate(db, &id, journal::AgentGateReason::Quota)
            .await
            .unwrap();
        id
    }
    async fn snapshot(db: &Db, id: &str) -> (Option<i64>, i64) {
        sqlx::query_as("SELECT j.reviewed_at,v.revision FROM usage_attempt_journal_v2 j JOIN usage_attempt_versions_v2 v ON v.attempt_id=j.attempt_id WHERE j.attempt_id=?").bind(id).fetch_one(db.pool()).await.unwrap()
    }
    #[tokio::test]
    async fn journal_native_cancel_and_channel_failure_preserve_rows() {
        let db = Db::connect_in_memory().await.unwrap();
        let id = gate(&db).await;
        let before = snapshot(&db, &id).await;
        let controls = Controls::default();
        let request = uuid::Uuid::new_v4().to_string();
        let display_id = id.clone();
        let result = execute(
            &db,
            &controls,
            "owned",
            &request,
            &id,
            || Ok(()),
            |text| async move {
                assert!(text.contains(&display_id));
                assert!(text.contains("scope："));
                assert!(text.contains("revision："));
                Ok(false)
            },
        )
        .await
        .unwrap();
        assert_eq!(result, ReviewResult::Cancelled);
        assert_eq!(snapshot(&db, &id).await, before);
        assert!(execute(
            &db,
            &controls,
            "owned",
            &request,
            &id,
            || Err("ready lost".into()),
            |_| async {
                panic!("no native window");
                #[allow(unreachable_code)]
                Ok(true)
            }
        )
        .await
        .is_err());
        assert_eq!(snapshot(&db, &id).await, before);
        assert!(controls.active.lock().is_none());
        db.pool().close().await;
    }
    #[tokio::test]
    async fn journal_native_cancel_is_owner_bound_and_busy_until_dialog_close() {
        let db = Db::connect_in_memory().await.unwrap();
        let id = gate(&db).await;
        let controls = Controls::default();
        let request = uuid::Uuid::new_v4().to_string();
        let before = snapshot(&db, &id).await;
        let result = execute(
            &db,
            &controls,
            "owned",
            &request,
            &id,
            || Ok(()),
            |_| async {
                assert!(!controls.cancel("foreign", &request));
                assert!(controls.cancel("owned", &request));
                assert!(controls
                    .begin("owned", &uuid::Uuid::new_v4().to_string())
                    .is_err());
                Ok(true)
            },
        )
        .await
        .unwrap();
        assert_eq!(result, ReviewResult::Cancelled);
        assert_eq!(snapshot(&db, &id).await, before);
        assert!(controls.active.lock().is_none());
        db.pool().close().await;
    }
    #[tokio::test]
    async fn journal_native_display_retains_exact_snapshot_and_refuses_changed_version() {
        let db = Db::connect_in_memory().await.unwrap();
        let id = gate(&db).await;
        let controls = Controls::default();
        let db_ref = &db;
        let id_ref = &id;
        let result=execute(&db,&controls,"owned",&uuid::Uuid::new_v4().to_string(),&id,||Ok(()),|text|async move {
            assert!(text.contains("数据库 generation：")); assert!(text.contains("运行时："));
            // A write can complete while native confirmation is pending; it
            // changes revision even when the value remains identical.
            sqlx::query("UPDATE usage_attempt_journal_v2 SET agent_gate_reason=agent_gate_reason WHERE attempt_id=?").bind(id_ref).execute(db_ref.pool()).await.unwrap();
            Ok(true)
        }).await.unwrap_err();
        assert!(result.contains("client_usage_journal_state_conflict"));
        assert_eq!(snapshot(&db, &id).await.0, None);
        assert!(controls.active.lock().is_none());
        let result = execute(
            &db,
            &controls,
            "owned",
            &uuid::Uuid::new_v4().to_string(),
            &id,
            || Ok(()),
            |_| async { Ok(true) },
        )
        .await
        .unwrap();
        assert_eq!(result, ReviewResult::Reviewed { reviewed_count: 1 });
        db.pool().close().await;
    }
}

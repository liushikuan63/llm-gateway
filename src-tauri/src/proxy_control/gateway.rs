//! Owns only the listener task started by this controller.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::proxy::server::{serve_ready, GatewayState};

const SHUTDOWN_DRAIN: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct GatewayServiceStatus {
    pub running: bool,
    pub bound_addr: Option<String>,
    pub last_error: Option<String>,
}

pub struct GatewayService {
    state: Arc<GatewayState>,
    runtime: Mutex<Runtime>,
}

#[derive(Default)]
struct Runtime {
    listener: Option<OwnedListener>,
    status: GatewayServiceStatus,
}

struct OwnedListener {
    task: JoinHandle<Result<(), String>>,
    ready: Option<oneshot::Receiver<Result<SocketAddr, String>>>,
    shutdown: Option<oneshot::Sender<()>>,
    stopping: bool,
}

impl OwnedListener {
    fn request_stop(&mut self) {
        self.stopping = true;
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

impl Drop for OwnedListener {
    fn drop(&mut self) {
        // The supervisor owns the server and its bounded drain even if its IPC
        // waiter or the last controller owner disappears. Do not abort it here:
        // accepted connections must receive graceful shutdown before cleanup.
        self.request_stop();
    }
}

struct ServerTask(JoinHandle<Result<(), String>>);

impl Drop for ServerTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn supervise_server(
    mut server: ServerTask,
    shutdown: oneshot::Sender<()>,
    stop: oneshot::Receiver<()>,
    state: Arc<GatewayState>,
) -> Result<(), String> {
    let result = tokio::select! {
        result = &mut server.0 => {
            result.unwrap_or_else(|error| Err(format!("网关服务任务异常结束: {error}")))
        }
        _ = stop => {
            // This deadline belongs to the supervisor, never the IPC caller.
            // Allow Axum's watch signal to reach the accepted connections.
            let _ = shutdown.send(());
            match tokio::time::timeout(SHUTDOWN_DRAIN, &mut server.0).await {
                Ok(result) => result.unwrap_or_else(|error| Err(format!("网关服务任务异常结束: {error}"))),
                Err(_) => {
                    server.0.abort();
                    let _ = (&mut server.0).await;
                    Err("网关关闭等待超过 5 秒，已取消自有监听任务".to_owned())
                }
            }
        }
    };
    // The supervisor owns this drain even if its IPC waiter is cancelled.
    // Accepted connections have finished before clean completion is reported.
    let audits = state.drain_audits(SHUTDOWN_DRAIN).await;
    match (result, audits) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(server), Err(audit)) => Err(format!("{server}; {audit}")),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
    }
}

impl GatewayService {
    pub fn new(state: Arc<GatewayState>) -> Self {
        Self {
            state,
            runtime: Mutex::new(Runtime::default()),
        }
    }

    fn include_audit_failure(&self, runtime: &mut Runtime) {
        if runtime.status.last_error.is_none() {
            runtime.status.last_error = self.state.audit_failure();
        }
    }

    pub async fn status(&self) -> GatewayServiceStatus {
        let mut runtime = self.runtime.lock().await;
        runtime.refresh().await;
        self.include_audit_failure(&mut runtime);
        runtime.status.clone()
    }

    pub async fn set_enabled(&self, enabled: bool) -> Result<GatewayServiceStatus, String> {
        let mut runtime = self.runtime.lock().await;
        if !enabled {
            let had_listener = runtime.listener.is_some();
            if let Some(listener) = runtime.listener.as_mut() {
                listener.request_stop();
            }
            // Await the owned task, including ListenerLifetime's drop, before
            // releasing this mutex and letting another caller restart it.
            runtime.refresh().await;
            if !had_listener {
                // Covers pending writes after an earlier stopped listener.
                if let Err(error) = self.state.drain_audits(SHUTDOWN_DRAIN).await {
                    runtime.status.last_error = Some(error);
                }
            }
            self.include_audit_failure(&mut runtime);
            return Ok(runtime.status.clone());
        }

        runtime.refresh().await;
        self.include_audit_failure(&mut runtime);
        if runtime.status.running {
            return Ok(runtime.status.clone());
        }

        let (ready_tx, ready_rx) = oneshot::channel();
        let (server_shutdown_tx, server_shutdown_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let state = self.state.clone();
        let server = ServerTask(tokio::spawn(async move {
            serve_ready(state, ready_tx, server_shutdown_rx)
                .await
                .map_err(|error| error.to_string())
        }));
        runtime.listener = Some(OwnedListener {
            task: tokio::spawn(supervise_server(
                server,
                server_shutdown_tx,
                shutdown_rx,
                self.state.clone(),
            )),
            ready: Some(ready_rx),
            shutdown: Some(shutdown_tx),
            stopping: false,
        });
        // Keep both receiver and task in Runtime while awaiting. Cancelling an
        // IPC caller leaves ownership intact for status() or the next command.
        runtime.refresh().await;
        self.include_audit_failure(&mut runtime);
        if runtime.status.running {
            Ok(runtime.status.clone())
        } else {
            Err(runtime
                .status
                .last_error
                .clone()
                .unwrap_or_else(|| "网关服务未能启动".to_owned()))
        }
    }
}

impl Runtime {
    async fn refresh(&mut self) {
        let Some(listener) = self.listener.as_mut() else {
            return;
        };
        if !listener.stopping {
            if let Some(ready) = listener.ready.as_mut() {
                let result = ready.await;
                listener.ready = None;
                match result {
                    Ok(Ok(addr)) => {
                        self.status.running = true;
                        self.status.bound_addr = Some(addr.to_string());
                        self.status.last_error = None;
                    }
                    Ok(Err(error)) => {
                        self.status.last_error = Some(error);
                        // The startup failed; still await its final cleanup.
                        listener.stopping = true;
                    }
                    Err(_) => {
                        self.status.last_error = Some("网关服务在确认监听前已结束".to_owned());
                        listener.stopping = true;
                    }
                }
            }
        }

        if listener.stopping || listener.task.is_finished() {
            let intentional_stop = listener.stopping;
            // Await by mutable reference: cancelling this waiter must not take
            // the JoinHandle out of Runtime and detach unfinished cleanup.
            let result = (&mut listener.task).await;
            self.listener = None;
            self.status.running = false;
            self.status.bound_addr = None;
            if intentional_stop {
                match result {
                    Ok(Err(error)) => self.status.last_error = Some(error),
                    Err(error) => {
                        self.status.last_error = Some(format!("网关关闭任务异常结束: {error}"));
                    }
                    Ok(Ok(())) => {}
                }
            } else {
                self.status.last_error = Some(match result {
                    Ok(Err(error)) => error,
                    Err(error) => format!("网关服务任务异常结束: {error}"),
                    Ok(Ok(())) => "网关服务已意外结束".to_owned(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn join_error_drain_control(stop_first: bool) {
        for audit_fails in [false, true] {
            let db = crate::db::Db::connect_in_memory().await.unwrap();
            let state = Arc::new(GatewayState::new(
                db.clone(),
                crate::config::AppConfig::default(),
            ));
            let release = Arc::new(tokio::sync::Notify::new());
            let (audit_started, audit_arrived) = oneshot::channel();
            state.audit_tasks().spawn({let release=release.clone();let db=db.clone(); async move {
                let _=audit_started.send(());
                release.notified().await;
                if audit_fails {
                    sqlx::query("INSERT INTO owned_missing_audit_table VALUES (1)").execute(db.pool()).await?;
                } else {
                    sqlx::query("INSERT INTO requests(ts,requested_model,status) VALUES(1,'owned-join-audit',499)").execute(db.pool()).await?;
                }
                Ok::<_, sqlx::Error>(())
            }});
            audit_arrived.await.unwrap();
            let (server_stop, server_stop_rx) = oneshot::channel();
            let (stop, stop_rx) = oneshot::channel();
            let mut stop = Some(stop);
            let (crash, crash_rx) = oneshot::channel();
            let (crashed, saw_crash) = oneshot::channel();
            async fn fail_server(
                stop_first: bool,
                stop: oneshot::Receiver<()>,
                crash: oneshot::Receiver<()>,
                crashed: oneshot::Sender<()>,
            ) -> Result<(), String> {
                if stop_first {
                    let _ = stop.await;
                } else {
                    let _ = crash.await;
                }
                let _ = crashed.send(());
                panic!("owned supervised server failure");
            }
            let server = ServerTask(tokio::spawn(fail_server(
                stop_first,
                server_stop_rx,
                crash_rx,
                crashed,
            )));
            let mut supervised = tokio::spawn(supervise_server(
                server,
                server_stop,
                stop_rx,
                state.clone(),
            ));
            if stop_first {
                let _ = stop.take().unwrap().send(());
            } else {
                let _ = crash.send(());
            }
            saw_crash.await.unwrap();
            if tokio::time::timeout(Duration::from_millis(80), &mut supervised)
                .await
                .is_ok()
            {
                panic!("JoinError skipped the outstanding owned audit drain");
            }
            release.notify_one();
            let result = tokio::time::timeout(Duration::from_secs(2), supervised)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(result.contains("网关服务任务异常结束"), "{result}");
            if audit_fails {
                assert!(
                    result.contains("审计") && result.contains("失败"),
                    "both shutdown failures must remain visible: {result}"
                );
            }
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(
                rows,
                if audit_fails { 0 } else { 1 },
                "server panic cannot turn a scheduled audit into clean shutdown"
            );
        }
    }

    #[tokio::test]
    async fn join_error_before_stop_still_drains_owned_audits() {
        join_error_drain_control(false).await;
    }
    #[tokio::test]
    async fn join_error_during_stop_still_drains_owned_audits() {
        join_error_drain_control(true).await;
    }

    #[tokio::test]
    async fn cancelled_stop_waiter_does_not_reset_the_owned_drain_deadline() {
        struct Cleanup(Option<oneshot::Sender<()>>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(cleaned) = self.0.take() {
                    let _ = cleaned.send(());
                }
            }
        }
        let (server_shutdown_tx, server_shutdown_rx) = oneshot::channel();
        let (signal_seen_tx, signal_seen_rx) = oneshot::channel();
        let (cleaned_tx, mut cleaned_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = ServerTask(tokio::spawn(async move {
            let _cleanup = Cleanup(Some(cleaned_tx));
            server_shutdown_rx.await.unwrap();
            let _ = signal_seen_tx.send(());
            // A deliberately stalled in-flight task must not hang shutdown.
            std::future::pending::<Result<(), String>>().await
        }));
        let state = Arc::new(GatewayState::new(
            crate::db::Db::connect_in_memory().await.unwrap(),
            crate::config::AppConfig::default(),
        ));
        let mut runtime = Runtime {
            listener: Some(OwnedListener {
                task: tokio::spawn(supervise_server(
                    server,
                    server_shutdown_tx,
                    shutdown_rx,
                    state,
                )),
                ready: None,
                shutdown: Some(shutdown_tx),
                stopping: false,
            }),
            status: GatewayServiceStatus {
                running: true,
                bound_addr: Some("127.0.0.1:1".to_owned()),
                last_error: None,
            },
        };
        runtime.listener.as_mut().unwrap().request_stop();
        // The server itself confirms receipt before cancellation is simulated.
        signal_seen_rx.await.unwrap();
        let mut waiter = Box::pin(runtime.refresh());
        assert!(futures_util::poll!(waiter.as_mut()).is_pending());
        drop(waiter);
        // This delay intentionally crosses part of the drain interval. A new
        // waiter must await only its remainder, not grant another five seconds.
        tokio::time::sleep(Duration::from_secs(3)).await;
        tokio::time::timeout(Duration::from_secs(3), runtime.refresh())
            .await
            .expect("caller cancellation reset or lost the stop deadline");
        assert!(runtime.listener.is_none());
        assert!(!runtime.status.running);
        assert!(runtime.status.last_error.unwrap().contains("5 秒"));
        assert_eq!(cleaned_rx.try_recv(), Ok(()), "server cleanup was detached");
    }
}

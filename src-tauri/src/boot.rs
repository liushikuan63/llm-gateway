//! 启动状态：后端初始化在事件循环启动后异步执行，前端据此决定何时挂载主界面。
//!
//! 这样 `setup` 可以立即返回，先让 WebView 绘制静态启动动画，而不是阻塞在
//! 配置读取、SQLite 迁移与 Provider 预加载上。

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct BootStateView {
    /// "loading" | "ready" | "error"
    pub status: String,
    pub error: Option<String>,
    /// 非致命降级提示：配置坏掉时应用仍能用默认值启动，这里告诉用户原文件被挪到哪了。
    /// `status` 仍是 `ready` —— 能用就不是错误，但必须让他知道配置没生效。
    pub warning: Option<String>,
}

#[derive(Clone, Default)]
pub struct BootState {
    ready: Arc<AtomicBool>,
    error: Arc<parking_lot::Mutex<Option<String>>>,
    warning: Arc<parking_lot::Mutex<Option<String>>>,
}

impl BootState {
    pub fn mark_ready(&self) {
        *self.error.lock() = None;
        self.ready.store(true, Ordering::Release);
    }

    pub fn mark_error(&self, error: impl Into<String>) {
        *self.error.lock() = Some(error.into());
        self.ready.store(false, Ordering::Release);
    }

    /// 记一条非致命降级提示。
    ///
    /// 刻意**不**改 `status`：能用就不是错误。但如果只 `mark_ready` 而不留下痕迹，
    /// 用户会以为自己的配置生效了 —— 而它其实被丢弃了，那比报错更糟。
    pub fn mark_warning(&self, warning: impl Into<String>) {
        *self.warning.lock() = Some(warning.into());
    }

    pub fn view(&self) -> BootStateView {
        if let Some(error) = self.error.lock().clone() {
            return BootStateView {
                status: "error".to_string(),
                error: Some(error),
                warning: self.warning.lock().clone(),
            };
        }
        BootStateView {
            status: if self.ready.load(Ordering::Acquire) {
                "ready"
            } else {
                "loading"
            }
            .to_string(),
            error: None,
            warning: self.warning.lock().clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_state_transitions_from_loading_to_ready_and_error() {
        let state = BootState::default();
        assert_eq!(state.view().status, "loading");

        state.mark_ready();
        let ready = state.view();
        assert_eq!(ready.status, "ready");
        assert!(ready.error.is_none());

        state.mark_error("database unavailable");
        let failed = state.view();
        assert_eq!(failed.status, "error");
        assert_eq!(failed.error.as_deref(), Some("database unavailable"));

        state.mark_ready();
        assert_eq!(state.view().status, "ready");
        assert!(state.view().error.is_none());
    }
}

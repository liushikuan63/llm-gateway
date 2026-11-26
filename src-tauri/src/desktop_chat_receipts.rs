//! Durable at-most-once admission for ordinary buffered desktop chat.
use crate::{
    desktop_chat::{self, ChatInput, ChatResult},
    AppState,
};
use serde::Serialize;
use tauri::{Runtime, State, Webview, Window};
use uuid::Uuid;

const GATEWAY_CLOSED: &str = "LLM 网关服务未开启，请在设置页或托盘开启后发送";
const UNCONFIRMED: &str = "本次回复未能确认；请核对原请求的审计及用量。";
const MAX_CURSOR: i64 = 9_007_199_254_740_991; // Number.MAX_SAFE_INTEGER
const PAGE_SIZE: i64 = 50;

#[derive(Serialize, sqlx::FromRow)]
pub(crate) struct Receipt {
    cursor: i64,
    operation_id: String,
    state: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Writeback {
    Recorded,
    Unknown,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Reply {
        operation_id: String,
        receipt: Writeback,
        result: Box<ChatResult>,
    },
    Unknown {
        operation_id: String,
        message: &'static str,
    },
}

#[tauri::command]
pub(crate) async fn list_desktop_chat_receipts<R: Runtime>(
    window: Window<R>,
    webview: Webview<R>,
    state: State<'_, AppState>,
    before: Option<i64>,
) -> Result<Vec<Receipt>, String> {
    let pool = state.db.pool();
    // Scope comes from Tauri rather than the invoke body or conversation ID.
    let page = match before {
        Some(cursor) if (1..=MAX_CURSOR).contains(&cursor) => {
            sqlx::query_as::<_, Receipt>(
                "SELECT cursor, operation_id, state FROM desktop_chat_operation_receipts \
             WHERE window_label = ? AND webview_label = ? AND cursor < ? \
             ORDER BY cursor DESC LIMIT ?",
            )
            .bind(window.label())
            .bind(webview.label())
            .bind(cursor)
            .bind(PAGE_SIZE)
            .fetch_all(pool)
            .await
        }
        Some(_) => return Err("聊天操作记录的游标无效".into()),
        None => {
            sqlx::query_as::<_, Receipt>(
                "SELECT cursor, operation_id, state FROM desktop_chat_operation_receipts \
             WHERE window_label = ? AND webview_label = ? ORDER BY cursor DESC LIMIT ?",
            )
            .bind(window.label())
            .bind(webview.label())
            .bind(PAGE_SIZE)
            .fetch_all(pool)
            .await
        }
    };
    page.map_err(|_| "无法读取本机聊天操作记录".into())
}

#[tauri::command]
pub(crate) async fn desktop_chat_v2<R: Runtime>(
    window: Window<R>,
    webview: Webview<R>,
    state: State<'_, AppState>,
    input: ChatInput,
    request_id: String,
) -> Result<Outcome, String> {
    if input.personal_test {
        return Err("本机聊天操作记录只适用于普通非流式聊天".into());
    }
    let operation = Uuid::parse_str(&request_id)
        .ok()
        .filter(|id| !id.is_nil() && id.to_string() == request_id)
        .ok_or("聊天操作标识无效")?;
    desktop_chat::validate(&input)?;

    let unknown = || Outcome::Unknown {
        operation_id: request_id.clone(),
        message: UNCONFIRMED,
    };
    let pool = state.db.pool();
    let Some(listener) = state.gateway.listener_snapshot() else {
        // Only an unused ID can be known not to have dispatched in an earlier run.
        let seen = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM desktop_chat_operation_receipts WHERE operation_id = ?)",
        )
        .bind(&request_id)
        .fetch_one(pool)
        .await;
        return match seen {
            Ok(false) => Err(GATEWAY_CLOSED.into()),
            _ => Ok(unknown()),
        };
    };

    // The unique ID and committed INSERT decide the only caller allowed to
    // dispatch. A duplicate, including a caller with different input or a
    // different window, gets the same result without reading the old response.
    let claim = sqlx::query(
        "INSERT INTO desktop_chat_operation_receipts \
         (operation_id, window_label, webview_label, state) VALUES (?, ?, ?, 'claimed') \
         ON CONFLICT(operation_id) DO NOTHING",
    )
    .bind(&request_id)
    .bind(window.label())
    .bind(webview.label())
    .execute(pool)
    .await;
    if !matches!(claim, Ok(result) if result.rows_affected() == 1) {
        return Ok(unknown());
    }

    // Once claimed, any uncertainty leaves the operation non-dispatchable.
    if !state
        .gateway
        .listener_snapshot()
        .is_some_and(|current| current.bound_addr == listener.bound_addr)
    {
        return Ok(unknown());
    }
    let config = state.gateway.cfg_snapshot();
    let Ok(result) =
        desktop_chat::send_with_trace(&config, listener.bound_addr, input, Some(operation)).await
    else {
        return Ok(unknown());
    };

    let completed = sqlx::query(
        "UPDATE desktop_chat_operation_receipts SET state = 'completed' \
         WHERE operation_id = ? AND window_label = ? AND webview_label = ? AND state = 'claimed'",
    )
    .bind(&request_id)
    .bind(window.label())
    .bind(webview.label())
    .execute(pool)
    .await;
    // An already received response can still be shown in this invocation.
    // The operation record never contains that response.
    let receipt = if matches!(completed, Ok(write) if write.rows_affected() == 1) {
        Writeback::Recorded
    } else {
        Writeback::Unknown
    };
    Ok(Outcome::Reply {
        operation_id: request_id,
        receipt,
        result: Box::new(result),
    })
}

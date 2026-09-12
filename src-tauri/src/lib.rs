pub mod bundle;
pub mod cli_tools;
mod commands;
pub mod config;
pub mod context;
pub mod crypto;
pub mod db;
pub mod domain;
pub mod error;
pub mod media;
pub mod model_catalog;
pub mod pricing;
pub mod pricing_refresh;
pub mod protocol;
pub mod provider_quota;
pub mod proxy;
pub mod router;

use crate::config::AppConfig;
use crate::proxy::server::GatewayState;
use std::sync::Arc;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};

/// 全局共享状态：SQLite 池 + 运行时配置 + 网关状态（路由表 / 健康表 / 限流计数器）
pub struct AppState {
    pub db: db::Db,
    pub config: Arc<parking_lot::RwLock<AppConfig>>,
    pub gateway: Arc<GatewayState>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 1. 初始化日志（Release 下写入 %APPDATA%/llm-gateway/logs/）
    let log_dir = config::app_data_dir().join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "llm_gateway=info,tower_http=warn".into()),
        )
        .with_writer(
            std::fs::File::options()
                .create(true)
                .append(true)
                .open(log_dir.join("gateway.log"))
                .unwrap_or_else(|_| {
                    std::fs::File::create(std::env::temp_dir().join("llm-gateway.log"))
                        .expect("open log")
                }),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();

            // 2. 阻塞初始化：配置 -> 密钥 -> 数据库 -> 网关状态
            let cfg = AppConfig::load_or_init()?;
            let db = tauri::async_runtime::block_on(db::Db::connect(&cfg))?;
            let gateway = Arc::new(GatewayState::new(db.clone(), cfg.clone()));
            // 服务开始监听前完成首次加载，避免启动后的首个请求因缓存尚为空而 404。
            tauri::async_runtime::block_on(gateway.reload_providers())?;

            app.manage(AppState {
                db: db.clone(),
                config: Arc::new(parking_lot::RwLock::new(cfg.clone())),
                gateway: gateway.clone(),
            });

            // 3. 启动本地网关 HTTP 服务（默认 127.0.0.1:15721）
            let gw = gateway.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = proxy::server::serve(gw).await {
                    tracing::error!("gateway server exited: {e}");
                }
            });

            // 4. 后台任务：健康探测 + 冷却恢复 + 用量聚合
            let gw2 = gateway.clone();
            tauri::async_runtime::spawn(async move { gw2.background_loop().await });

            // 5. 系统托盘：CC Switch 式的极速切换就靠它
            build_tray(&handle)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_providers,
            provider_quota::get_provider_quota,
            commands::discover_provider_models,
            commands::upsert_provider,
            commands::delete_provider,
            commands::test_provider,
            commands::set_active_provider,
            commands::get_config,
            commands::update_config,
            commands::list_models,
            commands::get_unified_key,
            commands::rotate_unified_key,
            commands::list_remote_access_keys,
            commands::create_remote_access_key,
            commands::update_remote_access_key,
            commands::delete_remote_access_key,
            commands::list_sessions,
            commands::get_session_messages,
            commands::delete_session,
            commands::compact_session,
            commands::list_snapshots,
            commands::apply_snapshot,
            commands::create_snapshot,
            commands::stats_overview,
            commands::recent_requests,
            commands::apply_takeover,
            commands::export_bundle,
            commands::import_bundle,
            commands::detect_cli_tools,
            commands::detect_cli_tools_with_updates,
            commands::install_cli_tool,
            commands::run_gateway_self_check,
            commands::refresh_pricing,
            commands::pricing_status,
            commands::list_token_calibrations,
            commands::clear_token_calibrations,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::CloseRequested { api, .. },
                ..
            } = event
            {
                if should_hide_to_tray(&label) {
                    // 只拦截主窗口的关闭请求；若拦截 ExitRequested，托盘菜单
                    // 触发的 handle.exit(0) 也会被错误地阻止。
                    api.prevent_close();
                    if let Some(window) = app.get_webview_window(&label) {
                        let _ = window.hide();
                    }
                }
            }
        });
}

fn should_hide_to_tray(window_label: &str) -> bool {
    window_label == "main"
}

fn build_tray(app: &tauri::AppHandle) -> anyhow::Result<()> {
    let show = MenuItemBuilder::with_id("show", "打开控制面板").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;
    let menu = MenuBuilder::new(app).items(&[&show, &quit]).build()?;

    let handle = app.clone();
    TrayIconBuilder::with_id("main")
        .icon(tauri::image::Image::from_bytes(include_bytes!(
            "../icons/tray.png"
        ))?)
        .menu(&menu)
        .tooltip("LLM Gateway")
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                if let Some(window) = tray.app_handle().get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
        })
        .on_menu_event(move |_tray, ev| match ev.id().as_ref() {
            "show" => {
                if let Some(w) = handle.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }
            }
            "quit" => handle.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::should_hide_to_tray;

    #[test]
    fn only_main_window_close_is_intercepted_for_tray_residency() {
        assert!(should_hide_to_tray("main"));
        assert!(!should_hide_to_tray("settings"));
    }
}

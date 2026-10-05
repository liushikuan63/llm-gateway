pub mod autostart;
pub mod boot;
pub mod bundle;
pub mod cache;
pub mod cli_tools;
mod commands;
pub mod config;
pub mod context;
pub mod crypto;
pub mod db;
pub mod domain;
pub mod error;
pub mod intellect;
pub mod local_models;
pub mod log_rotate;
pub mod media;
pub mod model_catalog;
pub mod pet_window;
pub mod petdex;
pub mod pricing;
pub mod pricing_refresh;
pub mod protocol;
pub mod provider_quota;
pub mod proxy;
pub mod router;
pub mod search;
pub mod stale_models;

use crate::config::AppConfig;
use crate::proxy::server::GatewayState;
use std::sync::Arc;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};

/// 全局共享状态：SQLite 池 + 运行时配置 + 网关状态（路由表 / 健康表 / 限流计数器）
pub struct AppState {
    pub db: db::Db,
    pub config: Arc<parking_lot::RwLock<AppConfig>>,
    pub gateway: Arc<GatewayState>,
}

/// 在事件循环启动后异步完成后端初始化，避免阻塞首帧渲染。
async fn initialize_backend(app: tauri::AppHandle, boot: boot::BootState) {
    let started = std::time::Instant::now();
    // 读坏了也要起得来：坏配置会被备份成 config.corrupt-<时间戳>.toml，
    // 这里先用默认配置把应用带起来，同时把「原配置没生效」透给界面。
    // 直接失败的话，一个枚举值拼错就是「双击没反应」，用户既用不了也没法自救。
    let (cfg, config_warning) = match AppConfig::load_or_init_with_warning() {
        Ok(pair) => pair,
        Err(error) => {
            // 连文件都读不动（权限/占用）时才真的无路可走。
            tracing::error!("load config failed: {error}");
            boot.mark_error(error.to_string());
            return;
        }
    };
    if let Some(warning) = config_warning {
        boot.mark_warning(warning);
    }
    let db = match db::Db::connect(&cfg).await {
        Ok(db) => db,
        Err(error) => {
            tracing::error!("open database failed: {error}");
            boot.mark_error(error.to_string());
            return;
        }
    };
    let gateway = Arc::new(GatewayState::new(db.clone(), cfg.clone()));
    // 服务开始监听前完成首次加载，避免启动后的首个请求因缓存尚为空而 404。
    if let Err(error) = gateway.reload_providers().await {
        tracing::error!("load providers failed: {error}");
        boot.mark_error(error.to_string());
        return;
    }

    app.manage(AppState {
        db,
        config: Arc::new(parking_lot::RwLock::new(cfg)),
        gateway: gateway.clone(),
    });

    let gw = gateway.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = proxy::server::serve(gw).await {
            tracing::error!("gateway server exited: {error}");
        }
    });
    let gw2 = gateway.clone();
    tauri::async_runtime::spawn(async move { gw2.background_loop().await });

    tracing::info!("backend ready in {} ms", started.elapsed().as_millis());
    boot.mark_ready();
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
            // RotatingWriter instead of a plain appending File: this is a
            // long-running desktop app and the log had neither rotation nor a
            // size cap. Falls back to a temp-dir writer when the log
            // directory is not writable, so logging never becomes a hard
            // startup dependency (the old code used .expect here, which would
            // panic the whole app if temp dir was also unavailable).
            // `with_writer` takes a `MakeWriter`, not a value, and the subscriber
            // is `Send + Sync`. `SharedWriterHandle` is the adapter: it hands
            // out a cheap clone per event and serialises the actual write so
            // rotation never interleaves with another thread's append.
            tracing_subscriber::fmt::writer::BoxMakeWriter::new(log_rotate::SharedWriterHandle(
                std::sync::Arc::new(log_rotate::SharedWriter::new(
                    log_rotate::RotatingWriter::new(&log_dir, "gateway.log")
                        .map(|writer| Box::new(writer) as Box<dyn std::io::Write + Send + Sync>)
                        .unwrap_or_else(|_| {
                            std::fs::File::create(std::env::temp_dir().join("llm-gateway.log"))
                                .map(|file| Box::new(file) as Box<dyn std::io::Write + Send + Sync>)
                                .unwrap_or_else(|_| {
                                    Box::new(std::io::sink())
                                        as Box<dyn std::io::Write + Send + Sync>
                                })
                        }),
                )),
            )),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();

            // 2. 先把窗口背景设为深色，避免 WebView 首帧白闪。
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_background_color(Some(tauri::window::Color(11, 18, 21, 255)));
            }

            // 3. 启动状态：配置、SQLite 迁移和 Provider 预加载放到异步任务中，
            //    让事件循环先启动，静态启动动画可以立即绘制。
            let boot = boot::BootState::default();
            app.manage(boot.clone());
            let init_handle = app.handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                tauri::async_runtime::block_on(initialize_backend(init_handle, boot));
            });

            // 4.5 桌宠原生菜单：选择宠物 / 打开主窗口 / 暂停监控 / 隐藏。
            let menu_handle = app.handle().clone();
            app.on_menu_event(move |_app, event| {
                let id = event.id().as_ref().to_owned();
                let Some(action) = id.strip_prefix("pet-menu:") else {
                    return;
                };
                match action {
                    "hide" => {
                        if let Some(window) = menu_handle.get_webview_window("pet") {
                            let _ = window.close();
                        }
                    }
                    "open-main" => {
                        if let Some(window) = menu_handle.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                    "toggle-panel" => {
                        let _ = menu_handle.emit(
                            "pet-menu-action",
                            serde_json::json!({ "action": "toggle-panel" }),
                        );
                    }
                    "toggle-pause" => {
                        let _ = menu_handle.emit(
                            "pet-menu-action",
                            serde_json::json!({ "action": "toggle-pause" }),
                        );
                    }
                    other => {
                        if let Some(slug) = other.strip_prefix("select:") {
                            let _ = menu_handle.emit(
                                "pet-menu-action",
                                serde_json::json!({ "action": "select-pet", "slug": slug }),
                            );
                        }
                    }
                }
            });

            // 5. 系统托盘：CC Switch 式的极速切换就靠它
            build_tray(&handle)?;

            // 6. 自启静默：只藏主窗口，托盘/桌宠照常。
            // 放在 build_tray 之后 —— 先确保图标存在，用户才「看得见它在跑」。
            if should_start_hidden() {
                if let Some(window) = handle.get_webview_window("main") {
                    let _ = window.hide();
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_boot_state,
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
            commands::get_pet_status,
            commands::get_pet_asset,
            commands::open_pet_window,
            commands::close_pet_window,
            commands::set_pet_window_size,
            commands::set_pet_window_expanded,
            commands::set_pet_window_bubble_hidden,
            commands::get_pet_window_layout,
            commands::refresh_pet_window_layout,
            commands::set_pet_bubbles,
            commands::show_pet_menu,
            commands::focus_main_window,
            commands::focus_ai_tool,
            commands::open_ai_task,
            commands::open_task_project,
            commands::stop_ai_tool,
            commands::petdex_catalog,
            commands::petdex_install_pet,
            commands::run_gateway_self_check,
            commands::refresh_pricing,
            commands::pricing_status,
            commands::list_token_calibrations,
            commands::clear_token_calibrations,
            commands::list_local_runtimes,
            commands::list_local_models,
            commands::register_local_model,
            commands::pull_local_model,
            commands::get_search_settings,
            commands::update_search_settings,
            commands::test_search_backend,
            commands::classify_preview,
            commands::jev_probe,
            commands::calibrate_classifier,
            commands::calibrate_default_samples,
            commands::scan_stale_models,
            commands::delete_models,
            commands::get_autostart_state,
            commands::set_autostart,
            commands::calibrate_default_samples,
            commands::scan_stale_models,
            commands::delete_models,
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

/// 自启启动（`--minimized`）时要不要隐藏主窗口。
///
/// 自启的用户是「希望它在后台跑着」，不是「想看界面」—— 每次开机弹窗
/// 逼着用户手动关一次，托盘常驻的意义也就没了。所以只隐藏窗口，
/// 托盘图标照常出现，点它就能打开面板。
///
/// 判据要**只看参数不看窗口**：托盘/桌宠窗口也是通过同一个 setup
/// 创建的，把它们一起藏掉会出现「自启后连图标都没有，完全不知道它在跑」。
fn should_start_hidden() -> bool {
    std::env::args().any(|a| a == autostart::MINIMIZED_FLAG)
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
    use super::{should_hide_to_tray, should_start_hidden};

    #[test]
    fn only_main_window_close_is_intercepted_for_tray_residency() {
        assert!(should_hide_to_tray("main"));
        assert!(!should_hide_to_tray("settings"));
    }

    /// 判定逻辑抽成可测的纯函数：直接读 `std::env::args()` 没法在测试里
    /// 换掉参数，而「有/无 --minimized」这两个分支都必须被覆盖。
    fn starts_hidden_with(args: &[&str]) -> bool {
        args.contains(&crate::autostart::MINIMIZED_FLAG)
    }

    #[test]
    fn 静默启动只认_minimized_参数() {
        assert!(
            !starts_hidden_with(&["llm-gateway.exe"]),
            "正常双击启动必须显示面板，否则用户以为程序没起来"
        );
        assert!(
            starts_hidden_with(&["llm-gateway.exe", "--minimized"]),
            "自启必须静默：开机弹窗逼着用户手动关一次"
        );
        // 大小写与位置都不能影响判定
        assert!(!starts_hidden_with(&["llm-gateway.exe", "--MINIMIZED"]));
        assert!(starts_hidden_with(&["--minimized", "llm-gateway.exe"]));
        // 相近参数不得误判
        assert!(!starts_hidden_with(&[
            "llm-gateway.exe",
            "--minimized=false"
        ]));
        assert!(!starts_hidden_with(&["llm-gateway.exe", "-m"]));
    }

    #[test]
    fn 当前进程不带_minimized_时不该隐藏() {
        // 测试进程自己没有这个参数 —— 防止有人把判定写反。
        assert!(!should_start_hidden());
    }
}

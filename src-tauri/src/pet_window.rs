//! 桌宠窗口：透明、无边框、置顶、不进任务栏的独立 WebView 窗口。
//!
//! 窗口在用户显式开启时创建（不在启动时自动弹出），关闭即销毁；
//! 位置与大小由系统记忆，拖动由前端 `data-tauri-drag-region` 发起。
//! 右键菜单使用系统原生菜单：宠物窗口本身可能很小，自绘菜单放不下。

use tauri::menu::{
    CheckMenuItemBuilder, ContextMenu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem,
};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub const PET_WINDOW_LABEL: &str = "pet";
const DEFAULT_WIDTH: f64 = 144.0;
const DEFAULT_HEIGHT: f64 = 156.0;
const MIN_WIDTH: f64 = 120.0;
const MIN_HEIGHT: f64 = 130.0;
const MAX_SIZE: f64 = 640.0;
/// 界面可选的缩放范围：0.5× ~ 2×（基准 240×260）；默认 0.6×。
pub const MIN_SCALE: f64 = 0.5;
pub const MAX_SCALE: f64 = 2.0;
/// 基准尺寸：缩放比例 1.0 对应 240×260。
pub const BASE_WIDTH: f64 = 240.0;
pub const BASE_HEIGHT: f64 = 260.0;
pub const DEFAULT_SCALE: f64 = DEFAULT_WIDTH / BASE_WIDTH;

pub fn ensure_pet_window(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(PET_WINDOW_LABEL) {
        window
            .show()
            .map_err(|error| format!("显示桌宠窗口失败：{error}"))?;
        let _ = window.set_focus();
        return Ok(());
    }

    WebviewWindowBuilder::new(app, PET_WINDOW_LABEL, WebviewUrl::App("pet.html".into()))
        .title("LLM Gateway Pet")
        .inner_size(DEFAULT_WIDTH, DEFAULT_HEIGHT)
        .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
        .max_inner_size(MAX_SIZE, MAX_SIZE)
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .resizable(true)
        .build()
        .map_err(|error| format!("创建桌宠窗口失败：{error}"))?;
    Ok(())
}

/// 按缩放比例调整桌宠窗口大小（0.5× ~ 2×）。窗口未打开时明确报错，不静默忽略。
pub fn set_pet_window_size(app: &AppHandle, scale: f64) -> Result<f64, String> {
    if !scale.is_finite() {
        return Err("缩放比例不合法".to_string());
    }
    let scale = scale.clamp(MIN_SCALE, MAX_SCALE);
    let window = app
        .get_webview_window(PET_WINDOW_LABEL)
        .ok_or_else(|| "桌宠窗口未打开；先点「开启桌宠」再调整大小".to_string())?;
    window
        .set_size(tauri::LogicalSize::new(
            BASE_WIDTH * scale,
            BASE_HEIGHT * scale,
        ))
        .map_err(|error| format!("调整桌宠大小失败：{error}"))?;
    Ok(scale)
}

/// 在桌宠窗口上弹出原生右键菜单。
/// 菜单项 id 约定：`pet-menu:*`（由 lib.rs 的统一处理器消费）。
pub fn show_pet_menu(
    app: &AppHandle,
    current_slug: Option<&str>,
    pets: &[crate::petdex::InstalledPet],
    paused: bool,
) -> Result<(), String> {
    // 原生菜单需要窗口句柄（不是 webview 句柄）。
    let webview_window = app
        .get_webview_window(PET_WINDOW_LABEL)
        .ok_or_else(|| "桌宠窗口未打开".to_string())?;
    let window = webview_window.as_ref().window();

    let mut builder = MenuBuilder::new(app);
    for pet in pets {
        let item = CheckMenuItemBuilder::with_id(
            format!("pet-menu:select:{}", pet.slug),
            pet.display_name.clone(),
        )
        .checked(Some(pet.slug.as_str()) == current_slug)
        .build(app)
        .map_err(|error| format!("构建菜单项失败：{error}"))?;
        builder = builder.item(&item);
    }
    if !pets.is_empty() {
        builder = builder.item(
            &PredefinedMenuItem::separator(app)
                .map_err(|error| format!("构建分隔符失败：{error}"))?,
        );
    }
    let open_main = MenuItemBuilder::with_id("pet-menu:open-main", "打开主窗口")
        .build(app)
        .map_err(|error| format!("构建菜单项失败：{error}"))?;
    let toggle_pause = MenuItemBuilder::with_id(
        "pet-menu:toggle-pause",
        if paused {
            "恢复状态监控"
        } else {
            "暂停状态监控"
        },
    )
    .build(app)
    .map_err(|error| format!("构建菜单项失败：{error}"))?;
    let hide = MenuItemBuilder::with_id("pet-menu:hide", "隐藏桌宠")
        .build(app)
        .map_err(|error| format!("构建菜单项失败：{error}"))?;
    let menu = builder
        .items(&[&open_main, &toggle_pause, &hide])
        .build()
        .map_err(|error| format!("构建菜单失败：{error}"))?;

    menu.popup(window)
        .map_err(|error| format!("弹出菜单失败：{error}"))?;
    Ok(())
}

//! 桌宠窗口：透明、无边框、置顶、不进任务栏的独立 WebView 窗口。
//!
//! 宠物本体的 1.00× 尺寸为 120×130，最小 0.50× = 60×65。展开信息面板时窗口会额外加宽，
//! 而不是把内容硬塞进宠物区域；收起后面板消失，窗口恢复到宠物本体大小。
//! 位置与大小由系统记忆，拖动由前端 `data-tauri-drag-region` 发起。
//! 右键菜单使用系统原生菜单：即使面板收起，宠物窗口也可以完成菜单操作。

use serde::Serialize;
use std::sync::OnceLock;
use tauri::menu::{
    CheckMenuItemBuilder, ContextMenu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem,
};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

pub const PET_WINDOW_LABEL: &str = "pet";
/// 宠物本体的基准尺寸：缩放比例 1.00 对应 120×130。
pub const BASE_WIDTH: f64 = 120.0;
pub const BASE_HEIGHT: f64 = 130.0;
pub const MIN_SCALE: f64 = 0.5;
pub const MAX_SCALE: f64 = 3.0;
pub const DEFAULT_SCALE: f64 = 1.0;
const DEFAULT_EXPANDED: bool = false;
/// 气泡与完整 HUD 共用同一宽度；每个气泡只占一条标题 + 一行缩略，多个任务纵向堆叠。
const BUBBLE_WIDTH: f64 = 340.0;
/// 单个气泡的固定行高 / 行间距 / 底部留白，必须与 `src/pet.css` 的 `.pet-bubble`、
/// `.pet-bubble-stack` 保持一致，窗口高度按同一套数值计算。
const BUBBLE_ROW_HEIGHT: f64 = 66.0;
const BUBBLE_GAP: f64 = 8.0;
/// 底部留白必须容纳元素阴影（否则阴影会被窗口边缘硬切，形成灰色横条）。
/// 与 `src/pet.css` 的 `.pet-bubble` 阴影半径（0 4px 8px）匹配：16px 足够衰减到不可见。
const BUBBLE_BOTTOM_MARGIN: f64 = 16.0;
/// 顶部留白：关闭 / 展开按钮悬在第一个气泡上沿，窗口不留白就会被裁掉。
const BUBBLE_TOP_MARGIN: f64 = 12.0;
/// 宠物本体下方的留白：给宠物阴影留出空间，同样避免被窗口边缘切掉。
const PET_BOTTOM_MARGIN: f64 = 12.0;
/// 桌宠最多同时显示的气泡数量，前端 `BUBBLE_LIMIT` 与之一致。
pub const MAX_BUBBLES: usize = 3;
/// 多个气泡收起时的堆叠步进：只露出这一条高度，鼠标悬浮 / 任务结束时再展开。
const BUBBLE_STACK_OFFSET: f64 = 18.0;
/// HUD 的最小宽度与高度；展开后窗口宽度为宠物区域 + 间距 + HUD。
const HUD_WIDTH: f64 = 340.0;
/// HUD 最小高度已包含底部 12px 阴影留白。
const HUD_MIN_HEIGHT: f64 = 372.0;
const PET_GAP: f64 = 8.0;
const MAX_WINDOW_WIDTH: f64 = 1_200.0;
const MAX_WINDOW_HEIGHT: f64 = 1_000.0;
const LAYOUT_EVENT: &str = "pet-layout-changed";

#[derive(Debug, Clone, Copy)]
struct PetLayoutState {
    scale: f64,
    expanded: bool,
    bubble_hidden: bool,
    bubble_left: bool,
    /// 前端上报的可见气泡数量（关闭单个气泡后会变小）。
    bubble_count: usize,
    /// 气泡堆叠是否展开。
    bubbles_expanded: bool,
}

impl Default for PetLayoutState {
    fn default() -> Self {
        Self {
            scale: DEFAULT_SCALE,
            expanded: DEFAULT_EXPANDED,
            bubble_hidden: false,
            bubble_left: false,
            bubble_count: 1,
            bubbles_expanded: false,
        }
    }
}

/// 窗口布局结果同时供设置页和桌宠窗口读取，避免两边各算一套尺寸。
#[derive(Debug, Clone, Serialize)]
pub struct PetWindowLayout {
    pub scale: f64,
    pub expanded: bool,
    pub bubble_hidden: bool,
    pub bubble_left: bool,
    /// 当前展示的气泡数量（按最近任务计算，1 ~ MAX_BUBBLES）。
    pub bubble_count: usize,
    /// 气泡数量上限，前端据此裁剪任务列表。
    pub bubble_limit: usize,
    /// 气泡堆叠是否处于展开状态。
    pub bubbles_expanded: bool,
    pub width: f64,
    pub height: f64,
    pub window_open: bool,
}

fn layout_state() -> &'static parking_lot::Mutex<PetLayoutState> {
    static STATE: OnceLock<parking_lot::Mutex<PetLayoutState>> = OnceLock::new();
    STATE.get_or_init(|| parking_lot::Mutex::new(PetLayoutState::default()))
}

fn normalize_scale(scale: f64) -> Result<f64, String> {
    if !scale.is_finite() {
        return Err("缩放比例不合法".to_string());
    }
    Ok(scale.clamp(MIN_SCALE, MAX_SCALE))
}

/// 首次创建窗口时按最近任务条数估算气泡数量，之后由前端上报真实值。
fn detected_bubble_count() -> usize {
    crate::petdex::list_tasks().len().clamp(1, MAX_BUBBLES)
}

/// 气泡区总高度：多个气泡收起时按堆叠步进，展开后每个气泡完整显示。
fn stack_height(bubble_count: usize, bubbles_expanded: bool) -> f64 {
    let count = bubble_count.clamp(1, MAX_BUBBLES) as f64;
    let rows = if count > 1.0 && !bubbles_expanded {
        BUBBLE_ROW_HEIGHT + BUBBLE_STACK_OFFSET * (count - 1.0)
    } else {
        BUBBLE_ROW_HEIGHT * count + BUBBLE_GAP * (count - 1.0)
    };
    rows + BUBBLE_BOTTOM_MARGIN + BUBBLE_TOP_MARGIN
}

/// 宠物本体与窗口是两个概念：面板展开时窗口更大，但宠物本体仍按 scale 缩放。
/// 气泡按行堆叠，窗口高度 = 宠物高度与气泡总高度中的较大者。
fn dimensions(
    scale: f64,
    expanded: bool,
    bubble_hidden: bool,
    bubble_count: usize,
    bubbles_expanded: bool,
) -> (f64, f64) {
    let pet_width = BASE_WIDTH * scale;
    let pet_height = BASE_HEIGHT * scale;
    if !expanded && bubble_hidden {
        return (pet_width, pet_height + PET_BOTTOM_MARGIN);
    }
    if !expanded {
        return (
            (pet_width + PET_GAP + BUBBLE_WIDTH).min(MAX_WINDOW_WIDTH),
            (pet_height + PET_BOTTOM_MARGIN)
                .max(stack_height(bubble_count, bubbles_expanded))
                .min(MAX_WINDOW_HEIGHT),
        );
    }
    (
        (pet_width + PET_GAP + HUD_WIDTH).min(MAX_WINDOW_WIDTH),
        (pet_height + PET_BOTTOM_MARGIN).clamp(HUD_MIN_HEIGHT, MAX_WINDOW_HEIGHT),
    )
}

/// 保持窗口底边不动地调整高度：返回新的窗口顶边坐标。
fn anchored_top(current_height: f64, next_height: f64, current_top: f64) -> f64 {
    current_top - (next_height - current_height)
}

fn bubble_left_for_position(
    monitor_left: f64,
    monitor_width: f64,
    window_left: f64,
    pet_width: f64,
    bubble_width: f64,
    gap: f64,
    currently_left: bool,
) -> bool {
    let pet_center = if currently_left {
        window_left + bubble_width + gap + pet_width / 2.0
    } else {
        window_left + pet_width / 2.0
    };
    let switch_left_at = monitor_left + monitor_width * 0.72;
    let switch_right_at = monitor_left + monitor_width * 0.55;
    if currently_left {
        pet_center > switch_right_at
    } else {
        pet_center > switch_left_at
    }
}

fn compute_bubble_left(app: &AppHandle, state: &PetLayoutState) -> bool {
    if state.expanded || state.bubble_hidden {
        return false;
    }
    let Some(window) = app.get_webview_window(PET_WINDOW_LABEL) else {
        return false;
    };
    let Ok(position) = window.outer_position() else {
        return false;
    };
    let Ok(Some(monitor)) = window.current_monitor() else {
        return false;
    };
    let factor = window.scale_factor().unwrap_or(1.0);
    bubble_left_for_position(
        monitor.position().x as f64,
        monitor.size().width as f64,
        position.x as f64,
        BASE_WIDTH * state.scale * factor,
        BUBBLE_WIDTH * factor,
        PET_GAP * factor,
        state.bubble_left,
    )
}

fn current_layout(app: &AppHandle) -> PetWindowLayout {
    let state = {
        let mut guard = layout_state().lock();
        let mut next = *guard;
        next.bubble_left = compute_bubble_left(app, &next);
        *guard = next;
        next
    };
    let (width, height) = dimensions(
        state.scale,
        state.expanded,
        state.bubble_hidden,
        state.bubble_count,
        state.bubbles_expanded,
    );
    PetWindowLayout {
        scale: state.scale,
        expanded: state.expanded,
        bubble_hidden: state.bubble_hidden,
        bubble_left: state.bubble_left,
        bubble_count: state.bubble_count,
        bubble_limit: MAX_BUBBLES,
        bubbles_expanded: state.bubbles_expanded,
        width,
        height,
        window_open: app.get_webview_window(PET_WINDOW_LABEL).is_some(),
    }
}

fn emit_layout(app: &AppHandle, layout: &PetWindowLayout) {
    let _ = app.emit_to(PET_WINDOW_LABEL, LAYOUT_EVENT, layout.clone());
}

fn apply_layout(app: &AppHandle, next: PetLayoutState) -> Result<PetWindowLayout, String> {
    {
        let mut state = layout_state().lock();
        *state = next;
    }
    if let Some(window) = app.get_webview_window(PET_WINDOW_LABEL) {
        let (width, height) = dimensions(
            next.scale,
            next.expanded,
            next.bubble_hidden,
            next.bubble_count,
            next.bubbles_expanded,
        );
        // 宠物与气泡都是底部对齐：窗口变高时保持底边不动、向上扩展，
        // 否则每多一个气泡就会把宠物整体往下推。
        let factor = window.scale_factor().unwrap_or(1.0);
        let current_height = window
            .outer_size()
            .ok()
            .map(|size| size.to_logical::<f64>(factor).height);
        if let (Some(current_height), Ok(position)) = (current_height, window.outer_position()) {
            let next_top = anchored_top(current_height, height, position.y as f64 / factor);
            if (next_top * factor - position.y as f64).abs() >= 1.0 {
                let _ = window.set_position(tauri::PhysicalPosition::new(
                    position.x,
                    (next_top * factor).round() as i32,
                ));
            }
        }
        window
            .set_size(tauri::LogicalSize::new(width, height))
            .map_err(|error| format!("调整桌宠窗口大小失败：{error}"))?;
    }
    let layout = current_layout(app);
    emit_layout(app, &layout);
    Ok(layout)
}

/// 创建或显示桌宠窗口。`120×130` 是 1.00× 的宠物本体，不是窗口上限。
pub fn ensure_pet_window(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(PET_WINDOW_LABEL) {
        window
            .show()
            .map_err(|error| format!("显示桌宠窗口失败：{error}"))?;
        let _ = window.set_focus();
        return Ok(());
    }

    // 先按检测到的任务条数估算一次，前端挂载后会立刻用真实数量覆盖。
    let state = {
        let mut guard = layout_state().lock();
        guard.bubble_count = detected_bubble_count();
        *guard
    };
    let (width, height) = dimensions(
        state.scale,
        state.expanded,
        state.bubble_hidden,
        state.bubble_count,
        state.bubbles_expanded,
    );
    WebviewWindowBuilder::new(app, PET_WINDOW_LABEL, WebviewUrl::App("pet.html".into()))
        .title("LLM Gateway Pet")
        .inner_size(width, height)
        .min_inner_size(
            BASE_WIDTH * MIN_SCALE,
            BASE_HEIGHT * MIN_SCALE + PET_BOTTOM_MARGIN,
        )
        .max_inner_size(MAX_WINDOW_WIDTH, MAX_WINDOW_HEIGHT)
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

/// 按 0.50× ~ 3.00× 调整宠物本体大小；信息面板的展开状态保持不变。
pub fn set_pet_window_size(app: &AppHandle, scale: f64) -> Result<PetWindowLayout, String> {
    let scale = normalize_scale(scale)?;
    let state = *layout_state().lock();
    apply_layout(app, PetLayoutState { scale, ..state })
}

/// 展开或收起当前任务 / AI 软件信息面板。
pub fn set_pet_window_expanded(app: &AppHandle, expanded: bool) -> Result<PetWindowLayout, String> {
    let state = *layout_state().lock();
    apply_layout(
        app,
        PetLayoutState {
            expanded,
            bubble_hidden: if expanded { false } else { state.bubble_hidden },
            ..state
        },
    )
}

pub fn set_pet_window_bubble_hidden(
    app: &AppHandle,
    bubble_hidden: bool,
) -> Result<PetWindowLayout, String> {
    let state = *layout_state().lock();
    apply_layout(
        app,
        PetLayoutState {
            bubble_hidden,
            ..state
        },
    )
}

/// 前端上报可见气泡数量与堆叠状态：关闭单个气泡后窗口要立刻缩回，
/// 鼠标悬浮 / 任务结束时展开则把窗口加高。
pub fn set_pet_bubbles(
    app: &AppHandle,
    bubble_count: usize,
    bubbles_expanded: bool,
) -> Result<PetWindowLayout, String> {
    let state = *layout_state().lock();
    apply_layout(
        app,
        PetLayoutState {
            bubble_count: bubble_count.clamp(1, MAX_BUBBLES),
            bubbles_expanded,
            ..state
        },
    )
}

/// 任务数量变化后重算窗口布局：气泡堆叠高度必须跟着任务数量走。
pub fn refresh_pet_window(app: &AppHandle) -> Result<PetWindowLayout, String> {
    let state = *layout_state().lock();
    apply_layout(app, state)
}

pub fn pet_window_layout(app: &AppHandle) -> PetWindowLayout {
    current_layout(app)
}

/// 在桌宠窗口上弹出原生右键菜单。
/// 菜单项 id 约定：`pet-menu:*`（由 lib.rs 的统一处理器消费）。
pub fn show_pet_menu(
    app: &AppHandle,
    current_slug: Option<&str>,
    pets: &[crate::petdex::InstalledPet],
    paused: bool,
    expanded: bool,
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
    let toggle_panel = MenuItemBuilder::with_id(
        "pet-menu:toggle-panel",
        if expanded {
            "收起信息面板"
        } else {
            "展开信息面板"
        },
    )
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
        .items(&[&open_main, &toggle_panel, &toggle_pause, &hide])
        .build()
        .map_err(|error| format!("构建菜单失败：{error}"))?;

    menu.popup(window)
        .map_err(|error| format!("弹出菜单失败：{error}"))?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_size_is_one_hundred_percent() {
        assert_eq!(normalize_scale(1.0).unwrap(), 1.0);
        let (width, height) = dimensions(1.0, false, false, 1, false);
        assert!(width >= BASE_WIDTH + BUBBLE_WIDTH);
        assert_eq!(height, BASE_HEIGHT + PET_BOTTOM_MARGIN);
        assert_eq!(normalize_scale(0.2).unwrap(), MIN_SCALE);
        assert_eq!(normalize_scale(9.0).unwrap(), MAX_SCALE);
    }

    #[test]
    fn scale_range_allows_half_size_with_shadow_margin() {
        assert_eq!(MIN_SCALE, 0.5);
        // 收起气泡后窗口 = 宠物本体 + 底部阴影留白，最小 60×65 + 12。
        assert_eq!(
            dimensions(0.5, false, true, MAX_BUBBLES, false),
            (BASE_WIDTH * 0.5, BASE_HEIGHT * 0.5 + PET_BOTTOM_MARGIN)
        );
        // 显示气泡时宽度按 50% 宠物本体计算。
        let (half_width, _) = dimensions(0.5, false, false, 1, false);
        assert_eq!(half_width, BASE_WIDTH * 0.5 + PET_GAP + BUBBLE_WIDTH);
        assert!(half_width < dimensions(1.0, false, false, 1, false).0);
    }

    #[test]
    fn bubble_layout_keeps_pet_and_single_task_preview() {
        let (width, height) = dimensions(1.0, false, false, 1, false);
        assert!(
            width >= BASE_WIDTH + BUBBLE_WIDTH,
            "气泡宽度应包含宠物与缩略内容：{width}"
        );
        assert!(
            height >= BUBBLE_ROW_HEIGHT + BUBBLE_BOTTOM_MARGIN,
            "气泡高度应能显示标题与一行缩略：{height}"
        );
    }

    #[test]
    fn growing_height_keeps_the_bottom_edge() {
        // 底边 = 顶边 + 高度：变高时顶边必须等量上移，宠物才不会被新气泡推下去。
        let (top, height) = (500.0, 130.0);
        let next_top = anchored_top(height, 231.0, top);
        assert_eq!(next_top + 231.0, top + height);
        assert_eq!(next_top, top - 101.0);
        // 变矮（收起面板 / 关闭气泡）时同样保持底边。
        assert_eq!(anchored_top(231.0, 130.0, next_top) + 130.0, top + height);
    }

    #[test]
    fn bubble_side_switch_has_hysteresis_and_does_not_oscillate() {
        let decide = |window_left: f64, currently_left: bool| {
            bubble_left_for_position(0.0, 1920.0, window_left, 120.0, 340.0, 8.0, currently_left)
        };

        // 进入右侧区域时切到“气泡在左”。
        assert!(!decide(1322.0, false));
        assert!(decide(1324.0, false));
        // 在切换死区内来回移动时保持当前方向，不来回闪动。
        assert!(decide(1300.0, true));
        assert!(decide(800.0, true));
        assert!(!decide(640.0, true));
        assert!(!decide(1322.0, false));
    }

    #[test]
    fn compact_stack_expands_on_hover_or_task_end() {
        let compact = dimensions(1.0, false, false, MAX_BUBBLES, false).1;
        let expanded = dimensions(1.0, false, false, MAX_BUBBLES, true).1;
        assert!(
            compact < expanded,
            "收起时必须比展开矮：{compact} < {expanded}"
        );
        let expected = BUBBLE_ROW_HEIGHT
            + BUBBLE_STACK_OFFSET * (MAX_BUBBLES - 1) as f64
            + BUBBLE_BOTTOM_MARGIN
            + BUBBLE_TOP_MARGIN;
        assert!(
            (stack_height(MAX_BUBBLES, false) - expected).abs() < 0.01,
            "收起高度必须等于一个完整气泡加堆叠步进：{} vs {expected}",
            stack_height(MAX_BUBBLES, false)
        );
        // 窗口高度不会小于宠物本体：100% 时宠物比收起堆叠高，所以窗口取宠物高度。
        assert!(compact >= BASE_HEIGHT + PET_BOTTOM_MARGIN);
        // 单个气泡没有“收起”概念，两种状态高度一致。
        assert_eq!(
            dimensions(1.0, false, false, 1, false).1,
            dimensions(1.0, false, false, 1, true).1
        );
    }

    #[test]
    fn stacked_bubbles_grow_height_but_not_width() {
        let (single_width, single_height) = dimensions(1.0, false, false, 1, false);
        let (stack_width, stack_height) = dimensions(1.0, false, false, MAX_BUBBLES, true);
        assert_eq!(single_width, stack_width, "多个气泡只增加高度");
        assert_eq!(stack_width, BASE_WIDTH + PET_GAP + BUBBLE_WIDTH);
        let needed = BUBBLE_ROW_HEIGHT * MAX_BUBBLES as f64
            + BUBBLE_GAP * (MAX_BUBBLES - 1) as f64
            + BUBBLE_BOTTOM_MARGIN
            + BUBBLE_TOP_MARGIN;
        assert!(
            stack_height >= needed,
            "窗口高度必须容纳 {MAX_BUBBLES} 个气泡：{stack_height}"
        );
        assert!(stack_height > single_height);
        // 超出上限的数量必须收敛，避免窗口无限变长。
        assert_eq!(dimensions(1.0, false, false, 99, true).1, stack_height);
        // 隐藏气泡时窗口必须缩回宠物本体，不能留下透明点击区。
        assert_eq!(
            dimensions(1.0, false, true, MAX_BUBBLES, false).1,
            BASE_HEIGHT + PET_BOTTOM_MARGIN
        );
    }

    #[test]
    fn expanded_layout_adds_hud_instead_of_squeezing_pet() {
        let (width, height) = dimensions(1.0, true, false, 1, false);
        assert!(
            width >= BASE_WIDTH + HUD_WIDTH,
            "展开宽度必须包含宠物与 HUD：{width}"
        );
        assert!(
            height >= HUD_MIN_HEIGHT,
            "展开高度必须容纳任务面板：{height}"
        );
        let (wide_width, wide_height) = dimensions(3.0, true, false, 1, false);
        assert!(wide_width > width, "放大宠物时窗口也应继续加宽");
        assert!(wide_height > height, "放大宠物时窗口也应继续加高");
    }
}

// main.rs —— Tauri 桌面外壳入口。
// 说明：真正的应用逻辑全部在 lib.rs 的 `run()` 里，
//       这样 exe 与移动端/测试可以复用同一套逻辑。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(code) = llm_gateway_lib::benefit_task::try_headless() {
        std::process::exit(code);
    }
    llm_gateway_lib::run()
}

// main.rs —— Tauri 桌面外壳入口。
// 说明：真正的应用逻辑全部在 lib.rs 的 `run()` 里，
//       这样 exe 与移动端/测试可以复用同一套逻辑。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    llm_gateway_lib::run()
}

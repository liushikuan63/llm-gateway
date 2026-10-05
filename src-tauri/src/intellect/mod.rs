//! 智能模式：请求定性。
//!
//! 网关在决定「这次该派给谁」之前，先回答一个问题——**这是个什么类型的活**。
//! 三类任务对模型的要求正好互斥：
//!
//! | 类别 | 典型请求 | 需要什么 |
//! | --- | --- | --- |
//! | [`TaskClass::Simple`] | 改个变量名、格式化、翻译一句话 | 快、便宜，别烧推理预算 |
//! | [`TaskClass::Vision`] | 带截图问「这里为什么报错」 | 必须能看图（硬约束） |
//! | [`TaskClass::Reasoning`] | 设计方案、定位根因、长链路重构 | 必须会思考 |
//!
//! 判定链路是三级递降，**任何一级失败都不阻断请求**：
//!
//! 1. [`classify::classify_by_rules`] 硬规则 —— 永远先跑，模型覆盖不了；
//! 2. [`jev::JevClient`] 决策模型 —— 只在置信度**和**边际同时达标时才被采纳；
//! 3. [`classify::classify_by_heuristic`] 启发式兜底 —— 永远返回非空结果。
//!
//! 第 2 级的「弃权」是**正常路径**：本机 edgeJev（laya-multilingual）在
//! 「任务复杂度」这个离域问题上多数时候分不清，此时正确行为是让启发式说了算，
//! 而不是硬把一个错判塞进路由。想提高 Jev 的话语权，先跑校准拿数据，调阈值。
//!
//! [`refine`] 是第四件事，独立于分类：提示词预优化。Jev 只判断「值不值得改写」，
//! 改写本身靠一次独立的小模型调用——edgeJev 走 scoring pass，产不出文本。

pub mod autostart;
pub mod calibrate;
pub mod classify;
pub mod jev;
pub mod refine;

pub use crate::router::score::TaskClass;
pub use autostart::SpawnOutcome;
pub use calibrate::{build_report, CalibrationReport, LabeledSample, SampleOutcome};
pub use classify::{
    classify, classify_by_heuristic, classify_by_rules, ClassifierSource, ClassifyInput,
    TaskIntent, REASONING_THRESHOLD,
};
pub use jev::{JevAnswer, JevClient, JevError};
pub use refine::{RefineOutcome, RefineTarget};

//! D2 能力数据的**多来源**处理。
//!
//! 与 [`crate::domain::capability`] 的分工：
//! - `domain::capability`（D1）是**数据类型** —— 一个模型的能力值长什么样
//! - 本模块（D2）是**多来源的账本** —— 同一维度被几个来源各写过一次时，
//!   按信任度取谁、把冲突暴露出来、导出/导入带走
//!
//! 两处都叫 capability 但路径不同，这是任务卡输出清单的分法。
//! 混在一起时「这个函数是存还是算」要靠读实现才知道。

pub mod source;

pub use source::{CapabilitySet, Dimension, ImportReport, SourcedValue, WriteOutcome};

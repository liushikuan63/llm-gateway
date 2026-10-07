//! B8：把**描述文件**变成可用的适配器。
//!
//! ## 为什么需要这一层
//!
//! 在它之前，B8 的四条判据都满足，但**没有任何一条生产路径会起插件进程** ——
//! `ProcessTransport` 只在测试里被构造。证据很硬：打包出来的 exe 里
//! **没有** `plugin-processes-` 这个字符串，因为整条链被死代码消除掉了。
//! 判据满足 ≠ 能力可用，这是又一次。
//!
//! ## 三条硬性口径
//!
//! 1. **总开关关着就一个目录都不扫**（模式隔离）。不是"扫了再过滤"——
//!    扫本身就会去读用户指定的路径，而关着的功能不该碰文件系统。
//! 2. **一个插件坏掉不拖垮其他**（`skipped` 逐条记原因，其余照常加载）。
//!    用户在目录里放了一个写错的 JSON，不该让整批插件消失。
//! 3. **失败不影响网关启动**。最坏是这个插件用不了，而那比
//!    「网关因为一个描述文件起不来」轻得多。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::plugin::{parse_manifest, validate_manifest, ExternalAdapter, PluginTransport};
use super::plugin_process::ProcessTransport;
use super::{AdapterRegistry, AgentAdapter};

/// 一次加载的结果。**两个列表都要能拿到** ——
/// 只报「加载了 N 个」而把失败吞掉的话，用户会以为配好了。
#[derive(Default)]
pub struct PluginLoadOutcome {
    /// 成功建好的适配器（已实现 `AgentAdapter`）。
    pub loaded: Vec<Arc<dyn AgentAdapter>>,
    /// 被跳过的：`"<路径>：<原因>"`。**必须带路径**，
    /// 否则用户拿不到任何线索去修。
    pub skipped: Vec<String>,
}

impl PluginLoadOutcome {
    /// 把加载好的注册进注册表，返回（加载数，跳过数）。
    pub fn register_into(self, registry: &mut AdapterRegistry) -> (usize, usize) {
        let loaded = self.loaded.len();
        for adapter in self.loaded {
            registry.register(adapter);
        }
        (loaded, self.skipped.len())
    }
}

/// 扫这些目录里的 `*.json`，逐个变成适配器。
///
/// `enabled` 为 false 时**直接返回空**，一个目录都不读（见模块文档第 1 条）。
pub fn load_plugins(dirs: &[PathBuf], enabled: bool) -> PluginLoadOutcome {
    let mut outcome = PluginLoadOutcome::default();
    if !enabled {
        return outcome;
    }
    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                // 目录不存在 / 读不了：记一条，继续下一个目录。
                outcome
                    .skipped
                    .push(format!("{}：读不了目录（{error}）", dir.display()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            match load_one(&path, dirs) {
                Ok(adapter) => outcome.loaded.push(adapter),
                Err(why) => outcome.skipped.push(format!("{}：{why}", path.display())),
            }
        }
    }
    outcome
}

/// 单个描述文件 → 适配器。`allowed` 是白名单目录（校验用），
/// 传整个 `dirs` 而不是当前目录 —— 描述文件可以放在任一白名单目录里。
fn load_one(path: &Path, allowed: &[PathBuf]) -> Result<Arc<dyn AgentAdapter>, String> {
    let raw = std::fs::read_to_string(path).map_err(|error| format!("读不了（{error}）"))?;
    let manifest = parse_manifest(&raw)?;
    validate_manifest(&manifest, path, allowed)?;
    // 进程 transport：**每个插件一个**，进程各自长驻、各自一本台账。
    let transport: Arc<dyn PluginTransport> = Arc::new(ProcessTransport::new(
        manifest.id.clone(),
        manifest.exe.clone(),
        manifest.args.clone(),
    ));
    Ok(Arc::new(ExternalAdapter::new(&manifest, transport)))
}

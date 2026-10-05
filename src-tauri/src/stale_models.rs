//! 失效模型扫描。
//!
//! ## 为什么需要
//!
//! 上游会下线模型、换 id、改窗口，而网关里的登记是**一次性快照**。
//! 实测（2026-10-05）库里`cohere/north-mini-code:free` 已从 OpenRouter 下架，
//! 实际调用时回 400；这类模型会一直留在候选链里拖慢自动切换 ——
//! 路由选到它、请求失败、再换下一家，日志里表现为「自动切换异常」。
//!
//! ## 判定口径（两级，缺一不可）
//!
//! 1. **目录差集**：拉上游 `/models`，本地登记的 id 不在上游目录里 → 判定失效。
//!    快、零额度消耗，但发现不了「目录里还在、调用却报错」的情况。
//! 2. **实调探测**：对**只做第1 级会漏判**的可疑项发一次最小请求。
//!    只在第 1 级「上游没给出目录」或「本地 id 在目录里但与上游自报能力冲突」时启用。
//!
//! 为什么不做「对每个模型都发一次探测」：本机137 个模型、27B CPU 推理，
//! 全量探测要几十分钟且可能烧额度。差集先行能把量级降到个位数。
//!
//! ## 为什么默认不删
//!
//! 扫描与删除分成两个命令。删���不可逆，且「上游目录偶发返回不全」这种
//! 暂态会误杀整批模型 —— 所以必须由用户看着清单勾选确认。

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::domain::{Dialect, Provider};

/// 判定结果分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleVerdict {
    /// 上游目录里已经没有这个 id —— 确定失效。
    MissingFromCatalog,
    /// 上游目录不可用（网络/鉴权/不支持列目录），无法判定。**不算失效**。
    CatalogUnavailable,
    /// 目录里还在，但实际调用失败。需实调确认后才下结论。
    ProbeFailed,
    /// 实调确认失效。
    ProbeRejected,
    /// 仍然可用。
    Healthy,
}

/// 单个模型的扫描结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleEntry {
    pub provider_id: String,
    pub provider_name: String,
    pub alias: String,
    pub upstream: String,
    pub verdict: StaleVerdict,
    /// 给用户看的一句话说明（不写凭据、不写完整响应体）。
    pub detail: String,
}

impl StaleEntry {
    /// 是否**确定**可以删。
    ///
    /// `CatalogUnavailable` 明确不算：上游列表拉不到时把所有模型都判失效，
    /// 一次网络抖动就能让用户误删整个供应商。
    pub fn is_removable(&self) -> bool {
        matches!(
            self.verdict,
            StaleVerdict::MissingFromCatalog | StaleVerdict::ProbeRejected
        )
    }
}

/// 一次扫描的完整结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleScanResult {
    pub entries: Vec<StaleEntry>,
    /// 拉不到目录的供应商 id。这些供应商的模型**一条都没判**。
    pub catalog_unavailable: Vec<String>,
    /// 是否做了实调探测（第二级）。
    pub probed: bool,
}

impl StaleScanResult {
    pub fn removable(&self) -> Vec<&StaleEntry> {
        self.entries.iter().filter(|e| e.is_removable()).collect()
    }
}

/// 第一级判定：拿上游目录做差集。
///
/// `catalog` 为 `None` 表示上游不支持列目录或这次没拉到 —— 此时**一律判为
/// 无法判定**，绝不把「拉不到」当成「没有」。
pub fn scan_by_catalog(provider: &Provider, catalog: Option<&BTreeSet<String>>) -> Vec<StaleEntry> {
    let Some(upstream_ids) = catalog else {
        return provider
            .models
            .iter()
            .map(|m| StaleEntry {
                provider_id: provider.id.clone(),
                provider_name: provider.name.clone(),
                alias: m.alias.clone(),
                upstream: m.upstream.clone(),
                verdict: StaleVerdict::CatalogUnavailable,
                detail: "上游目录不可用，无法判定是否失效".into(),
            })
            .collect();
    };

    provider
        .models
        .iter()
        .map(|m| {
            let upstream_id = m.upstream.trim();
            // 归一化后再比：上游目录常写成 `model:free`，而本地可能带了前缀，
            // 但**不做大小写折叠** —— 模型 id 大小写敏感（OpenRouter 上
            // `GPT-4` 与 `gpt-4` 是两个东西），折叠会误判成「还在」。
            let normalized = normalize_model_id(upstream_id);
            let alive = upstream_ids
                .iter()
                .any(|id| normalize_model_id(id) == normalized);
            StaleEntry {
                provider_id: provider.id.clone(),
                provider_name: provider.name.clone(),
                alias: m.alias.clone(),
                upstream: m.upstream.clone(),
                verdict: if alive {
                    StaleVerdict::Healthy
                } else {
                    StaleVerdict::MissingFromCatalog
                },
                detail: if alive {
                    "上游目录中仍存在".into()
                } else {
                    "上游目录中已无此 id".to_owned()
                },
            }
        })
        .collect()
}

/// 统一 id 形态：去空白、去尾部 `/`、去重复斜杠。
fn normalize_model_id(id: &str) -> String {
    id.trim().trim_end_matches('/').replace("//", "/")
}

/// 该供应商的方言是否支持用「列目录」来判活跃。
///
/// Ollama 与 OpenAI 兼容都能列；Anthropic 的 `/v1/models` 只列自家模型，
/// 拿它判别家聚合站会全判失效 —— 所以这类**必须走实调**。
pub fn catalog_is_authoritative(dialect: Dialect) -> bool {
    !matches!(dialect, Dialect::Anthropic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ModelRef, ModelType};

    fn model(alias: &str, upstream: &str) -> ModelRef {
        ModelRef {
            alias: alias.into(),
            upstream: upstream.into(),
            context_window: 128_000,
            supports_tools: true,
            supports_vision: false,
            supports_audio: false,
            supports_video: false,
            supports_thinking: false,
            supports_stream: true,
            model_type: ModelType::Chat,
            upstream_path: None,
            price: None,
            overrides: None,
            local: None,
        }
    }

    fn provider_with(dialect: Dialect, models: Vec<ModelRef>) -> Provider {
        Provider {
            id: "openrouter".into(),
            name: "openrouter".into(),
            dialect,
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key_enc: "cipher".into(),
            enabled: true,
            priority: 10,
            models,
            rpm_limit: 0,
            intelligence: 70,
            note: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn 目录里没有的判为失效() {
        let provider = provider_with(
            Dialect::OpenAI,
            vec![
                model("a", "vendor/live-model"),
                model("b", "vendor/dead-model"),
            ],
        );
        let catalog: BTreeSet<String> = ["vendor/live-model"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let entries = scan_by_catalog(&provider, Some(&catalog));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].verdict, StaleVerdict::Healthy);
        assert_eq!(entries[1].verdict, StaleVerdict::MissingFromCatalog);
        assert!(entries[1].is_removable());
        assert!(!entries[0].is_removable());
    }

    /// 反向判据：一次网络抖动导致目录拉空，若把它当成「上游什么都没有」，
    /// 用户一点确认就会删掉整个供应商的所有模型。
    #[test]
    fn 拉不到目录时一律判为无法判定_不得当成失效() {
        let provider = provider_with(
            Dialect::OpenAI,
            vec![model("a", "vendor/one"), model("b", "vendor/two")],
        );
        let entries = scan_by_catalog(&provider, None);
        assert_eq!(entries.len(), 2);
        assert!(entries
            .iter()
            .all(|e| e.verdict == StaleVerdict::CatalogUnavailable));
        assert!(
            entries.iter().all(|e| !e.is_removable()),
            "无法判定时绝不能列为可删 —— 这是防误删的第一道闸"
        );
    }

    /// 对照组：空目录是「上游真的一个模型都没有」，与「拉不到」不同。
    #[test]
    fn 空目录_是上游确实没有_判失效() {
        let provider = provider_with(Dialect::OpenAI, vec![model("a", "vendor/one")]);
        let catalog = BTreeSet::new();
        let entries = scan_by_catalog(&provider, Some(&catalog));
        assert_eq!(entries[0].verdict, StaleVerdict::MissingFromCatalog);
        assert!(entries[0].is_removable());
    }

    #[test]
    fn 模型_id_不得做大小写折叠() {
        // OpenRouter 上 `GPT-4` 与 `gpt-4` 是两个不同模型。
        // 折叠大小写会把「上游已下线的 GPT-4」误判成「gpt-4 还在」。
        let provider = provider_with(Dialect::OpenAI, vec![model("x", "vendor/GPT-4")]);
        let catalog: BTreeSet<String> = ["vendor/gpt-4"].iter().map(|s| s.to_string()).collect();
        let entries = scan_by_catalog(&provider, Some(&catalog));
        assert_eq!(
            entries[0].verdict,
            StaleVerdict::MissingFromCatalog,
            "大小写不同就是另一个模型，必须判失效"
        );
    }

    #[test]
    fn 尾部斜杠与重复斜杠应归一() {
        let provider = provider_with(Dialect::OpenAI, vec![model("x", "vendor/model/")]);
        let catalog: BTreeSet<String> = ["vendor/model"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            scan_by_catalog(&provider, Some(&catalog))[0].verdict,
            StaleVerdict::Healthy
        );
    }

    #[test]
    fn anthropic_的目录不可信_必须走实调() {
        // /v1/models 只列 Anthropic 自家模型；拿它判别家聚合站会全判失效。
        assert!(!catalog_is_authoritative(Dialect::Anthropic));
        assert!(catalog_is_authoritative(Dialect::OpenAI));
        assert!(catalog_is_authoritative(Dialect::Ollama));
        assert!(catalog_is_authoritative(Dialect::Gemini));
    }
}

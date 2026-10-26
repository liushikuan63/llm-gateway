//! Immutable reference catalogs. Persist the ID with configured prices before scanning.
use super::{ClientUsageRow, UsagePriceReference};
use crate::domain::{Currency, ModelPrice, PriceRule, PriceSource, PriceTier};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) enum CatalogVersion {
    #[serde(rename = "2026-10-14")]
    Legacy20261014,
    #[serde(rename = "2026-10-15")]
    Verified20261015,
}
pub(super) const CURRENT: CatalogVersion = CatalogVersion::Verified20261015;

pub(super) struct PriceSnapshot {
    pub configured: Vec<UsagePriceReference>,
    pub catalog: CatalogVersion,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema_version: u32,
    catalog: CatalogVersion,
    configured: Vec<UsagePriceReference>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Stored {
    Envelope(Envelope),
    Legacy(Vec<UsagePriceReference>),
}
impl PriceSnapshot {
    pub fn current(configured: Vec<UsagePriceReference>) -> Self {
        Self {
            configured,
            catalog: CURRENT,
        }
    }
    pub fn encode(self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&Envelope {
            schema_version: 1,
            catalog: self.catalog,
            configured: self.configured,
        })
    }
    pub fn decode(raw: &str) -> Result<Self, serde_json::Error> {
        match serde_json::from_str(raw)? {
            Stored::Legacy(configured) => Ok(Self {
                configured,
                catalog: CatalogVersion::Legacy20261014,
            }),
            Stored::Envelope(envelope) => {
                if envelope.schema_version != 1 {
                    return Err(serde::de::Error::custom(
                        "unsupported price snapshot schema",
                    ));
                }
                Ok(Self {
                    configured: envelope.configured,
                    catalog: envelope.catalog,
                })
            }
        }
    }
}

pub(super) struct CatalogPrice {
    pub price: ModelPrice,
    pub reference: String,
    pub force_time_range: bool,
}

// Version data is deliberately independent. Never edit an already persisted catalog;
// add a new version when rates, identity conditions or reference semantics change.
const LEGACY_OPENAI: [(&str, [f64; 4]); 4] = [
    ("gpt-6.1-sol", [2., 0.1, 2.5, 10.]),
    ("gpt-6-astra", [10., 1., 12.5, 50.]),
    ("gpt-6-sol", [2., 0.2, 2.5, 10.]),
    ("gpt-6-luna", [0.1, 0.01, 0.125, 0.5]),
];
const VERIFIED_OPENAI: [(&str, [f64; 4]); 4] = [
    ("gpt-6.1-sol", [2., 0.1, 2.5, 10.]),
    ("gpt-6-astra", [10., 1., 12.5, 50.]),
    ("gpt-6-sol", [2., 0.2, 2.5, 10.]),
    ("gpt-6-luna", [0.1, 0.01, 0.125, 0.5]),
];
fn openai(row: &ClientUsageRow, entries: &[(&str, [f64; 4])], date: &str) -> Option<CatalogPrice> {
    if row.source != "codex"
        || !row
            .provider
            .as_deref()
            .map_or(true, |p| p.eq_ignore_ascii_case("openai"))
    {
        return None;
    }
    let (_, [input, read, write, output]) =
        entries.iter().find(|(model, _)| *model == row.model)?;
    Some(CatalogPrice {
        price: ModelPrice {
            prompt: *input,
            completion: *output,
            cache_read: Some(*read),
            cache_creation: Some(*write),
            currency: Currency::Usd,
            tiers: vec![PriceTier {
                min_prompt_tokens: 272001,
                prompt: input * 2.,
                completion: output * 1.5,
                cache_read: Some(read * 2.),
                cache_creation: Some(write * 2.),
            }],
            rules: vec![],
            source: PriceSource::Catalog,
        },
        reference: format!("OpenAI API标准参考价 · {date} · {}", row.model),
        force_time_range: false,
    })
}
fn verified_dsh(row: &ClientUsageRow) -> Option<CatalogPrice> {
    if row.source != "dsh" {
        return None;
    }
    let provider = row.provider.as_deref()?;
    let provenance = if provider.eq_ignore_ascii_case("commandcode")
        && row.model == "deepseek/deepseek-v4.1-flash"
    {
        "CommandCode API标准参考价 · https://commandcode.ai/models/deepseek-v4-1-flash"
    } else if provider.eq_ignore_ascii_case("deepseek-account") && row.model == "deepseek-flash" {
        "DeepSeek 官方API等价参考价（非账号或订阅实际账单） · https://api-docs.deepseek.com/quick_start/pricing/"
    } else {
        return None;
    };
    let calendar_note = if provider.eq_ignore_ascii_case("commandcode") {
        "缺完整时段/日历分配，保留峰谷参考区间"
    } else {
        "缺时分及工作日/中国公众假日分配，保留峰谷参考区间"
    };
    Some(CatalogPrice {
        price: ModelPrice {
            prompt: 0.15,
            completion: 0.6,
            cache_read: Some(0.003),
            cache_creation: Some(0.15),
            currency: Currency::Usd,
            tiers: vec![],
            rules: [(60, 240), (360, 600)]
                .into_iter()
                .map(|(start_minute, end_minute)| PriceRule {
                    label: "工作日峰时参考（未分配日历）".into(),
                    start_minute,
                    end_minute,
                    prompt_multiplier: 2.,
                    completion_multiplier: 2.,
                })
                .collect(),
            source: PriceSource::Catalog,
        },
        reference: format!(
            "{provenance} · 2026-10-15 · {} · {calendar_note}",
            row.model
        ),
        force_time_range: true,
    })
}
pub(super) fn official(row: &ClientUsageRow, catalog: CatalogVersion) -> Option<CatalogPrice> {
    match catalog {
        CatalogVersion::Legacy20261014 => openai(row, &LEGACY_OPENAI, "2026-10-14"),
        CatalogVersion::Verified20261015 => {
            openai(row, &VERIFIED_OPENAI, "2026-10-15").or_else(|| verified_dsh(row))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn row(source: &str, provider: Option<&str>, model: &str) -> ClientUsageRow {
        let mut rows = super::super::Rows::new();
        super::super::push(
            &mut rows,
            source,
            "default",
            model.into(),
            provider.map(str::to_owned),
            "event",
            None,
            [Some(60), Some(40), Some(20), Some(30)],
            Some(1),
            None,
        )
        .row
        .clone()
    }
    #[test]
    fn snapshots_have_strict_schema_and_legacy_arrays_keep_their_values_and_catalog() {
        let configured: Vec<UsagePriceReference> = serde_json::from_value(json!([{"provider":"manual","models":["m"],"price":{"prompt":-1,"completion":2,"currency":"usd"}}])).unwrap();
        let value = serde_json::to_value(&configured).unwrap();
        let encoded = PriceSnapshot::current(configured).encode().unwrap();
        let decoded = PriceSnapshot::decode(&encoded).unwrap();
        assert_eq!(decoded.catalog, CURRENT);
        assert_eq!(serde_json::to_value(&decoded.configured).unwrap(), value);
        for raw in ["[]".to_owned(), value.to_string()] {
            let old = PriceSnapshot::decode(&raw).unwrap();
            assert_eq!(old.catalog, CatalogVersion::Legacy20261014);
            assert_eq!(
                serde_json::to_value(old.configured).unwrap(),
                serde_json::from_str::<serde_json::Value>(&raw).unwrap()
            );
        }
        assert_eq!(
            PriceSnapshot::decode(r#"{"schema_version":1,"catalog":"2026-10-14","configured":[]}"#)
                .unwrap()
                .catalog,
            CatalogVersion::Legacy20261014
        );
        for raw in [
            "null",
            "1",
            "true",
            "\"array\"",
            "[",
            "{}",
            "[null]",
            r#"{"schema_version":2,"catalog":"2026-10-15","configured":[]}"#,
            r#"{"schema_version":1,"catalog":"future","configured":[]}"#,
            r#"{"schema_version":1,"configured":[]}"#,
            r#"{"catalog":"2026-10-15","configured":[]}"#,
            r#"{"schema_version":1,"catalog":"2026-10-15"}"#,
            r#"{"schema_version":1,"catalog":"2026-10-15","configured":null}"#,
            r#"{"schema_version":1,"catalog":"2026-10-15","configured":{}}"#,
            r#"{"schema_version":1,"catalog":"2026-10-15","configured":[],"extra":1}"#,
            r#"{"schema_version":1,"schema_version":1,"catalog":"2026-10-15","configured":[]}"#,
            r#"{"schema_version":1,"catalog":"2026-10-14","catalog":"2026-10-15","configured":[]}"#,
            r#"{"schema_version":1,"catalog":"2026-10-15","configured":[],"configured":[]}"#,
        ] {
            assert!(PriceSnapshot::decode(raw).is_err(), "must reject {raw}");
        }
    }
    #[test]
    fn independently_frozen_openai_catalogs_keep_rates_tiers_and_identity_contracts() {
        for (version, date) in [
            (CatalogVersion::Legacy20261014, "2026-10-14"),
            (CURRENT, "2026-10-15"),
        ] {
            for (model, [prompt, read, write, output]) in [
                ("gpt-6-astra", [10., 1., 12.5, 50.]),
                ("gpt-6.1-sol", [2., 0.1, 2.5, 10.]),
                ("gpt-6-sol", [2., 0.2, 2.5, 10.]),
                ("gpt-6-luna", [0.1, 0.01, 0.125, 0.5]),
            ] {
                for provider in [None, Some("openai"), Some("OpenAI")] {
                    let r = official(&row("codex", provider, model), version).unwrap();
                    assert_eq!(
                        [
                            r.price.prompt,
                            r.price.cache_read.unwrap(),
                            r.price.cache_creation.unwrap(),
                            r.price.completion
                        ],
                        [prompt, read, write, output]
                    );
                    assert_eq!(r.price.currency, Currency::Usd);
                    assert_eq!(r.price.source, PriceSource::Catalog);
                    assert!(!r.force_time_range && r.price.rules.is_empty());
                    assert!(r.reference.contains(date));
                    assert_eq!(r.price.tiers.len(), 1);
                    let tier = &r.price.tiers[0];
                    assert_eq!(tier.min_prompt_tokens, 272001);
                    assert_eq!(
                        [
                            tier.prompt,
                            tier.cache_read.unwrap(),
                            tier.cache_creation.unwrap(),
                            tier.completion
                        ],
                        [prompt * 2., read * 2., write * 2., output * 1.5]
                    );
                    assert_eq!(r.price.effective_unit(272000).0, prompt);
                    assert_eq!(r.price.effective_unit(272001).0, prompt * 2.);
                }
                for (source, provider) in [
                    ("dsh", Some("openai")),
                    ("qoder", Some("openai")),
                    ("codex", Some("other")),
                    ("codex", Some("unknown")),
                ] {
                    assert!(official(&row(source, provider, model), version).is_none());
                }
            }
        }
    }
    #[test]
    fn only_verified_dsh_catalog_adds_two_exact_pairs_and_complete_peak_rules() {
        for (provider, model) in [
            ("commandcode", "deepseek/deepseek-v4.1-flash"),
            ("deepseek-account", "deepseek-flash"),
        ] {
            let r = row("dsh", Some(provider), model);
            assert!(official(&r, CatalogVersion::Legacy20261014).is_none());
            let reference = official(&r, CURRENT).unwrap();
            let price = reference.price;
            assert!(reference.force_time_range);
            assert!(price.tiers.is_empty());
            assert_eq!(
                [
                    price.prompt,
                    price.completion,
                    price.cache_read.unwrap(),
                    price.cache_creation.unwrap()
                ],
                [0.15, 0.6, 0.003, 0.15]
            );
            assert_eq!(
                price
                    .rules
                    .iter()
                    .map(|r| (
                        r.start_minute,
                        r.end_minute,
                        r.prompt_multiplier,
                        r.completion_multiplier
                    ))
                    .collect::<Vec<_>>(),
                vec![(60, 240, 2., 2.), (360, 600, 2., 2.)]
            );
            for (minute, multiplier) in [
                (59, 1.),
                (60, 2.),
                (239, 2.),
                (240, 1.),
                (359, 1.),
                (360, 2.),
                (599, 2.),
                (600, 1.),
            ] {
                assert_eq!(
                    price
                        .active_rule(minute)
                        .map_or(1., |r| r.prompt_multiplier),
                    multiplier
                );
            }
            assert!(
                reference.reference.contains("峰谷参考区间")
                    && reference.reference.contains("2026-10-15")
            );
            if provider == "commandcode" {
                assert!(reference.reference.contains("缺完整时段/日历分配"));
                assert!(!reference.reference.contains("中国公众假日"));
            } else {
                assert!(reference.reference.contains("中国公众假日"));
                assert!(reference.reference.contains("非账号或订阅实际账单"));
            }
            let upper = provider.to_ascii_uppercase();
            assert!(official(&row("dsh", Some(&upper), model), CURRENT).is_some());
        }
    }
}

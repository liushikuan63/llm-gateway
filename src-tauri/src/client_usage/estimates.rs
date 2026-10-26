//! API-equivalent references and conservative intervals, never a supplier bill.
use super::price_catalog::{self, CatalogPrice};
use super::{Aggregate, ClientUsageRow, UsagePriceReference};
use crate::domain::ModelPrice;
use chrono::{DateTime, Timelike};

pub(super) fn minute(timestamp: &serde_json::Value) -> Option<u16> {
    DateTime::parse_from_rfc3339(timestamp.as_str()?)
        .ok()
        .map(|time| {
            let utc = time.with_timezone(&chrono::Utc);
            (utc.hour() * 60 + utc.minute()) as u16
        })
}
fn resolve(
    row: &ClientUsageRow,
    configured: &[UsagePriceReference],
) -> Result<CatalogPrice, &'static str> {
    if row.provider.as_deref() == Some("unknown") {
        return Err("provider_unknown");
    }
    let matches: Vec<_> = configured
        .iter()
        .filter(|p| {
            p.models.iter().any(|m| m == &row.model)
                && row
                    .provider
                    .as_ref()
                    .map_or(true, |provider| provider.eq_ignore_ascii_case(&p.provider))
        })
        .collect();
    if let Some(first) = matches.first() {
        if matches.iter().any(|p| p.price != first.price) {
            return Err("price_ambiguous");
        }
        if !first.price.is_valid() {
            return Err("invalid_price");
        }
        return Ok(CatalogPrice {
            price: first.price.clone(),
            reference: format!("本机参考价 · {} · {}", first.provider, row.model),
            force_time_range: false,
        });
    }
    if configured
        .iter()
        .any(|p| p.models.iter().any(|m| m == &row.model))
    {
        return Err("provider_price_unmatched");
    }
    if let Some(price) = price_catalog::official(row, super::jobs::price_catalog()) {
        return Ok(price);
    }
    Err("price_missing")
}
fn range(
    price: &ModelPrice,
    counts: [Option<u64>; 4],
    request: bool,
    time: Option<u16>,
) -> Result<(f64, f64, Option<&'static str>), &'static str> {
    let (Some(input), Some(output)) = (counts[0], counts[1]) else {
        return Err("usage_incomplete");
    };
    let read = counts[2].unwrap_or(0);
    let write = counts[3].unwrap_or(0);
    if read > input || write > input - read {
        return Err("invalid_usage");
    }
    let remaining = input - read - write;
    let mut allocations = vec![(read, write)];
    if counts[2].is_none() {
        allocations.push((read + remaining, write));
    }
    if counts[3].is_none() {
        allocations.push((read, write + remaining));
    }
    let mut units = vec![price.clone()];
    if !request && !price.tiers.is_empty() {
        units[0].tiers.clear();
        for tier in &price.tiers {
            // Every individual request is no larger than this known aggregate.
            // A tier above its total input cannot possibly have applied.
            if tier.min_prompt_tokens <= 0 || tier.min_prompt_tokens > input as i64 {
                continue;
            }
            let mut unit = price.clone();
            unit.prompt = tier.prompt;
            unit.completion = tier.completion;
            unit.cache_read = tier.cache_read.or(price.cache_read);
            unit.cache_creation = tier.cache_creation.or(price.cache_creation);
            unit.tiers.clear();
            units.push(unit);
        }
    }
    let minutes = if let Some(value) = time {
        vec![value]
    } else {
        let mut values = vec![0];
        for rule in &price.rules {
            values.extend([rule.start_minute, rule.end_minute]);
        }
        values.sort_unstable();
        values.dedup();
        values
    };
    // Different requests may use different tiers/time windows. Bound each token
    // category separately; a homogeneous aggregate is not a valid envelope when
    // input and output rates change in opposite directions.
    let mut unit_low = [f64::INFINITY; 4];
    let mut unit_high = [0f64; 4];
    for unit in units {
        for minute in &minutes {
            let (prompt, completion, tier) = unit.effective_unit(input as i64);
            let read = tier
                .and_then(|t| t.cache_read)
                .or(unit.cache_read)
                .unwrap_or(prompt);
            let write = tier
                .and_then(|t| t.cache_creation)
                .or(unit.cache_creation)
                .unwrap_or(prompt);
            let (pm, cm) = unit
                .active_rule(*minute)
                .map(|r| (r.prompt_multiplier, r.completion_multiplier))
                .unwrap_or((1., 1.));
            for (index, value) in [prompt * pm, completion * cm, read * pm, write * pm]
                .into_iter()
                .enumerate()
            {
                if !value.is_finite() {
                    return Err("invalid_price");
                }
                unit_low[index] = unit_low[index].min(value);
                unit_high[index] = unit_high[index].max(value);
            }
        }
    }
    let charge = |units: &[f64; 4], cached: u64, written: u64| {
        ((input - cached - written) as f64 * units[0]
            + output as f64 * units[1]
            + cached as f64 * units[2]
            + written as f64 * units[3])
            / 1_000_000.
    };
    let low = allocations
        .iter()
        .map(|(r, w)| charge(&unit_low, *r, *w))
        .fold(f64::INFINITY, f64::min);
    let high = allocations
        .iter()
        .map(|(r, w)| charge(&unit_high, *r, *w))
        .fold(0., f64::max);
    if !low.is_finite() || !high.is_finite() {
        return Err("invalid_price");
    }
    let note = if low == high {
        None
    } else if (!request && !price.tiers.is_empty()) || (time.is_none() && !price.rules.is_empty()) {
        Some("conditional_price")
    } else {
        Some("cache_fields_unknown")
    };
    Ok((low, high, note))
}
pub(super) fn add_event(
    a: &mut Aggregate,
    counts: [Option<u64>; 4],
    prices: Option<&[UsagePriceReference]>,
    request: bool,
    time: Option<u16>,
) {
    let Some(prices) = prices else {
        return;
    };
    let first = a.priced_events == 0;
    a.priced_events += 1;
    let computed = resolve(&a.row, prices).and_then(|resolved| {
        let CatalogPrice {
            price,
            reference,
            force_time_range,
        } = resolved;
        a.row.price_reference = Some(reference);
        a.row.estimated_currency = Some(price.currency.code().into());
        range(
            &price,
            counts,
            request,
            if force_time_range { None } else { time },
        )
    });
    match computed {
        Ok((low, high, note)) => {
            a.row.estimated_cost_min = if first {
                Some(low)
            } else {
                a.row
                    .estimated_cost_min
                    .map(|old| old + low)
                    .filter(|v| v.is_finite())
            };
            a.row.estimated_cost_max = if first {
                Some(high)
            } else {
                a.row
                    .estimated_cost_max
                    .map(|old| old + high)
                    .filter(|v| v.is_finite())
            };
            a.row.estimated_cost = a
                .row
                .estimated_cost_min
                .zip(a.row.estimated_cost_max)
                .filter(|(a, b)| a == b)
                .map(|(value, _)| value);
            if first || note.is_some() {
                a.row.estimate_note = note.map(str::to_owned);
            }
        }
        Err(note) => {
            a.row.estimated_cost = None;
            a.row.estimated_cost_min = None;
            a.row.estimated_cost_max = None;
            a.row.estimate_note = Some(note.into());
        }
    }
}
pub(super) fn apply_aggregate(row: &mut ClientUsageRow, prices: &[UsagePriceReference]) {
    let mut a = Aggregate {
        row: row.clone(),
        seen: [0; 4],
        missing: [0; 4],
        priced_events: 0,
    };
    a.row.estimated_cost = None;
    a.row.estimated_cost_min = None;
    a.row.estimated_cost_max = None;
    a.row.price_reference = None;
    a.row.estimated_currency = None;
    if row
        .partial_fields
        .iter()
        .any(|field| field == "input_tokens" || field == "output_tokens")
    {
        a.row.estimate_note = Some("usage_incomplete".into());
    } else {
        let mut counts = [
            row.input_tokens,
            row.output_tokens,
            row.cache_read_tokens,
            row.cache_write_tokens,
        ];
        if row.partial_fields.iter().any(|f| f == "cache_read_tokens") {
            counts[2] = None;
        }
        if row.partial_fields.iter().any(|f| f == "cache_write_tokens") {
            counts[3] = None;
        }
        add_event(&mut a, counts, Some(prices), false, None);
    }
    *row = a.row;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dsh_official_lone_minute_still_uses_envelope_while_configured_minute_is_exact() {
        for (provider, model) in [
            ("commandcode", "deepseek/deepseek-v4.1-flash"),
            ("deepseek-account", "deepseek-flash"),
        ] {
            for time in [None, Some(0), Some(60), Some(240), Some(360), Some(600)] {
                let mut rows = super::super::Rows::new();
                let aggregate = super::super::push(
                    &mut rows,
                    "dsh",
                    "default",
                    model.into(),
                    Some(provider.into()),
                    "event",
                    None,
                    [Some(60), Some(40), Some(20), Some(30)],
                    Some(1),
                    None,
                );
                add_event(
                    aggregate,
                    [Some(60), Some(40), Some(20), Some(30)],
                    Some(&[]),
                    true,
                    time,
                );
                assert!((aggregate.row.estimated_cost_min.unwrap() - 0.00003006).abs() < 1e-12);
                assert!((aggregate.row.estimated_cost_max.unwrap() - 0.00006012).abs() < 1e-12);
                assert!(aggregate.row.estimated_cost.is_none());
                let configured = UsagePriceReference {
                    provider: provider.into(),
                    models: vec![model.into()],
                    price: price_catalog::official(&aggregate.row, price_catalog::CURRENT)
                        .unwrap()
                        .price,
                };
                let mut manual = Aggregate {
                    row: aggregate.row.clone(),
                    seen: [0; 4],
                    missing: [0; 4],
                    priced_events: 0,
                };
                add_event(
                    &mut manual,
                    [Some(60), Some(40), Some(20), Some(30)],
                    Some(&[configured]),
                    true,
                    time,
                );
                if let Some(minute) = time {
                    let expected = if (60..240).contains(&minute) || (360..600).contains(&minute) {
                        0.00006012
                    } else {
                        0.00003006
                    };
                    assert!((manual.row.estimated_cost.unwrap() - expected).abs() < 1e-12);
                } else {
                    assert!(manual.row.estimated_cost.is_none());
                }
            }
        }
    }
}

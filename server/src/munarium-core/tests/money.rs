// SPDX-License-Identifier: Apache-2.0
use chrono::{TimeZone, Utc};
use munarium_core::money::*;
use std::collections::BTreeMap;

fn price() -> PriceSnapshot {
    PriceSnapshot {
        id: "fictional-v1".into(),
        provider: "fictional".into(),
        route: "a".repeat(64),
        model: "test".into(),
        currency: "USD".into(),
        valid_from: Utc.timestamp_opt(100, 0).unwrap(),
        valid_until: Utc.timestamp_opt(200, 0).unwrap(),
        basis: Basis::Inclusive,
        rates: BTreeMap::from([
            (
                "input".into(),
                Rate {
                    micro_units: 1,
                    per_tokens: 3,
                },
            ),
            (
                "output".into(),
                Rate {
                    micro_units: 0,
                    per_tokens: 1,
                },
            ),
        ]),
    }
}
fn usage() -> MoneyUsage {
    MoneyUsage {
        input: Some(1),
        output: Some(0),
        ..Default::default()
    }
}

#[test]
fn fractional_rounding_explicit_zero_and_exact_limits() {
    let p = price();
    for (tokens, expected) in [(0, "0"), (1, "1"), (3, "1"), (4, "2")] {
        let u = MoneyUsage {
            input: Some(tokens),
            ..usage()
        };
        assert_eq!(
            calculate(Some(&p), p.valid_from, &u)
                .unwrap()
                .micro_units
                .as_deref(),
            Some(expected)
        );
    }
    let mut zero = p.clone();
    zero.rates.get_mut("input").unwrap().micro_units = 0;
    assert_eq!(
        calculate(Some(&zero), p.valid_from, &usage())
            .unwrap()
            .micro_units
            .as_deref(),
        Some("0")
    );
}

#[test]
fn missing_expired_and_legacy_are_unknown_never_free() {
    let p = price();
    assert!(
        calculate(None, p.valid_from, &usage())
            .unwrap()
            .missing_price
    );
    assert!(
        calculate(Some(&p), p.valid_until, &usage())
            .unwrap()
            .missing_price
    );
    assert!(
        calculate(
            Some(&p),
            p.valid_from - chrono::Duration::seconds(1),
            &usage()
        )
        .unwrap()
        .missing_price
    );
    assert!(calculate(
        Some(&p),
        p.valid_until - chrono::Duration::seconds(1),
        &usage()
    )
    .unwrap()
    .micro_units
    .is_some());
    let legacy = calculate(Some(&p), p.valid_from, &MoneyUsage::default()).unwrap();
    assert!(legacy.missing_usage && legacy.micro_units.is_none());
    let mut missing = p.clone();
    missing.rates.remove("output");
    assert!(
        calculate(Some(&missing), p.valid_from, &usage())
            .unwrap()
            .missing_price
    );
    let unknown = calculate(
        Some(&p),
        p.valid_from,
        &MoneyUsage {
            unknown_categories: true,
            ..usage()
        },
    )
    .unwrap();
    assert!(unknown.unknown_categories && unknown.micro_units.is_none());
}

#[test]
fn categories_are_disjoint_and_inclusive_does_not_double_charge_reasoning() {
    let mut p = price();
    for rate in p.rates.values_mut() {
        *rate = Rate {
            micro_units: 1,
            per_tokens: 1,
        };
    }
    let u = MoneyUsage {
        input: Some(10),
        output: Some(20),
        cache_read: Some(3),
        cache_write: Some(2),
        reasoning: Some(7),
        context: Some(0),
        unknown_categories: false,
    };
    assert_eq!(
        calculate(Some(&p), p.valid_from, &u)
            .unwrap()
            .micro_units
            .as_deref(),
        Some("30")
    );
    p.basis = Basis::Partitioned;
    for key in ["cache_read", "cache_write", "reasoning", "context"] {
        p.rates.insert(
            key.into(),
            Rate {
                micro_units: 1,
                per_tokens: 1,
            },
        );
    }
    assert_eq!(
        calculate(Some(&p), p.valid_from, &u)
            .unwrap()
            .micro_units
            .as_deref(),
        Some("30")
    );
    p.rates.get_mut("cache_read").unwrap().micro_units = 0;
    assert_eq!(
        calculate(Some(&p), p.valid_from, &u)
            .unwrap()
            .micro_units
            .as_deref(),
        Some("27")
    );
    assert!(
        calculate(Some(&p), p.valid_from, &usage())
            .unwrap()
            .missing_usage
    );
    assert!(calculate(
        Some(&p),
        p.valid_from,
        &MoneyUsage {
            reasoning: Some(21),
            ..u
        }
    )
    .is_err());
}

#[test]
fn checked_wide_arithmetic_and_tariff_validation() {
    let mut p = price();
    p.rates.insert(
        "input".into(),
        Rate {
            micro_units: u64::MAX,
            per_tokens: u64::MAX,
        },
    );
    let u = MoneyUsage {
        input: Some(u64::MAX),
        ..usage()
    };
    assert_eq!(
        calculate(Some(&p), p.valid_from, &u).unwrap().micro_units,
        Some(u64::MAX.to_string())
    );
    p.rates.get_mut("input").unwrap().per_tokens = 1;
    assert!(calculate(Some(&p), p.valid_from, &u).is_err());
    p.rates.get_mut("input").unwrap().per_tokens = 0;
    assert!(p.validate().is_err());
    p = price();
    p.rates.insert(
        "reasoning".into(),
        Rate {
            micro_units: 1,
            per_tokens: 1,
        },
    );
    assert!(p.validate().is_err()); // inclusive output plus reasoning would overlap
}

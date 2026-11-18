use super::*;

#[test]
fn micro_usd_zero_and_from_micros_roundtrip_exact_integers() {
    let zero = std::hint::black_box(money::MicroUsd::ZERO);
    assert!(zero.is_zero());
    let amount = money::MicroUsd::from_micros(25_000);
    assert!(!amount.is_zero());
    assert_eq!(amount.micros(), 25_000);
}

#[test]
fn micro_usd_checked_add_sums_and_detects_overflow() {
    let a = money::MicroUsd::from_micros(100);
    let b = money::MicroUsd::from_micros(250);
    assert_eq!(a.checked_add(b), Some(money::MicroUsd::from_micros(350)));
    assert!(money::MicroUsd::from_micros(u64::MAX)
        .checked_add(money::MicroUsd::from_micros(1))
        .is_none());
}

#[test]
fn micro_usd_ordering_compares_underlying_micro_amounts() {
    let small = money::MicroUsd::from_micros(10);
    let large = money::MicroUsd::from_micros(1_000);
    assert!(small < large);
}

#[test]
fn micro_usd_serde_transparent_roundtrips_as_u64() {
    let original = money::MicroUsd::from_micros(42_000);
    let json = serde_json::to_string(&original).expect("serialize");
    assert_eq!(json, "42000");
    let decoded: money::MicroUsd = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded, original);
}

#[test]
fn micro_usd_display_formats_dollars_and_parse_usd_roundtrips() {
    let parsed = money::MicroUsd::parse_usd("1.25").expect("valid dollar amount");
    assert_eq!(parsed, money::MicroUsd::from_micros(1_250_000));
    assert_eq!(parsed.to_string(), "$1.250000");
    assert_eq!(money::MicroUsd::ZERO.to_string(), "$0.000000");
}

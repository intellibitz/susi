use super::*;
use crate::money::MicroUsd;
use crate::pricing::TokenUsage;

const DOC: &str = r#"{"schema":"susi/harness/prices/v1","models":{"deepseek-flash":{"input_miss":150000,"input_hit":3000,"output":600000}}}"#;

#[test]
fn a_catalogue_prices_its_models() {
    let c = PriceCatalogue::from_json(DOC).unwrap();
    assert_eq!(c.len(), 1);
    let p = c.price("deepseek-flash").unwrap();
    assert_eq!(p.input_miss, MicroUsd::from_micros(150_000));
    let cost = p.cost(TokenUsage::new(3_000_000, 2_400_000, 200_000));
    assert_eq!(cost, MicroUsd::from_micros(217_200));
    assert!(c.price("unknown-model").is_none());
}

#[test]
fn a_foreign_schema_is_refused() {
    let e = PriceCatalogue::from_json(r#"{"schema":"other","models":{}}"#).unwrap_err();
    assert!(e.to_string().contains("schema"));
}

#[test]
fn a_blank_model_id_is_refused() {
    let d = r#"{"schema":"susi/harness/prices/v1","models":{"":{"input_miss":1,"input_hit":1,"output":1}}}"#;
    assert!(PriceCatalogue::from_json(d).is_err());
}

#[test]
fn a_catalogue_loads_from_a_file() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let p = dir.path().join("prices.json");
    std::fs::write(&p, DOC).unwrap();
    assert_eq!(PriceCatalogue::load(&p).unwrap().len(), 1);
}

#[test]
fn a_missing_file_is_an_error() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    assert!(PriceCatalogue::load(&dir.path().join("absent.json")).is_err());
}

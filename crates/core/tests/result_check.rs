use orm_contracts::{extension::ResultCheck, ir::{ColType, EnumIr, EnumStorage, EnumValueIr, FieldIr}};
use orm_core::result_check::{prepare, Category, EnumScalar, Warnings};
use serde_json::json;

#[test]
fn physical_enum_values_remain_representable_and_warnings_aggregate_without_values() {
    let mut field = FieldIr::plain("status", ColType::String);
    field.enum_idx = Some(0); field.enum_name = Some("Status".into());
    let enumeration = EnumIr { name: "Status".into(), db_name: "status".into(), storage: EnumStorage::Text,
        values: vec![EnumValueIr { name: "ACTIVE".into(), value: json!("active") }, EnumValueIr { name: "OLD".into(), value: json!("old-private-value") }], comment: None };
    let check = ResultCheck { model: "Active".into(), field: "status".into(), non_null: true, allowed: Some(vec!["ACTIVE".into()]), code: "acme.shape".into() };
    let prepared = prepare("Active", &[field], &[enumeration], &[&check]).unwrap().unwrap();
    let contract = &prepared.fields[0];
    assert!(contract.accepts_enum(EnumScalar::Text("active")));
    assert!(!contract.accepts_enum(EnumScalar::Text("old-private-value")));
    let mut warnings = Warnings::default();
    for _ in 0..100 { warnings.record(&prepared, contract, Category::EnumSubset); }
    warnings.record(&prepared, contract, Category::Null);
    let warnings = warnings.finish();
    assert_eq!(warnings.len(), 2);
    assert_eq!(warnings.iter().find(|w| w.category == Category::EnumSubset).unwrap().occurrence_count, 100);
    let json = serde_json::to_string(&warnings).unwrap();
    assert!(!json.contains("old-private-value"));
    assert!(json.contains("expected_shape") && json.contains("Active") && json.contains("acme.shape"));
}

#[test]
fn models_with_proxy_records_and_no_views_ask_for_a_new_generate() {
    let ir = orm_contracts::ir::SchemaIr::from_json(r#"{"models":[{"name":"A","table":"a","fields":[{"name":"id","column":"id","type":"int"}],"primary_key":["id"]}],"behavior":{"proxy_models":[{"model":"A","parent":"A"}]}}"#).unwrap();
    let Err(error) = orm_core::schema::Schema::from_ir(ir) else { panic!("accepted") };
    // A build without the proxy capability rejects the records first.
    assert!(error.contains("run `orm generate`") || error.contains("rebuild native artifact"), "{error}");
}

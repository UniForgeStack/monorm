use orm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use orm_engine::{params::NoParams, plan::{Output, Plan, Planner}};
use serde_json::json;

fn schema(defaults: bool) -> serde_json::Value {
    let mut schema = json!({"models":[{"name":"Selected","table":"selected","fields":[
        {"name":"value","column":"value","type":"text"},
        {"name":"id","column":"id","type":"int","primary_key":true},
        {"name":"active","column":"active","type":"bool"}
    ]}]});
    if defaults { schema["behavior"] = json!({"schema_contract":1,"query_defaults":[{"model":"Selected","filter":{"t":"col","path":[],"name":"active"},"fields":["value"]}]}); }
    schema
}

#[test]
fn explicit_shape_retains_private_identity() {
    let schema = Schema::from_ir(serde_json::from_value(schema(false)).unwrap()).unwrap();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let operation: Operation = serde_json::from_value(json!({"op":"select","model":"Selected","model_fields":["value"]})).unwrap();
        let Plan::Select(plan) = Planner::plan(&schema, Target::new(dialect), &operation, &NoParams).unwrap() else { panic!("select") };
        let Output::Instances { shape: Some(shape), .. } = plan.output else { panic!("partial shape") };
        assert_eq!(shape.fields.len(), 2);
        assert!(shape.fields[0].public); assert!(!shape.fields[1].public);
        assert_eq!(shape.fields[1].field.position, 1); assert_eq!(shape.fields[1].physical, Some(1));
    }
}

#[test]
fn duplicate_or_unknown_selection_rejected() {
    let schema = Schema::from_ir(serde_json::from_value(schema(false)).unwrap()).unwrap();
    for fields in [vec!["value", "value"], vec!["missing"]] {
        let operation = serde_json::from_value(json!({"op":"select","model":"Selected","model_fields":fields})).unwrap();
        assert!(Planner::plan(&schema, Target::new(Dialect::Sqlite), &operation, &NoParams).is_err());
    }
}

#[cfg(not(feature = "query-defaults"))]
#[test]
fn disabled_artifact_rejects_policies_at_definition() {
    let error = Schema::from_ir(serde_json::from_value(schema(true)).unwrap()).err().unwrap();
    assert!(error.contains("query-defaults") && error.contains("rebuild"));
}

#[cfg(feature = "query-defaults")]
#[test]
fn enabled_policy_shape_is_prepared() {
    let schema = Schema::from_ir(serde_json::from_value(schema(true)).unwrap()).unwrap();
    assert!(schema.models[0].query_defaults.filter.is_some());
    let operation = serde_json::from_value(json!({"op":"select","model":"Selected"})).unwrap();
    let Plan::Select(plan) = Planner::plan(&schema, Target::new(Dialect::Sqlite), &operation, &NoParams).unwrap() else { panic!("select") };
    assert!(matches!(plan.output, Output::Instances { shape: Some(_), .. }));
}

#[cfg(feature = "query-defaults")]
#[test]
fn policies_reject_bad_fields_and_recursive_loading() {
    let mut invalid = schema(true);
    invalid["behavior"]["query_defaults"][0]["fields"] = json!(["missing"]);
    assert!(Schema::from_ir(serde_json::from_value(invalid).unwrap()).is_err());
    let mut recursive = schema(true);
    recursive["models"][0]["relations"] = json!([{"name":"self_ref","kind":"one","target":"Selected","from":"id","to":"id"}]);
    recursive["behavior"]["query_defaults"][0]["related"] = json!([["self_ref"]]);
    let error = Schema::from_ir(serde_json::from_value(recursive).unwrap()).err().unwrap();
    assert!(error.contains("recursive default loading"));
}

#[cfg(feature = "query-defaults")]
#[test]
fn generated_types_distinguish_default_missing_fields_and_filtered_targets() {
    let mut raw = schema(true);
    raw["models"][0]["relations"] = json!([{"name":"target","kind":"one","target":"Selected","from":"id","to":"id","foreign_key":true}]);
    let ir = serde_json::from_value(raw.clone()).unwrap();
    let schema = Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap();
    let py = orm_core::codegen::python::generate(&ir, &schema, "selection-test").unwrap();
    assert!(py.stub.contains("f.BelongsTo[Selected | None"));
    let ts = orm_core::codegen::typescript::generate(&ir, &schema, "selection-test", "orm").unwrap();
    assert!(ts.contains("readonly active?: boolean"));
    assert!(ts.contains("readonly value: string"));
    assert!(ts.contains("Hop<\"target\", \"opt\""));
    // Select-out never changes insert requirements.
    assert!(ts.contains("  active: In<boolean>;"));
}

#[cfg(feature = "query-defaults")]
#[test]
fn a_many_to_many_path_applies_the_default_filter_of_the_join_model() {
    use orm_engine::{db, plan::plan_prefetch_only};
    let id = json!({"name":"id","column":"id","type":"big_int","primary_key":true});
    let raw = json!({"models":[
        {"name":"Post","table":"posts","fields":[id],
         "relations":[{"name":"tags","kind":"many","target":"Tag","from":"id","to":"id","through":{"model":"PostTag","source":"post_id","target":"tag_id"}}]},
        {"name":"Tag","table":"tags","fields":[id, {"name":"name","column":"name","type":"text"}]},
        {"name":"PostTag","table":"post_tags","fields":[id,
            {"name":"post_id","column":"post_id","type":"big_int"}, {"name":"tag_id","column":"tag_id","type":"big_int"},
            {"name":"live","column":"live","type":"bool"}]}
    ],"behavior":{"schema_contract":1,"query_defaults":[{"model":"PostTag","filter":{"t":"col","path":[],"name":"live"}}]}});
    let schema = Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap();
    let target = Target::new(Dialect::Postgres);
    let filter = json!({"t":"cmp","op":"eq","l":{"t":"col","path":["tags"],"name":"name"},"r":{"t":"text","value":"b"}});
    for (without_defaults, expected) in [(false, true), (true, false)] {
        let op: Operation = serde_json::from_value(json!({"op":"select","model":"Post","filters":[filter],"without_defaults":without_defaults,
            "prefetch":[{"relation":"tags","model":"Tag"}]})).unwrap();
        let Operation::Select(q) = &op else { panic!("select") };
        let Plan::Select(plan) = Planner::plan(&schema, target, &op, &NoParams).unwrap() else { panic!("select") };
        let filtered = db::build(Dialect::Postgres, &plan.stmt).0;
        assert_eq!(filtered.contains(r#""live""#), expected, "{filtered}");
        let prefetch = plan_prefetch_only(&schema, target, q, &NoParams).unwrap();
        let sql = db::build(Dialect::Postgres, &prefetch[0].statement(vec![vec![1.into()]])).0;
        assert_eq!(sql.contains(r#""live""#), expected, "{sql}");
    }
}

#[cfg(feature = "query-defaults")]
mod read_flags {
    use super::*;
    use orm_core::ir::Select;

    fn schema() -> Schema {
        let raw = json!({"models":[
            {"name":"Owner","table":"owner","fields":[
                {"name":"id","column":"id","type":"int","primary_key":true},
                {"name":"active","column":"active","type":"bool"}]},
            {"name":"Plain","table":"plain","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]},
            {"name":"Child","table":"child","fields":[
                {"name":"id","column":"id","type":"int","primary_key":true},
                {"name":"owner_id","column":"owner_id","type":"int"},
                {"name":"plain_id","column":"plain_id","type":"int"}],
             "relations":[
                {"name":"owner","kind":"one","target":"Owner","from":"owner_id","to":"id","foreign_key":true},
                {"name":"plain","kind":"one","target":"Plain","from":"plain_id","to":"id","foreign_key":true}]}],
            "behavior":{"schema_contract":1,"query_defaults":[
                {"model":"Owner","filter":{"t":"col","path":[],"name":"active"}},
                {"model":"Child","order":[{"field":"id","desc":true}]}]}});
        Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap()
    }

    fn select(op: serde_json::Value) -> Select {
        let Operation::Select(q) = serde_json::from_value(op).unwrap() else { panic!("select") };
        q
    }

    fn plan(schema: &Schema, op: serde_json::Value) -> orm_engine::plan::SelectPlan {
        let operation = serde_json::from_value(op).unwrap();
        let Plan::Select(plan) = Planner::plan(schema, Target::new(Dialect::Postgres), &operation, &NoParams).unwrap() else { panic!("select") };
        plan
    }

    #[test]
    fn join_and_root_flags_follow_filter_and_bypass() {
        let schema = schema();
        let joined = |bypass: bool| plan(&schema, json!({"op":"select","model":"Child","select_related":[["owner"],["plain"]],"without_defaults":bypass}));
        let Output::Instances { joins, .. } = joined(false).output else { panic!("instances") };
        assert_eq!(joins.iter().map(|j| (j.attr.as_str(), j.filtered)).collect::<Vec<_>>(), [("owner", true), ("plain", false)]);
        let Output::Instances { joins, .. } = joined(true).output else { panic!("instances") };
        assert!(joins.iter().all(|j| !j.filtered));
        assert!(!joined(false).filtered, "Child has an order but no filter");
        assert!(plan(&schema, json!({"op":"select","model":"Owner"})).filtered);
        assert!(!plan(&schema, json!({"op":"select","model":"Owner","without_defaults":true})).filtered);
    }

    #[test]
    fn prepared_flags_and_effective_order() {
        let schema = schema();
        let flags = |name: &str| schema.models.iter().find(|m| m.ir.name == name).unwrap().read_flags.clone();
        assert!(flags("Owner").filtered && !flags("Child").filtered && !flags("Plain").filtered);
        let order = |op| orm_engine::plan::effective_order(&schema, &select(op)).unwrap();
        assert_eq!(order(json!({"op":"select","model":"Child"})).len(), 1);
        assert!(order(json!({"op":"select","model":"Child","without_defaults":true})).is_empty());
        let explicit = json!({"op":"select","model":"Child","order":[{"expr":{"t":"col","path":[],"name":"owner_id"},"desc":false}]});
        assert_eq!(order(explicit.clone()).len(), 1);
        assert!(!order(explicit)[0].desc);
    }
}

mod unset_when_absent {
    use super::*;
    use orm_engine::plan::plan_prefetch_only;

    // `Child.owner` is a to-one that holds the key; `Owner.profile` is a has-one; `Owner.children` is a has-many.
    fn schema(filter_owner: bool) -> Schema {
        let id = json!({"name":"id","column":"id","type":"int","primary_key":true});
        let mut raw = json!({"models":[
            {"name":"Owner","table":"owner","fields":[id, {"name":"active","column":"active","type":"bool"}],
             "relations":[
                {"name":"profile","kind":"one","target":"Child","from":"id","to":"owner_id"},
                {"name":"children","kind":"many","target":"Child","from":"id","to":"owner_id"}]},
            {"name":"Child","table":"child","fields":[id, {"name":"owner_id","column":"owner_id","type":"int"}],
             "relations":[{"name":"owner","kind":"one","target":"Owner","from":"owner_id","to":"id","foreign_key":true}]}]});
        if filter_owner {
            raw["behavior"] = json!({"schema_contract":1,"query_defaults":[{"model":"Owner","filter":{"t":"col","path":[],"name":"active"}}]});
        }
        Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap()
    }

    fn select(op: serde_json::Value) -> orm_core::ir::Select {
        let Operation::Select(q) = serde_json::from_value(op).unwrap() else { panic!("select") };
        q
    }

    fn prefetch(schema: &Schema, model: &str, prefetch: serde_json::Value, bypass: bool) -> Vec<(bool, bool)> {
        let q = select(json!({"op":"select","model":model,"without_defaults":bypass,"prefetch":[prefetch]}));
        plan_prefetch_only(schema, Target::new(Dialect::Postgres), &q, &NoParams).unwrap().iter().map(|p| (p.filtered, p.unset_when_absent)).collect()
    }

    #[test]
    fn one_row_offset_uses_a_per_parent_window() {
        use orm_engine::db;
        let s = schema(false);
        for dialect in [Dialect::Postgres, Dialect::Sqlite] {
            for desc in [false, true] {
                let q = select(json!({"op":"select","model":"Owner","prefetch":[{
                    "relation":"children","model":"Child","attr":"chosen","one":true,
                    "limit":1,"offset":1,"order":[{"expr":{"t":"col","path":[],"name":"id"},"desc":desc}]
                }]}));
                let plans = plan_prefetch_only(&s, Target::new(dialect), &q, &NoParams).unwrap();
                assert!(!plans[0].many);
                let (sql, values) = db::build(dialect, &plans[0].statement(vec![vec![1.into()], vec![2.into()]]));
                assert_eq!(&values[values.len() - 2..], &[sea_query::Value::BigUnsigned(Some(1)), sea_query::Value::BigUnsigned(Some(2))]);
                assert!(sql.contains("ROW_NUMBER() OVER (PARTITION BY"), "{sql}");
                assert!(sql.contains(if desc { " DESC" } else { " ASC" }), "{sql}");
                assert!(sql.contains("\"p\".\"_rn\" >") && sql.contains("\"p\".\"_rn\" <="), "{sql}");
            }
        }
        for item in [
            json!({"relation":"children","model":"Child","one":true,"limit":1}),
            json!({"relation":"profile","model":"Child","one":true,"attr":"chosen","limit":1}),
            json!({"relation":"children","model":"Child","one":true,"attr":"chosen","limit":2}),
        ] {
            let q = select(json!({"op":"select","model":"Owner","prefetch":[item]}));
            assert!(plan_prefetch_only(&s, Target::new(Dialect::Sqlite), &q, &NoParams).is_err());
        }
    }

    fn join(schema: &Schema, bypass: bool) -> Vec<bool> {
        let operation: Operation = serde_json::from_value(json!({"op":"select","model":"Child","select_related":[["owner"]],"without_defaults":bypass})).unwrap();
        let Plan::Select(plan) = Planner::plan(schema, Target::new(Dialect::Postgres), &operation, &NoParams).unwrap() else { panic!("select") };
        let Output::Instances { joins, .. } = plan.output else { panic!("instances") };
        joins.iter().map(|j| j.unset_when_absent).collect()
    }

    #[test]
    fn prefetch_flag_follows_direction_label_and_cardinality() {
        let s = schema(false);
        assert_eq!(prefetch(&s, "Child", json!({"relation":"owner","model":"Owner"}), false), [(false, true)], "belongs-to");
        assert_eq!(prefetch(&s, "Child", json!({"relation":"owner","model":"Owner","attr":"label"}), false), [(false, false)], "label");
        assert_eq!(prefetch(&s, "Owner", json!({"relation":"profile","model":"Child"}), false), [(false, false)], "has-one");
        assert_eq!(prefetch(&s, "Owner", json!({"relation":"children","model":"Child"}), false), [(false, false)], "has-many");
        assert_eq!(prefetch(&s, "Owner", json!({"relation":"children","model":"Child","attr":"first","one":true}), false), [(false, false)], "one=true");
        assert_eq!(join(&s, false), [true]);
    }

    #[cfg(feature = "query-defaults")]
    #[test]
    fn filtered_targets_never_unset_unless_bypassed() {
        let s = schema(true);
        assert_eq!(prefetch(&s, "Child", json!({"relation":"owner","model":"Owner"}), false), [(true, false)], "filtered");
        assert_eq!(prefetch(&s, "Child", json!({"relation":"owner","model":"Owner"}), true), [(false, true)], "bypassed");
        assert_eq!(join(&s, false), [false]);
        assert_eq!(join(&s, true), [true]);
    }

    #[cfg(feature = "query-defaults")]
    #[test]
    fn binding_json_wrappers_have_one_shape() {
        let s = schema(true);
        let flags = orm_engine::plan::read_flags_json(&s);
        assert_eq!(flags["Owner"]["filtered"], json!(true));
        assert_eq!(flags["Child"]["filtered"], json!(false));
        assert!(flags["Owner"].get("fields").is_some() && flags["Owner"].get("related").is_some());
        let order = orm_engine::plan::effective_order_json(&s, r#"{"model":"Child","order":[{"expr":{"t":"col","path":[],"name":"owner_id"},"desc":true}]}"#).unwrap();
        assert_eq!(order, json!([{"field":"owner_id","desc":true,"nulls":null}]));
    }
}

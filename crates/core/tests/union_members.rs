//! The validation of a union relation (schema contract 5.9): distinct, conditioned to-one members.
use monorm_core::ir::{RelKind, RelationCondition, RelationIr, SchemaIr};
use monorm_core::dsl;
use serde_json::json;

const SOURCE: &str = r#"
datasource db {
  provider = "sqlite"
}

model Post {
  id Int @id
}

model Photo {
  id Int @id
}

model Tag {
  id          Int @id
  target_type Int?
  target_id   Int?
}
"#;

fn condition(field: &str, value: i64) -> RelationCondition {
    serde_json::from_value(json!({"side": "source", "field": field, "value": value})).unwrap()
}

fn member(target: &str, value: i64) -> RelationIr {
    let mut member = RelationIr::one(&format!("target#{target}"), target, "target_id", "id");
    member.conditions.push(condition("target_type", value));
    member
}

/// `Tag.target` with the members `edit` leaves.
fn schema_with(edit: impl FnOnce(&mut RelationIr)) -> Result<(), String> {
    let mut ir: SchemaIr = dsl::compile(SOURCE, None).unwrap();
    let mut union = RelationIr::one("target", "Post", "target_id", "id");
    union.members = vec![member("Post", 1), member("Photo", 2)];
    edit(&mut union);
    ir.models.iter_mut().find(|m| m.name == "Tag").unwrap().relations.push(union);
    dsl::check(ir).map(|_| ())
}

const INVALID: &str = "relation Tag.target needs distinct, conditioned to-one members";

#[test]
fn a_union_of_distinct_conditioned_members_is_valid() {
    schema_with(|_| {}).unwrap();
}

#[test]
fn a_union_with_a_bad_member_is_refused() {
    let cases: Vec<(&str, Box<dyn FnOnce(&mut RelationIr)>)> = vec![
        ("a to-many union", Box::new(|u| u.kind = RelKind::Many)),
        ("a condition on the union itself", Box::new(|u| u.conditions.push(condition("target_type", 1)))),
        ("a member that is a to-many", Box::new(|u| u.members[0].kind = RelKind::Many)),
        ("a member with the wrong name", Box::new(|u| u.members[0].name = "target#Other".into())),
        ("a member with no source condition", Box::new(|u| u.members[0].conditions.clear())),
        ("a repeated type value", Box::new(|u| u.members[1].conditions[0].value = json!(1))),
        ("a different type field on one member", Box::new(|u| u.members[1].conditions[0].field = "target_id".into())),
        ("a nested union", Box::new(|u| u.members[0].members.push(member("Post", 3)))),
    ];
    for (what, edit) in cases {
        let error = schema_with(edit).err().unwrap_or_else(|| panic!("{what} was accepted"));
        assert_eq!(error, INVALID, "{what}");
    }
}

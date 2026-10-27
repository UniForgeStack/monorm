//! `HookPlan::finish` fills the lookup data of a plan and drops a plan that has no hook.
use orm_contracts::abi::{Arg, Value};
use orm_core::hooks::{HookPlan, HookRef, RecordHook, StaticHook};

fn field(_: &mut Value) -> Result<(), String> { Ok(()) }

fn record(_: &[Arg]) -> Result<(), String> { Ok(()) }

fn hook() -> HookRef { HookRef::Static(StaticHook::Field(field)) }

fn rule(positions: &[usize]) -> RecordHook { RecordHook { positions: positions.to_vec(), hook: HookRef::Static(StaticHook::Record(record)) } }

#[test]
fn a_plan_without_a_hook_is_none() {
    assert!(HookPlan::default().finish().is_none());
}

#[test]
fn watched_and_required_are_sorted_without_duplicates() {
    let plan = HookPlan {
        transforms: vec![(3, hook()), (1, hook())],
        validators: vec![(3, hook())],
        records: vec![rule(&[5, 1])],
        ..Default::default()
    }
    .finish()
    .expect("a plan");
    assert_eq!(plan.watched, [1, 3]);
    assert_eq!(plan.required, [1, 3, 5]);
    assert!(plan.changes_set());
    assert!(plan.no_upsert);
}

#[test]
fn a_record_dependency_is_required_but_not_watched() {
    let plan = HookPlan { validators: vec![(1, hook())], records: vec![rule(&[4])], ..Default::default() }.finish().expect("a plan");
    assert!(plan.watches(1));
    assert!(!plan.watches(4));
    assert_eq!(plan.required, [1, 4]);
}

#[test]
fn a_plan_of_only_a_hook_4_is_kept_and_allows_an_upsert() {
    let plan = HookPlan { assign: vec![(2, hook())], ..Default::default() }.finish().expect("a plan");
    assert!(!plan.changes_set());
    assert!(!plan.no_upsert);
    assert!(plan.watched.is_empty() && plan.required.is_empty());
    assert!(plan.assigner(2).is_some());
    assert!(plan.assigner(1).is_none());
}

#[test]
fn a_plan_of_static_functions_does_not_read_the_request_context() {
    let plan = HookPlan { transforms: vec![(1, hook())], assign: vec![(2, hook())], records: vec![rule(&[1, 2])], ..Default::default() }.finish().expect("a plan");
    assert!(!plan.reads_context());
}

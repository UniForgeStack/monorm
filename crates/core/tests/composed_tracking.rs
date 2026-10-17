//! Composed writes cannot maintain tracking roles on any owner in their chain.
#[cfg(all(feature = "model-composition", feature = "updated-at", feature = "soft-delete", feature = "optimistic-locking"))]
#[test]
fn composed_tracking_roles_are_rejected_through_the_root() {
    for (field, role) in [
        ("changed DateTime @default(now()) @timestamps.updated_at", "@timestamps.updated_at"),
        ("deleted DateTime? @soft_delete.deleted_at", "@soft_delete.deleted_at"),
        ("version Int @default(0) @locking.version", "@locking.version"),
    ] {
        for owner in ["Person", "Employee", "Manager"] {
            let declared = |name| if owner == name { field } else { "" };
            let source = format!(r#"
model Person {{
 id Int @id
 {}
}}
model Employee {{
 salary Int
 {}
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
}}
model Manager {{
 level Int
 {}
 @@composition.model(parent: "Employee", parentRef: "employee", childRef: "manager")
}}
"#, declared("Person"), declared("Employee"), declared("Manager"));
            let mut ir = orm_core::dsl::compile(&source, None).unwrap();
            // Validate the leaf first so a role on the root must be found through
            // two owner links rather than through the immediate child alone.
            ir.models.sort_by_key(|m| m.name != "Manager");
            let error = orm_core::dsl::check(ir).err().expect("tracking role must be rejected").to_string();
            assert!(error.contains("Manager") && error.contains(owner) && error.contains(role) && error.contains(field.split_whitespace().next().unwrap()), "{error}");
        }
    }
}

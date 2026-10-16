//! Shared validation for schema-file and serialized-IR partition declarations.
use std::collections::HashSet;
use crate::ir::{ConstraintIr, ModelIr, SchemaIr};

pub fn validate(ir: &SchemaIr) -> Result<(), String> {
    if ir.partitions.is_empty() && ir.models.iter().all(|m| m.partition.is_none()) { return Ok(()); }
    let models: Vec<&ModelIr> = ir.models.iter().chain(ir.partitions.iter().map(|p| &p.objects)).collect();
    let mut names = HashSet::new();
    let mut tables = HashSet::new();
    for m in &models {
        if !names.insert(&m.name) { return Err(format!("{}: duplicate partition/model name", m.name)); }
        // Ordinary proxies share their root's storage and do not declare a physical parent.
        if ir.behavior.proxy_models.iter().any(|p| p.model == m.name) { continue; }
        if !tables.insert(&m.table) { return Err(format!("{}: duplicate partition/model table {}", m.name, m.table)); }
        let Some(key) = &m.partition else { continue };
        if !ir.dialect.is_postgres() { return Err(format!("{}: @@partition requires PostgreSQL", m.name)); }
        if !["list", "range", "hash"].contains(&key.strategy.as_str()) || key.keys.is_empty() || (key.strategy == "list" && key.keys.len() != 1) {
            return Err(format!("{}: invalid partition strategy or key count (list takes one key)", m.name));
        }
        let mut columns = vec![];
        for k in &key.keys {
            if k.desc || k.nulls.is_some() || k.opclass.is_some() || k.collation.is_some() || (k.field.is_some() == k.expr.is_some()) { return Err(format!("{}: partition key takes fields or sql expressions without index modifiers", m.name)); }
            if let Some(f) = &k.field {
                if !m.fields.iter().any(|x| &x.name == f) { return Err(format!("{}.{f}: unknown partition key field", m.name)); }
                if columns.contains(&f.as_str()) { return Err(format!("{}.{f}: duplicate partition key field", m.name)); }
                columns.push(f.as_str());
            }
        }
        let check = |fields: Vec<&str>| -> Result<(), String> {
            if key.keys.iter().any(|k| k.expr.is_some()) || columns.iter().any(|c| !fields.contains(c)) {
                return Err(format!("{}.{}: primary/unique rule must contain every partition key column; declare the rule on each partition, where it is unique only inside that partition", m.name, fields.join(",")));
            }
            Ok(())
        };
        if !m.key_fields().is_empty() { check(m.key_fields())?; }
        for f in m.fields.iter().filter(|f| f.unique) { check(vec![&f.name])?; }
        for c in &m.constraints { if let ConstraintIr::Unique { fields, .. } = c { check(fields.iter().map(String::as_str).collect())?; } }
        for i in m.indexes.iter().filter(|i| i.unique) { check(i.columns.iter().filter_map(|k| k.field.as_deref()).collect())?; }
        // Descendant partition keys are also required in every ancestor's unique key.
        let mut parent = ir.partitions.iter().find(|p| p.objects.name == m.name);
        let mut seen = HashSet::new();
        while let Some(p) = parent {
            if !seen.insert(&p.of) { return Err(format!("{}: cyclic partition parent", m.name)); }
            let ancestor = models.iter().find(|a| a.name == p.of).ok_or_else(|| format!("{}: unknown parent {}", m.name, p.of))?;
            if !ancestor.key_fields().is_empty() { check(ancestor.key_fields())?; }
            for f in ancestor.fields.iter().filter(|f| f.unique) { check(vec![&f.name])?; }
            for c in &ancestor.constraints { if let ConstraintIr::Unique { fields, .. } = c { check(fields.iter().map(String::as_str).collect())?; } }
            for i in ancestor.indexes.iter().filter(|i| i.unique) { check(i.columns.iter().filter_map(|k| k.field.as_deref()).collect())?; }
            parent = ir.partitions.iter().find(|p| p.objects.name == ancestor.name);
        }
        let children: Vec<_> = ir.partitions.iter().filter(|p| p.of == m.name).collect();
        if children.is_empty() { return Err(format!("{}: @@partition has no partition", m.name)); }
        let mut defaults = 0;
        let mut values = HashSet::new();
        for child in children {
            if child.bound == "DEFAULT" { defaults += 1; continue; }
            let prefix = match key.strategy.as_str() { "list" => "FOR VALUES IN (", "range" => "FOR VALUES FROM (", _ => "FOR VALUES WITH (" };
            if !child.bound.starts_with(prefix) { return Err(format!("{}: bound does not match {} strategy", child.objects.name, key.strategy)); }
            if key.strategy == "list" {
                for v in split_sql(child.bound.strip_prefix(prefix).unwrap().trim_end_matches(')')) {
                    if !values.insert(v.to_owned()) { return Err(format!("{}: overlapping list value {v}", child.objects.name)); }
                    if let Some(e) = key.keys[0].field.as_ref().and_then(|n| m.fields.iter().find(|f| &f.name == n)).and_then(|f| f.enum_name.as_ref()).and_then(|n| ir.enums.iter().find(|e| &e.name == n)) {
                        if !e.values.iter().any(|m| match &m.value { serde_json::Value::String(s) => crate::migrate::model::quote_literal(s) == v, v2 => serde_json::from_str::<serde_json::Value>(v).ok().as_ref() == Some(v2) }) { return Err(format!("{}: {v} is not a member of enum {}", child.objects.name, e.name)); }
                    }
                }
            }
        }
        if defaults > 1 { return Err(format!("{}: second default partition", m.name)); }
        if defaults > 0 && key.strategy == "hash" { return Err(format!("{}: hash partition cannot have a default", m.name)); }
    }
    for p in &ir.partitions {
        let parent = models.iter().find(|m| m.name == p.of).ok_or_else(|| format!("{}: unknown parent {}", p.objects.name, p.of))?;
        if ir.behavior.proxy_models.iter().any(|proxy| proxy.model == p.of) { return Err(format!("{}: partition blocks require a physical parent, not proxy {}", p.objects.name, p.of)); }
        if parent.partition.is_none() { return Err(format!("{}: {} has no @@partition", p.objects.name, p.of)); }
    }
    Ok(())
}

/// Split PostgreSQL expression lists while preserving quoted commas and nested calls.
pub fn split_sql(s: &str) -> Vec<&str> {
    let (mut depth, mut quote, mut start) = (0, None, 0);
    let mut out = vec![];
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if let Some(q) = quote {
            if c == q { if chars.peek().is_some_and(|(_, c)| *c == q) { chars.next(); } else { quote = None; } }
        } else { match c {
            '\'' | '"' => quote = Some(c), '(' => depth += 1, ')' => depth -= 1,
            ',' if depth == 0 => { out.push(s[start..i].trim()); start = i + 1; }, _ => {}
        }}
    }
    out.push(s[start..].trim());
    out
}

/// Missing enum coverage is advisory: PostgreSQL rejects those inserts until a partition is added.
pub fn warnings(ir: &SchemaIr) -> Vec<String> {
    let mut out = vec![];
    for m in ir.models.iter().chain(ir.partitions.iter().map(|p| &p.objects)) {
        if ir.behavior.proxy_models.iter().any(|p| p.model == m.name) { continue; }
        let Some(key) = &m.partition else { continue };
        if key.strategy != "list" || key.keys.len() != 1 { continue; }
        let parts: Vec<_> = ir.partitions.iter().filter(|p| p.of == m.name).collect();
        if parts.iter().any(|p| p.bound == "DEFAULT") { continue; }
        let Some(e) = key.keys[0].field.as_ref().and_then(|n| m.fields.iter().find(|f| &f.name == n)).and_then(|f| f.enum_name.as_ref()).and_then(|n| ir.enums.iter().find(|e| &e.name == n)) else { continue };
        let values: HashSet<_> = parts.iter().flat_map(|p| p.bound.strip_prefix("FOR VALUES IN (").and_then(|s| s.strip_suffix(')')).map(split_sql).unwrap_or_default()).collect();
        for member in &e.values {
            let value = match &member.value { serde_json::Value::String(s) => crate::migrate::model::quote_literal(s), v => v.to_string() };
            if !values.contains(value.as_str()) { out.push(format!("{}.{}: enum member {} has no partition; inserts with that value fail", m.name, key.keys[0].field.as_deref().unwrap_or(""), member.name)); }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::{dsl, migrate};
    const LIST: &str = r#"
model Reward {
 id Int @default(autoincrement())
 kind String @db.Text
 amount Int? @check("amount < 100")
 @@id([id, kind])
 @@partition(list: [kind])
 @@index([amount])
}
partition reward_a {
of = Reward
values = ["a"]
@@check("amount IS NOT NULL AND amount > 0")
@@unique([amount])
}
partition reward_other {
of = Reward
default = true
}
"#;
    fn schema(s: &str) -> crate::schema::Schema { dsl::check(dsl::compile(s, None).unwrap()).unwrap().1 }
    #[test]
    fn list_objects_and_pull_roundtrip() {
        let s = schema(LIST);
        let plan = migrate::plan(&s, &Default::default()).unwrap();
        assert!(plan.up[0].sql.ends_with("PARTITION BY LIST (\"kind\")"));
        assert!(plan.up[1].sql.contains("PARTITION OF \"reward\""));
        assert!(plan.up[1].sql.contains("UNIQUE (\"amount\")"));
        assert!(plan.up[1].sql.contains("amount IS NOT NULL"));
        assert_eq!(plan.snapshot.tables[1].checks.len(), 1); // inherited field check stays on the parent
        assert!(migrate::plan(&s, &plan.snapshot).unwrap().up.is_empty());
        let pulled = migrate::pull::pull(&plan.snapshot, &[]);
        assert!(pulled.gaps.is_empty(), "{:?}", pulled.gaps);
        let roundtrip = migrate::plan(&schema(&pulled.schema), &plan.snapshot).unwrap();
        assert!(roundtrip.up.is_empty(), "{:?}\n{}", roundtrip.up, pulled.schema);
    }
    #[test]
    fn migration_add_remove_rebound_rename_and_strategy_rebuild() {
        let old = migrate::snapshot(&schema(LIST)).unwrap();
        let rebound = migrate::plan(&schema(&LIST.replace("[\"a\"]", "[\"b\"]")), &old).unwrap();
        assert!(rebound.up[0].sql.contains("DETACH PARTITION"));
        assert!(rebound.up[1].sql.ends_with("FOR VALUES IN ('b')"));
        let renamed = LIST.replace("partition reward_a {", "partition reward_b {\n@@renamed_from(\"reward_a\")");
        let renamed = migrate::plan(&schema(&renamed), &old).unwrap();
        assert_eq!(renamed.up.len(), 3); // table and auto-named local constraints
        assert!(renamed.up.iter().any(|s| s.sql == "ALTER TABLE \"reward_a\" RENAME TO \"reward_b\""));
        let siblings = LIST.replace("default = true", "values = [\"b\"]");
        let swapped = siblings.replace("[\"a\"]", "[\"temporary\"]").replace("[\"b\"]", "[\"a\"]").replace("[\"temporary\"]", "[\"b\"]");
        let swap = migrate::plan(&schema(&swapped), &migrate::snapshot(&schema(&siblings)).unwrap()).unwrap();
        assert_eq!(swap.up.len(), 4);
        assert!(swap.up[..2].iter().all(|s| s.sql.contains("DETACH PARTITION")));
        assert!(swap.up[2..].iter().all(|s| s.sql.contains("ATTACH PARTITION")));
        let remove = LIST.split("partition reward_a {").next().unwrap().to_owned() + "partition reward_other { of = Reward default = true }";
        let removed = migrate::plan(&schema(&remove), &old).unwrap();
        assert!(removed.up.iter().any(|s| s.sql.contains("DETACH PARTITION")));
        assert!(removed.up.iter().any(|s| s.warning.as_deref().is_some_and(|w| w.contains("all its rows"))));
        let added = migrate::plan(&schema(LIST), &migrate::snapshot(&schema(&remove)).unwrap()).unwrap();
        assert!(added.up.iter().any(|s| s.sql.starts_with("CREATE TABLE \"reward_a\" PARTITION OF")));
        let hash = "model Reward {\n id Int\n kind String\n amount Int?\n @@id([id, kind])\n @@partition(hash: [kind])\n}\npartition reward_h { of = Reward modulus = 2 remainder = 0 }";
        let column_rename = LIST.replace("kind", "category").replace("category String @db.Text", "category String @db.Text @renamed_from(\"kind\")");
        let renamed_column = migrate::plan(&schema(&column_rename), &old).unwrap();
        assert_eq!(renamed_column.up.len(), 1);
        assert!(renamed_column.up[0].sql.contains("RENAME COLUMN \"kind\" TO \"category\""));
        let renamed_hash = hash.replace("@@partition(hash: [kind])", "@@partition(hash: [kind])\n @@map(\"reward_new\")\n @@renamed_from(\"reward\")");
        let renamed_rebuild = migrate::plan(&schema(&renamed_hash), &old).unwrap();
        assert!(renamed_rebuild.up.iter().any(|s| s.sql == "DROP TABLE \"reward\""));
        assert!(renamed_rebuild.up.iter().any(|s| s.sql.starts_with("CREATE TABLE \"reward_new\"")));
        let rebuilt = migrate::plan(&schema(hash), &old).unwrap();
        assert!(rebuilt.up.iter().any(|s| s.warning.is_some() && s.sql == "DROP TABLE \"reward\""));
        assert!(rebuilt.up.iter().any(|s| s.sql.ends_with("PARTITION BY HASH (\"kind\")")));
    }
    #[test]
    fn invalid_partition_definitions_fail_early() {
        for (source, message) in [
            (LIST.replace("@@id([id, kind])", "").replace("id Int @default", "id Int @id @default"), "every partition key"),
            (LIST.replace("@@index([amount])", "@@unique([amount])"), "unique only inside"),
            (LIST.replace("@@index([amount])", "@@index([amount], unique: true)"), "every partition key"),
            (LIST.replace("@@index([amount])", "@@index([sql(\"lower(kind)\")], unique: true)"), "every partition key"),
            (LIST.replace("@@partition(list: [kind])", "@@partition(list: [missing])"), "unknown partition key"),
            (LIST.replace("@@partition(list: [kind])", "@@partition(list: [kind, id])"), "list takes one"),
            (LIST.replace("values = [\"a\"]", "from = [1]\nto = [2]"), "does not match"),
            (LIST.replace("default = true", "values = [\"a\"]"), "overlapping"),
            (LIST.replace("values = [\"a\"]", "default = true"), "second default"),
            (LIST.replace("@@partition(list: [kind])", ""), "has no @@partition"),
            (LIST.split("partition reward_a").next().unwrap().to_owned(), "no partition"),
            (format!("datasource db {{ provider = \"sqlite\" }}\n{LIST}"), "requires PostgreSQL"),
            (LIST.replace("of = Reward", "of = Unknown"), "unknown or cyclic"),
        ] {
            let error = dsl::compile(&source, None).unwrap_err();
            assert!(error.contains(message), "wanted {message}: {error}");
        }
    }
    #[test]
    fn enum_members_and_coverage() {
        let src = LIST.replace("kind String @db.Text", "kind Kind").replace("[\"a\"]", "[A]").replace("partition reward_other {\nof = Reward\ndefault = true\n}", "") + "\nenum Kind {\n A @map(\"a\")\n B @map(\"b\")\n }";
        let ir = dsl::compile(&src, None).unwrap();
        assert!(ir.partitions[0].bound.contains("'a'"));
        assert_eq!(super::warnings(&ir).len(), 1);
        assert!(dsl::compile(&src.replace("values = [A]", "values = [C]"), None).unwrap_err().contains("not a member"));
        assert!(dsl::compile(&src.replace("values = [A]", "values = [\"c\"]"), None).unwrap_err().contains("not a member"));
        let pulled = migrate::pull::pull(&migrate::snapshot(&schema(&src)).unwrap(), &[]);
        schema(&pulled.schema);
    }
    #[cfg(feature = "proxy-models")]
    #[test]
    fn ordinary_proxies_share_a_partitioned_base_without_owning_a_table() {
        let source = format!("{LIST}\nmodel RewardView {{\n @@proxy.of(Reward)\n}}\n");
        let s = schema(&source);
        let snapshot = migrate::snapshot(&s).unwrap();
        assert_eq!(snapshot.tables.len(), 3);
        assert_eq!(snapshot.tables.iter().filter(|t| t.partition_of.is_some()).count(), 2);
        assert!(migrate::plan(&s, &snapshot).unwrap().up.is_empty());
    }
    #[test]
    fn range_hash_and_subpartition_syntax() {
        let boolean = "model E {\n id Int\n flag Boolean\n @@id([id, flag])\n @@partition(list: [flag])\n}\npartition e_true { of = E values = [true] }\npartition e_other { of = E default = true }";
        let snapshot = migrate::snapshot(&schema(boolean)).unwrap();
        schema(&migrate::pull::pull(&snapshot, &[]).schema);
        for bound in ["from = [MINVALUE] to = [10]", "from = [10] to = [MAXVALUE]"] {
            let src = format!("model E {{\n id Int @id\n @@partition(range: [id])\n}}\npartition e_p {{ of = E {bound} }}");
            let snapshot = migrate::snapshot(&schema(&src)).unwrap();
            schema(&migrate::pull::pull(&snapshot, &[]).schema);
        }
        let src = "model E {\n id Int @id\n @@partition(range: [id])\n}\npartition e_p { of = E from = [1] to = [10] @@partition(hash: [id]) }\npartition e_h { of = e_p modulus = 2 remainder = 0 @@index([id]) }";
        let snapshot = migrate::snapshot(&schema(src)).unwrap();
        let sql = migrate::create_statements(&snapshot);
        assert!(sql[1].ends_with("PARTITION BY HASH (\"id\")"));
        assert!(sql[2].contains("PARTITION OF \"e_p\""));
        schema(&migrate::pull::pull(&snapshot, &[]).schema);
        let renamed = src.replace("id", "number").replace("@number", "@id").replace("number Int @id", "number Int @id @renamed_from(\"id\")");
        let renamed = migrate::plan(&schema(&renamed), &snapshot).unwrap();
        assert!(renamed.up.iter().all(|step| !step.sql.starts_with("DROP TABLE")));
    }
}

//! The startup check of the database catalog: a prefix search on a btree that PostgreSQL cannot use for `LIKE 'x%'`.

use monorm_core::dialect::Dialect;
use monorm_engine::db::Executor;

use crate::site::Mode;
use crate::site::Site;
use crate::sql::Table;

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// One warning per prefix search field whose only btree is a plain one while the column collation is not C.
/// Such a btree cannot serve `LIKE 'x%'`: it needs `text_pattern_ops`. One catalog query covers every field.
pub async fn prefix_warnings(site: &Site, conn: &dyn Executor) -> Result<Vec<String>, String> {
    if conn.dialect() != Dialect::Postgres {
        return Ok(Vec::new());
    }
    let mut fields: Vec<(String, String, String, String)> = Vec::new();
    for resource in &site.resources {
        let model = site.schema.model(resource.model);
        let table = Table::of(site, &model.ir.name)?;
        for s in resource.search.iter().filter(|s| s.text && s.indexed && s.default == Mode::Prefix) {
            let column = table.column(&s.column.field)?.trim_matches('"').replace("\"\"", "\"");
            fields.push((resource.name.clone(), s.column.field.clone(), table.name.clone(), column));
        }
    }
    if fields.is_empty() {
        return Ok(Vec::new());
    }
    let pairs: Vec<String> = fields.iter().map(|(_, _, t, c)| format!("({}, {})", literal(t), literal(c))).collect();
    // `to_jsonb(d)` reads `datlocprovider` and `datlocale`, which older servers lack.
    let sql = format!(
        "SELECT c.relname, a.attname, \
         CASE WHEN a.attcollation = 0 THEN 'C' \
              WHEN a.attcollation = 100 THEN (SELECT CASE WHEN coalesce(to_jsonb(d) ->> 'datlocprovider', 'c') = 'c' THEN d.datcollate ELSE coalesce(to_jsonb(d) ->> 'datlocale', to_jsonb(d) ->> 'daticulocale') END FROM pg_database d WHERE d.datname = current_database()) \
              ELSE (SELECT collname FROM pg_collation WHERE oid = a.attcollation) END, \
         EXISTS (SELECT 1 FROM pg_index i JOIN pg_opclass o ON o.oid = i.indclass[0] \
                 WHERE i.indrelid = c.oid AND i.indkey[0] = a.attnum AND i.indpred IS NULL AND o.opcname LIKE '%\\_pattern\\_ops') \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace JOIN pg_attribute a ON a.attrelid = c.oid \
         WHERE n.nspname = current_schema() AND NOT a.attisdropped AND (c.relname, a.attname) IN (VALUES {})",
        pairs.join(", ")
    );
    let rows = conn.query_text(sql).await.map_err(|e| format!("the catalog check failed: {}", e.message.lines().next().unwrap_or_default()))?;
    let mut out = Vec::new();
    for row in rows {
        let cell = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        let (table, column, collation, pattern) = (cell(0), cell(1), cell(2), cell(3));
        if pattern == "t" || pattern == "true" || matches!(collation.as_str(), "C" | "POSIX") {
            continue;
        }
        if let Some((resource, field, _, _)) = fields.iter().find(|(_, _, t, c)| *t == table && *c == column) {
            out.push(format!(
                "{resource}: search field {field:?} is a prefix search on a plain btree, and the collation {collation:?} is not C: PostgreSQL cannot use the btree for LIKE 'x%'; add an index with text_pattern_ops (or varchar_pattern_ops), or use a C collation"
            ));
        }
    }
    Ok(out)
}

//! The startup check of the database catalog: a prefix search on a btree that PostgreSQL cannot use for `LIKE 'x%'`.

use monorm_core::dialect::Dialect;
use monorm_engine::db::Executor;

use crate::site::Mode;
use crate::site::Site;
use crate::sql::Table;

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The SQL text of the name of the collation with the OID `oid`.
/// OID 100 is `default`: the collation of the database, which `datcollate` names, or `datlocale` / `daticulocale` under a locale provider.
/// `to_jsonb(d)` reads `datlocprovider` and `datlocale`, which older servers lack.
fn collation_name(oid: &str) -> String {
    format!(
        "CASE WHEN {oid} = 0 THEN 'C' \
              WHEN {oid} = 100 THEN (SELECT CASE WHEN coalesce(to_jsonb(d) ->> 'datlocprovider', 'c') = 'c' THEN d.datcollate ELSE coalesce(to_jsonb(d) ->> 'datlocale', to_jsonb(d) ->> 'daticulocale') END FROM pg_database d WHERE d.datname = current_database()) \
              ELSE (SELECT collname FROM pg_collation WHERE oid = {oid}) END"
    )
}

fn is_c(collation: &str) -> bool {
    matches!(collation, "C" | "POSIX")
}

/// One warning per prefix search field whose btree cannot serve `LIKE 'x%'`: no `text_pattern_ops` index, and the collation of the index is not C.
/// The collation of an index is the one in `pg_index.indcollation`, which an explicit `COLLATE` can set apart from the column.
/// A field with no btree in the catalog falls back to the column collation. One catalog query covers every field.
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
    let sql = format!(
        "SELECT c.relname, a.attname, {column_collation}, \
         EXISTS (SELECT 1 FROM pg_index i JOIN pg_opclass o ON o.oid = i.indclass[0] \
                 WHERE i.indrelid = c.oid AND i.indkey[0] = a.attnum AND i.indpred IS NULL AND o.opcname LIKE '%\\_pattern\\_ops'), \
         (SELECT string_agg(DISTINCT {index_collation}, ',') FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid JOIN pg_am m ON m.oid = ic.relam \
          WHERE i.indrelid = c.oid AND i.indkey[0] = a.attnum AND i.indpred IS NULL AND m.amname = 'btree') \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace JOIN pg_attribute a ON a.attrelid = c.oid \
         WHERE n.nspname = current_schema() AND NOT a.attisdropped AND (c.relname, a.attname) IN (VALUES {pairs})",
        column_collation = collation_name("a.attcollation"),
        index_collation = collation_name("i.indcollation[0]"),
        pairs = pairs.join(", ")
    );
    let rows = conn.query_text(sql).await.map_err(|e| format!("the catalog check failed: {}", e.message.lines().next().unwrap_or_default()))?;
    let mut out = Vec::new();
    for row in rows {
        let cell = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        let (table, column, collation, pattern, indexes) = (cell(0), cell(1), cell(2), cell(3), cell(4));
        let collations: Vec<&str> = if indexes.is_empty() { vec![collation.as_str()] } else { indexes.split(',').collect() };
        if pattern == "t" || pattern == "true" || collations.iter().any(|c| is_c(c)) {
            continue;
        }
        let collation = collations.join(", ");
        if let Some((resource, field, _, _)) = fields.iter().find(|(_, _, t, c)| *t == table && *c == column) {
            out.push(format!(
                "{resource}: search field {field:?} is a prefix search on a plain btree, and the collation {collation:?} is not C: PostgreSQL cannot use the btree for LIKE 'x%'; add an index with text_pattern_ops (or varchar_pattern_ops), or use a C collation"
            ));
        }
    }
    Ok(out)
}

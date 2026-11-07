//! The startup check of the admin's own tables.

use monorm_core::schema::Schema;
use monorm_engine::db::Executor;

use crate::sql::quote;

/// The models of the `app:admin` schema.
pub const MODELS: [&str; 3] = ["AdminLogEntry", "UserPreference", "AdminSessionRevocation"];

/// Fails with a clear message when the schema lacks an admin model or the database lacks its table.
pub async fn check(schema: &Schema, conn: &dyn Executor) -> Result<(), String> {
    for name in MODELS {
        let Ok(idx) = schema.model_idx(name) else {
            return Err(format!("the schema has no model {name}: import the admin app in schema.prisma (`import \"app:monorm-admin\"` in Python, `import \"app:admin\"` in TypeScript), then run makemigrations and migrate"));
        };
        let model = schema.model(idx);
        let columns: Vec<String> = model.fields().iter().map(|f| quote(&f.column)).collect();
        let sql = format!("SELECT {} FROM {} WHERE 1 = 0", columns.join(", "), quote(model.table()));
        if let Err(e) = conn.query_text(sql).await {
            return Err(format!("the admin table {:?} is missing or out of date ({}): run makemigrations and migrate", model.table(), e.message.lines().next().unwrap_or_default()));
        }
    }
    Ok(())
}

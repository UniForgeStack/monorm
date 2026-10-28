//! Neutral write records. An extension fills them at prepare time; core resolves them to
//! field positions on the prepared model. No record names an extension or a role: `source`
//! only labels the extension that pushed it, for errors.
use serde::{Deserialize, Serialize};

/// A statement kind that a `WriteAssignment` applies to.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WriteKind {
    Update,
    /// The `DO UPDATE` arm of an upsert.
    Upsert,
    /// The update that a `DeleteRewrite` makes of a delete.
    DeleteRewrite,
}

/// The value the ORM assigns.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssignValue {
    /// The current time; the field is a DateTime.
    Now,
    /// The stored value plus one; the field is an Int or BigInt.
    Increment,
    /// The value that a library hook (point 4) returns. The `source` of the `WriteAssignment` owns the export,
    /// and the hook binding of the field names it; the hook reads the request context.
    Hook { export: String },
    /// A query IR expression (the JSON of `Expr`) over the row's own columns and `scope.<name>`. The model's
    /// `define()` parses it once; the statement puts it in the SET list. A column reads the row as it was before
    /// the update, as in SQL.
    Expr(serde_json::Value),
    /// A query IR expression that a library hook (point 6) returns. The hook reads the request context, and the
    /// engine calls it once for each update statement. `monorm generate` evaluates a hook that reads no context once
    /// and stores the result as `Expr`.
    ExprHook { export: String },
}

/// Whether the default `on_conflict(update=True)` column list takes the field.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpsertDefault {
    /// The default list leaves the field out; the assignment sets it.
    #[default]
    Drop,
    /// The default list keeps the field.
    Pick,
}

/// One column the ORM sets on writes.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WriteAssignment {
    pub source: String,
    pub model: String,
    /// The schema (snake_case) field name.
    pub field: String,
    pub on: Vec<WriteKind>,
    pub value: AssignValue,
    #[serde(default)]
    pub upsert: UpsertDefault,
}

/// A delete that becomes an update of `field` to the current time, unless the caller asks for a hard delete.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeleteRewrite {
    pub source: String,
    pub model: String,
    pub field: String,
    /// The query IR filter of the rows that the rewrite applies to, for example `{"t":"is_null",...}`.
    pub guard: serde_json::Value,
}

/// A predicate the bindings add to an instance update or delete: the loaded value of `field` must match.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InstanceGuard {
    pub source: String,
    pub model: String,
    pub field: String,
}

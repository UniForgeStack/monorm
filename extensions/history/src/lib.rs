//! `@@history.track`: row events from database triggers. Each tracker adds an event model and the
//! triggers that write one event per insert, update or delete of the tracked table. The records
//! that the runtimes read hold the built SQL, so no binding writes SQL of its own.
use monorm::{
    dialect::Dialect,
    extension::{AddedTrigger, ExtensionRecord},
    ir::{ColType, FieldIr, IndexColumnIr, IndexIr, ModelIr, OnDelete, RelationIr, SchemaIr, ScopeSettingsIr, TriggerEvent, TriggerIr, TriggerTiming},
    sql::{column_type, ident, literal, skip_guard},
    tracking::Tracked,
    writes::{AssignValue, PublishedScopeKey, WriteKind},
};
use serde::{Deserialize, Serialize};

const SOURCE: &str = "monorm.history";
const KIND: &str = "tracker";
const VERSIONS_KIND: &str = "versions";
const VERSIONS: &str = "history.versions";
const ATTRIBUTE: &str = "history.track";
/// Attributes of the namespace that the versions mode reads itself.
const OTHERS: &[&str] = &[VERSIONS];
/// The fields that the versions mode adds to a version model.
const VERSION_FIELDS: &[&str] = &["version_number", "snapshot", "content_hash", "effective_at", "change_type", "restored_from", "xact_id"];
/// The `change_type` values of a version.
const CHANGE_TYPES: &[&str] = &["create", "update", "delete", "restore"];
const TRACKED: Tracked = Tracked { attribute: ATTRIBUTE, field: "" };
const EVENT_FIELDS: &[&str] = &["id", "label", "data", "created_at", "xact_id"];
const KINDS: &[&str] = &["insert", "update", "delete"];
/// The label of the event that a restore writes.
const RESTORE_LABEL: &str = "restore";
/// The transaction-local setting that a restore sets to the event table: the triggers of that table then write `restore`.
const RESTORE_SETTING: &str = "monorm.history_restore";
/// `jsonb_build_object` takes at most 100 arguments.
const PAIRS: usize = 50;
/// `json_object` of SQLite takes at most 1000 arguments.
const SQLITE_FIELDS: usize = 500;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct KeyColumn { field: String, column: String, event_field: String, event_column: String }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct TrackedField { name: String, column: String, ty: ColType, array: bool, nullable: bool, required: bool }

/// What the triggers and the runtimes need to know about one tracker. It is the `data` of the record.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Tracker {
    event_model: String,
    event_table: String,
    root_model: String,
    root_table: String,
    kinds: Vec<String>,
    row: String,
    label: Option<String>,
    when: Option<String>,
    /// The tracked fields are every stored field of the root: an event holds a whole row.
    full: bool,
    key: Vec<KeyColumn>,
    fields: Vec<TrackedField>,
    scope: Vec<String>,
    trigger_suffix: String,
    event_sql: String,
    backfill_sql: String,
    /// PostgreSQL: `set_config` of the restore setting; the parameter is the event table (or empty to clear it). SQLite triggers cannot read settings, so it has none.
    #[serde(default)]
    restore_sql: Option<String>,
}

/// `file:line:column: @@history.track: message`.
fn fail(d: &monorm::extension::Declaration, message: impl std::fmt::Display) -> String { d.error_with("@@", message) }

/// A scope key must be snake_case and must not clash with a field of the generated model; `owner` names that model in the error.
fn check_scope(d: &monorm::extension::Declaration, scope: &[String], owner: &str, reserved: impl Fn(&str) -> bool) -> Result<(), String> {
    for key in scope {
        if !is_snake_case(key) { return Err(fail(d, format!("scope key {key} must be snake_case"))); }
        if reserved(key) { return Err(fail(d, format!("scope key {key} is a field of the {owner} model"))); }
    }
    Ok(())
}

/// Each scope key of a tracker or a version model is a scope key that history owns.
fn publish_scope_keys(ir: &mut SchemaIr, scope: &[String]) {
    for key in scope {
        let published = PublishedScopeKey { source: SOURCE.into(), key: key.clone() };
        if !ir.behavior.published_scope_keys.contains(&published) { ir.behavior.published_scope_keys.push(published); }
    }
}

fn is_snake_case(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c == '_') && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn snake_case(name: &str) -> String {
    let mut out = String::new();
    let mut previous: Option<char> = None;
    for c in name.chars() {
        if c.is_ascii_uppercase() && previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit()) { out.push('_'); }
        out.push(c.to_ascii_lowercase());
        previous = Some(c);
    }
    out
}

fn is_model_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.all(|c| c.is_ascii_alphanumeric())
}

fn text(d: &monorm::extension::Declaration, key: &str) -> Result<Option<String>, String> {
    match d.arguments.get(key) {
        None => Ok(None),
        Some(v) => v.as_str().filter(|s| !s.is_empty()).map(|s| Some(s.to_owned())).ok_or_else(|| fail(d, format!("{key} must be a non-empty string"))),
    }
}

/// A list argument: `[a, b]`, or one name. The empty list is an error.
fn names(d: &monorm::extension::Declaration, key: &str) -> Result<Option<Vec<String>>, String> {
    let Some(value) = d.arguments.get(key) else { return Ok(None) };
    let items: Vec<&serde_json::Value> = match value { serde_json::Value::Array(a) => a.iter().collect(), one => vec![one] };
    let list = items.into_iter()
        .map(|i| i.as_str().filter(|s| !s.is_empty()).map(str::to_owned).ok_or_else(|| fail(d, format!("{key} must be a list of names"))))
        .collect::<Result<Vec<_>, _>>()?;
    if list.is_empty() { return Err(fail(d, format!("{key} must not be empty"))); }
    if let Some(name) = list.iter().enumerate().find(|(i, n)| list[..*i].contains(n)).map(|(_, n)| n) { return Err(fail(d, format!("{key} names {name} twice"))); }
    Ok(Some(list))
}

fn event_kinds(d: &monorm::extension::Declaration) -> Result<Vec<String>, String> {
    let given = names(d, "events")?;
    let mut kinds: Vec<String> = given.unwrap_or_else(|| KINDS.iter().map(|k| k.to_string()).collect());
    if let Some(bad) = kinds.iter().find(|k| !KINDS.contains(&k.as_str())) { return Err(fail(d, format!("events must be insert, update or delete, not {bad}"))); }
    kinds.sort_by_key(|k| KINDS.iter().position(|x| x == k));
    Ok(kinds)
}

fn is_stored(field: &FieldIr) -> bool { field.generated.is_none() }

fn tracked_field(field: &FieldIr) -> TrackedField {
    let required = !field.nullable && field.default.is_none() && !field.auto_increment && !field.default_now && field.default_sql.is_none() && field.client_default.is_none();
    TrackedField { name: field.name.clone(), column: field.column.clone(), ty: field.ty, array: field.array, nullable: field.nullable, required }
}

// ---- SQL ----

fn xact(dialect: Dialect) -> Option<&'static str> {
    match dialect { Dialect::Postgres => Some("pg_current_xact_id()::text::bigint"), Dialect::Sqlite => None }
}

fn scope_value(key: &str, names: &ScopeSettingsIr) -> String { format!("nullif(current_setting({}, true), '')", literal(&names.key_setting(key))) }

fn sqlite_value(field: &TrackedField, value: &str) -> String {
    if field.ty == ColType::Json || field.array { format!("json({value})") }
    else if field.ty == ColType::Bool { format!("json(CASE WHEN {value} IS NULL THEN 'null' WHEN {value} THEN 'true' ELSE 'false' END)") }
    else { value.to_owned() }
}

/// The JSON object of the tracked fields of the row that `prefix` names (`NEW`, `OLD` or a table alias).
fn data_expr(dialect: Dialect, fields: &[TrackedField], prefix: &str) -> String {
    let pair = |f: &TrackedField| {
        let value = format!("{prefix}.{}", ident(&f.column));
        // A decimal is text in `data`: a JSON number decodes as a float and loses digits.
        let pg = if f.ty == ColType::Decimal { format!("{value}::{}", if f.array { "text[]" } else { "text" }) } else { value.clone() };
        match dialect { Dialect::Postgres => format!("{}, {pg}", literal(&f.name)), Dialect::Sqlite => format!("{}, {}", literal(&f.name), sqlite_value(f, &value)) }
    };
    match dialect {
        Dialect::Postgres => fields.chunks(PAIRS).map(|c| format!("jsonb_build_object({})", c.iter().map(pair).collect::<Vec<_>>().join(", "))).collect::<Vec<_>>().join(" || "),
        Dialect::Sqlite => format!("json_object({})", fields.iter().map(pair).collect::<Vec<_>>().join(", ")),
    }
}

fn event_columns(dialect: Dialect, t: &Tracker) -> Vec<String> {
    let mut columns: Vec<String> = t.key.iter().map(|k| ident(&k.event_column)).collect();
    columns.extend([ident("label"), ident("data")]);
    if dialect == Dialect::Postgres {
        columns.push(ident("xact_id"));
        columns.extend(t.scope.iter().map(|k| ident(k)));
    }
    columns
}

/// The values of one event, in the order of `event_columns`; `prefix` names the row, `label` is a SQL expression.
fn event_values(dialect: Dialect, names: &ScopeSettingsIr, t: &Tracker, label: &str, prefix: &str) -> Vec<String> {
    let mut values: Vec<String> = t.key.iter().map(|k| format!("{prefix}.{}", ident(&k.column))).collect();
    values.extend([label.to_owned(), data_expr(dialect, &t.fields, prefix)]);
    if let Some(x) = xact(dialect) {
        values.push(x.to_owned());
        values.extend(t.scope.iter().map(|k| scope_value(k, names)));
    }
    values
}

fn placeholder(dialect: Dialect, n: usize) -> String {
    match dialect { Dialect::Postgres => format!("${n}"), Dialect::Sqlite => "?".to_owned() }
}

fn select_from_root(dialect: Dialect, names: &ScopeSettingsIr, t: &Tracker) -> String {
    let label = format!("CAST({} AS text)", placeholder(dialect, 1));
    format!("INSERT INTO {} ({}) SELECT {} FROM {} t", ident(&t.event_table), event_columns(dialect, t).join(", "), event_values(dialect, names, t, &label, "t").join(", "), ident(&t.root_table))
}

/// One event of one row; parameters: the label, then the key values. Returns the id of the event.
fn manual_sql(dialect: Dialect, names: &ScopeSettingsIr, t: &Tracker) -> String {
    let keys: Vec<String> = t.key.iter().enumerate().map(|(i, k)| format!("t.{} = {}", ident(&k.column), placeholder(dialect, i + 2))).collect();
    format!("{} WHERE {} RETURNING {}", select_from_root(dialect, names, t), keys.join(" AND "), ident("id"))
}

/// One event per row of the table, in key order; parameter: the label. Returns one row with the count `n` on PostgreSQL, and one row per event on SQLite.
fn backfill_sql(dialect: Dialect, names: &ScopeSettingsIr, t: &Tracker) -> String {
    let order: Vec<String> = t.key.iter().map(|k| format!("t.{}", ident(&k.column))).collect();
    let insert = format!("{} ORDER BY {}", select_from_root(dialect, names, t), order.join(", "));
    match dialect {
        Dialect::Postgres => format!("WITH ins AS ({insert} RETURNING 1) SELECT count(*) AS n FROM ins"),
        Dialect::Sqlite => format!("{insert} RETURNING 1"),
    }
}

/// The label of a trigger event: the restore label while a restore sets the setting, else the tracker label.
fn label_expr(dialect: Dialect, t: &Tracker, label: &str) -> String {
    match dialect {
        Dialect::Postgres => format!("CASE WHEN current_setting({}, true) = {} THEN {} ELSE {label} END", literal(RESTORE_SETTING), literal(&t.event_table), literal(RESTORE_LABEL)),
        Dialect::Sqlite => label.to_owned(),
    }
}

fn restore_sql(dialect: Dialect) -> Option<String> {
    (dialect == Dialect::Postgres).then(|| format!("SELECT set_config({}, $1, true)", literal(RESTORE_SETTING)))
}

fn trigger_name(t: &Tracker, kind: &str) -> String {
    let short = match kind { "insert" => "ins", "update" => "upd", _ => "del" };
    format!("h_{}_{short}", t.trigger_suffix)
}

/// The columns whose change makes an update an event. The column that the ORM touches on each update is left out, so a touch alone writes nothing.
fn changed_columns(t: &Tracker, touched: Option<&str>) -> Vec<String> {
    let all: Vec<&TrackedField> = t.fields.iter().collect();
    let rest: Vec<&TrackedField> = all.iter().copied().filter(|f| Some(f.column.as_str()) != touched).collect();
    let used = if rest.is_empty() { all } else { rest };
    used.into_iter().map(|f| f.column.clone()).collect()
}

fn change_test(dialect: Dialect, columns: &[String]) -> String {
    match dialect {
        Dialect::Postgres => {
            let side = |p: &str| { let list: Vec<String> = columns.iter().map(|c| format!("{p}.{}", ident(c))).collect(); if list.len() == 1 { list[0].clone() } else { format!("({})", list.join(", ")) } };
            format!("{} IS DISTINCT FROM {}", side("OLD"), side("NEW"))
        }
        Dialect::Sqlite => columns.iter().map(|c| format!("OLD.{c} IS NOT NEW.{c}", c = ident(c))).collect::<Vec<_>>().join(" OR "),
    }
}

/// The triggers of one tracker, in the order of its event kinds.
fn build_triggers(dialect: Dialect, names: &ScopeSettingsIr, t: &Tracker, touched: Option<&str>, roles: &[String]) -> Vec<TriggerIr> {
    t.kinds.iter().map(|kind| {
        let (event, prefix) = match kind.as_str() {
            "insert" => (TriggerEvent::Insert, "NEW"),
            "delete" => (TriggerEvent::Delete, "OLD"),
            _ => (TriggerEvent::Update, if t.row == "old" { "OLD" } else { "NEW" }),
        };
        let mut when = None;
        if kind == "update" {
            let test = change_test(dialect, &changed_columns(t, touched));
            when = Some(match &t.when { Some(w) => format!("({test}) AND ({w})"), None => test });
        }
        // A block of `skip_triggers("history")` switches the PostgreSQL trigger off for a bypass role.
        if let (Dialect::Postgres, Some(guard)) = (dialect, skip_guard("history", roles)) {
            when = Some(match when { Some(w) => format!("({w}) AND {guard}"), None => guard });
        }
        let label = label_expr(dialect, t, &literal(t.label.as_deref().unwrap_or(kind)));
        let insert = format!("INSERT INTO {} ({}) VALUES ({})", ident(&t.event_table), event_columns(dialect, t).join(", "), event_values(dialect, names, t, &label, prefix).join(", "));
        let body = match dialect { Dialect::Postgres => format!("BEGIN {insert}; RETURN NULL; END;"), Dialect::Sqlite => insert };
        TriggerIr { name: trigger_name(t, kind), timing: TriggerTiming::After, events: vec![event], when, body: Some(body), skippable: Some("history".into()), ..Default::default() }
    }).collect()
}

// ---- the events pass ----

fn event_model(t: &Tracker, root: &ModelIr) -> Result<ModelIr, String> {
    let mut model: ModelIr = serde_json::from_value(serde_json::json!({"name": t.event_model, "table": t.event_table, "fields": []})).map_err(|e| e.to_string())?;
    model.app = root.app.clone();
    let mut id = FieldIr::plain("id", ColType::BigInt);
    (id.nullable, id.primary_key, id.auto_increment) = (false, true, true);
    model.fields.push(id);
    for key in &t.key {
        let source = root.fields.iter().find(|f| f.name == key.field).ok_or_else(|| format!("{} has no field {}", root.name, key.field))?;
        let mut field = FieldIr::key_like(&key.event_field, source);
        (field.column, field.nullable) = (key.event_column.clone(), false);
        model.fields.push(field);
    }
    let mut label = FieldIr::plain("label", ColType::String);
    label.nullable = false;
    let mut data = FieldIr::plain("data", ColType::Json);
    data.nullable = false;
    let mut created = FieldIr::plain("created_at", ColType::DateTime);
    (created.nullable, created.default_now) = (false, true);
    model.fields.extend([label, data, created, FieldIr::plain("xact_id", ColType::BigInt)]);
    model.fields.extend(t.scope.iter().map(|k| FieldIr::plain(k, ColType::String)));
    let mut index_fields: Vec<String> = t.key.iter().map(|k| k.event_field.clone()).collect();
    index_fields.push("id".into());
    model.indexes.push(IndexIr { name: Some(format!("{}_row_idx", t.event_table)), columns: index_fields.into_iter().map(|f| IndexColumnIr { field: Some(f), ..Default::default() }).collect(), ..Default::default() });
    Ok(model)
}

fn parse(ir: &SchemaIr, d: &monorm::extension::Declaration) -> Result<(Tracker, ModelIr), String> {
    if !d.positional.is_empty() { return Err(fail(d, "takes named arguments only")); }
    let declared = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| fail(d, "unknown model"))?;
    if ir.behavior.field_storage.iter().any(|f| f.model == d.model && f.owner != d.model) {
        return Err(fail(d, format!("{} is a composed model, whose rows live in several tables; track each part model, or use versions mode", d.model)));
    }
    let root_name = ir.behavior.model_views.iter().find(|v| v.model == d.model).map_or(d.model.as_str(), |v| v.root.as_str());
    let root = ir.models.iter().find(|m| m.name == root_name).ok_or_else(|| fail(d, format!("unknown model {root_name}")))?;
    let key_names = root.key_fields();
    if key_names.is_empty() { return Err(fail(d, format!("{} has no primary key", root.name))); }

    let kinds = event_kinds(d)?;
    let row = match text(d, "row")?.as_deref() { None | Some("new") => "new", Some("old") => "old", Some(_) => return Err(fail(d, "row must be old or new")) }.to_owned();
    let (label, when) = (text(d, "label")?, text(d, "when")?);
    // The condition is a test of an update (`OLD` and `NEW`); an insert or a delete trigger has only one of them.
    if when.is_some() && !kinds.iter().any(|k| k == "update") { return Err(fail(d, "when applies to update events; add update to events or remove when")); }

    // A proxy tracks the fields that it keeps, found on the root by name.
    let candidates: Vec<&FieldIr> = declared.fields.iter().filter_map(|f| root.fields.iter().find(|r| r.name == f.name)).filter(|f| is_stored(f)).collect();
    let known = |list: &[String], key: &str| -> Result<(), String> {
        match list.iter().find(|n| !candidates.iter().any(|f| &f.name == *n)) {
            Some(n) => Err(fail(d, format!("{key} names {n}, which is not a stored field of {}", d.model))),
            None => Ok(()),
        }
    };
    let chosen = names(d, "fields")?;
    let excluded = names(d, "exclude")?;
    if let Some(l) = &chosen { known(l, "fields")?; }
    if let Some(l) = &excluded { known(l, "exclude")?; }
    let tracked: Vec<TrackedField> = candidates.iter()
        .filter(|f| chosen.as_ref().is_none_or(|l| l.contains(&f.name)) && excluded.as_ref().is_none_or(|l| !l.contains(&f.name)))
        .map(|f| tracked_field(f)).collect();
    if tracked.is_empty() { return Err(fail(d, "tracks no field")); }
    let full = root.fields.iter().filter(|f| is_stored(f)).all(|f| tracked.iter().any(|t| t.name == f.name));
    if ir.dialect == Dialect::Sqlite && tracked.len() > SQLITE_FIELDS { return Err(fail(d, format!("tracks {} fields; SQLite takes at most {SQLITE_FIELDS} in one event", tracked.len()))); }

    let scope = names(d, "scope")?.unwrap_or_default();
    check_scope(d, &scope, "event", |key| EVENT_FIELDS.contains(&key) || key.starts_with("row_"))?;

    let model_name = text(d, "model")?.unwrap_or_else(|| format!("{}Event", d.model));
    if !is_model_name(&model_name) { return Err(fail(d, format!("model {model_name} must be a model name"))); }
    let (event_table, trigger_suffix) = if d.arguments.contains_key("model") {
        let table = snake_case(&model_name);
        (table.clone(), table)
    } else {
        (format!("{}_events", root.table), "events".to_owned())
    };
    let taken = |what: &str| fail(d, format!("{what}; name the event model with model: \"Other\""));
    if ir.models.iter().any(|m| m.name == model_name) { return Err(taken(&format!("the model {model_name} exists"))); }
    if ir.models.iter().any(|m| m.table == event_table) { return Err(taken(&format!("the table {event_table} exists"))); }
    let key = key_names.iter().map(|name| {
        let field = root.fields.iter().find(|f| f.name == *name).ok_or_else(|| fail(d, format!("{} has no field {name}", root.name)))?;
        Ok(KeyColumn { field: field.name.clone(), column: field.column.clone(), event_field: format!("row_{}", field.name), event_column: format!("row_{}", field.column) })
    }).collect::<Result<Vec<_>, String>>()?;
    for k in &key {
        if scope.contains(&k.event_field) { return Err(fail(d, format!("scope key {} is a field of the event model", k.event_field))); }
    }
    if records(ir)?.iter().any(|r| r.root_table == root.table && r.trigger_suffix == trigger_suffix) {
        return Err(taken(&format!("another tracker of {} already uses the trigger names h_{trigger_suffix}_*", root.table)));
    }
    if ir.dialect == Dialect::Postgres {
        let longest = kinds.iter().map(|k| format!("{}_h_{trigger_suffix}_{}", root.table, &k[..3])).map(|n| n.len()).max().unwrap_or(0);
        if longest > 63 { return Err(taken("the trigger function name is longer than 63 characters")); }
    }

    let mut tracker = Tracker { event_model: model_name, event_table, root_model: root.name.clone(), root_table: root.table.clone(), kinds, row, label, when, full, key, fields: tracked, scope, trigger_suffix, event_sql: String::new(), backfill_sql: String::new(), restore_sql: restore_sql(ir.dialect) };
    tracker.event_sql = manual_sql(ir.dialect, &ir.scope_settings, &tracker);
    tracker.backfill_sql = backfill_sql(ir.dialect, &ir.scope_settings, &tracker);
    let model = event_model(&tracker, root).map_err(|e| fail(d, e))?;
    Ok((tracker, model))
}

/// The event model and the record of each tracker; runs after the passes that lower proxies and composed models.
pub fn events(ir: &mut SchemaIr) -> Result<(), String> {
    for d in &TRACKED.declarations_besides(ir, OTHERS)? {
        let (tracker, model) = parse(ir, d)?;
        use monorm::writes::{WriteAction, WriteLayer, WriteRule};
        ir.behavior.write_rules.push(WriteRule {
            source: SOURCE.into(), model: model.name.clone(), name: "append_only".into(),
            operations: vec![TriggerEvent::Update, TriggerEvent::Delete], fields: vec![],
            condition: None, changed: false, action: WriteAction::Refuse, layer: WriteLayer::Both, skippable: true, declaration: None,
        });
        publish_scope_keys(ir, &tracker.scope);
        if let Some(storage) = &mut ir.behavior.storage { storage.models.push(model.clone()); }
        ir.models.push(model);
        let data = serde_json::to_value(&tracker).map_err(|e| e.to_string())?;
        ir.behavior.extension_records.push(ExtensionRecord { source: SOURCE.into(), model: d.model.clone(), kind: KIND.into(), data });
    }
    Ok(())
}

// ---- versions mode ----

/// What the runtimes need to know about one version model. It is the `data` of the record.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct VersionModel {
    model: String,
    table: String,
    of_field: String,
    of_column: String,
    pk_field: String,
    scope: Vec<String>,
    /// PostgreSQL: takes the advisory lock of one entity for the rest of the transaction; parameter: the entity id. SQLite has one writer, so it has none.
    #[serde(default)]
    lock_sql: Option<String>,
    /// The latest version of one entity: the columns `id`, `content_hash` and `change_type`; parameter: the entity id.
    latest_sql: String,
    /// One version with the next number; parameters: the entity id, the snapshot, the hash, the change type, `restored_from`. Returns the column `id`.
    insert_sql: String,
}

/// A parameter of the versions SQL: PostgreSQL needs the type of a parameter that has no column to take it from.
fn typed(dialect: Dialect, n: usize, field: &FieldIr) -> String {
    match dialect {
        Dialect::Postgres => {
            let ty = field.db_type.clone().unwrap_or_else(|| column_type(field.ty, field.max_length, field.array));
            format!("CAST(${n} AS {ty})")
        }
        Dialect::Sqlite => "?".to_owned(),
    }
}

fn version_model(dialect: Dialect, names: &ScopeSettingsIr, model: &ModelIr, of: &FieldIr, pk: &FieldIr, scope: &[String]) -> VersionModel {
    let (table, number, of_column, pk_column) = (ident(&model.table), ident("version_number"), ident(&of.column), ident(&pk.column));
    let entity = typed(dialect, 1, of);
    let latest_sql = format!("SELECT {pk_column} AS id, {} AS content_hash, {} AS change_type FROM {table} WHERE {of_column} = {entity} ORDER BY {number} DESC LIMIT 1", ident("content_hash"), ident("change_type"));
    // The entity id is a parameter once, in a CTE: a `?` placeholder cannot repeat.
    let mut columns = vec![of_column.clone(), number.clone(), ident("snapshot"), ident("content_hash"), ident("change_type"), ident("restored_from")];
    let snapshot = match dialect { Dialect::Postgres => "CAST($2 AS jsonb)".to_owned(), Dialect::Sqlite => "?".to_owned() };
    let text_param = |n: usize| match dialect { Dialect::Postgres => format!("CAST(${n} AS text)"), Dialect::Sqlite => "?".to_owned() };
    let mut values = vec![
        "k.v".to_owned(),
        format!("coalesce((SELECT max({number}) FROM {table} WHERE {of_column} = k.v), 0) + 1"),
        snapshot, text_param(3), text_param(4), typed(dialect, 5, pk),
    ];
    if let Some(expr) = xact(dialect) {
        columns.push(ident("xact_id"));
        values.push(expr.to_owned());
        columns.extend(scope.iter().map(|key| ident(key)));
        values.extend(scope.iter().map(|key| scope_value(key, names)));
    }
    let insert_sql = format!("WITH k AS (SELECT {entity} AS v) INSERT INTO {table} ({}) SELECT {} FROM k RETURNING {pk_column} AS id", columns.join(", "), values.join(", "));
    let lock_sql = (dialect == Dialect::Postgres).then(|| format!("SELECT 1 AS locked FROM (SELECT pg_advisory_xact_lock(hashtext({}), hashtext(CAST($1 AS text)))) l", literal(&model.table)));
    VersionModel { model: model.name.clone(), table: model.table.clone(), of_field: of.name.clone(), of_column: of.column.clone(), pk_field: pk.name.clone(), scope: scope.to_vec(), lock_sql, latest_sql, insert_sql }
}

/// Append `fields` and `index` to the model of `d`, in the logical and the physical list.
fn add_version_fields(ir: &mut SchemaIr, d: &monorm::extension::Declaration, fields: &[FieldIr], index: &IndexIr, restored_from: &RelationIr) -> Result<(), String> {
    let mut lists = vec![&mut ir.models];
    if let Some(storage) = &mut ir.behavior.storage { lists.push(&mut storage.models); }
    for list in lists {
        let model = list.iter_mut().find(|m| m.name == d.model).ok_or_else(|| fail(d, "unknown model"))?;
        for field in fields {
            if model.fields.iter().any(|f| f.name == field.name) || model.relations.iter().any(|r| r.name == field.name) {
                return Err(fail(d, format!("adds the field {}.{}, which already exists; rename or remove that field", d.model, field.name)));
            }
            if let Some(owner) = model.fields.iter().find(|f| f.column == field.column) {
                return Err(fail(d, format!("adds the field {}.{}, but the field {}.{} already uses the column {}", d.model, field.name, d.model, owner.name, field.column)));
            }
            model.fields.push(field.clone());
        }
        if model.fields.iter().any(|f| f.name == restored_from.name) || model.relations.iter().any(|r| r.name == restored_from.name) {
            return Err(fail(d, format!("adds the relation {}.{}, which already exists; rename or remove it", d.model, restored_from.name)));
        }
        model.relations.push(restored_from.clone());
        model.indexes.push(index.clone());
    }
    Ok(())
}

fn version_fields(pk: &FieldIr, scope: &[String]) -> Vec<FieldIr> {
    let required = |name: &str, ty: ColType| { let mut f = FieldIr::plain(name, ty); f.nullable = false; f };
    let mut change = required("change_type", ColType::String);
    change.check = Some(format!("{} IN ({})", ident("change_type"), CHANGE_TYPES.iter().map(|c| literal(c)).collect::<Vec<_>>().join(", ")));
    let mut effective = required("effective_at", ColType::DateTime);
    effective.default_now = true;
    let restored = FieldIr::key_like("restored_from", pk);
    let mut fields = vec![required("version_number", ColType::Int), required("snapshot", ColType::Json), required("content_hash", ColType::Text), effective, change, restored, FieldIr::plain("xact_id", ColType::BigInt)];
    fields.extend(scope.iter().map(|k| FieldIr::plain(k, ColType::String)));
    fields
}

/// The version columns and the record of each `@@history.versions` model; runs with the events pass.
pub fn versions(ir: &mut SchemaIr) -> Result<(), String> {
    let versions = Tracked { attribute: VERSIONS, field: "" };
    let declarations = versions.declarations_besides(ir, &[ATTRIBUTE])?;
    for (i, d) in declarations.iter().enumerate() {
        if declarations[..i].iter().any(|other| other.model == d.model) { return Err(fail(d, "a model takes one versions declaration")); }
        if ir.behavior.declarations.iter().any(|o| !o.lowered && o.attribute == ATTRIBUTE && o.model == d.model) {
            return Err(fail(d, format!("{} has @@history.track as well; a model is a version model or a tracked model, not both", d.model)));
        }
        if !d.positional.is_empty() { return Err(fail(d, "takes named arguments only")); }
        let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| fail(d, "unknown model"))?.clone();
        if ir.behavior.field_storage.iter().any(|f| f.model == d.model && f.owner != d.model) || ir.behavior.model_views.iter().any(|v| v.model == d.model && v.root != d.model) {
            return Err(fail(d, format!("{} is a composed or proxy model; a version model is a model with a table of its own", d.model)));
        }
        let of_name = text(d, "of")?.ok_or_else(|| fail(d, "of names the field that holds the id of the entity"))?;
        let of = model.fields.iter().find(|f| f.name == of_name && is_stored(f)).ok_or_else(|| fail(d, format!("of names {of_name}, which is not a stored field of {}", d.model)))?.clone();
        let key = model.key_fields();
        let pk = match key.as_slice() {
            [name] => model.fields.iter().find(|f| &f.name == name).cloned(),
            _ => None,
        }.ok_or_else(|| fail(d, format!("{} needs a single-field primary key", d.model)))?;
        if !pk.auto_increment && pk.default_sql.is_none() { return Err(fail(d, format!("the primary key {}.{} needs a database default (autoincrement or a default expression): the version number insert does not give an id", d.model, pk.name))); }
        if pk.name == of.name { return Err(fail(d, "of must not name the primary key")); }
        let scope = names(d, "scope")?.unwrap_or_default();
        check_scope(d, &scope, "version", |key| VERSION_FIELDS.contains(&key))?;
        let index = IndexIr { name: Some(format!("{}_{}_version_key", model.table, of.column)), unique: true, columns: [of.name.as_str(), "version_number"].iter().map(|f| IndexColumnIr { field: Some((*f).to_owned()), ..Default::default() }).collect(), ..Default::default() };
        // A version that a restore points to can be pruned later; the restore version then keeps its row.
        let restored_from = RelationIr { foreign_key: true, on_delete: Some(OnDelete::SetNull), ..RelationIr::one("restored_from_version", &model.name, "restored_from", &pk.name) };
        add_version_fields(ir, d, &version_fields(&pk, &scope), &index, &restored_from)?;
        publish_scope_keys(ir, &scope);
        let record = version_model(ir.dialect, &ir.scope_settings, &model, &of, &pk, &scope);
        let data = serde_json::to_value(&record).map_err(|e| e.to_string())?;
        ir.behavior.extension_records.push(ExtensionRecord { source: SOURCE.into(), model: d.model.clone(), kind: VERSIONS_KIND.into(), data });
    }
    Ok(())
}

// ---- the triggers pass ----

fn records(ir: &SchemaIr) -> Result<Vec<Tracker>, String> {
    ir.behavior.extension_records.iter().filter(|r| r.source == SOURCE && r.kind == KIND)
        .map(|r| serde_json::from_value(r.data.clone()).map_err(|e| format!("{}: a history record is damaged: {e}", r.model))).collect()
}

/// The triggers of each tracker go to the table that it tracks. A root of an earlier batch gets them through `added_triggers`.
pub fn triggers(ir: &mut SchemaIr) -> Result<(), String> {
    let (dialect, roles) = (ir.dialect, ir.bypass_roles.clone());
    for t in records(ir)? {
        let root = ir.models.iter().find(|m| m.name == t.root_model).ok_or_else(|| format!("history: the tracked model {} is missing", t.root_model))?;
        let touched = ir.behavior.write_assignments.iter()
            .find(|a| a.model == t.root_model && a.on.contains(&WriteKind::Update) && matches!(a.value, AssignValue::Now))
            .and_then(|a| root.fields.iter().find(|f| f.name == a.field)).map(|f| f.column.clone());
        let lowered = ir.behavior.lowered_models.contains(&t.root_model);
        for trigger in build_triggers(dialect, &ir.scope_settings, &t, touched.as_deref(), &roles) {
            let present = ir.models.iter().find(|m| m.name == t.root_model).is_some_and(|m| m.triggers.iter().any(|x| x.name == trigger.name));
            if present { continue; }
            if lowered {
                let queued = ir.behavior.added_triggers.iter().any(|a| a.model == t.root_model && a.trigger.name == trigger.name);
                if !queued { ir.behavior.added_triggers.push(AddedTrigger { model: t.root_model.clone(), trigger }); }
                continue;
            }
            for model in ir.models_named_mut(&t.root_model) {
                if !model.triggers.iter().any(|x| x.name == trigger.name) { model.triggers.push(trigger.clone()); }
            }
        }
    }
    Ok(())
}

/// The one logical pass: the event models of the trackers, then the version models. Two passes cannot share an effect.
pub fn models(ir: &mut SchemaIr) -> Result<(), String> {
    events(ir)?;
    versions(ir)
}

// The package entry point; Rust callers keep the rlib API.
monorm::extension!(("monorm.history.models", models), ("monorm.history.triggers", triggers));

#[cfg(test)]
mod tests {
    use super::*;
    use monorm::extension::pass_effects;

    fn at(model: &str, arguments: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"attribute": "history.track", "model": model, "field": null, "positional": [], "arguments": arguments, "location": {"file": "schema.prisma", "line": 7, "column": 3}})
    }

    fn post() -> serde_json::Value {
        serde_json::json!({"name": "Post", "table": "posts", "fields": [
            {"name": "id", "column": "id", "type": "int", "primary_key": true, "auto_increment": true, "nullable": false},
            {"name": "title", "column": "title", "type": "string", "nullable": false},
            {"name": "body", "column": "body", "type": "text", "nullable": true},
            {"name": "meta", "column": "meta", "type": "json", "nullable": true},
            {"name": "updated_at", "column": "updated_at", "type": "date_time", "nullable": false, "default_now": true}
        ]})
    }

    fn schema(dialect: &str, declarations: Vec<serde_json::Value>) -> SchemaIr {
        serde_json::from_value(serde_json::json!({
            "dialect": dialect, "models": [post()],
            "behavior": {"schema_contract": 5, "declarations": declarations,
                "write_assignments": [{"source": "monorm.timestamps", "model": "Post", "field": "updated_at", "on": ["update"], "value": "now"}]}
        })).unwrap()
    }

    fn lowered(dialect: &str, declarations: Vec<serde_json::Value>) -> Result<SchemaIr, String> {
        let mut ir = schema(dialect, declarations);
        monorm::extension::capture_storage(&mut ir)?;
        events(&mut ir)?;
        triggers(&mut ir)?;
        Ok(ir)
    }

    fn migration(ir: &SchemaIr) -> Vec<String> {
        let storage = serde_json::json!({"dialect": ir.dialect, "models": ir.behavior.storage.as_ref().unwrap().models});
        let schema = monorm_core::schema::Schema::from_ir(serde_json::from_value(storage).unwrap()).unwrap();
        monorm_core::migrate::plan(&schema, &Default::default()).unwrap().up.into_iter().map(|s| s.sql).collect()
    }

    fn track(arguments: serde_json::Value) -> Vec<serde_json::Value> { vec![at("Post", arguments)] }

    #[test]
    fn a_tracker_adds_the_event_model_to_logical_and_physical_models_and_one_record() {
        let ir = lowered("postgres", track(serde_json::json!({}))).unwrap();
        for model in [ir.models.last().unwrap(), ir.behavior.storage.as_ref().unwrap().models.last().unwrap()] {
            assert_eq!((model.name.as_str(), model.table.as_str()), ("PostEvent", "posts_events"));
            assert!(ir.behavior.write_rules.iter().any(|r| r.model == model.name));
            let names: Vec<&str> = model.fields.iter().map(|f| f.name.as_str()).collect();
            assert_eq!(names, ["id", "row_id", "label", "data", "created_at", "xact_id"]);
            assert!(model.fields.iter().all(|f| f.nullable == (f.name == "xact_id")));
            assert_eq!(model.indexes[0].name.as_deref(), Some("posts_events_row_idx"));
        }
        let record = &ir.behavior.extension_records[0];
        assert_eq!((record.source.as_str(), record.model.as_str(), record.kind.as_str()), ("monorm.history", "Post", "tracker"));
        let tracker: Tracker = serde_json::from_value(record.data.clone()).unwrap();
        assert!(tracker.full && tracker.event_sql.contains("RETURNING") && tracker.backfill_sql.contains("count(*)"));
        assert_eq!(tracker.fields.len(), 5);
    }

    #[test]
    fn both_passes_name_their_effects_and_run_twice_without_change() {
        let mut ir = schema("postgres", track(serde_json::json!({"scope": ["user"]})));
        monorm::extension::capture_storage(&mut ir).unwrap();
        let effects = pass_effects(&ir, events).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.extension_records", "behavior.published_scope_keys", "behavior.storage", "behavior.write_rules", "models"]);
        events(&mut ir).unwrap();
        let effects = pass_effects(&ir, triggers).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.storage", "models"]);
    }

    #[test]
    fn scope_keys_are_published_once_and_become_nullable_columns() {
        let two = vec![at("Post", serde_json::json!({"scope": ["user", "request"]}))];
        let ir = lowered("sqlite", two).unwrap();
        let keys: Vec<&str> = ir.behavior.published_scope_keys.iter().map(|k| k.key.as_str()).collect();
        assert_eq!(keys, ["user", "request"]);
        let event = ir.models.last().unwrap();
        assert!(event.fields.iter().filter(|f| ["user", "request"].contains(&f.name.as_str())).all(|f| f.nullable && f.ty == ColType::String));
    }

    #[test]
    fn the_scope_columns_of_an_event_read_the_settings_of_the_ir() {
        let declarations = vec![at("Post", serde_json::json!({"scope": ["user"]}))];
        let build = |names: ScopeSettingsIr| {
            let mut ir = schema("postgres", declarations.clone());
            ir.scope_settings = names;
            monorm::extension::capture_storage(&mut ir).unwrap();
            events(&mut ir).unwrap();
            triggers(&mut ir).unwrap();
            let sql: Vec<String> = ir.models.iter().flat_map(|m| m.triggers.iter()).filter_map(|t| t.body.clone()).collect();
            let record = serde_json::to_string(&ir.behavior.extension_records).unwrap();
            format!("{}\n{record}", sql.join("\n"))
        };
        let default = build(ScopeSettingsIr::default());
        assert!(default.contains("current_setting('scope.user', true)"), "{default}");
        let renamed = build(ScopeSettingsIr { prefix: "tenant".into(), ..Default::default() });
        assert!(renamed.contains("current_setting('tenant.user', true)") && !renamed.contains("scope.user"), "{renamed}");
    }

    #[test]
    fn the_postgres_triggers_are_skippable_and_the_update_trigger_ignores_the_touched_column() {
        let ir = lowered("postgres", track(serde_json::json!({}))).unwrap();
        let sql = migration(&ir).join("\n");
        for name in ["h_events_ins", "h_events_upd", "h_events_del"] {
            assert!(sql.contains(&format!("\"posts_{name}\"()")), "{name} in {sql}");
        }
        assert!(sql.contains("IS DISTINCT FROM") && sql.contains("pg_current_xact_id()"));
        let update = ir.models[0].triggers.iter().find(|t| t.name == "h_events_upd").unwrap();
        assert!(!update.when.as_ref().unwrap().contains("updated_at"));
        assert_eq!(update.skippable.as_deref(), Some("history"));
    }

    #[test]
    fn the_triggers_of_a_lowered_root_go_through_added_triggers() {
        let mut ir = schema("postgres", track(serde_json::json!({})));
        monorm::extension::capture_storage(&mut ir).unwrap();
        events(&mut ir).unwrap();
        ir.behavior.lowered_models.push("Post".into());
        triggers(&mut ir).unwrap();
        triggers(&mut ir).unwrap();
        assert!(ir.models[0].triggers.is_empty());
        assert_eq!(ir.behavior.added_triggers.len(), 3);
    }

    #[test]
    fn validation_errors_name_the_attribute_and_the_place() {
        let err = |a: serde_json::Value| lowered("postgres", track(a)).unwrap_err();
        assert_eq!(err(serde_json::json!({"fields": ["nope"]})), "schema.prisma:7:3: @@history.track: fields names nope, which is not a stored field of Post");
        assert!(err(serde_json::json!({"fields": ["id"], "exclude": ["id"]})).ends_with("tracks no field"));
        assert!(err(serde_json::json!({"row": "middle"})).ends_with("row must be old or new"));
        assert!(err(serde_json::json!({"scope": ["Bad"]})).contains("snake_case"));
        assert!(err(serde_json::json!({"scope": ["label"]})).contains("a field of the event model"));
        assert!(err(serde_json::json!({"model": "Post"})).contains("name the event model"));
        let mut two = track(serde_json::json!({}));
        two.push(at("Post", serde_json::json!({})));
        assert!(lowered("postgres", two).unwrap_err().contains("name the event model with model:"));
        assert!(err(serde_json::json!({"events": ["insert", "delete"], "when": "true"})).ends_with("when applies to update events; add update to events or remove when"));
    }

    #[test]
    fn a_second_tracker_with_the_trigger_suffix_of_the_first_is_an_error() {
        // `model: "Events"` makes the suffix `events`, the suffix of the default tracker.
        let two = vec![at("Post", serde_json::json!({})), at("Post", serde_json::json!({"model": "Events"}))];
        let err = lowered("postgres", two).unwrap_err();
        assert!(err.contains("already uses the trigger names h_events_*"), "{err}");
    }

    #[test]
    fn a_postgres_trigger_reads_the_restore_setting_and_a_sqlite_trigger_does_not() {
        let body = |dialect: &str| lowered(dialect, track(serde_json::json!({"label": "edited"}))).unwrap().models[0].triggers.iter().filter_map(|t| t.body.clone()).collect::<Vec<_>>().join("\n");
        let pg = body("postgres");
        assert!(pg.contains("CASE WHEN current_setting('monorm.history_restore', true) = 'posts_events' THEN 'restore' ELSE 'edited' END"), "{pg}");
        assert!(!body("sqlite").contains("monorm.history_restore"));
        let ir = lowered("postgres", track(serde_json::json!({}))).unwrap();
        let t = &records(&ir).unwrap()[0];
        assert_eq!(t.restore_sql.as_deref(), Some("SELECT set_config('monorm.history_restore', $1, true)"));
        assert_eq!(records(&lowered("sqlite", track(serde_json::json!({}))).unwrap()).unwrap()[0].restore_sql, None);
    }

    #[test]
    fn two_trackers_of_one_model_use_their_own_tables_and_trigger_names() {
        let two = vec![at("Post", serde_json::json!({})), at("Post", serde_json::json!({"model": "PostAudit", "events": ["delete"]}))];
        let ir = lowered("postgres", two).unwrap();
        let names: Vec<&str> = ir.models[0].triggers.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["h_events_ins", "h_events_upd", "h_events_del", "h_post_audit_del"]);
    }

    #[test]
    fn a_composed_model_is_refused() {
        let mut ir = schema("postgres", track(serde_json::json!({})));
        ir.behavior.field_storage.push(serde_json::from_value(serde_json::json!({"model": "Post", "field": "title", "owner": "Other", "column": "title"})).unwrap());
        monorm::extension::capture_storage(&mut ir).unwrap();
        assert!(events(&mut ir).unwrap_err().contains("composed model"));
    }

    fn sqlite(ir: &SchemaIr) -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for sql in migration(ir) { db.execute_batch(&sql).unwrap_or_else(|e| panic!("{e}: {sql}")); }
        db
    }

    fn events_of(db: &rusqlite::Connection) -> Vec<(String, String)> {
        let mut q = db.prepare("SELECT label, data FROM posts_events ORDER BY id").unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
    }

    #[test]
    fn sqlite_triggers_record_insert_update_and_delete_but_not_a_touch() {
        let db = sqlite(&lowered("sqlite", track(serde_json::json!({}))).unwrap());
        db.execute_batch("INSERT INTO posts (id, title, meta) VALUES (1, 'a', '{\"k\":[1]}');").unwrap();
        db.execute_batch("UPDATE posts SET updated_at = '2030-01-01 00:00:00' WHERE id = 1;").unwrap();
        assert_eq!(events_of(&db).len(), 1);
        db.execute_batch("UPDATE posts SET title = 'b' WHERE id = 1; DELETE FROM posts WHERE id = 1;").unwrap();
        let events = events_of(&db);
        assert_eq!(events.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(), ["insert", "update", "delete"]);
        let first: serde_json::Value = serde_json::from_str(&events[0].1).unwrap();
        assert_eq!(first["title"], "a");
        let last: serde_json::Value = serde_json::from_str(&events[2].1).unwrap();
        assert_eq!(last["title"], "b");
        assert_eq!(first["meta"], serde_json::json!({"k": [1]}));
    }

    #[test]
    fn sqlite_when_and_row_old_choose_what_the_update_event_holds() {
        let args = serde_json::json!({"events": ["update"], "row": "old", "label": "changed", "when": "NEW.\"title\" <> 'skip'"});
        let db = sqlite(&lowered("sqlite", track(args)).unwrap());
        db.execute_batch("INSERT INTO posts (id, title) VALUES (1, 'a'); UPDATE posts SET title = 'skip' WHERE id = 1; UPDATE posts SET title = 'c' WHERE id = 1;").unwrap();
        let events = events_of(&db);
        assert_eq!(events.len(), 1);
        let data: serde_json::Value = serde_json::from_str(&events[0].1).unwrap();
        assert_eq!((events[0].0.as_str(), data["title"].as_str()), ("changed", Some("skip")));
    }

    fn bundle() -> serde_json::Value {
        serde_json::json!({"name": "BundleVersion", "table": "bundle_versions", "fields": [
            {"name": "id", "column": "id", "type": "int", "primary_key": true, "auto_increment": true, "nullable": false},
            {"name": "bundle_id", "column": "bundle_id", "type": "int", "nullable": false},
            {"name": "note", "column": "note", "type": "string", "nullable": true}
        ]})
    }

    fn versioned(dialect: &str, arguments: serde_json::Value) -> Result<SchemaIr, String> {
        let declaration = serde_json::json!({"attribute": "history.versions", "model": "BundleVersion", "field": null, "positional": [], "arguments": arguments, "location": {"file": "schema.prisma", "line": 9, "column": 3}});
        let mut ir: SchemaIr = serde_json::from_value(serde_json::json!({"dialect": dialect, "models": [bundle()], "behavior": {"schema_contract": 5, "declarations": [declaration]}})).unwrap();
        monorm::extension::capture_storage(&mut ir)?;
        versions(&mut ir)?;
        Ok(ir)
    }

    #[test]
    fn versions_add_the_columns_the_unique_index_and_one_record_to_both_model_lists() {
        let ir = versioned("postgres", serde_json::json!({"of": "bundle_id", "scope": ["user"]})).unwrap();
        for model in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let names: Vec<&str> = model.fields.iter().map(|f| f.name.as_str()).collect();
            assert_eq!(names, ["id", "bundle_id", "note", "version_number", "snapshot", "content_hash", "effective_at", "change_type", "restored_from", "xact_id", "user"]);
            assert!(model.indexes[0].unique && model.indexes[0].columns.len() == 2);
            let link = model.relations.iter().find(|r| r.name == "restored_from_version").unwrap();
            assert!(link.foreign_key && link.target == "BundleVersion" && link.from == ["restored_from"] && link.to == ["id"]);
            assert!(model.fields.iter().find(|f| f.name == "change_type").unwrap().check.as_ref().unwrap().contains("'restore'"));
            assert!(model.fields.iter().find(|f| f.name == "restored_from").unwrap().nullable);
        }
        assert_eq!(ir.behavior.published_scope_keys.iter().map(|k| k.key.as_str()).collect::<Vec<_>>(), ["user"]);
        let record = &ir.behavior.extension_records[0];
        assert_eq!((record.source.as_str(), record.model.as_str(), record.kind.as_str()), ("monorm.history", "BundleVersion", "versions"));
        let v: VersionModel = serde_json::from_value(record.data.clone()).unwrap();
        assert!(v.lock_sql.unwrap().contains("pg_advisory_xact_lock(hashtext('bundle_versions'), hashtext(CAST($1 AS text)))"));
        assert!(v.insert_sql.contains("coalesce((SELECT max(\"version_number\")") && v.insert_sql.contains("current_setting('scope.user', true)") && v.insert_sql.contains("pg_current_xact_id()"));
    }

    #[test]
    fn the_sqlite_version_sql_has_no_lock_no_xact_and_one_placeholder_per_parameter() {
        let ir = versioned("sqlite", serde_json::json!({"of": "bundle_id", "scope": ["user"]})).unwrap();
        let v: VersionModel = serde_json::from_value(ir.behavior.extension_records[0].data.clone()).unwrap();
        assert!(v.lock_sql.is_none() && !v.insert_sql.contains("xact") && !v.insert_sql.contains("$"));
        assert_eq!(v.insert_sql.matches('?').count(), 5, "{}", v.insert_sql);
        assert_eq!(v.latest_sql.matches('?').count(), 1);
    }

    #[test]
    fn the_versions_pass_names_its_effects_and_its_errors() {
        let ir = versioned("postgres", serde_json::json!({"of": "bundle_id"})).unwrap();
        assert_eq!(ir.models[0].fields.len(), 10);
        let err = |a: serde_json::Value| versioned("postgres", a).unwrap_err();
        assert_eq!(err(serde_json::json!({})), "schema.prisma:9:3: @@history.versions: of names the field that holds the id of the entity");
        assert!(err(serde_json::json!({"of": "nope"})).ends_with("of names nope, which is not a stored field of BundleVersion"));
        assert!(err(serde_json::json!({"of": "id"})).ends_with("of must not name the primary key"));
        assert!(err(serde_json::json!({"of": "bundle_id", "scope": ["Bad"]})).contains("snake_case"));
        assert!(err(serde_json::json!({"of": "bundle_id", "scope": ["snapshot"]})).contains("a field of the version model"));
        assert!(err(serde_json::json!({"of": "bundle_id", "scope": ["note"]})).contains("adds the field BundleVersion.note"));
    }

    #[test]
    fn a_version_model_cannot_also_be_tracked_and_needs_a_database_default_key() {
        let mut ir: SchemaIr = serde_json::from_value(serde_json::json!({"dialect": "sqlite", "models": [bundle()], "behavior": {"schema_contract": 5, "declarations": [
            {"attribute": "history.versions", "model": "BundleVersion", "field": null, "positional": [], "arguments": {"of": "bundle_id"}, "location": {"file": "s.prisma", "line": 2, "column": 1}},
            {"attribute": "history.track", "model": "BundleVersion", "field": null, "positional": [], "arguments": {}, "location": {"file": "s.prisma", "line": 3, "column": 1}}]}})).unwrap();
        monorm::extension::capture_storage(&mut ir).unwrap();
        assert!(versions(&mut ir).unwrap_err().contains("a version model or a tracked model, not both"));
        let mut ir = schema("sqlite", vec![at("Post", serde_json::json!({}))]);
        ir.behavior.declarations[0].attribute = "history.versions".into();
        ir.behavior.declarations[0].arguments.insert("of".into(), "title".into());
        ir.models[0].fields[0].auto_increment = false;
        monorm::extension::capture_storage(&mut ir).unwrap();
        assert!(versions(&mut ir).unwrap_err().contains("needs a database default"));
    }

    #[test]
    fn sqlite_numbers_versions_and_the_unique_index_refuses_a_repeat() {
        let ir = versioned("sqlite", serde_json::json!({"of": "bundle_id"})).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for sql in migration(&ir) { db.execute_batch(&sql).unwrap_or_else(|e| panic!("{e}: {sql}")); }
        let v: VersionModel = serde_json::from_value(ir.behavior.extension_records[0].data.clone()).unwrap();
        let insert = |entity: i64, change: &str| db.query_row(&v.insert_sql, rusqlite::params![entity, "{\"a\":1}", "h", change, rusqlite::types::Null], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!((insert(1, "create"), insert(1, "update"), insert(2, "create")), (1, 2, 3));
        let numbers: Vec<(i64, i64)> = db.prepare("SELECT bundle_id, version_number FROM bundle_versions ORDER BY id").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
        assert_eq!(numbers, [(1, 1), (1, 2), (2, 1)]);
        assert!(db.execute("INSERT INTO bundle_versions (bundle_id, version_number, snapshot, content_hash, change_type) VALUES (1, 2, '{}', 'h', 'update')", []).is_err());
        assert!(db.execute("INSERT INTO bundle_versions (bundle_id, version_number, snapshot, content_hash, change_type) VALUES (1, 9, '{}', 'h', 'bogus')", []).is_err());
        let latest: (i64, String) = db.query_row(&v.latest_sql, [1], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(latest, (2, "h".to_owned()));
    }
}

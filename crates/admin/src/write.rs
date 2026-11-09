//! Plain writes: create, change and delete one record.
//!
//! A write runs in the request transaction: the permission checks, a `SELECT ... FOR UPDATE` with the
//! `queryset` filter and the `can=` rule, a compare of the stored values with the values that the form
//! loaded, the statement, one audit row, then the commit. The admin ignores tenancy: a write sets no
//! tenant value, and the read defaults that read a `scope.<name>` value are off in the lock read too.
//!
//! A model with host behavior (hooks, a hook assignment or default) or `@@protected_write` is read-only here.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::str::FromStr;

use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{DateTime, NaiveDate, NaiveDateTime};
use monorm_core::ir::{self, ColType, ConstraintIr, EnumStorage, FieldIr, NestedStorage, Operation, ValueType};
use monorm_core::schema::Schema;
use monorm_core::writes::AssignValue;
use monorm_engine::db::{DbError, ErrorKind};
use monorm_engine::exec::{self, Outcome};
use monorm_engine::plan::Planner;
use monorm_engine::{Error, NoParams};
use sea_query::Value as Bind;
use serde_json::{json, Map, Value};

use crate::audit::{self, Entry};
use crate::form::Section;
use crate::json;
use crate::key;
use crate::list::{self, Binds, ListError};
use crate::permissions::{Grants, Op};
use crate::relations::{self, Relation, LINKS_PER_WRITE};
use crate::scope::Scoped;
use crate::server::{authorize, deny, error_response, reach, reject, Shared, State_, User};
use crate::service::{ErrorNote, RequestId};
use crate::site::{Column, Resource, Site, Source};

/// The largest body of a write: a form, not an upload.
const MAX_BODY: usize = 1 << 20;
/// The width of the record label in the audit row.
const LABEL_WIDTH: usize = 200;

/// Why the admin does not write to this model, or none when it does.
pub fn read_only_reason(schema: &Schema, r: &Resource) -> Option<String> {
    model_read_only_reason(schema, r.model)
}

/// `read_only_reason` for a model that is not a resource, such as the join model of a many-to-many relation.
pub(crate) fn model_read_only_reason(schema: &Schema, model: usize) -> Option<String> {
    let m = schema.model(model);
    if m.ir.protected_write {
        return Some("this model is @@protected_write: its writes need host code, which the admin does not run yet".into());
    }
    let hook_assignment = m.assignments.iter().any(|a| matches!(a.value, AssignValue::Hook { .. } | AssignValue::ExprHook { .. }));
    let hook_default = m.client_defaults.iter().any(|(_, d)| matches!(d, monorm_core::client_default::ClientDefault::Hook(_)));
    if m.hooks.is_some() || hook_assignment || hook_default {
        return Some("this model has host behavior (hooks), which the admin does not run yet".into());
    }
    None
}

/// The metadata of the form of a resource: whether it is writable, and what each field needs.
pub fn form_meta(schema: &Schema, r: &Resource) -> Value {
    let reason = read_only_reason(schema, r);
    let model = schema.model(r.model);
    let fields: Vec<Value> = r
        .detail
        .iter()
        .filter(|c| writable(c))
        .filter_map(|c| {
            let f = model.field(&c.field).ok()?;
            let key = model.ir.primary_key.contains(&f.name) || f.primary_key;
            let relation = r.relations.iter().find(|x| !x.field.is_empty() && x.field == c.field).map(Relation::meta);
            Some(json!({
                "name": c.label,
                "relation": relation,
                "file": r.files.contains(&c.field),
                "required": !f.nullable && !has_default(f),
                "default": has_default(f),
                "readonly_on_update": key,
                "readonly": r.form.readonly.contains(&c.label),
                "widget": r.form.widgets.get(&c.label).map_or_else(|| crate::meta::widget(c), String::as_str),
                "max_length": f.max_length,
            }))
        })
        .collect();
    let sections = json!({"create": r.form.create.iter().map(Section::json).collect::<Vec<_>>(), "edit": r.form.edit.iter().map(Section::json).collect::<Vec<_>>()});
    let chips: Vec<Value> = r.relations.iter().filter(|x| x.through.is_some()).map(Relation::meta).collect();
    json!({"writable": reason.is_none(), "read_only_reason": reason, "form": fields, "chips": chips, "sections": sections, "form_component": r.form.component.is_some()})
}

pub(crate) fn has_default(f: &FieldIr) -> bool {
    f.default.is_some() || f.default_now || f.default_sql.is_some() || f.client_default.is_some() || f.auto_increment
}

pub(crate) fn writable(c: &Column) -> bool {
    matches!(c.source, Source::Field) && c.path.is_empty()
}

/// Messages for fields, and messages for the form.
#[derive(Default)]
pub(crate) struct Problems {
    fields: BTreeMap<String, Vec<String>>,
    form: Vec<String>,
}

impl Problems {
    pub(crate) fn field(&mut self, name: &str, message: impl Into<String>) {
        self.fields.entry(name.to_owned()).or_default().push(message.into());
    }

    fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.form.is_empty()
    }

    pub(crate) fn response(self) -> Response {
        let message = "the values were not saved";
        (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"ok": false, "error": {"kind": "Invalid", "message": message}, "fields": self.fields, "form": self.form}))).into_response()
    }
}

pub(crate) struct Meta {
    pub(crate) request_id: String,
    pub(crate) ip: String,
    pub(crate) agent: String,
}

/// A checked request: the user, the transaction, the resource and the body.
pub(crate) struct Call<'a> {
    pub(crate) resource: &'a Resource,
    pub(crate) user: User,
    pub(crate) grants: Grants,
    pub(crate) scope: Scoped,
    pub(crate) meta: Meta,
    pub(crate) uri: Uri,
    pub(crate) body: Map<String, Value>,
}

/// The request id, the client and the user agent of a request, and its URI.
pub(crate) fn meta_of(s: &State_, req: &Request) -> (Meta, Uri) {
    let trusted: Vec<IpAddr> = s.bridge.as_ref().map(|(_, a)| a.security.options.trusted_proxies.clone()).unwrap_or_default();
    let meta = Meta {
        request_id: req.extensions().get::<RequestId>().map(|r| r.0.clone()).unwrap_or_default(),
        ip: crate::auth::client_ip(req, &trusted),
        agent: req.headers().get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned(),
    };
    (meta, req.uri().clone())
}

/// The steps that every write shares: the user (401), the resource (404), the permission (403),
/// a model that is read-only here (403). The body stays unread: the caller reads it.
pub(crate) async fn open_head<'a>(s: &'a State_, user: Option<Extension<User>>, name: &str, op: Op, (meta, uri): (Meta, Uri)) -> Result<Call<'a>, Box<Response>> {
    let (user, scope, grants) = authorize(s, user).await?;
    let Some(resource) = s.site.resource(name) else {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", &format!("no resource {name:?}"))));
    };
    if !resource.allow.contains(&op) {
        return Err(Box::new(deny(&s.site, scope, &user, &resource.name, json!({"reason": "allow"})).await));
    }
    if !s.site.allows(&grants, resource, op) {
        return Err(Box::new(deny(&s.site, scope, &user, &resource.name, json!({"needs": s.site.requires(resource, op)})).await));
    }
    if let Some(reason) = read_only_reason(&s.site.schema, resource) {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::FORBIDDEN, "ReadOnly", &reason)));
    }
    Ok(Call { resource, user, grants, scope, meta, uri, body: Map::new() })
}

/// `open_head`, then the JSON body (400, 413).
async fn open<'a>(s: &'a State_, user: Option<Extension<User>>, name: &str, op: Op, req: Request) -> Result<Call<'a>, Box<Response>> {
    let mut c = open_head(s, user, name, op, meta_of(s, &req)).await?;
    let bytes = match axum::body::to_bytes(req.into_body(), MAX_BODY).await {
        Ok(b) => b,
        Err(_) => {
            c.scope.rollback().await;
            return Err(Box::new(reject(StatusCode::PAYLOAD_TOO_LARGE, "PayloadTooLarge", "the body is too large")));
        }
    };
    let body = if bytes.iter().all(u8::is_ascii_whitespace) {
        Map::new()
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(m)) => m,
            _ => {
                c.scope.rollback().await;
                return Err(Box::new(reject(StatusCode::BAD_REQUEST, "BadRequest", "the body is a JSON object")));
            }
        }
    };
    c.body = body;
    Ok(c)
}

impl Call<'_> {
    async fn bad(self, message: &str) -> Response {
        self.scope.rollback().await;
        reject(StatusCode::BAD_REQUEST, "BadRequest", message)
    }

    /// The `set` object: field to value.
    fn set(&self) -> Result<Map<String, Value>, String> {
        match self.body.get("set") {
            Some(Value::Object(m)) => Ok(m.clone()),
            None if self.body.contains_key("links") => Ok(Map::new()),
            _ => Err("the body needs a \"set\" object".into()),
        }
    }

    fn loaded(&self) -> Map<String, Value> {
        match self.body.get("loaded") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        }
    }
}

/// A server error as a form message with the request id. The cause goes to the log, not to the client.
pub(crate) fn server_error(meta: &Meta, cause: &str) -> Response {
    let id = if meta.request_id.is_empty() { "unknown" } else { meta.request_id.as_str() };
    let message = format!("The record was not saved: an error occurred. Request id: {id}");
    let mut response = (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": {"kind": "ServerError", "message": message}, "fields": {}, "form": [message]}))).into_response();
    response.extensions_mut().insert(ErrorNote(cause.to_owned()));
    response
}

pub(crate) fn list_failure(meta: &Meta, e: ListError) -> Response {
    match e {
        ListError::Engine(m) => server_error(meta, &m),
        other => error_response(other),
    }
}

/// The failure of a statement: a constraint becomes a message for a field or the form; anything else a server error.
fn statement_failure(site: &Site, r: &Resource, meta: &Meta, written: &[String], deleting: bool, e: Error) -> Response {
    if let Error::Db(d) = &e {
        if let Some(p) = constraint_problems(&site.schema, r, written, deleting, d) {
            return p.response();
        }
    }
    if let Error::Value(m) = &e {
        let mut p = Problems::default();
        p.form.push(m.clone());
        return p.response();
    }
    match list::engine(e) {
        ListError::Engine(m) => server_error(meta, &m),
        other => error_response(other),
    }
}

/// The text between the first `open` and the next `close`.
fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

/// The columns of a unique or a foreign key failure: the DETAIL of PostgreSQL, or the message of SQLite.
fn failed_columns(d: &DbError) -> Vec<String> {
    let clean = |c: &str| c.trim().trim_matches('"').rsplit('.').next().unwrap_or_default().trim_matches('"').to_owned();
    if let Some(list) = d.detail.as_deref().and_then(|t| between(t, "Key (", ")=")) {
        return list.split(',').map(clean).collect();
    }
    match d.message.split_once("constraint failed: ") {
        Some((_, rest)) => rest.split(',').map(clean).collect(),
        None => Vec::new(),
    }
}

/// The SQLSTATE of an integrity failure. SQLite sets none for some of them: its message names the kind.
fn sqlstate(d: &DbError) -> Option<&str> {
    if d.sqlstate.is_some() {
        return d.sqlstate.as_deref();
    }
    [("UNIQUE constraint failed", "23505"), ("PRIMARY KEY constraint failed", "23505"), ("FOREIGN KEY constraint failed", "23503"), ("NOT NULL constraint failed", "23502"), ("CHECK constraint failed", "23514")].iter().find(|(m, _)| d.message.contains(m)).map(|(_, c)| *c)
}

fn constraint_problems(schema: &Schema, r: &Resource, written: &[String], deleting: bool, d: &DbError) -> Option<Problems> {
    if d.kind != ErrorKind::Integrity {
        return None;
    }
    let model = schema.model(r.model);
    let mut p = Problems::default();
    let field_of = |column: &str| model.fields().iter().find(|f| f.column == column).map(|f| f.name.clone()).filter(|n| r.detail.iter().any(|c| writable(c) && &c.label == n));
    match sqlstate(d) {
        Some("23505") => {
            let fields: Vec<String> = failed_columns(d).iter().filter_map(|c| field_of(c)).collect();
            if fields.is_empty() {
                p.form.push("A record with these values already exists.".into());
            }
            for f in fields {
                p.field(&f, "A record with this value already exists.");
            }
        }
        Some("23503" | "23001") => {
            if deleting {
                p.form.push("Other records still refer to this record, so it cannot be deleted.".into());
            } else {
                let mut fields: Vec<String> = failed_columns(d).iter().filter_map(|c| field_of(c)).collect();
                if fields.is_empty() {
                    // SQLite does not name the column: the one foreign key field of the write.
                    let keys: Vec<&String> = model.ir.relations.iter().filter(|rel| rel.foreign_key).flat_map(|rel| &rel.from).filter(|f| written.contains(f)).collect();
                    if let [only] = keys.as_slice() {
                        fields.push((*only).clone());
                    }
                }
                if fields.is_empty() {
                    p.form.push("A related record does not exist.".into());
                }
                for f in fields {
                    p.field(&f, "No such record.");
                }
            }
        }
        Some("23502") => {
            let column = between(&d.message, "column \"", "\"").map(str::to_owned).or_else(|| d.message.split_once("constraint failed: ").map(|(_, c)| c.rsplit('.').next().unwrap_or(c).trim().to_owned()));
            match column.as_deref().and_then(field_of) {
                Some(f) => p.field(&f, "This field is required."),
                None => p.form.push("A required value is missing.".into()),
            }
        }
        Some("23514") => {
            let named = model.ir.constraints.iter().find_map(|c| match c {
                ConstraintIr::Check { name: Some(n), .. } if d.constraint.as_deref() == Some(n) || d.message.contains(n.as_str()) => Some(n.clone()),
                _ => None,
            });
            if let Some(n) = named {
                p.form.push(r.check_messages.get(&n).cloned().unwrap_or_else(|| format!("The values break the rule {n}.")));
            } else {
                let column = model.fields().iter().find(|f| f.check.is_some() && written.contains(&f.name) && (d.constraint.as_deref().is_some_and(|c| c.contains(&format!("_{}_", f.column))) || d.message.contains(&f.column)));
                match column {
                    Some(f) => p.field(&f.name, "This value is not allowed."),
                    None => p.form.push("A value is not allowed.".into()),
                }
            }
        }
        _ => return None,
    }
    Some(p)
}

// ---- values ----

fn whole(v: &Value) -> Result<i64, String> {
    match v {
        Value::Number(n) => match n.as_i64() {
            Some(i) => Ok(i),
            None => n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 9.0e15).map(|f| f as i64).ok_or_else(|| "must be a whole number".to_owned()),
        },
        Value::String(t) => t.trim().parse().map_err(|_| "must be a whole number".to_owned()),
        _ => Err("must be a whole number".into()),
    }
}

fn enum_bind(schema: &Schema, idx: u32, f: &FieldIr, v: &Value) -> Result<Bind, String> {
    let e = schema.enums.get(idx as usize).ok_or("unknown enum")?;
    let given = match v {
        Value::String(t) => t.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Err("must be one of the listed values".into()),
    };
    let found = e.values.iter().find(|x| x.name == given || x.value.as_str() == Some(given.as_str()) || x.value.as_i64().is_some_and(|i| Some(i) == given.parse().ok())).filter(|x| f.enum_subset.as_ref().is_none_or(|s| s.contains(&x.name)));
    let Some(found) = found else { return Err("must be one of the listed values".into()) };
    Ok(match e.storage {
        EnumStorage::Int => Bind::Int(Some(i32::try_from(found.value.as_i64().ok_or("the enum value is not an integer")?).map_err(|_| "the enum value is too large")?)),
        _ => Bind::String(Some(found.value.as_str().unwrap_or(&found.name).to_owned())),
    })
}

fn scalar(schema: &Schema, f: &FieldIr, ty: ValueType, v: &Value) -> Result<Bind, String> {
    if let Some(idx) = ty.enum_idx {
        return enum_bind(schema, idx, f, v);
    }
    Ok(match ty.ty {
        ColType::Bool => Bind::Bool(Some(v.as_bool().ok_or("must be true or false")?)),
        ColType::Int => Bind::Int(Some(i32::try_from(whole(v)?).map_err(|_| "is out of range")?)),
        ColType::BigInt => Bind::BigInt(Some(whole(v)?)),
        ColType::Float => {
            let n = match v {
                Value::Number(n) => n.as_f64(),
                Value::String(t) => t.trim().parse::<f64>().ok(),
                _ => None,
            };
            Bind::Double(Some(n.filter(|n| n.is_finite()).ok_or("must be a number")?))
        }
        ColType::Decimal => {
            let text = match v {
                Value::Number(n) => n.to_string(),
                Value::String(t) => t.trim().to_owned(),
                _ => return Err("must be a number".into()),
            };
            Bind::BigDecimal(Some(Box::new(bigdecimal::BigDecimal::from_str(&text).map_err(|_| "must be a number")?)))
        }
        ColType::String | ColType::Text => {
            let t = v.as_str().ok_or("must be text")?;
            if let Some(max) = f.max_length {
                if t.chars().count() > max as usize {
                    return Err(format!("must have at most {max} characters"));
                }
            }
            Bind::String(Some(t.to_owned()))
        }
        ColType::DateTime => {
            let t = v.as_str().ok_or("must be a date and time")?.trim();
            let parsed = DateTime::parse_from_rfc3339(t).ok().or_else(|| {
                ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M"].iter().find_map(|fmt| NaiveDateTime::parse_from_str(t, fmt).ok()).map(|n| n.and_utc().fixed_offset())
            });
            Bind::ChronoDateTimeWithTimeZone(Some(parsed.ok_or("must be a date and time")?))
        }
        ColType::Date => Bind::ChronoDate(Some(NaiveDate::parse_from_str(v.as_str().ok_or("must be a date")?.trim(), "%Y-%m-%d").map_err(|_| "must be a date (YYYY-MM-DD)")?)),
        ColType::Uuid => Bind::Uuid(Some(v.as_str().ok_or("must be a UUID")?.trim().parse().map_err(|_| "must be a UUID")?)),
        ColType::Json => {
            if let Some(n) = f.nested.as_ref().filter(|n| n.storage == NestedStorage::Json) {
                shape(schema, &n.ty, n.list, v)?;
            }
            Bind::Json(Some(Box::new(v.clone())))
        }
    })
}

/// The value fits the nested type: known subfields, required subfields, and the type of each leaf.
fn shape(schema: &Schema, type_name: &str, list: bool, v: &Value) -> Result<(), String> {
    if list {
        let items = v.as_array().ok_or("must be a list")?;
        return items.iter().enumerate().try_for_each(|(i, item)| shape(schema, type_name, false, item).map_err(|e| format!("[{i}] {e}")));
    }
    let obj = v.as_object().ok_or("must be an object")?;
    let ty = schema.types.iter().find(|t| t.name == type_name).ok_or_else(|| format!("unknown type {type_name}"))?;
    if let Some(k) = obj.keys().find(|k| !ty.fields.iter().any(|f| &f.name == *k)) {
        return Err(format!("{k}: unknown subfield"));
    }
    for tf in &ty.fields {
        let at = |e: String| format!("{}: {e}", tf.name);
        match obj.get(&tf.name) {
            None | Some(Value::Null) => {
                let default = tf.leaf.as_ref().is_some_and(has_default);
                if !tf.nullable && !default {
                    return Err(at("is required".into()));
                }
            }
            Some(x) => match (&tf.type_name, &tf.leaf) {
                (Some(t), _) => shape(schema, t, tf.list, x).map_err(at)?,
                (None, Some(leaf)) if tf.list => x.as_array().ok_or_else(|| at("must be a list".into()))?.iter().try_for_each(|item| scalar(schema, leaf, leaf.value_type().element(), item).map(|_| ()).map_err(at))?,
                (None, Some(leaf)) => {
                    scalar(schema, leaf, leaf.value_type(), x).map_err(at)?;
                }
                _ => {}
            },
        }
    }
    Ok(())
}

/// A JSON value as the bind value of `f`; null is the typed null of a nullable field.
pub(crate) fn convert(schema: &Schema, f: &FieldIr, v: &Value) -> Result<Bind, String> {
    let ty = f.value_type();
    if v.is_null() {
        return if f.nullable { Ok(monorm_engine::params::null_of(Some(ty))) } else { Err("This field is required.".into()) };
    }
    if ty.array {
        let items = v.as_array().ok_or("must be a list")?;
        let mut out = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            out.push(scalar(schema, f, ty.element(), item).map_err(|e| format!("item {}: {e}", i + 1))?);
        }
        return Ok(Bind::Array(monorm_engine::params::array_type(ty.ty), Some(Box::new(out))));
    }
    scalar(schema, f, ty, v)
}

/// The fields of a `set`, as bind values. A field that the form does not have, or a message, goes to `problems`.
fn convert_set(site: &Site, r: &Resource, set: &Map<String, Value>, updating: bool, problems: &mut Problems) -> Vec<(String, Bind)> {
    let model = site.schema.model(r.model);
    let mut out = Vec::new();
    for (name, v) in set {
        let Some(c) = r.detail.iter().find(|c| writable(c) && &c.label == name) else {
            problems.field(name, "This field cannot be written.");
            continue;
        };
        let Ok(f) = model.field(&c.field) else {
            problems.field(name, "This field cannot be written.");
            continue;
        };
        if r.form.readonly.contains(name) {
            problems.field(name, "This field is read-only.");
            continue;
        }
        if updating && (f.primary_key || model.ir.primary_key.contains(&f.name)) {
            problems.field(name, "The key of a record cannot change.");
            continue;
        }
        match convert(&site.schema, f, v) {
            Ok(b) => out.push((name.clone(), b)),
            Err(m) => problems.field(name, m.strip_suffix('.').map_or_else(|| capitalize(&m), capitalize)),
        }
    }
    out
}

fn capitalize(m: &str) -> String {
    let mut c = m.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

// ---- audit ----

fn user_key_text(user: &User) -> String {
    match &user.0["key"] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The first text value that is not a key: the record in words. Else the key.
fn record_label(r: &Resource, cells: &[Value], key_text: &str) -> String {
    let label = r.detail.iter().zip(cells).find(|(c, v)| !r.key_columns.iter().any(|k| k.field == c.field) && matches!(c.ty.ty, ColType::String | ColType::Text) && !c.ty.array && c.ty.enum_idx.is_none() && v.as_str().is_some_and(|t| !t.is_empty())).and_then(|(_, v)| v.as_str());
    label.unwrap_or(key_text).chars().take(LABEL_WIDTH).collect()
}

pub(crate) async fn write_audit(s: &State_, c: &Call<'_>, action: &str, key_text: &str, cells: &[Value], diff: Value) -> Result<(), String> {
    let changes = json!({"request_id": c.meta.request_id, "ip": c.meta.ip, "user_agent": c.meta.agent, "diff": diff});
    let entry = Entry { user_key: &user_key_text(&c.user), action, resource: &c.resource.name, record_key: key_text, record_label: &record_label(c.resource, cells, key_text), changes: Some(changes) };
    audit::record(&s.site, c.scope.executor(), entry).await
}

pub(crate) async fn finish(c: Call<'_>, response: Response) -> Response {
    match c.scope.commit().await {
        Ok(()) => response,
        Err(e) => server_error(&c.meta, &e.message),
    }
}


// ---- relation fields ----

/// The chips of one relation that a write changes: the keys to add and to remove, canonical and without repeats.
struct Link<'a> {
    relation: &'a Relation,
    add: Vec<Value>,
    remove: Vec<Value>,
}

/// The `links` of a body: `{<relation>: {"add": [keys], "remove": [keys]}}`. A wrong shape is a bad request, a wrong value a field problem.
fn parse_links<'a>(s: &State_, resource: &'a Resource, body: &Map<String, Value>, problems: &mut Problems) -> Result<Vec<Link<'a>>, String> {
    let Some(links) = body.get("links") else { return Ok(Vec::new()) };
    let Value::Object(links) = links else { return Err("\"links\" is an object".into()) };
    let mut out = Vec::new();
    for (name, sides) in links {
        let Some(relation) = resource.relations.iter().find(|r| &r.name == name && r.through.is_some()) else {
            problems.field(name, "This field cannot be written.");
            continue;
        };
        let Value::Object(sides) = sides else { return Err(format!("links.{name} is an object with \"add\" and \"remove\"")) };
        if let Some(extra) = sides.keys().find(|k| *k != "add" && *k != "remove") {
            return Err(format!("links.{name} has no {extra:?}"));
        }
        let mut side = |key: &str| -> Result<Vec<Value>, String> {
            let keys = match sides.get(key) {
                None => return Ok(Vec::new()),
                Some(Value::Array(a)) => a,
                Some(_) => return Err(format!("links.{name}.{key} is a list of keys")),
            };
            if keys.len() > LINKS_PER_WRITE {
                problems.field(name, format!("Change at most {LINKS_PER_WRITE} links in one save."));
                return Ok(Vec::new());
            }
            let mut out: Vec<Value> = Vec::new();
            for k in keys {
                match relations::canonical(s, relation, k) {
                    Ok(c) if !out.contains(&c) => out.push(c),
                    Ok(_) => {}
                    Err(_) => problems.field(name, "A key is not valid."),
                }
            }
            Ok(out)
        };
        let (add, remove) = (side("add")?, side("remove")?);
        if add.iter().any(|k| remove.contains(k)) {
            problems.field(name, "A record is in both lists.");
        }
        out.push(Link { relation, add, remove });
    }
    Ok(out)
}

/// The picker values of the body that must be rows the list offers: `only` limits them to the changed fields.
async fn check_picks(s: &State_, c: &Call<'_>, set: &Map<String, Value>, only: Option<&[String]>, problems: &mut Problems) -> Result<(), Response> {
    let resource = c.resource;
    for relation in resource.relations.iter().filter(|r| !r.field.is_empty()) {
        let Some(col) = resource.detail.iter().find(|d| d.field == relation.field) else { continue };
        let Some(v) = set.get(&col.label).filter(|v| !v.is_null()) else { continue };
        if only.is_some_and(|names| !names.contains(&col.label)) || problems.fields.contains_key(&col.label) {
            continue;
        }
        let Ok(key) = relations::canonical(s, relation, v) else { continue };
        let found = async {
            let rows = relations::offer(s, relation, &c.user, &c.scope, true).await?;
            relations::offered(s, c.scope.executor(), relation, &rows, std::slice::from_ref(&key)).await
        }
        .await
        .map_err(|e| list_failure(&c.meta, e))?;
        if found.is_empty() {
            problems.field(&col.label, "Choose a record that the list offers.");
        }
    }
    Ok(())
}

/// A plain `set` cannot write a file field: the upload route does.
fn refuse_files(resource: &Resource, set: &Map<String, Value>, problems: &mut Problems) {
    for col in resource.detail.iter().filter(|d| resource.files.contains(&d.field)) {
        if set.contains_key(&col.label) {
            problems.field(&col.label, "Use the upload to change a file.");
        }
    }
}

/// Adds and removes the links in the request transaction. The result is the audit diff of the links that changed.
async fn apply_links(s: &State_, c: &Call<'_>, links: &[Link<'_>], source: &Value, problems: &mut Problems) -> Result<Map<String, Value>, Response> {
    let (site, resource) = (&s.site, c.resource);
    let mut diff = Map::new();
    for link in links {
        let (rel, through) = (link.relation, link.relation.through.as_ref().expect("chips have a join model"));
        let target = site.resource(&rel.target).expect("`build` checked the target");
        let (skey, tkey) = (&resource.key_columns[0], &target.key_columns[0]);
        let failed = |e: ListError| list_failure(&c.meta, e);
        if !link.add.is_empty() {
            let rows = relations::offer(s, rel, &c.user, &c.scope, true).await.map_err(failed)?;
            let found = relations::offered(s, c.scope.executor(), rel, &rows, &link.add).await.map_err(failed)?;
            if link.add.iter().any(|k| !found.iter().any(|(f, _)| json::same(tkey.ty.ty, f, k))) {
                problems.field(&rel.name, "Choose records that the list offers.");
                continue;
            }
        }
        let both: Vec<Value> = link.add.iter().chain(&link.remove).cloned().collect();
        let have = relations::current(s, c.scope.executor(), rel, resource, source, Some(&both)).await.map_err(failed)?;
        let has = |k: &Value| have.iter().any(|h| json::same(tkey.ty.ty, h, k));
        let to_add: Vec<&Value> = link.add.iter().filter(|k| !has(k)).collect();
        let to_remove: Vec<&Value> = link.remove.iter().filter(|k| has(k)).collect();
        let bind = |ty: ColType, v: &Value| json::bind(ty, v);
        let from = bind(skey.ty.ty, source).map_err(|m| server_error(&c.meta, &m))?;
        if !to_add.is_empty() {
            let mut rows = Vec::new();
            for k in &to_add {
                rows.push(vec![Some(from.clone()), Some(bind(tkey.ty.ty, k).map_err(|m| server_error(&c.meta, &m))?)]);
            }
            let bypass = if through.scope_defaults.is_empty() { ir::Bypass::default() } else { ir::Bypass::Names(through.scope_defaults.clone()) };
            let fields = [through.source.clone(), through.target.clone()];
            let planned = exec::plan_inserts(&site.schema, s.target, &through.model, &fields, rows, None, &NoParams, None, false, None, &bypass);
            let ran = match planned {
                Ok(plans) => exec::run_inserts(c.scope.executor(), s.target, plans, false, false).await,
                Err(e) => Err(e),
            };
            if let Err(e) = ran {
                return Err(server_error(&c.meta, &e.to_string()));
            }
        }
        if !to_remove.is_empty() {
            let mut binds = Binds::default();
            let one = binds.push(from.clone());
            let mut params = Vec::new();
            for k in &to_remove {
                params.push(binds.push(bind(tkey.ty.ty, k).map_err(|m| server_error(&c.meta, &m))?));
            }
            let mut op = json!({"op": "delete", "model": through.model, "filters": [
                {"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": through.source}, "r": one},
                {"t": "in", "item": {"t": "col", "path": [], "name": through.target}, "values": params},
            ]});
            if !through.scope_defaults.is_empty() {
                op["without_defaults"] = json!(through.scope_defaults);
            }
            if let Err(e) = run_mutation(site, c, s.target, op, binds).await {
                return Err(server_error(&c.meta, &e.to_string()));
            }
        }
        if !to_add.is_empty() || !to_remove.is_empty() {
            diff.insert(format!("links.{}", rel.name), json!({"added": to_add, "removed": to_remove}));
        }
    }
    Ok(diff)
}

// ---- handlers ----

/// The key of a record from its row of detail cells.
fn key_of(r: &Resource, cells: &[Value]) -> Vec<Value> {
    r.key_columns.iter().filter_map(|k| r.detail.iter().position(|d| d.field == k.field).map(|p| cells[p].clone())).collect()
}

pub(crate) async fn create(State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, req: Request) -> Response {
    let c = match open(&s, user, &name, Op::Create, req).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let set = match c.set() {
        Ok(m) => m,
        Err(m) => return c.bad(&m).await,
    };
    let (site, resource) = (&s.site, c.resource);
    let model = site.schema.model(resource.model);
    let mut problems = Problems::default();
    let values = convert_set(site, resource, &set, false, &mut problems);
    refuse_files(resource, &set, &mut problems);
    let links = match parse_links(&s, resource, &c.body, &mut problems) {
        Ok(l) => l,
        Err(m) => return c.bad(&m).await,
    };
    for col in resource.detail.iter().filter(|col| writable(col)) {
        if let Ok(f) = model.field(&col.field) {
            if !f.nullable && !has_default(f) && !set.contains_key(&col.label) {
                problems.field(&col.label, "This field is required.");
            }
        }
    }
    if problems.is_empty() {
        if let Err(r) = check_picks(&s, &c, &set, None, &mut problems).await {
            c.scope.rollback().await;
            return r;
        }
    }
    if !problems.is_empty() {
        c.scope.rollback().await;
        return problems.response();
    }
    let names: Vec<String> = values.iter().map(|(n, _)| n.clone()).collect();
    let fields: Vec<String> = values.iter().filter_map(|(n, _)| resource.detail.iter().find(|d| &d.label == n).map(|d| d.field.clone())).collect();
    let row: Vec<Option<Bind>> = values.into_iter().map(|(_, b)| Some(b)).collect();
    let bypass = if resource.scope_defaults.is_empty() { ir::Bypass::default() } else { ir::Bypass::Names(resource.scope_defaults.clone()) };
    let planned = exec::plan_inserts(&site.schema, s.target, &model.ir.name, &fields, vec![row], None, &NoParams, None, true, None, &bypass);
    let outcome = match planned {
        Ok(plans) => exec::run_inserts(c.scope.executor(), s.target, plans, false, true).await,
        Err(e) => Err(e),
    };
    let cells = match outcome {
        Ok(Outcome::Rows { rows, types, shape: None, .. }) if !rows.is_empty() => {
            let mut cells = Vec::new();
            for col in &resource.detail {
                let cell = model.field_pos(&col.field).map_err(Error::query).and_then(|pos| rows.cell(0, pos, types[pos]).map(|cell| json::cell(&site.schema, types[pos], cell)).map_err(Error::Db));
                match cell {
                    Ok(v) => cells.push(v),
                    Err(e) => {
                        let response = server_error(&c.meta, &e.to_string());
                        c.scope.rollback().await;
                        return response;
                    }
                }
            }
            cells
        }
        Ok(_) => {
            let response = server_error(&c.meta, "the insert returned no row");
            c.scope.rollback().await;
            return response;
        }
        Err(e) => {
            let response = statement_failure(site, resource, &c.meta, &names, false, e);
            c.scope.rollback().await;
            return response;
        }
    };
    let new_key = key_of(resource, &cells);
    let key_text = key::encode(&new_key);
    let mut diff: Map<String, Value> = resource.detail.iter().zip(&cells).filter(|(_, v)| !v.is_null()).map(|(d, v)| (d.label.clone(), json!({"from": null, "to": v}))).collect();
    if !links.is_empty() {
        let source = new_key.first().cloned().unwrap_or(Value::Null);
        match apply_links(&s, &c, &links, &source, &mut problems).await {
            Ok(d) => diff.extend(d),
            Err(r) => {
                c.scope.rollback().await;
                return r;
            }
        }
        if !problems.is_empty() {
            c.scope.rollback().await;
            return problems.response();
        }
    }
    if let Err(m) = write_audit(&s, &c, "create", &key_text, &cells, Value::Object(diff)).await {
        let response = server_error(&c.meta, &m);
        c.scope.rollback().await;
        return response;
    }
    let body = Json(json!({"ok": true, "resource": resource.name, "key": key_text, "values": cells}));
    let response = (StatusCode::CREATED, body).into_response();
    finish(c, response).await
}

/// The key segment of `/resources/<name>/<key>`, decoded to the values of the key columns.
fn key_of_path(r: &Resource, uri: &Uri) -> Result<(String, Vec<Value>), String> {
    decode_key(r, crate::server::segment(uri, 0))
}

/// The key values of a raw key segment, with the segment itself.
pub(crate) fn decode_key(r: &Resource, raw: &str) -> Result<(String, Vec<Value>), String> {
    let types: Vec<_> = r.key_columns.iter().map(|c| c.ty.ty).collect();
    Ok((raw.to_owned(), key::decode(&types, raw)?))
}

/// A client value as the JSON of a stored cell of the column: `5` and `"5"` for an int both read `5`. A value that does not convert stays as sent.
pub(crate) fn as_cell(site: &Site, r: &Resource, d: &Column, v: &Value) -> Value {
    let Ok(f) = site.schema.model(r.model).field(&d.field) else { return v.clone() };
    convert(&site.schema, f, v).map_or_else(|_| v.clone(), |b| json::bind_json(&site.schema, f.value_type(), &b))
}

/// The loaded values that no longer match the stored row: field to `{loaded, current}`. A value counts by what it stores, not by how the client wrote it.
fn conflicts(site: &Site, r: &Resource, cells: &[Value], loaded: &Map<String, Value>, fields: &[String]) -> Map<String, Value> {
    let mut out = Map::new();
    for name in fields {
        let Some(at) = r.detail.iter().position(|d| &d.label == name) else { continue };
        if let Some(was) = loaded.get(name) {
            if !json::same(r.detail[at].ty.ty, &as_cell(site, r, &r.detail[at], was), &cells[at]) {
                out.insert(name.clone(), json!({"loaded": was, "current": cells[at]}));
            }
        }
    }
    out
}

/// The converted fields whose stored value really changes: name, bind value and the new value as the JSON of a cell.
fn changed_values(site: &Site, r: &Resource, values: Vec<(String, Bind)>, cells: &[Value]) -> Vec<(String, Bind, Value)> {
    let model = site.schema.model(r.model);
    values
        .into_iter()
        .filter_map(|(n, b)| {
            let at = r.detail.iter().position(|d| d.label == n)?;
            let to = json::bind_json(&site.schema, model.field(&r.detail[at].field).ok()?.value_type(), &b);
            (!json::same(r.detail[at].ty.ty, &to, &cells[at])).then_some((n, b, to))
        })
        .collect()
}

fn conflict_response(map: Map<String, Value>) -> Response {
    (StatusCode::CONFLICT, Json(json!({"ok": false, "error": {"kind": "Conflict", "message": "the record changed since the form loaded"}, "conflicts": map}))).into_response()
}

pub(crate) async fn update(State(s): Shared, user: Option<Extension<User>>, Path((name, _)): Path<(String, String)>, req: Request) -> Response {
    let c = match open(&s, user, &name, Op::Update, req).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let (site, resource) = (&s.site, c.resource);
    let model = site.schema.model(resource.model);
    let set = match c.set() {
        Ok(m) => m,
        Err(m) => return c.bad(&m).await,
    };
    let loaded = c.loaded();
    let (raw, key) = match key_of_path(resource, &c.uri) {
        Ok(k) => k,
        Err(m) => return c.bad(&m).await,
    };
    let guards: Vec<String> = model.guards.iter().filter_map(|&p| model.fields().get(p)).filter_map(|f| resource.detail.iter().find(|d| d.field == f.name && writable(d)).map(|d| d.label.clone())).collect();
    if let Some(g) = guards.iter().find(|g| !loaded.contains_key(*g)) {
        return c.bad(&format!("\"loaded\" needs the guard field {g:?}")).await;
    }
    if let Some(n) = set.keys().find(|n| !loaded.contains_key(*n) && resource.detail.iter().any(|d| &d.label == *n && writable(d))) {
        return c.bad(&format!("\"loaded\" needs the value of {n:?}")).await;
    }
    let mut problems = Problems::default();
    let values = convert_set(site, resource, &set, true, &mut problems);
    refuse_files(resource, &set, &mut problems);
    let links = match parse_links(&s, resource, &c.body, &mut problems) {
        Ok(l) => l,
        Err(m) => return c.bad(&m).await,
    };
    if !problems.is_empty() {
        c.scope.rollback().await;
        return problems.response();
    }
    let (scope, reach) = match reach(&s, resource, &c.user, &c.grants, Vec::new(), c.scope).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let c = Call { scope, ..c };
    let record = match list::read_record(site, resource, c.scope.executor(), s.target, &key, &reach.rows, true).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            c.scope.rollback().await;
            return reject(StatusCode::NOT_FOUND, "NotFound", "no such record");
        }
        Err(e) => {
            let response = list_failure(&c.meta, e);
            c.scope.rollback().await;
            return response;
        }
    };
    if record.flags.get("change") == Some(&Value::Bool(false)) {
        return deny(site, c.scope, &c.user, &resource.name, json!({"reason": "can", "rule": "change"})).await;
    }
    let mut compared: Vec<String> = set.keys().cloned().collect();
    compared.extend(guards);
    let found = conflicts(site, resource, &record.cells, &loaded, &compared);
    if !found.is_empty() {
        c.scope.rollback().await;
        return conflict_response(found);
    }
    let changed = changed_values(site, resource, values, &record.cells);
    let changed_names: Vec<String> = changed.iter().map(|(n, _, _)| n.clone()).collect();
    if let Err(r) = check_picks(&s, &c, &set, Some(&changed_names), &mut problems).await {
        c.scope.rollback().await;
        return r;
    }
    if !problems.is_empty() {
        c.scope.rollback().await;
        return problems.response();
    }
    if changed.is_empty() && links.is_empty() {
        let response = Json(json!({"ok": true, "key": raw, "changed": []})).into_response();
        return finish(c, response).await;
    }
    if !changed.is_empty() {
        if let Err(r) = run_update(&s, &c, &reach, &key, &changed).await {
            c.scope.rollback().await;
            return r;
        }
    }
    let mut diff: Map<String, Value> = changed.iter().filter_map(|(n, _, to)| resource.detail.iter().position(|d| &d.label == n).map(|at| (n.clone(), json!({"from": record.cells[at], "to": to})))).collect();
    if !links.is_empty() {
        match apply_links(&s, &c, &links, &key[0], &mut problems).await {
            Ok(d) => diff.extend(d),
            Err(r) => {
                c.scope.rollback().await;
                return r;
            }
        }
        if !problems.is_empty() {
            c.scope.rollback().await;
            return problems.response();
        }
    }
    if diff.is_empty() {
        let response = Json(json!({"ok": true, "key": raw, "changed": []})).into_response();
        return finish(c, response).await;
    }
    if let Err(m) = write_audit(&s, &c, "update", &raw, &record.cells, Value::Object(diff.clone())).await {
        let response = server_error(&c.meta, &m);
        c.scope.rollback().await;
        return response;
    }
    let names: Vec<&String> = diff.keys().collect();
    let response = Json(json!({"ok": true, "key": raw, "changed": names})).into_response();
    finish(c, response).await
}

pub(crate) async fn destroy(State(s): Shared, user: Option<Extension<User>>, Path((name, _)): Path<(String, String)>, req: Request) -> Response {
    let c = match open(&s, user, &name, Op::Delete, req).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let (site, resource) = (&s.site, c.resource);
    let model = site.schema.model(resource.model);
    let loaded = c.loaded();
    let (raw, key) = match key_of_path(resource, &c.uri) {
        Ok(k) => k,
        Err(m) => return c.bad(&m).await,
    };
    let (scope, reach) = match reach(&s, resource, &c.user, &c.grants, Vec::new(), c.scope).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let c = Call { scope, ..c };
    let record = match list::read_record(site, resource, c.scope.executor(), s.target, &key, &reach.rows, true).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            c.scope.rollback().await;
            return reject(StatusCode::NOT_FOUND, "NotFound", "no such record");
        }
        Err(e) => {
            let response = list_failure(&c.meta, e);
            c.scope.rollback().await;
            return response;
        }
    };
    if record.flags.get("delete") == Some(&Value::Bool(false)) {
        return deny(site, c.scope, &c.user, &resource.name, json!({"reason": "can", "rule": "delete"})).await;
    }
    let compared: Vec<String> = loaded.keys().cloned().collect();
    let found = conflicts(site, resource, &record.cells, &loaded, &compared);
    if !found.is_empty() {
        c.scope.rollback().await;
        return conflict_response(found);
    }
    let mut binds = Binds::default();
    let filter = match list::key_filter(resource, &key, &mut binds) {
        Ok(f) => f,
        Err(e) => {
            let response = list_failure(&c.meta, e);
            c.scope.rollback().await;
            return response;
        }
    };
    let mut filters = vec![filter];
    filters.extend(reach.rows.push(&mut binds));
    let mut op = json!({"op": "delete", "model": model.ir.name, "filters": filters});
    reach.rows.apply(&mut op);
    match run_mutation(site, &c, s.target, op, binds).await {
        Ok(Outcome::Affected(1)) => {}
        Ok(_) => {
            c.scope.rollback().await;
            return reject(StatusCode::NOT_FOUND, "NotFound", "no such record");
        }
        Err(e) => {
            let response = statement_failure(site, resource, &c.meta, &[], true, e);
            c.scope.rollback().await;
            return response;
        }
    }
    let diff: Map<String, Value> = resource.detail.iter().zip(&record.cells).filter(|(_, v)| !v.is_null()).map(|(d, v)| (d.label.clone(), json!({"from": v, "to": null}))).collect();
    if let Err(m) = write_audit(&s, &c, "delete", &raw, &record.cells, Value::Object(diff)).await {
        let response = server_error(&c.meta, &m);
        c.scope.rollback().await;
        return response;
    }
    let response = Json(json!({"ok": true, "key": raw})).into_response();
    finish(c, response).await
}

/// The UPDATE of the changed fields of one record, with the `queryset` filter and the `can=` rule. The caller rolls back on an error.
pub(crate) async fn run_update(s: &State_, c: &Call<'_>, reach: &crate::server::Reach, key: &[Value], changed: &[(String, Bind, Value)]) -> Result<(), Response> {
    let (site, resource) = (&s.site, c.resource);
    let model = site.schema.model(resource.model);
    let mut binds = Binds::default();
    let filter = list::key_filter(resource, key, &mut binds).map_err(|e| list_failure(&c.meta, e))?;
    let mut filters = vec![filter];
    let mut assignments = Vec::new();
    let mut written = Vec::new();
    for (n, b, _) in changed {
        let field = resource.detail.iter().find(|d| &d.label == n).map(|d| d.field.clone()).unwrap_or_default();
        assignments.push(json!({"field": field, "value": binds.push(b.clone())}));
        written.push(field);
    }
    filters.extend(reach.rows.push(&mut binds));
    let mut op = json!({"op": "update", "model": model.ir.name, "filters": filters, "set": assignments});
    reach.rows.apply(&mut op);
    match run_mutation(site, c, s.target, op, binds).await {
        Ok(Outcome::Affected(1)) => Ok(()),
        Ok(_) => Err(reject(StatusCode::NOT_FOUND, "NotFound", "no such record")),
        Err(e) => Err(statement_failure(site, resource, &c.meta, &written, false, e)),
    }
}

/// Plans and runs an update or a delete on the request transaction.
async fn run_mutation(site: &Site, c: &Call<'_>, target: monorm_core::dialect::Target, op: Value, binds: Binds) -> Result<Outcome, Error> {
    let op: Operation = serde_json::from_value(op).map_err(|e| Error::query(e.to_string()))?;
    let plan = Planner::plan(&site.schema, target, &op, &binds)?;
    exec::run_in(c.scope.executor(), target, plan, false).await
}

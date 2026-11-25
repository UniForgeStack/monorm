//! Writes that run in host code: a model with host behavior, a protected model that the
//! resource opens, and a resource with a service (`create`, `update`, `delete`).
//!
//! The admin checks the user, the permission, the field rules, the `queryset` filter and the `can=`
//! rule. The default writer (`admin.write`) runs in the transaction of the request: the row lock, the
//! write and the audit row commit or roll back together, and `db_settings` and row-level security apply.
//! A service function owns its transactions: the admin rolls its transaction back and calls it with none.
//! The host answers with an envelope that this module maps to 201, 200, 404, 409, 422 or 500. The audit
//! row is the host's (`ctx.audit()`, or the default writer) or, when the host wrote none, the admin's.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use monorm_core::schema::Schema;
use monorm_core::writes::AssignValue;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::audit::{self, Entry};
use crate::bridge::{BridgeError, CallContext};
use crate::key;
use crate::list;
use crate::permissions::Op;
use crate::scope::Scoped;
use crate::server::{reach, reject, State_, User};
use crate::error::Fault;
use crate::site::{err, err_in, Column, Resource};
use crate::write::{audit_changes, conflicts, finish, list_failure, server_error, write_audit_with, Call, Problems};

/// One service function of a resource. `sync` is a plain Python function; it runs in a thread.
#[derive(Deserialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ServiceOp {
    #[serde(default)]
    pub sync: bool,
    /// A sync function that the author marked as threaded: it runs in a thread, and its async database calls go through `ctx.run`.
    #[serde(default)]
    pub threaded: bool,
}

#[derive(Deserialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub create: Option<ServiceOp>,
    pub update: Option<ServiceOp>,
    pub delete: Option<ServiceOp>,
}

impl ServiceConfig {
    pub fn get(&self, op: Op) -> Option<&ServiceOp> {
        match op {
            Op::Create => self.create.as_ref(),
            Op::Update => self.update.as_ref(),
            Op::Delete => self.delete.as_ref(),
            Op::Read => None,
        }
    }

    fn any(&self) -> bool {
        self.create.is_some() || self.update.is_some() || self.delete.is_some()
    }
}

/// A control that the form shows next to the fields. Only a service reads its value.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    pub name: String,
    #[serde(rename = "type", default = "text")]
    pub kind: String,
    pub label: Option<String>,
    #[serde(default)]
    pub required: bool,
    pub help: Option<String>,
    #[serde(default)]
    pub choices: Vec<String>,
    /// The audit row stores a mask, not the value.
    #[serde(default)]
    pub secret: bool,
}

/// What the audit row stores for the value of a secret input.
pub const SECRET_MASK: &str = "***";

fn text() -> String {
    "text".into()
}

pub const INPUT_KINDS: [&str; 5] = ["text", "textarea", "number", "checkbox", "select"];
/// An action input may also hold a file or a password.
pub const ACTION_INPUT_KINDS: [&str; 7] = ["text", "textarea", "number", "checkbox", "select", "file", "password"];

impl InputConfig {
    pub fn meta(&self) -> Value {
        json!({"name": self.name, "type": self.kind, "label": self.label.clone().unwrap_or_else(|| crate::meta::humanize(&self.name)), "required": self.required, "help": self.help, "choices": self.choices})
    }
}

/// The inputs as the audit row stores them: the value of a secret input is the mask. An input that the request left out stays out.
pub(crate) fn audit_inputs(r: &Resource, inputs: &Map<String, Value>) -> Map<String, Value> {
    audit_input_list(&r.inputs, inputs)
}

pub(crate) fn audit_input_list(list: &[InputConfig], inputs: &Map<String, Value>) -> Map<String, Value> {
    inputs.iter().map(|(k, v)| (k.clone(), if !v.is_null() && list.iter().any(|i| &i.name == k && (i.secret || i.kind == "password")) { Value::String(SECRET_MASK.into()) } else { v.clone() })).collect()
}

/// The checks of `Site::build` for the service and the inputs of one resource.
pub fn check_config(name: &str, detail: &[Column], service: &ServiceConfig, inputs: &[InputConfig]) -> Result<(), Fault> {
    if !inputs.is_empty() && !service.any() {
        return err_in(name, "inputs need a service (create, update or delete): only a service reads an input".to_string());
    }
    for (op, s) in [("create", &service.create), ("update", &service.update), ("delete", &service.delete)] {
        if s.as_ref().is_some_and(|s| s.sync && !s.threaded) {
            return err_in(name, format!("the {op} service is a sync function. It runs in a thread, so it cannot use the async database API of the transaction. Make it async, or mark it with admin.threaded(...): it then runs in a thread, and its async database calls go through ctx.run(...)"));
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    for i in inputs {
        if i.name.is_empty() || !i.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return err_in(name, format!("input {:?} has a name of letters, digits and _", i.name));
        }
        if seen.contains(&i.name.as_str()) {
            return err_in(name, format!("input {:?} is declared twice", i.name));
        }
        if detail.iter().any(|d| d.label == i.name) {
            return err_in(name, format!("input {:?} has the name of a field", i.name));
        }
        if !INPUT_KINDS.contains(&i.kind.as_str()) {
            return err_in(name, format!("input {:?} has type {:?}, not one of {}", i.name, i.kind, INPUT_KINDS.join(", ")));
        }
        if (i.kind == "select") == i.choices.is_empty() {
            return err_in(name, format!("input {:?}: choices go with the type select, and only with it", i.name));
        }
        seen.push(&i.name);
    }
    Ok(())
}

/// The checks of the inputs of one action.
pub fn check_action_inputs(resource: &str, action: &str, inputs: &[InputConfig]) -> Result<(), Fault> {
    let mut seen: Vec<&str> = Vec::new();
    for i in inputs {
        let at = format!("{resource}: action {action:?}: input {:?}", i.name);
        if i.name.is_empty() || !i.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return err(format!("{at} has a name of letters, digits and _"));
        }
        if seen.contains(&i.name.as_str()) {
            return err(format!("{at} is declared twice"));
        }
        if !ACTION_INPUT_KINDS.contains(&i.kind.as_str()) {
            return err(format!("{at} has type {:?}, not one of {}", i.kind, ACTION_INPUT_KINDS.join(", ")));
        }
        if (i.kind == "select") == i.choices.is_empty() {
            return err(format!("{at}: choices go with the type select, and only with it"));
        }
        if i.kind == "file" && i.secret {
            return err(format!("{at}: a file input cannot be secret"));
        }
        seen.push(&i.name);
    }
    Ok(())
}

/// The model has host behavior in its schema: hooks, a hook assignment or a hook default.
pub fn schema_host_behavior(schema: &Schema, model: usize) -> bool {
    let m = schema.model(model);
    let hook_assignment = m.assignments.iter().any(|a| matches!(a.value, AssignValue::Hook { .. } | AssignValue::ExprHook { .. }));
    let hook_default = m.client_defaults.iter().any(|(_, d)| matches!(d, monorm_core::client_default::ClientDefault::Hook(_)));
    m.hooks.is_some() || hook_assignment || hook_default
}

/// The host function that runs a write, or none when the admin writes the plain way.
pub fn route(schema: &Schema, r: &Resource, op: Op) -> Option<String> {
    if r.service.get(op).is_some() {
        return Some(format!("service.{}.{}", r.name, op_name(op)));
    }
    let protected = schema.rules_for_model(r.model).any(|rule| rule.layer.app() || rule.skippable);
    (r.host_behavior || schema_host_behavior(schema, r.model) || (protected && r.allow_protected)).then(|| "admin.write".to_owned())
}

pub fn op_name(op: Op) -> &'static str {
    match op {
        Op::Create => "create",
        Op::Update => "update",
        Op::Delete => "delete",
        Op::Read => "read",
    }
}

/// The values of the `inputs` object of a body: only declared names, each of its type. A problem names the input.
pub(crate) fn parse_inputs(r: &Resource, body: &Map<String, Value>, problems: &mut Problems) -> Result<Map<String, Value>, String> {
    parse_input_list(&r.inputs, body, problems)
}

/// `parse_inputs` for any list of inputs: the list of a resource or of an action.
pub(crate) fn parse_input_list(list: &[InputConfig], body: &Map<String, Value>, problems: &mut Problems) -> Result<Map<String, Value>, String> {
    let given = match body.get("inputs") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err("\"inputs\" is an object".into()),
    };
    let mut out = Map::new();
    for name in given.keys().filter(|n| !list.iter().any(|i| &i.name == *n)) {
        problems.field(name, "This input does not exist.");
    }
    for i in list {
        let v = given.get(&i.name).cloned().unwrap_or(Value::Null);
        let empty = v.is_null() || v.as_str().is_some_and(str::is_empty);
        if empty {
            if i.required && i.kind != "checkbox" {
                problems.field(&i.name, "This field is required.");
            }
            out.insert(i.name.clone(), if i.kind == "checkbox" { Value::Bool(false) } else { Value::Null });
            continue;
        }
        let ok = match i.kind.as_str() {
            "number" => v.is_number() || v.as_str().is_some_and(|t| t.trim().parse::<f64>().is_ok()),
            "checkbox" => v.is_boolean(),
            "select" => v.as_str().is_some_and(|t| i.choices.iter().any(|c| c == t)),
            "file" => v.get("name").is_some_and(Value::is_string) && v.get("base64").is_some_and(Value::is_string),
            _ => v.is_string(),
        };
        if !ok {
            problems.field(&i.name, "This value is not valid.");
            continue;
        }
        let v = match (i.kind.as_str(), &v) {
            ("number", Value::String(t)) => t.trim().parse::<f64>().ok().and_then(serde_json::Number::from_f64).map_or(v.clone(), Value::Number),
            _ => v,
        };
        out.insert(i.name.clone(), v);
    }
    Ok(out)
}

/// What the route read before the call: the values to write and the loaded values by field name, the stored key and row.
pub(crate) struct Prepared {
    pub op: Op,
    pub function: String,
    pub values: Map<String, Value>,
    pub loaded: Map<String, Value>,
    pub inputs: Map<String, Value>,
    /// The key of the record in the order of the key columns (update, delete).
    pub key: Option<Vec<Value>>,
    /// The detail cells of the record before the write (update, delete).
    pub before: Option<Vec<Value>>,
    /// The form labels whose loaded value the write compares with the stored row (update, delete).
    pub compared: Vec<String>,
    /// The checked change set of the inlines (`with_parent`): the rows the service writes with the record.
    pub inlines: Option<Value>,
}

/// A map by form label as a map by field name.
pub(crate) fn by_field(r: &Resource, m: &Map<String, Value>) -> Value {
    Value::Object(m.iter().map(|(k, v)| (r.detail.iter().find(|d| &d.label == k).map_or_else(|| k.clone(), |d| d.field.clone()), v.clone())).collect())
}

pub(crate) fn key_object(r: &Resource, key: &[Value]) -> Value {
    Value::Object(r.key_columns.iter().zip(key).map(|(c, v)| (c.field.clone(), v.clone())).collect())
}

/// The payload of one write for the host function.
pub(crate) fn payload_of(s: &State_, resource: &Resource, user: &User, meta: &crate::write::Meta, p: &Prepared) -> Value {
    let protected = s.site.schema.rules_for_model(resource.model).any(|rule| rule.layer.app() || rule.skippable);
    json!({
        "resource": resource.name,
        "model": s.site.schema.model(resource.model).ir.name,
        "op": op_name(p.op),
        "key": p.key.as_ref().map(|k| key_object(resource, k)),
        "key_text": p.key.as_ref().map(|k| key::encode(k)),
        "values": by_field(resource, &p.values),
        "loaded": by_field(resource, &p.loaded),
        "lock": s.site.schema.dialect == monorm_core::dialect::Dialect::Postgres,
        "compare": p.compared.iter().map(|l| resource.detail.iter().find(|d| &d.label == l).map_or_else(|| l.clone(), |d| d.field.clone())).collect::<Vec<_>>(),
        "inputs": p.inputs,
        "audit_inputs": audit_inputs(resource, &p.inputs),
        "inlines": p.inlines,
        "user": user.0,
        "request_id": meta.request_id,
        "ip": meta.ip,
        "user_agent": meta.agent,
        "allow_protected": protected && resource.allow_protected,
        "service": resource.service.get(p.op).is_some(),
        "sync": resource.service.get(p.op).is_some_and(|o| o.sync),
    })
}

/// One call of a host function. A service gets no transaction, so the admin rolls its own back first;
/// the default writer gets the transaction of the request. The call comes back with a scope that is open again.
/// A failure is the final answer: the scope is rolled back.
pub(crate) async fn call_host<'a>(s: &State_, c: Call<'a>, op: Op, function: &str, payload: impl FnOnce(&Call<'a>) -> Value) -> Result<(Call<'a>, Value), Response> {
    let payload = payload(&c);
    let Call { resource: sight, user, grants, scope, meta, uri, body } = c;
    let resource = &*sight;
    let Some((bridge, auth)) = &s.bridge else {
        scope.rollback().await;
        return Err(reject(StatusCode::FORBIDDEN, "ReadOnly", "this write needs host code, and this admin has no host"));
    };
    let held = if resource.service.get(op).is_some() {
        scope.rollback().await;
        None
    } else {
        Some(scope)
    };
    let ctx = CallContext { request_id: auth.security.call_id(), scope: json!({}), tx: held.as_ref().map(Scoped::transaction) };
    let answer = match bridge.call(function, payload, ctx).await {
        Ok(v) => v,
        Err(e) => {
            if let Some(scope) = held {
                scope.rollback().await;
            }
            return Err(host_failure(&meta, function, e));
        }
    };
    let new_scope = match held {
        Some(scope) => scope,
        None => match Scoped::begin(s.conn.as_ref()).await {
            Ok(sc) => sc,
            Err(e) => return Err(server_error(&meta, &e.message)),
        },
    };
    Ok((Call { resource: sight, user, grants, scope: new_scope, meta, uri, body }, answer))
}

/// Calls the host and answers the request.
pub(crate) async fn run(s: &State_, c: Call<'_>, p: Prepared) -> Response {
    let function = p.function.clone();
    let (c, answer) = match call_host(s, c, p.op, &function, |c| payload_of(s, &c.resource, &c.user, &c.meta, &p)).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    answer_of(s, c, p, answer).await
}

pub(crate) fn host_failure(meta: &crate::write::Meta, name: &str, e: BridgeError) -> Response {
    server_error(meta, &format!("{name}: {}: {}", e.kind(), e.message()))
}

pub(crate) fn texts(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

/// The envelope of the host: `{ok: true, key, audited}` or `{ok: false, kind, ...}`.
async fn answer_of(s: &State_, c: Call<'_>, p: Prepared, answer: Value) -> Response {
    let sight = c.resource.clone();
    let resource = &*sight;
    if answer["ok"] != Value::Bool(true) {
        let kind = answer["kind"].as_str().unwrap_or("");
        return match kind {
            "not_found" => {
                c.scope.rollback().await;
                reject(StatusCode::NOT_FOUND, "NotFound", "no such record")
            }
            "conflict" => conflict(s, c, &p).await,
            "invalid" => {
                c.scope.rollback().await;
                let mut problems = Problems::default();
                if let Some(fields) = answer["fields"].as_object() {
                    for (name, messages) in fields {
                        for m in texts(Some(messages)) {
                            problems.field(name, m);
                        }
                    }
                }
                problems.form = texts(answer.get("form"));
                problems.response()
            }
            "sync_service" => {
                let cause = answer["message"].as_str().unwrap_or("a sync service used the async database API").to_owned();
                let response = server_error(&c.meta, &cause);
                c.scope.rollback().await;
                response
            }
            _ => {
                let cause = format!("the host answered {answer}");
                let response = server_error(&c.meta, &cause);
                c.scope.rollback().await;
                response
            }
        };
    }
    let (c, key_text, before, after) = match settle(s, c, &p, &answer).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let body = match p.op {
        Op::Create => (StatusCode::CREATED, Json(json!({"ok": true, "resource": resource.name, "key": key_text, "values": after.unwrap_or_default()}))),
        Op::Update => (StatusCode::OK, Json(json!({"ok": true, "key": key_text, "changed": changed_labels(resource, before.as_deref(), after.as_deref())}))),
        _ => (StatusCode::OK, Json(json!({"ok": true, "key": key_text}))),
    };
    finish(c, body.into_response()).await
}

/// The cells before and after a write.
type Cells = Option<Vec<Value>>;

/// What follows a host write that answered ok: the record as stored now and its audit row. It returns the key text and the cells before and after.
/// A failure is the final answer: the scope is rolled back.
pub(crate) async fn settle<'a>(s: &State_, c: Call<'a>, p: &Prepared, answer: &Value) -> Result<(Call<'a>, String, Cells, Cells), Response> {
    let resource = c.resource.clone();
    let resource = &*resource;
    let new_key: Vec<Value> = match p.op {
        Op::Create => {
            let k = &answer["key"];
            resource.key_columns.iter().map(|col| k.get(&col.field).cloned().unwrap_or(Value::Null)).collect()
        }
        _ => p.key.clone().unwrap_or_default(),
    };
    let key_text = key::encode(&new_key);
    let (c, after) = if p.op == Op::Delete { (c, None) } else { read_after(s, c, &new_key).await? };
    let before = p.before.clone();
    let cells = after.clone().or(before.clone()).unwrap_or_default();
    let service = resource.service.get(p.op).is_some();
    let audited = answer["audited"] == Value::Bool(true);
    if let Err(m) = audit::set_keys(&s.site, c.scope.executor(), answer).await {
        let response = server_error(&c.meta, &format!("the write ran but its audit key failed: {m}"));
        c.scope.rollback().await;
        return Err(response);
    }
    if audited && service {
        // The service wrote the row and sent no label: the label rule lives here.
        let label = crate::write::record_label(resource, &cells, &key_text);
        if let Err(m) = audit::fill_label(&s.site, c.scope.executor(), &resource.name, &key_text, &label).await {
            let response = server_error(&c.meta, &format!("the write ran but its audit label failed: {m}"));
            c.scope.rollback().await;
            return Err(response);
        }
    } else {
        let diff = diff_of(resource, p.op, before.as_deref(), after.as_deref());
        let mut extra = Map::new();
        extra.insert("inputs".into(), Value::Object(audit_inputs(resource, &p.inputs)));
        extra.insert("allow_protected_writes".into(), Value::Bool(s.site.schema.rules_for_model(resource.model).any(|rule| rule.layer.app() || rule.skippable) && resource.allow_protected));
        extra.insert("service".into(), Value::Bool(service));
        if let Some(rows) = &p.inlines {
            extra.insert("inlines".into(), rows.clone());
        }
        let result = if audited {
            // The default writer wrote the row in the write transaction. Only the admin knows the key text (of a create too), the diff and the label.
            let label = crate::write::record_label(resource, &cells, &key_text);
            match answer["audit_id"].as_i64() {
                Some(id) => audit::complete(&s.site, c.scope.executor(), id, &key_text, &label, audit_changes(&c.meta, diff, extra)).await,
                None => Err("the host answered audited with no audit_id".to_owned()),
            }
        } else {
            write_audit_with(s, &c, op_name(p.op), &key_text, &cells, diff, extra).await
        };
        if let Err(m) = result {
            let response = server_error(&c.meta, &format!("the write ran but its audit row failed: {m}"));
            c.scope.rollback().await;
            return Err(response);
        }
    }
    Ok((c, key_text, before, after))
}

/// The record as it is stored after the write, or none when the `queryset` hides it. It reads on the scope the caller holds.
/// An error is the final answer: the scope is rolled back.
pub(crate) async fn read_after<'a>(s: &State_, c: Call<'a>, key: &[Value]) -> Result<(Call<'a>, Option<Vec<Value>>), Response> {
    let Call { resource: sight, user, grants, scope, meta, uri, body } = c;
    let resource = &*sight;
    let (scope, rows) = reach(s, resource, &user, &grants, Vec::new(), scope).await.map_err(|r| *r)?;
    let record = list::read_record(&s.site, resource, scope.executor(), s.target, s.viewer(), key, &rows.rows, false).await;
    match record {
        Ok(r) => Ok((Call { resource: sight.clone(), user, grants, scope, meta, uri, body }, r.map(|r| r.cells))),
        Err(e) => {
            scope.rollback().await;
            Err(list_failure(&meta, e))
        }
    }
}

fn diff_of(r: &Resource, op: Op, before: Option<&[Value]>, after: Option<&[Value]>) -> Value {
    let mut diff = Map::new();
    for (at, d) in r.detail.iter().enumerate() {
        let from = before.map_or(Value::Null, |b| b[at].clone());
        let to = after.map_or(Value::Null, |a| a[at].clone());
        let skip = match op {
            Op::Update => d.same(&from, &to),
            _ => from.is_null() && to.is_null(),
        };
        if !skip && !(op == Op::Create && to.is_null()) && !(op == Op::Delete && from.is_null()) {
            diff.insert(d.label.clone(), json!({"from": from, "to": to}));
        }
    }
    Value::Object(diff)
}

pub(crate) fn changed_labels(r: &Resource, before: Option<&[Value]>, after: Option<&[Value]>) -> Vec<String> {
    match diff_of(r, Op::Update, before, after) {
        Value::Object(m) => m.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// 409: the stored row differs from the loaded values. The answer holds the values now.
async fn conflict(s: &State_, c: Call<'_>, p: &Prepared) -> Response {
    let (c, now) = match p.key.as_deref() {
        Some(k) => match read_after(s, c, k).await {
            Ok(done) => done,
            Err(r) => return r,
        },
        None => (c, None),
    };
    let Call { scope, resource: sight, .. } = c;
    let resource = &*sight;
    scope.rollback().await;
    let labels: Vec<String> = p.loaded.keys().cloned().collect();
    let found = match &now {
        Some(cells) => conflicts(&s.site, resource, cells, &p.loaded, &labels),
        None => Map::new(),
    };
    let current: Map<String, Value> = match &now {
        Some(cells) => resource.detail.iter().zip(cells).map(|(d, v)| (d.label.clone(), v.clone())).collect(),
        None => Map::new(),
    };
    (StatusCode::CONFLICT, Json(json!({"ok": false, "error": {"kind": "Conflict", "message": "the record changed since the form loaded"}, "conflicts": found, "current": current}))).into_response()
}

/// An audit row of host code outside a request: `admin.log(...)`.
pub async fn log_entry(site: &crate::site::Site, conn: &dyn monorm_engine::db::Executor, user_key: &str, action: &str, resource: &str, record_key: &str, changes: Option<Value>) -> Result<(), String> {
    audit::record(site, conn, Entry { user_key, action, resource, record_key, record_label: "", changes }).await
}

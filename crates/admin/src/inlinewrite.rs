//! Writes of inline rows.
//!
//! An inline with `save: with_parent` sends its rows in the body of the parent write, as one change set:
//! `inlines: {name: {create: [{set}], update: [{key, set, loaded}], delete: [key | {key, loaded}]}}`.
//! `check` reads and locks every row, applies the permissions, the cap and the page rules, and refuses the
//! whole write when one row fails. `apply` runs the checked rows in the transaction of the parent
//! (deletes, then updates, then creates), or `Changes::json` goes to the service of the parent.
//! An inline with `save: row` has its own routes: the child page writes one row, inside the pins of the parent.

use axum::extract::{Path, Request};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Extension;
use serde_json::{json, Map, Value};

use crate::key;
use crate::hostwrite::{by_field, key_object};
use crate::list::{self, RowScope};
use crate::permissions::Op;
use crate::server::{authorize, child_rows, deny, error_response, segment, reach, reject, Reach, Shared, State_, User};
use crate::site::{Column, Inline, InlineSave, Resource, MAX_INLINE_MAX};
use crate::write::{self, Call, Pin, Problems};

/// Why a change set stops: a reply to send after a rollback, or a refusal to audit.
pub(crate) enum Fail {
    Reply(Response),
    Deny { resource: String, changes: Value },
}

struct Created {
    set: Map<String, Value>,
    values: Vec<(String, sea_query::Value)>,
}

struct Updated {
    raw: String,
    key: Vec<Value>,
    set: Map<String, Value>,
    loaded: Map<String, Value>,
    changed: Vec<(String, sea_query::Value, Value)>,
    before: Vec<Value>,
}

struct Deleted {
    raw: String,
    key: Vec<Value>,
    before: Vec<Value>,
}

struct Part {
    inline: Inline,
    /// The child columns and the values that tie the rows to the parent. Empty while the parent is new.
    pins: Vec<(Column, Value)>,
    /// The row scope of the inline with the pins: what an update or delete may reach.
    rows: RowScope,
    creates: Vec<Created>,
    updates: Vec<Updated>,
    deletes: Vec<Deleted>,
}

/// The checked rows of the inlines of one parent write.
pub(crate) struct Changes {
    parts: Vec<Part>,
}

impl Changes {
    pub(crate) fn is_empty(&self) -> bool {
        self.parts.iter().all(|p| p.creates.is_empty() && p.updates.is_empty() && p.deletes.is_empty())
    }

    /// The change set as the service of the parent gets it: values by field name, keys as objects, the stored values before the change.
    /// `model` names the child model; `pin` pairs each child field with the parent field and its value (null while the parent is new).
    pub(crate) fn json(&self) -> Value {
        let mut out = Map::new();
        for p in &self.parts {
            let (child, name) = (&p.inline.resource, &p.inline.name);
            let create: Vec<Value> = p.creates.iter().map(|r| json!({"set": by_field(child, &r.set)})).collect();
            let update: Vec<Value> = p.updates.iter().map(|r| json!({"key": key_object(child, &r.key), "set": by_field(child, &r.set), "loaded": by_field(child, &r.loaded), "before": before(child, &r.before)})).collect();
            let delete: Vec<Value> = p.deletes.iter().map(|r| json!({"key": key_object(child, &r.key), "before": before(child, &r.before)})).collect();
            let pin: Vec<Value> = p.inline.child_to.iter().zip(&p.inline.parent_from).enumerate().map(|(i, (c, f))| json!({"child": c.field, "parent": f.field, "value": p.pins.get(i).map(|(_, v)| v)})).collect();
            out.insert(name.clone(), json!({"model": child.name, "pin": pin, "create": create, "update": update, "delete": delete}));
        }
        Value::Object(out)
    }
}

fn before(r: &Resource, cells: &[Value]) -> Value {
    Value::Object(r.detail.iter().zip(cells).map(|(d, v)| (d.field.clone(), v.clone())).collect())
}

fn bad(message: impl AsRef<str>) -> Fail {
    Fail::Reply(reject(StatusCode::BAD_REQUEST, "BadRequest", message.as_ref()))
}

#[allow(clippy::result_large_err)]
fn items<'v>(spec: &'v Map<String, Value>, what: &str) -> Result<&'v [Value], Fail> {
    match spec.get(what) {
        None => Ok(&[]),
        Some(Value::Array(a)) if a.len() <= MAX_INLINE_MAX => Ok(a),
        Some(Value::Array(_)) => Err(bad(format!("\"{what}\" holds at most {MAX_INLINE_MAX} rows"))),
        Some(_) => Err(bad(format!("\"{what}\" is a list"))),
    }
}

fn pins_of(inline: &Inline, values: Vec<Value>) -> Vec<(Column, Value)> {
    inline.child_to.iter().cloned().zip(values).collect()
}

/// A problem of one row under the name of the row: `<inline>.<row>.<field>`.
fn merge(problems: &mut Problems, prefix: &str, inline: &Inline, mut sub: Problems) {
    for (field, messages) in std::mem::take(&mut sub.fields) {
        problems.fields.entry(format!("{prefix}.{field}")).or_default().extend(messages);
    }
    problems.form.extend(sub.form.into_iter().map(|m| format!("{}: {m}", inline.label)));
}

/// Read, lock and check the rows of the change set in the body. The caller runs `apply`, or sends `json` to a service.
/// `parent` is the key and the row scope of the parent record on an update; a new parent has none.
pub(crate) async fn check(s: &State_, c: &Call<'_>, parent_op: Op, parent: Option<(&[Value], &RowScope)>, problems: &mut Problems) -> Result<Changes, Fail> {
    let mut changes = Changes { parts: Vec::new() };
    let Some(sets) = c.body.get("inlines") else { return Ok(changes) };
    let Value::Object(sets) = sets else { return Err(bad("\"inlines\" is an object")) };
    let site = &s.site;
    let mut conflicts = Map::new();
    for (name, spec) in sets {
        let Some(inline) = c.resource.inlines.iter().find(|i| &i.name == name) else { return Err(bad(format!("no inline {name:?}"))) };
        if inline.save != InlineSave::WithParent {
            return Err(bad(format!("the inline {name:?} saves by row")));
        }
        let Value::Object(spec) = spec else { return Err(bad(format!("the change set of {name:?} is an object"))) };
        if let Some(k) = spec.keys().find(|k| !matches!(k.as_str(), "create" | "update" | "delete")) {
            return Err(bad(format!("{name:?}: unknown key {k:?}")));
        }
        let (create, update, delete) = (items(spec, "create")?, items(spec, "update")?, items(spec, "delete")?);
        if parent_op == Op::Create && (!update.is_empty() || !delete.is_empty()) {
            return Err(bad(format!("{name:?}: a new record has only new rows")));
        }
        let child = &inline.resource;
        for (op, count) in [(Op::Create, create.len()), (Op::Update, update.len()), (Op::Delete, delete.len())] {
            if count == 0 {
                continue;
            }
            if op == Op::Delete && !inline.delete {
                return Err(Fail::Reply(reject(StatusCode::FORBIDDEN, "Forbidden", &format!("the inline {name:?} does not delete rows"))));
            }
            if !child.allow.contains(&op) {
                return Err(Fail::Deny { resource: child.name.clone(), changes: json!({"reason": "allow"}) });
            }
            if !site.inline_allows(&c.grants, &c.resource, inline, op) {
                return Err(Fail::Deny { resource: child.name.clone(), changes: json!({"needs": site.inline_requires(&c.resource, inline, op)}) });
            }
        }
        for (op, present) in [(Op::Create, !create.is_empty()), (Op::Update, !update.is_empty()), (Op::Delete, !delete.is_empty())] {
            if let Some(reason) = present.then(|| write::write_reason(&site.schema, child, op)).flatten() {
                return Err(Fail::Reply(reject(StatusCode::FORBIDDEN, "ReadOnly", &reason)));
            }
        }
        let unpinned = child_rows(s, child, &c.user, &c.scope).await.map_err(|e| Fail::Reply(write::list_failure(&c.meta, e)))?;
        let (pins, rows) = match parent {
            Some((key, parent_rows)) => {
                let related = site.inline_related(inline);
                let values = match list::parent_values(site, &c.resource, &related, c.scope.executor(), s.target, key, parent_rows).await {
                    Ok(Some(v)) => v,
                    Ok(None) => return Err(Fail::Reply(reject(StatusCode::NOT_FOUND, "NotFound", "no such record"))),
                    Err(e) => return Err(Fail::Reply(write::list_failure(&c.meta, e))),
                };
                let pins = pins_of(inline, values);
                let rows = list::pin_scope(&unpinned, &pins).map_err(|e| Fail::Reply(write::list_failure(&c.meta, e)))?;
                (pins, rows)
            }
            None => (Vec::new(), unpinned.clone()),
        };
        let pin_labels: Vec<&String> = inline.child_to.iter().map(|col| &col.label).collect();
        let mut part = Part { inline: inline.clone(), pins: pins.clone(), rows, creates: Vec::new(), updates: Vec::new(), deletes: Vec::new() };

        if !create.is_empty() {
            let existing = if pins.is_empty() {
                0
            } else {
                list::read_pinned(site, child, c.scope.executor(), s.target, s.viewer(), &pins, &unpinned, MAX_INLINE_MAX + 1, false).await.map_err(|e| Fail::Reply(write::list_failure(&c.meta, e)))?.len()
            };
            if (existing + create.len()).saturating_sub(delete.len()) > inline.max {
                problems.form.push(format!("{} holds at most {} rows.", inline.label, inline.max));
            }
        }
        let model = site.schema.model(child.model);
        for (i, item) in create.iter().enumerate() {
            let Some(set) = item.get("set").and_then(Value::as_object) else { return Err(bad(format!("{name:?}: a new row needs a \"set\" object"))) };
            let prefix = format!("{name}.new.{i}");
            let mut sub = Problems::default();
            let values = write::convert_set(site, child, set, false, &mut sub);
            for label in pin_labels.iter().filter(|l| set.contains_key(**l)) {
                sub.field(label, "The parent sets this field.");
            }
            for col in child.detail.iter().filter(|col| write::writable(col) && !pin_labels.contains(&&col.label)) {
                if let Ok(f) = model.field(&col.field) {
                    if !f.nullable && !write::has_default(f) && !set.contains_key(&col.label) {
                        sub.field(&col.label, "This field is required.");
                    }
                }
            }
            if sub.is_empty() {
                write::check_picks(s, c, child, set, None, &mut sub).await.map_err(Fail::Reply)?;
            }
            merge(problems, &prefix, inline, sub);
            part.creates.push(Created { set: set.clone(), values });
        }

        let mut seen = std::collections::HashSet::new();
        let guards: Vec<String> = model.guards.iter().filter_map(|&p| model.fields().get(p)).filter_map(|f| child.detail.iter().find(|d| d.field == f.name && write::writable(d)).map(|d| d.label.clone())).collect();
        for item in update {
            let Some(raw) = item.get("key").and_then(Value::as_str) else { return Err(bad(format!("{name:?}: an updated row needs a \"key\""))) };
            let Some(set) = item.get("set").and_then(Value::as_object) else { return Err(bad(format!("{name:?}: an updated row needs a \"set\" object"))) };
            let loaded = item.get("loaded").and_then(Value::as_object).cloned().unwrap_or_default();
            if let Some(g) = guards.iter().find(|g| !loaded.contains_key(*g)) {
                return Err(bad(format!("{name:?}: \"loaded\" needs the guard field {g:?}")));
            }
            if let Some(n) = set.keys().find(|n| !loaded.contains_key(*n) && child.detail.iter().any(|d| &d.label == *n && write::writable(d))) {
                return Err(bad(format!("{name:?}: \"loaded\" needs the value of {n:?}")));
            }
            let (raw, key) = write::decode_key(child, raw).map_err(bad)?;
            if !seen.insert(raw.clone()) {
                return Err(bad(format!("{name:?}: the row {raw:?} is in the change set twice")));
            }
            let record = fetch(s, c, child, &key, &part.rows, true).await?;
            if record.flags.get("change") == Some(&Value::Bool(false)) {
                return Err(Fail::Deny { resource: child.name.clone(), changes: json!({"reason": "can", "rule": "change"}) });
            }
            let mut compared: Vec<String> = set.keys().cloned().collect();
            compared.extend(guards.iter().cloned());
            for (field, v) in write::conflicts(site, child, &record.cells, &loaded, &compared) {
                conflicts.insert(format!("{name}.{raw}.{field}"), v);
            }
            let prefix = format!("{name}.{raw}");
            let mut sub = Problems::default();
            let values = write::convert_set(site, child, set, true, &mut sub);
            for label in pin_labels.iter().filter(|l| set.contains_key(**l)) {
                sub.field(label, "The parent sets this field.");
            }
            let changed = write::changed_values(site, child, values, &record.cells);
            if sub.is_empty() {
                let names: Vec<String> = changed.iter().map(|(n, _, _)| n.clone()).collect();
                write::check_picks(s, c, child, set, Some(&names), &mut sub).await.map_err(Fail::Reply)?;
            }
            merge(problems, &prefix, inline, sub);
            part.updates.push(Updated { raw, key, set: set.clone(), loaded, changed, before: record.cells });
        }

        for item in delete {
            let (raw, loaded) = match item {
                Value::String(raw) => (raw.as_str(), Map::new()),
                Value::Object(o) => match o.get("key").and_then(Value::as_str) {
                    Some(raw) => (raw, o.get("loaded").and_then(Value::as_object).cloned().unwrap_or_default()),
                    None => return Err(bad(format!("{name:?}: a deleted row needs a \"key\""))),
                },
                _ => return Err(bad(format!("{name:?}: a deleted row is a key"))),
            };
            let (raw, key) = write::decode_key(child, raw).map_err(bad)?;
            if !seen.insert(raw.clone()) {
                return Err(bad(format!("{name:?}: the row {raw:?} is in the change set twice")));
            }
            let record = fetch(s, c, child, &key, &part.rows, true).await?;
            if record.flags.get("delete") == Some(&Value::Bool(false)) {
                return Err(Fail::Deny { resource: child.name.clone(), changes: json!({"reason": "can", "rule": "delete"}) });
            }
            let fields: Vec<String> = loaded.keys().cloned().collect();
            for (field, v) in write::conflicts(site, child, &record.cells, &loaded, &fields) {
                conflicts.insert(format!("{name}.{raw}.{field}"), v);
            }
            part.deletes.push(Deleted { raw, key, before: record.cells });
        }
        changes.parts.push(part);
    }
    if !conflicts.is_empty() {
        return Err(Fail::Reply(write::conflict_response(conflicts)));
    }
    Ok(changes)
}

/// One locked row of the inline, inside the row scope of the inline: 404 when it is not one of the rows of the parent.
async fn fetch(s: &State_, c: &Call<'_>, child: &Resource, key: &[Value], rows: &RowScope, lock: bool) -> Result<list::Record, Fail> {
    match list::read_record(&s.site, child, c.scope.executor(), s.target, s.viewer(), key, rows, lock).await {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(Fail::Reply(reject(StatusCode::NOT_FOUND, "NotFound", "no such row"))),
        Err(e) => Err(Fail::Reply(write::list_failure(&c.meta, e))),
    }
}

/// Run the checked rows: deletes, then updates, then creates, with one audit row per row under the name of the child page.
/// `parent_cells` are the detail cells of a parent that the same request created. The caller rolls back on an error.
pub(crate) async fn apply(s: &State_, c: &Call<'_>, changes: &Changes, parent_cells: Option<&[Value]>) -> Result<(), Response> {
    for part in &changes.parts {
        let (inline, child) = (&part.inline, &part.inline.resource);
        let pins = match parent_cells {
            Some(cells) => new_pins(c, inline, cells)?,
            None => part.pins.clone(),
        };
        let extra = || -> Map<String, Value> { Map::from_iter([("inline".to_owned(), json!(format!("{}.{}", c.resource.name, inline.name)))]) };
        let reach = Reach { rows: part.rows.clone(), labels: Vec::new(), change: false };
        for row in &part.deletes {
            write::delete_row(s, c, child, &reach, &row.key).await?;
            let diff: Map<String, Value> = child.detail.iter().zip(&row.before).filter(|(_, v)| !v.is_null()).map(|(d, v)| (d.label.clone(), json!({"from": v, "to": null}))).collect();
            write::write_audit_for(s, c, child, "delete", &row.raw, &row.before, Value::Object(diff), extra()).await.map_err(|m| write::server_error(&c.meta, &m))?;
        }
        for row in &part.updates {
            if row.changed.is_empty() {
                continue;
            }
            write::update_changed(s, c, child, &reach, &row.key, &row.changed).await?;
            let diff: Map<String, Value> = row.changed.iter().filter_map(|(n, _, to)| child.detail.iter().position(|d| &d.label == n).map(|at| (n.clone(), json!({"from": row.before[at], "to": to})))).collect();
            write::write_audit_for(s, c, child, "update", &row.raw, &row.before, Value::Object(diff), extra()).await.map_err(|m| write::server_error(&c.meta, &m))?;
        }
        for row in &part.creates {
            let cells = write::plain_insert(s, c, child, row.values.clone(), &pins).await?;
            let raw = key::encode(&write::key_of(child, &cells));
            let diff: Map<String, Value> = child.detail.iter().zip(&cells).filter(|(_, v)| !v.is_null()).map(|(d, v)| (d.label.clone(), json!({"from": null, "to": v}))).collect();
            write::write_audit_for(s, c, child, "create", &raw, &cells, Value::Object(diff), extra()).await.map_err(|m| write::server_error(&c.meta, &m))?;
        }
    }
    Ok(())
}

/// The pins of the rows of a parent that this request created: the cells of the new record, matched to the columns by field.
#[allow(clippy::result_large_err)]
fn new_pins(c: &Call<'_>, inline: &Inline, cells: &[Value]) -> Result<Vec<(Column, Value)>, Response> {
    let mut values = Vec::new();
    for from in &inline.parent_from {
        let Some(at) = c.resource.detail.iter().position(|d| d.field == from.field && d.path.is_empty()) else {
            return Err(write::server_error(&c.meta, &format!("the page {:?} shows no field {:?} for the inline {:?}", c.resource.name, from.field, inline.name)));
        };
        values.push(cells[at].clone());
    }
    Ok(pins_of(inline, values))
}

/// End a call that `check` stopped: roll back and reply, or audit the refusal.
pub(crate) async fn refuse(s: &State_, c: Call<'_>, fail: Fail) -> Response {
    match fail {
        Fail::Reply(r) => {
            c.scope.rollback().await;
            r
        }
        Fail::Deny { resource, changes } => deny(&s.site, c.scope, &c.user, &resource, changes).await,
    }
}

/// The steps that every row route shares: user, parent page, inline (404 unless it saves by row), the permission of the operation,
/// the pins from the parent record, the body. `key_back` counts the URL segments from the end to the parent key.
async fn open_inline<'a>(s: &'a State_, user: Option<Extension<User>>, parent: &str, inline: &str, op: Op, req: Request, key_back: usize) -> Result<(Call<'a>, Pin), Box<Response>> {
    let (user, scope, grants) = authorize(s, user).await?;
    let (meta, uri) = write::meta_of(s, &req);
    // The view of the user: an inline whose parent field is hidden is not there for the user.
    let sight = s.site.sight(parent, &grants);
    let found = sight.as_ref().and_then(|p| p.inlines.iter().find(|i| i.name == inline && i.save == InlineSave::Row).map(|i| (p, i)));
    let Some((parent, inline)) = found else {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", &format!("no inline {inline:?} on {parent:?}"))));
    };
    let child = &inline.resource;
    let at = parent.inlines.iter().position(|i| i.name == inline.name).expect("the inline was found in this page");
    let child_sight = match &sight {
        Some(crate::sight::Sight::View(view)) => crate::sight::Sight::Inline(view.clone(), at),
        _ => crate::sight::Sight::Page(&s.site.resource(&parent.name).expect("the page of the view").inlines[at].resource),
    };
    if op == Op::Delete && !inline.delete {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::FORBIDDEN, "Forbidden", &format!("the inline {:?} does not delete rows", inline.name))));
    }
    if !child.allow.contains(&op) {
        return Err(Box::new(deny(&s.site, scope, &user, &child.name, json!({"reason": "allow"})).await));
    }
    // A row write changes the parent: the parent `allow` and its read-only state hold, as on a write of the parent itself.
    if !parent.allow.contains(&Op::Update) {
        return Err(Box::new(deny(&s.site, scope, &user, &parent.name, json!({"reason": "allow"})).await));
    }
    if let Some(reason) = write::write_reason(&s.site.schema, parent, Op::Update).or_else(|| write::write_reason(&s.site.schema, child, op)) {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::FORBIDDEN, "ReadOnly", &reason)));
    }
    if !s.site.inline_allows(&grants, parent, inline, op) {
        return Err(Box::new(deny(&s.site, scope, &user, &child.name, json!({"needs": s.site.inline_requires(parent, inline, op)})).await));
    }
    if s.bridge.is_none() && crate::hostwrite::route(&s.site.schema, child, op).is_some() {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::FORBIDDEN, "ReadOnly", "this write needs host code, and this admin has no host")));
    }
    let (_, parent_key) = match write::decode_key(parent, segment(&uri, key_back)) {
        Ok(k) => k,
        Err(m) => {
            scope.rollback().await;
            return Err(Box::new(reject(StatusCode::BAD_REQUEST, "BadRequest", &m)));
        }
    };
    let (scope, parent_reach) = reach(s, parent, &user, &grants, Vec::new(), scope).await?;
    if parent.can.iter().any(|(name, _)| name == "change") {
        let locked = match list::read_record(&s.site, parent, scope.executor(), s.target, s.viewer(), &parent_key, &parent_reach.rows, false).await {
            Ok(Some(r)) => r.flags.get("change") == Some(&Value::Bool(false)),
            Ok(None) => {
                scope.rollback().await;
                return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", "no such record")));
            }
            Err(e) => {
                scope.rollback().await;
                return Err(Box::new(error_response(e)));
            }
        };
        if locked {
            return Err(Box::new(deny(&s.site, scope, &user, &parent.name, json!({"reason": "can", "rule": "change"})).await));
        }
    }
    let related = s.site.inline_related(inline);
    let values = match list::parent_values(&s.site, parent, &related, scope.executor(), s.target, &parent_key, &parent_reach.rows).await {
        Ok(Some(v)) => v,
        Ok(None) => {
            scope.rollback().await;
            return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", "no such record")));
        }
        Err(e) => {
            scope.rollback().await;
            return Err(Box::new(error_response(e)));
        }
    };
    let pin = Pin { pins: pins_of(inline, values), max: inline.max };
    let call = Call { resource: child_sight, user, grants, scope, meta, uri, body: Map::new() };
    Ok((write::read_body(call, req).await?, pin))
}

pub(crate) async fn row_create(axum::extract::State(s): Shared, user: Option<Extension<User>>, Path((name, _, inline)): Path<(String, String, String)>, req: Request) -> Response {
    match open_inline(&s, user, &name, &inline, Op::Create, req, 2).await {
        Ok((c, pin)) => write::create_in(&s, c, Some(&pin)).await,
        Err(r) => *r,
    }
}

pub(crate) async fn row_update(axum::extract::State(s): Shared, user: Option<Extension<User>>, Path((name, _, inline, _)): Path<(String, String, String, String)>, req: Request) -> Response {
    match open_inline(&s, user, &name, &inline, Op::Update, req, 3).await {
        Ok((c, pin)) => write::update_in(&s, c, Some(&pin)).await,
        Err(r) => *r,
    }
}

pub(crate) async fn row_destroy(axum::extract::State(s): Shared, user: Option<Extension<User>>, Path((name, _, inline, _)): Path<(String, String, String, String)>, req: Request) -> Response {
    match open_inline(&s, user, &name, &inline, Op::Delete, req, 3).await {
        Ok((c, pin)) => write::destroy_in(&s, c, Some(&pin)).await,
        Err(r) => *r,
    }
}

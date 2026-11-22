//! List edit: `PATCH /resources/<name>` saves the changed cells of several rows in one transaction.
//!
//! The body is `{"rows": [{"key", "set", "loaded"}]}`. A row follows the rules of one update (`write::update_in`):
//! the `queryset` and `can=` limits, the compare of `loaded` with the stored row, the form checks, one audit row.
//! The whole save fails when one row fails, and then nothing is written. The plain path reads and locks
//! the rows in key order, and checks every row before the first write. The host path sends all rows in one
//! `update_rows` call, in key order: the host opens one transaction and runs the update function once per row.

use std::collections::BTreeMap;

use axum::extract::{Path, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use monorm_engine::Error;
use serde_json::{json, Map, Value};

use crate::hostwrite::{self, Prepared};
use crate::list::{self, RowScope};
use crate::server::{deny, reach, reject, Shared, State_, User};
use crate::permissions::Op;
use crate::site::{Resource, MAX_PAGE_SIZE};
use crate::write::{self, Call, Problems, RowFailure};

/// One row of the request: the key as sent and decoded, the values to set and the values the page loaded.
struct Row {
    raw: String,
    key: Vec<Value>,
    set: Map<String, Value>,
    loaded: Map<String, Value>,
}

/// What the save knows of a row after the read: the stored cells, and the form labels it compares.
struct Read {
    cells: Vec<Value>,
    compared: Vec<String>,
}

pub(crate) async fn save(axum::extract::State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, req: Request) -> Response {
    let c = match write::open_head(&s, user, &name, Op::Update, write::meta_of(&s, &req)).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let c = match write::read_body(c, req).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    save_in(&s, c).await
}

/// The field names of the model guards, as form labels: `loaded` must hold them.
fn guards_of(s: &State_, resource: &Resource) -> Vec<String> {
    write::guard_labels(s.site.schema.model(resource.model), &resource.detail)
}

/// The rows of the body (400 for a wrong shape).
fn parse(resource: &Resource, guards: &[String], body: &Map<String, Value>) -> Result<Vec<Row>, String> {
    let Some(Value::Array(list)) = body.get("rows") else { return Err("the body needs a \"rows\" array".into()) };
    if list.is_empty() {
        return Err("\"rows\" holds no row".into());
    }
    if list.len() > MAX_PAGE_SIZE {
        return Err(format!("\"rows\" holds {} rows: at most {MAX_PAGE_SIZE} rows in one save", list.len()));
    }
    let mut rows: Vec<Row> = Vec::new();
    for (at, item) in list.iter().enumerate() {
        let Value::Object(item) = item else { return Err(format!("row {at}: a row is an object")) };
        let Some(Value::String(raw)) = item.get("key") else { return Err(format!("row {at}: a row needs a \"key\" text")) };
        let (raw, key) = write::decode_key(resource, raw).map_err(|m| format!("row {at}: {m}"))?;
        if rows.iter().any(|r| r.raw == raw) {
            return Err(format!("row {at}: the key {raw:?} is in the save twice"));
        }
        let Some(Value::Object(set)) = item.get("set") else { return Err(format!("row {at}: a row needs a \"set\" object")) };
        let Some(Value::Object(loaded)) = item.get("loaded") else { return Err(format!("row {at}: a row needs a \"loaded\" object")) };
        if let Some(n) = set.keys().find(|n| !resource.editable.contains(n)) {
            return Err(format!("row {at}: {n:?} is not an editable field of the list"));
        }
        if let Some(g) = guards.iter().find(|g| !loaded.contains_key(*g)) {
            return Err(format!("row {at}: \"loaded\" needs the guard field {g:?}"));
        }
        if let Some(n) = set.keys().find(|n| !loaded.contains_key(*n)) {
            return Err(format!("row {at}: \"loaded\" needs the value of {n:?}"));
        }
        rows.push(Row { raw, key, set: set.clone(), loaded: loaded.clone() });
    }
    Ok(rows)
}

/// 400 with the problems of each row by its index in the request.
fn rows_response(rows: BTreeMap<usize, Problems>) -> Response {
    let rows: Map<String, Value> = rows.into_iter().map(|(at, p)| (at.to_string(), json!({"fields": p.fields, "form": p.form}))).collect();
    (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": {"kind": "Invalid", "message": "the values were not saved"}, "rows": rows}))).into_response()
}

/// 409 with the stored value of each loaded field that changed, by the row index in the request.
fn conflicts_response(rows: BTreeMap<usize, Map<String, Value>>) -> Response {
    let rows: Map<String, Value> = rows.into_iter().map(|(at, m)| (at.to_string(), Value::Object(m))).collect();
    (StatusCode::CONFLICT, Json(json!({"ok": false, "error": {"kind": "Conflict", "message": "a record changed since the list loaded"}, "conflicts": rows}))).into_response()
}

async fn fail(c: Call<'_>, response: Response) -> Response {
    c.scope.rollback().await;
    response
}

/// `fail` for an answer that reads the call: the rollback consumes the call, so the answer is made first.
async fn fail_with(c: Call<'_>, answer: impl FnOnce(&Call<'_>) -> Response) -> Response {
    let response = answer(&c);
    fail(c, response).await
}

async fn save_in(s: &State_, c: Call<'_>) -> Response {
    let sight = c.resource.clone();
    let (site, resource) = (&s.site, &*sight);
    if resource.editable.is_empty() {
        return c.bad("this list has no editable field for you").await;
    }
    let guards = guards_of(s, resource);
    let rows = match parse(resource, &guards, &c.body) {
        Ok(r) => r,
        Err(m) => return c.bad(&m).await,
    };
    let (scope, reach) = match reach(s, resource, &c.user, &c.grants, Vec::new(), c.scope).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let c = Call { scope, ..c };
    // Lock in key order, so two saves of the same rows never wait for each other in a circle.
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|a, b| rows[*a].raw.cmp(&rows[*b].raw));
    let service = resource.service.get(Op::Update).is_some();
    let mut read: BTreeMap<usize, Read> = BTreeMap::new();
    for &at in &order {
        let row = &rows[at];
        let record = match list::read_record(site, resource, c.scope.executor(), s.target, s.viewer(), &row.key, &reach.rows, true).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                // A row that the `queryset` hides is out of reach (403); a row that is not in the table is gone (404).
                let open = RowScope { without: reach.rows.without.clone(), user_key: reach.rows.user_key.clone(), ..RowScope::default() };
                return match list::read_record(site, resource, c.scope.executor(), s.target, s.viewer(), &row.key, &open, false).await {
                    Ok(Some(_)) => deny(site, c.scope, &c.user, &resource.name, json!({"reason": "queryset", "key": row.raw})).await,
                    Ok(None) => fail(c, reject(StatusCode::NOT_FOUND, "NotFound", &format!("no record {:?}", row.raw))).await,
                    Err(e) => fail_with(c, |c| write::list_failure(&c.meta, e)).await,
                };
            }
            Err(e) => return fail_with(c, |c| write::list_failure(&c.meta, e)).await,
        };
        if record.flags.get("change") == Some(&Value::Bool(false)) {
            return deny(site, c.scope, &c.user, &resource.name, json!({"reason": "can", "rule": "change", "key": row.raw})).await;
        }
        let mut compared: Vec<String> = row.set.keys().cloned().collect();
        compared.extend(guards.iter().filter(|g| !row.set.contains_key(*g)).cloned());
        read.insert(at, Read { cells: record.cells, compared });
    }
    // Every row is checked before the first write.
    let mut problems: BTreeMap<usize, Problems> = BTreeMap::new();
    let mut converted = BTreeMap::new();
    for (at, row) in rows.iter().enumerate() {
        let mut p = Problems::default();
        let values = write::convert_set(site, resource, &row.set, true, &mut p);
        if !p.is_empty() {
            problems.insert(at, p);
        }
        converted.insert(at, values);
    }
    if !problems.is_empty() {
        return fail(c, rows_response(problems)).await;
    }
    if !service {
        let found = stale(site, resource, &rows, &read, |_| true);
        if !found.is_empty() {
            return fail(c, conflicts_response(found)).await;
        }
    }
    let mut changed = BTreeMap::new();
    for (at, values) in converted {
        let ch = write::changed_values(site, resource, values, &read[&at].cells);
        let names: Vec<String> = ch.iter().map(|(n, _, _)| n.clone()).collect();
        let mut p = Problems::default();
        if let Err(r) = write::check_picks(s, &c, resource, &rows[at].set, Some(&names), &mut p).await {
            return fail(c, r).await;
        }
        if !p.is_empty() {
            problems.insert(at, p);
        }
        changed.insert(at, ch);
    }
    if !problems.is_empty() {
        return fail(c, rows_response(problems)).await;
    }
    if service {
        // The host compares the rows it gets; a row that stays out is compared here, as on the plain path.
        let found = stale(site, resource, &rows, &read, |at| changed[&at].is_empty());
        if !found.is_empty() {
            return fail(c, conflicts_response(found)).await;
        }
    }
    match hostwrite::route(&site.schema, resource, Op::Update) {
        Some(function) => host_save(s, c, function, &rows, read, &changed).await,
        None => plain_save(s, c, &reach, &rows, &read, changed).await,
    }
}

/// The loaded values of the picked rows that no longer match the stored row, by row index.
fn stale(site: &crate::site::Site, resource: &Resource, rows: &[Row], read: &BTreeMap<usize, Read>, pick: impl Fn(usize) -> bool) -> BTreeMap<usize, Map<String, Value>> {
    let mut found = BTreeMap::new();
    for (at, row) in rows.iter().enumerate().filter(|(at, _)| pick(*at)) {
        let r = &read[&at];
        let f = write::conflicts(site, resource, &r.cells, &row.loaded, &r.compared);
        if !f.is_empty() {
            found.insert(at, f);
        }
    }
    found
}

/// The body of a good save: every key, and the names that changed by key.
fn done(rows: &[Row], names: Vec<Vec<String>>) -> Response {
    let keys: Vec<&String> = rows.iter().map(|r| &r.raw).collect();
    let changed: Map<String, Value> = rows.iter().zip(names).map(|(r, n)| (r.raw.clone(), json!(n))).collect();
    Json(json!({"ok": true, "keys": keys, "changed": changed})).into_response()
}

/// The UPDATE of each changed row, with its audit row, on the transaction of the request.
async fn plain_save(s: &State_, c: Call<'_>, reach: &crate::server::Reach, rows: &[Row], read: &BTreeMap<usize, Read>, changed: BTreeMap<usize, Vec<(String, sea_query::Value, Value)>>) -> Response {
    let sight = c.resource.clone();
    let resource = &*sight;
    let mut names: Vec<Vec<String>> = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        let ch = &changed[&at];
        if ch.is_empty() {
            names.push(Vec::new());
            continue;
        }
        if let Err(f) = write::update_row(s, &c, resource, reach, &row.key, ch).await {
            return fail_with(c, |c| match f {
                RowFailure::Gone => reject(StatusCode::NOT_FOUND, "NotFound", &format!("no record {:?}", row.raw)),
                RowFailure::Other(r) => r,
                RowFailure::Statement(e, written) => statement_problems(s, c, resource, at, &written, e),
            })
            .await;
        }
        let cells = &read[&at].cells;
        let diff: Map<String, Value> = ch.iter().filter_map(|(n, _, to)| resource.detail.iter().position(|d| &d.label == n).map(|i| (n.clone(), json!({"from": cells[i], "to": to})))).collect();
        names.push(diff.keys().cloned().collect());
        if let Err(m) = write::write_audit(s, &c, "update", &row.raw, cells, Value::Object(diff)).await {
            return fail_with(c, |c| write::server_error(&c.meta, &m)).await;
        }
    }
    write::finish(c, done(rows, names)).await
}

/// A constraint of the database as a message of the row; anything else a server error.
fn statement_problems(s: &State_, c: &Call<'_>, resource: &Resource, at: usize, written: &[String], e: Error) -> Response {
    let mut p = Problems::default();
    match &e {
        Error::Db(d) => match write::constraint_problems(&s.site.schema, resource, written, false, d) {
            Some(found) => p = found,
            None => return write::server_error(&c.meta, &d.message),
        },
        Error::Value(m) => p.form.push(m.clone()),
        _ => return write::statement_failure(&s.site, resource, &c.meta, written, false, e),
    }
    rows_response(BTreeMap::from([(at, p)]))
}

/// One bridge call for all changed rows. The host runs the update function once per row in one transaction.
async fn host_save(s: &State_, c: Call<'_>, function: String, rows: &[Row], mut read: BTreeMap<usize, Read>, changed: &BTreeMap<usize, Vec<(String, sea_query::Value, Value)>>) -> Response {
    let sight = c.resource.clone();
    let resource = &*sight;
    // The host gets the rows that change, in key order like the plain path locks them; `sent` maps each to its place in the request.
    let mut sent: Vec<usize> = (0..rows.len()).filter(|at| !changed[at].is_empty()).collect();
    sent.sort_by(|a, b| rows[*a].raw.cmp(&rows[*b].raw));
    if sent.is_empty() {
        return write::finish(c, done(rows, vec![Vec::new(); rows.len()])).await;
    }
    let mut prepared: Vec<Prepared> = Vec::new();
    for &at in &sent {
        let r = read.remove(&at).expect("a row that was read");
        prepared.push(Prepared {
            op: Op::Update,
            function: function.clone(),
            values: rows[at].set.clone(),
            loaded: rows[at].loaded.clone(),
            inputs: Map::new(),
            key: Some(rows[at].key.clone()),
            before: Some(r.cells),
            compared: r.compared,
            inlines: None,
        });
    }
    let (mut c, answer) = match hostwrite::call_host(s, c, Op::Update, &function, |c| {
        let list: Vec<Value> = prepared.iter().map(|p| hostwrite::payload_of(s, &c.resource, &c.user, &c.meta, p)).collect();
        json!({"op": "update_rows", "resource": list[0]["resource"], "model": list[0]["model"], "rows": list})
    })
    .await
    {
        Ok(t) => t,
        Err(r) => return r,
    };
    if answer["ok"] != Value::Bool(true) {
        return host_refusal(s, c, rows, &sent, &prepared, &answer).await;
    }
    let results = answer["results"].as_array().cloned().unwrap_or_default();
    if results.len() != sent.len() {
        return fail_with(c, |c| write::server_error(&c.meta, &format!("the host answered {} results for {} rows", results.len(), sent.len()))).await;
    }
    let mut names: Vec<Vec<String>> = vec![Vec::new(); rows.len()];
    for ((&at, p), result) in sent.iter().zip(&prepared).zip(&results) {
        let (next, _, before, after) = match hostwrite::settle(s, c, p, result).await {
            Ok(t) => t,
            Err(r) => return r,
        };
        c = next;
        names[at] = hostwrite::changed_labels(resource, before.as_deref(), after.as_deref());
    }
    write::finish(c, done(rows, names)).await
}

/// The host refused the save: its answer names the row that failed (`index` into the rows it got).
async fn host_refusal(s: &State_, c: Call<'_>, rows: &[Row], sent: &[usize], prepared: &[Prepared], answer: &Value) -> Response {
    let sight = c.resource.clone();
    let (site, resource) = (&s.site, &*sight);
    let kind = answer["kind"].as_str().unwrap_or("");
    let Some(at) = answer["index"].as_u64().and_then(|i| sent.get(i as usize).copied()) else {
        return fail_with(c, |c| write::server_error(&c.meta, &format!("the host answered {answer}"))).await;
    };
    match kind {
        "not_found" => fail(c, reject(StatusCode::NOT_FOUND, "NotFound", &format!("no record {:?}", rows[at].raw))).await,
        "invalid" => {
            let mut p = Problems::default();
            if let Some(fields) = answer["fields"].as_object() {
                for (name, messages) in fields {
                    for m in hostwrite::texts(Some(messages)) {
                        p.field(name, m);
                    }
                }
            }
            p.form = hostwrite::texts(answer.get("form"));
            fail(c, rows_response(BTreeMap::from([(at, p)]))).await
        }
        "conflict" => {
            let p = &prepared[sent.iter().position(|a| *a == at).unwrap_or(0)];
            let (scope, reach) = match reach(s, resource, &c.user, &c.grants, Vec::new(), c.scope).await {
                Ok(t) => t,
                Err(r) => return *r,
            };
            let now = list::read_record(site, resource, scope.executor(), s.target, s.viewer(), &rows[at].key, &reach.rows, false).await;
            scope.rollback().await;
            let record = match now {
                Ok(Some(r)) => r,
                Ok(None) => return reject(StatusCode::NOT_FOUND, "NotFound", &format!("no record {:?}", rows[at].raw)),
                Err(e) => return write::list_failure(&c.meta, e),
            };
            let mut found = write::conflicts(site, resource, &record.cells, &rows[at].loaded, &p.compared);
            if found.is_empty() {
                // The stored value went back to the loaded one after the host compared: name the compared fields, so the page still marks a row.
                for name in &p.compared {
                    let (Some(i), Some(was)) = (resource.detail.iter().position(|d| &d.label == name), rows[at].loaded.get(name)) else { continue };
                    found.insert(name.clone(), json!({"loaded": was, "current": record.cells[i]}));
                }
            }
            conflicts_response(BTreeMap::from([(at, found)]))
        }
        "sync_service" => {
            let cause = answer["message"].as_str().unwrap_or("a sync service used the async database API").to_owned();
            fail_with(c, |c| write::server_error(&c.meta, &cause)).await
        }
        _ => fail_with(c, |c| write::server_error(&c.meta, &format!("the host answered {answer}"))).await,
    }
}

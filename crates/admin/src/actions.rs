//! Actions: a custom action or the built-in bulk delete, run on a selection or on all the rows that
//! match a filter, in one request.
//!
//! The admin builds the query set: the `queryset` filter, the `can=` rule, `applies_to`, the pins of a
//! tab and the selection or the list filters. It counts the rows (capped), asks for the second
//! confirmation above `COUNT_CAP`, then rolls back and calls the host with the query set as
//! filters and values. The host loops in batches and answers with a result. One summary row of the
//! admin log carries the request id, which is also the id of the rows that the action writes.

use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use base64::Engine as _;
use monorm_engine::exec::Outcome;
use serde_json::{json, Map, Value};

use crate::audit::{self, Entry};
use crate::bridge::CallContext;
use crate::files::{Temp, MAX_UPLOAD};
use crate::hostwrite::{self, audit_input_list, parse_input_list, InputConfig};
use crate::json::bind_tagged;
use crate::key;
use crate::list::{self, Binds, ListError, ListRequest, Position, RowScope};
use crate::permissions::{Grants, Op};
use crate::scope::Scoped;
use crate::server::{authorize, deny, error_response, filters_of, found_keys, open_resource_with, open_tab_with, reject, Reach, Shared, State_, User};
use crate::site::{Action, Column, Placement, Resource, BULK_DELETE, COUNT_CAP};
use crate::write::{self, meta_of, read_only_reason, server_error, statement_failure, user_key_text, Meta, Problems, MAX_BODY};

/// The most keys of one selection: a larger selection is "all the rows that match".
const MAX_KEYS: usize = 1000;
/// The keys that the summary row lists.
const LOGGED_KEYS: usize = 20;

/// `POST /actions/{name}/{action}`: the action on the rows of a list.
pub(crate) async fn on_list(State(s): Shared, user: Option<Extension<crate::server::User>>, Path((name, action)): Path<(String, String)>, req: Request) -> Response {
    let (meta, uri) = meta_of(&s, &req);
    let body = req.into_body();
    let (user, scope, grants) = match authorize(&s, user).await {
        Ok(x) => x,
        Err(r) => return *r,
    };
    let (scope, resource, reach, (user, grants)) = match open_resource_with(&s, user, scope, grants, &name, &uri).await {
        Ok(x) => x,
        Err(r) => return *r,
    };
    run(&s, Open { resource, user, grants, scope, reach, pins: Vec::new(), tab: false, meta, uri }, &action, body).await
}

/// `POST /tab-actions/{name}/{key}/{rel}/{action}`: the action on the rows of a to-many tab. The action is one of the child resource.
pub(crate) async fn on_tab(State(s): Shared, user: Option<Extension<User>>, Path((name, raw, rel, action)): Path<(String, String, String, String)>, req: Request) -> Response {
    let (meta, uri) = meta_of(&s, &req);
    let body = req.into_body();
    let (user, scope, grants) = match authorize(&s, user).await {
        Ok(x) => x,
        Err(r) => return *r,
    };
    let (scope, related, pins, reach) = match open_tab_with(&s, &user, scope, &grants, &name, &raw, &rel, &uri).await {
        Ok(x) => x,
        Err(r) => return *r,
    };
    run(&s, Open { resource: related.child, user, grants, scope, reach, pins, tab: true, meta, uri }, &action, body).await
}

struct Open<'a> {
    resource: &'a Resource,
    user: User,
    grants: Grants,
    scope: Scoped,
    reach: Reach,
    pins: Vec<(Column, Value)>,
    tab: bool,
    meta: Meta,
    uri: Uri,
}

#[derive(Clone, Copy)]
enum Kind<'a> {
    Delete,
    Custom(&'a Action),
}

enum Selection {
    Keys(Vec<Vec<Value>>),
    All,
}

impl Selection {
    fn name(&self, placement: Placement) -> &'static str {
        match (self, placement) {
            (Selection::Keys(_), _) => "keys",
            (Selection::All, Placement::List) => "filter",
            (Selection::All, _) => "all",
        }
    }
}

async fn fail(scope: Scoped, response: Response) -> Response {
    scope.rollback().await;
    response
}

/// The query set of a request: the filters and their values, and the row scope that turns read defaults off. `None`: no row can match.
struct QuerySet {
    filters: Vec<Value>,
    binds: Binds,
    rows: RowScope,
}

async fn run(s: &State_, o: Open<'_>, name: &str, body: axum::body::Body) -> Response {
    let Open { resource, user, grants, scope, reach, pins, tab, meta, uri } = o;
    let kind = if name == BULK_DELETE {
        Kind::Delete
    } else {
        match resource.actions.iter().find(|a| a.name == name && a.placement.is_some()) {
            Some(a) => Kind::Custom(a),
            None => return fail(scope, reject(StatusCode::NOT_FOUND, "NotFound", &format!("no action {name:?} on {:?}", resource.name))).await,
        }
    };
    let placement = match kind {
        Kind::Delete => Placement::Bulk,
        Kind::Custom(a) => a.placement.unwrap_or(Placement::Bulk),
    };
    if tab && !matches!(placement, Placement::Bulk | Placement::Row) {
        return fail(scope, reject(StatusCode::NOT_FOUND, "NotFound", "this action does not run on a tab")).await;
    }
    // Bulk delete needs `delete`; a custom action needs its own permission, or `manage`.
    match kind {
        Kind::Delete => {
            if !resource.bulk_delete {
                return fail(scope, reject(StatusCode::NOT_FOUND, "NotFound", "this resource has no bulk delete")).await;
            }
            if !resource.allow.contains(&Op::Delete) {
                return deny(&s.site, scope, &user, &resource.name, json!({"reason": "allow"})).await;
            }
            if !s.site.allows(&grants, resource, Op::Delete) {
                return deny(&s.site, scope, &user, &resource.name, json!({"needs": s.site.requires(resource, Op::Delete)})).await;
            }
            if let Some(reason) = read_only_reason(&s.site.schema, resource) {
                return fail(scope, reject(StatusCode::FORBIDDEN, "ReadOnly", &reason)).await;
            }
        }
        Kind::Custom(a) => {
            if !s.site.action_allowed(&grants, resource, a) {
                let needs = match &a.permission {
                    Some(p) => vec![format!("{}.{p}_{}", resource.label, resource.key)],
                    None => s.site.requires(resource, Op::Update),
                };
                return deny(&s.site, scope, &user, &resource.name, json!({"needs": needs})).await;
            }
        }
    }
    let inputs_list: &[InputConfig] = match kind {
        Kind::Delete => &[],
        Kind::Custom(a) => &a.inputs,
    };
    let limit = if inputs_list.iter().any(|i| i.kind == "file") { MAX_UPLOAD / 3 * 4 + (64 << 10) } else { MAX_BODY };
    let body = match read_object(body, limit).await {
        Ok(m) => m,
        Err(r) => return fail(scope, r).await,
    };
    let selection = match selection(resource, placement, &body) {
        Ok(x) => x,
        Err(m) => return fail(scope, reject(StatusCode::BAD_REQUEST, "BadRequest", &m)).await,
    };
    let mut problems = Problems::default();
    let mut inputs = match parse_input_list(inputs_list, &body, &mut problems) {
        Ok(m) => m,
        Err(m) => return fail(scope, reject(StatusCode::BAD_REQUEST, "BadRequest", &m)).await,
    };
    if !problems.is_empty() {
        return fail(scope, problems.response()).await;
    }
    let confirmed = body.get("confirm_large").and_then(Value::as_bool).unwrap_or(false);

    // The query set and its count.
    let set = match query_set(s, resource, kind, &reach, &pins, &selection, &uri, &scope).await {
        Ok(x) => x,
        Err(e) => return fail(scope, error_response(e)).await,
    };
    let model = s.site.schema.model(resource.model).ir.name.clone();
    let n = match &set {
        None => 0,
        Some(q) => {
            let mut op = json!({"op": "count", "model": model, "filters": q.filters, "limit": COUNT_CAP + 1});
            q.rows.apply(&mut op);
            match list::run_count(&s.site, scope.executor(), s.target, op, Binds(q.binds.0.clone())).await {
                Ok(n) => n as usize,
                Err(e) => return fail(scope, error_response(e)).await,
            }
        }
    };
    let capped = n > COUNT_CAP;
    let count = n.min(COUNT_CAP);
    if n == 0 && !matches!(placement, Placement::List) {
        if matches!(placement, Placement::Bulk) {
            return fail(scope, Json(json!({"ok": true, "count": 0, "result": {"kind": "message", "message": "No row matched."}})).into_response()).await;
        }
        return fail(scope, reject(StatusCode::NOT_FOUND, "NotFound", "no such record, or the action does not apply to it")).await;
    }
    if capped && !confirmed {
        let message = format!("more than {COUNT_CAP} rows match: confirm to run on all of them");
        let body = json!({"ok": false, "error": {"kind": "ConfirmLarge", "message": message}, "count": COUNT_CAP, "capped": true});
        return fail(scope, (StatusCode::CONFLICT, Json(body)).into_response()).await;
    }
    let set = set.unwrap_or(QuerySet { filters: vec![json!({"t": "lit", "v": false})], binds: Binds::default(), rows: RowScope::default() });

    // Files: a temp file for each file input, kept until the host answers.
    let mut audit_values = audit_input_list(inputs_list, &inputs);
    let mut temps: Vec<Temp> = Vec::new();
    for i in inputs_list.iter().filter(|i| i.kind == "file") {
        let Some(v) = inputs.get(&i.name).filter(|v| !v.is_null()).cloned() else { continue };
        let bytes = match base64::engine::general_purpose::STANDARD.decode(v["base64"].as_str().unwrap_or_default()) {
            Ok(b) if b.len() <= MAX_UPLOAD => b,
            Ok(_) => return fail(scope, reject(StatusCode::PAYLOAD_TOO_LARGE, "PayloadTooLarge", &format!("the file is larger than {} MiB", MAX_UPLOAD / 1024 / 1024))).await,
            Err(_) => {
                let mut p = Problems::default();
                p.field(&i.name, "This value is not valid.");
                return fail(scope, p.response()).await;
            }
        };
        let filename = v["name"].as_str().unwrap_or_default().to_owned();
        let content_type = v["content_type"].as_str().unwrap_or("application/octet-stream").to_owned();
        let size = bytes.len();
        let temp = Temp::new();
        if let Err(m) = temp.write(bytes).await {
            return fail(scope, server_error(&meta, &m)).await;
        }
        inputs.insert(i.name.clone(), json!({"filename": filename, "content_type": content_type, "size": size, "path": temp.0.to_string_lossy()}));
        audit_values.insert(i.name.clone(), json!({"name": filename, "content_type": content_type, "size": size}));
        temps.push(temp);
    }

    let direct = matches!(kind, Kind::Delete) && hostwrite::route(&s.site.schema, resource, Op::Delete).is_none();
    let (answer, scope) = if direct {
        let mut op = json!({"op": "delete", "model": model, "filters": set.filters});
        set.rows.apply(&mut op);
        let c = write::Call { resource, user: user.clone(), grants, scope, meta: Meta { request_id: meta.request_id.clone(), ip: meta.ip.clone(), agent: meta.agent.clone() }, uri: uri.clone(), body: Map::new() };
        let out = write::run_mutation(&s.site, &c, s.target, op, set.binds).await;
        let scope = c.scope;
        match out {
            Ok(Outcome::Affected(n)) => (json!({"ok": true, "affected": n, "result": {"kind": "message", "message": format!("Deleted {n} row(s).")}}), scope),
            Ok(_) => return fail(scope, server_error(&meta, "a delete affects rows")).await,
            Err(e) => {
                let response = statement_failure(&s.site, resource, &meta, &[], true, e);
                return fail(scope, response).await;
            }
        }
    } else {
        scope.rollback().await;
        let Some((bridge, auth)) = &s.bridge else {
            return reject(StatusCode::FORBIDDEN, "ReadOnly", "this action needs host code, and this admin has no host");
        };
        let function = match kind {
            Kind::Delete => hostwrite::route(&s.site.schema, resource, Op::Delete).unwrap_or_default(),
            Kind::Custom(_) => format!("action.{}.{}", resource.name, name),
        };
        let payload = json!({
            "resource": resource.name,
            "model": model,
            "action": name,
            "op": matches!(kind, Kind::Delete).then_some("delete"),
            "filters": set.filters,
            "values": set.binds.0.iter().map(bind_tagged).collect::<Vec<_>>(),
            "without": set.rows.without,
            "count": count,
            "capped": capped,
            "selection": selection.name(placement),
            "inputs": inputs,
            "audit_inputs": audit_values,
            "user": user.0,
            "request_id": meta.request_id,
            "ip": meta.ip,
            "user_agent": meta.agent,
            "allow_protected": s.site.schema.model(resource.model).ir.protected_write && resource.allow_protected,
        });
        let ctx = CallContext { request_id: auth.security.call_id(), tenant: None, scope: json!({}), tx: None };
        let answer = match bridge.call(&function, payload, ctx).await {
            Ok(v) => v,
            Err(e) => return hostwrite::host_failure(&meta, &function, e),
        };
        match Scoped::begin(s.conn.as_ref()).await {
            Ok(scope) => (answer, scope),
            Err(e) => return server_error(&meta, &e.message),
        }
    };
    drop(temps);
    let done = Done { resource, user: &user, meta: &meta, uri: &uri, name, delete: matches!(kind, Kind::Delete), placement, selection: &selection, count, capped, inputs: audit_values };
    finish(s, done, answer, scope).await
}

/// What the summary row of the admin log says.
struct Done<'a> {
    resource: &'a Resource,
    user: &'a User,
    meta: &'a Meta,
    uri: &'a Uri,
    name: &'a str,
    delete: bool,
    placement: Placement,
    selection: &'a Selection,
    count: usize,
    capped: bool,
    inputs: Map<String, Value>,
}

/// The answer of the host, as the envelope of the route, and the summary row of the admin log.
async fn finish(s: &State_, d: Done<'_>, answer: Value, scope: Scoped) -> Response {
    if answer["ok"] != Value::Bool(true) {
        scope.rollback().await;
        return failed(d.meta, &answer);
    }
    let result = match result_of(answer.get("result")) {
        Ok(r) => r,
        Err(m) => {
            scope.rollback().await;
            return server_error(d.meta, &m);
        }
    };
    let affected = answer.get("affected").and_then(Value::as_u64);
    let mut changes = json!({
        "request_id": d.meta.request_id,
        "ip": d.meta.ip,
        "user_agent": d.meta.agent,
        "action": d.name,
        "placement": d.placement.name(),
        "count": d.count,
        "capped": d.capped,
        "selection": d.selection.name(d.placement),
        "query": d.uri.query().unwrap_or_default(),
        "inputs": d.inputs,
        "result": result["kind"],
    });
    if let Some(n) = affected {
        changes["affected"] = json!(n);
    }
    let mut record_key = String::new();
    if let Selection::Keys(keys) = d.selection {
        let texts: Vec<String> = keys.iter().take(LOGGED_KEYS).map(|k| key::encode(k)).collect();
        if keys.len() == 1 {
            record_key = texts[0].clone();
        }
        changes["keys"] = json!(texts);
    }
    let action = if d.delete { BULK_DELETE.to_owned() } else { format!("action:{}", d.name) };
    let entry = Entry { user_key: &user_key_text(d.user), action: &action, resource: &d.resource.name, record_key: &record_key, record_label: "", changes: Some(changes) };
    if let Err(m) = audit::record(&s.site, scope.executor(), entry).await {
        scope.rollback().await;
        return server_error(d.meta, &m);
    }
    if let Err(e) = scope.commit().await {
        return server_error(d.meta, &e.message);
    }
    let mut out = json!({"ok": true, "count": d.count, "capped": d.capped, "result": result});
    if let Some(n) = affected {
        out["affected"] = json!(n);
    }
    Json(out).into_response()
}

/// A failed answer of the host as a response: the same kinds as a host write.
fn failed(meta: &Meta, answer: &Value) -> Response {
    match answer["kind"].as_str().unwrap_or("") {
        "not_found" => reject(StatusCode::NOT_FOUND, "NotFound", "no such record"),
        "conflict" => reject(StatusCode::CONFLICT, "Conflict", "the rows changed: reload and try again"),
        "invalid" => {
            let mut problems = Problems::default();
            if let Some(fields) = answer["fields"].as_object() {
                for (name, messages) in fields {
                    for m in hostwrite::texts(Some(messages)) {
                        problems.field(name, m);
                    }
                }
            }
            problems.form = hostwrite::texts(answer.get("form"));
            problems.response()
        }
        other => server_error(meta, &format!("the action failed: {other}")),
    }
}

/// The result of an action as one of four kinds. A missing result means a refresh.
fn result_of(v: Option<&Value>) -> Result<Value, String> {
    let Some(v) = v.filter(|v| !v.is_null()) else { return Ok(json!({"kind": "refresh"})) };
    let text = |name: &str| v.get(name).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| format!("the result needs a text {name:?}"));
    match v.get("kind").and_then(Value::as_str) {
        Some("refresh") => Ok(json!({"kind": "refresh"})),
        Some("message") => {
            let level = v.get("level").and_then(Value::as_str).filter(|l| ["info", "success", "warning", "error"].contains(l)).unwrap_or("info");
            Ok(json!({"kind": "message", "message": text("message")?, "level": level}))
        }
        Some("redirect") => {
            let to = text("to")?;
            if !to.starts_with('/') || to.starts_with("//") || to.contains('\\') {
                return Err("a redirect goes to a path of this site".into());
            }
            Ok(json!({"kind": "redirect", "to": to}))
        }
        Some("download") => Ok(json!({"kind": "download", "filename": text("filename")?, "content_type": text("content_type")?, "base64": text("base64")?})),
        other => Err(format!("the result kind {other:?} is not message, redirect, download or refresh")),
    }
}

/// The JSON object of the body (400, 413). An empty body is an empty object.
async fn read_object(body: axum::body::Body, limit: usize) -> Result<Map<String, Value>, Response> {
    let bytes = axum::body::to_bytes(body, limit).await.map_err(|_| reject(StatusCode::PAYLOAD_TOO_LARGE, "PayloadTooLarge", "the body is too large"))?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(reject(StatusCode::BAD_REQUEST, "BadRequest", "the body is a JSON object")),
    }
}

/// The rows of the request, by placement: keys or all. A bad shape is a message for a 400.
fn selection(resource: &Resource, placement: Placement, body: &Map<String, Value>) -> Result<Selection, String> {
    let all = match body.get("all") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("\"all\" is true or false".into()),
    };
    let raw: Option<Vec<String>> = match body.get("keys") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => Some(items.iter().map(|i| i.as_str().map(str::to_owned).ok_or("a key is a text")).collect::<Result<_, _>>()?),
        Some(_) => return Err("\"keys\" is a list of keys".into()),
    };
    if placement == Placement::List {
        return match (raw, all) {
            (None, _) => Ok(Selection::All),
            _ => Err("a list action takes no keys".into()),
        };
    }
    let types: Vec<_> = resource.key_columns.iter().map(|c| c.ty.ty).collect();
    let single = !matches!(placement, Placement::Bulk);
    match (raw, all) {
        (Some(_), true) => Err("send \"keys\" or \"all\", not both".into()),
        (None, true) if !single => Ok(Selection::All),
        (None, true) => Err("this action runs on one record".into()),
        (None, false) => Err("send \"keys\" or \"all\"".into()),
        (Some(keys), false) => {
            if keys.is_empty() || keys.len() > MAX_KEYS || (single && keys.len() != 1) {
                return Err(if single { "this action runs on one record".into() } else { format!("\"keys\" holds 1 to {MAX_KEYS} keys: use \"all\" for more") });
            }
            Ok(Selection::Keys(keys.iter().map(|k| key::decode(&types, k)).collect::<Result<_, _>>()?))
        }
    }
}

/// The query set: the pins, the `queryset` filter, the `can=` rule of the action, `applies_to`, then the keys or the list filters.
/// `None`: the search found no row.
#[allow(clippy::too_many_arguments)]
async fn query_set(s: &State_, resource: &Resource, kind: Kind<'_>, reach: &Reach, pins: &[(Column, Value)], selection: &Selection, uri: &Uri, scope: &Scoped) -> Result<Option<QuerySet>, ListError> {
    let mut rows = list::pin_scope(&reach.rows, pins)?;
    let rule = match kind {
        Kind::Delete => "delete",
        Kind::Custom(_) => "change",
    };
    let mut wanted = vec![rule.to_owned()];
    if let Kind::Custom(a) = kind {
        if a.applies.is_some() {
            wanted.push(a.flag());
        }
    }
    for (name, expr) in &resource.can {
        if wanted.contains(name) {
            rows.filters.push(expr.clone());
        }
    }
    let mut binds = Binds::default();
    let filters = match selection {
        Selection::Keys(keys) => {
            let mut items = Vec::new();
            for k in keys {
                items.push(list::key_filter(resource, k, &mut binds)?);
            }
            let mut filters = vec![json!({"t": "or", "items": items})];
            filters.extend(rows.push(&mut binds));
            filters
        }
        Selection::All => {
            let q = form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes()).find(|(k, _)| k == "q").map(|(_, v)| v.into_owned());
            let found = found_keys(s, resource, scope, q.as_deref()).await?;
            let (filters, named) = filters_of(uri)?;
            let mut request = ListRequest::new(Position::First);
            request.q = q;
            request.found = found;
            request.filters = filters;
            request.named = named;
            request.scope = rows.clone();
            let Some(filters) = list::where_filters(&s.site, resource, &request, &mut binds)? else { return Ok(None) };
            filters
        }
    };
    Ok(Some(QuerySet { filters, binds, rows }))
}

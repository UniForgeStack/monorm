//! Relation fields of a form: a picker for a to-one relation and chips for a many-to-many relation.
//!
//! The target of a picker is a resource. Its `queryset` filter and the `choices` of the relation limit what the picker
//! lists and what a write accepts. The admin ignores tenancy, and a `can=` rule is a flag of one row, not a read filter:
//! neither limits the picker.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use monorm_core::ir::{ColType, RelKind};
use monorm_core::schema::Schema;
use monorm_engine::db::Executor;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::json;
use crate::key;
use crate::list::{self, Binds, ListError, ListRequest, Position, RowScope};
use crate::permissions::{Grants, Op};
use crate::scope::Scoped;
use crate::server::{authorize, deny, error_response, finish, found_keys, queryset_rows, reject, Shared, State_, User};
use crate::site::{column, err, queryset_of, Column, ConfigError, Queryset, QuerysetConfig, Resource, Source};

/// The rows that one autocomplete call returns when the call sets no limit, and the most it returns.
const PICK_PAGE: usize = 20;
const PICK_MAX: usize = 50;
/// The most links that the chips of one record load, and the most keys in one side of a write.
pub(crate) const LINKS_CAP: usize = 200;
pub(crate) const LINKS_PER_WRITE: usize = 500;

/// The config of one relation field of a resource.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelationConfig {
    /// The rows of the target that the field accepts, on top of the `queryset` of the target.
    choices: Option<QuerysetConfig>,
    /// The form offers a modal that creates a record of the target.
    #[serde(default)]
    create: bool,
    /// The text column of the target that names a row in the picker. By default the first text column.
    label: Option<String>,
}

/// The join model of a many-to-many relation.
#[derive(Debug, Clone)]
pub struct Through {
    pub model: String,
    /// The field of the join model that holds the key of the source.
    pub source: String,
    /// The field of the join model that holds the key of the target.
    pub target: String,
    /// The read defaults of the join model that read a scope value: always off, as for a resource.
    pub scope_defaults: Vec<String>,
}

#[derive(Debug)]
pub struct Relation {
    /// The relation name.
    pub name: String,
    /// The foreign key field of the source that a picker fills. Empty for chips.
    pub field: String,
    /// The name of the target resource.
    pub target: String,
    /// The column of the target that names a row.
    pub label: Column,
    pub choices: Option<Queryset>,
    pub create: bool,
    /// Set for chips (many-to-many).
    pub through: Option<Through>,
}

impl Relation {
    /// What the form needs to draw the field: the name, the target resource and the label.
    pub fn meta(&self) -> Value {
        json!({"name": self.name, "target": self.target, "label": self.label.label, "create": self.create, "choices": self.choices.is_some()})
    }
}

fn plain_text(c: &Column) -> bool {
    matches!(c.ty.ty, ColType::String | ColType::Text) && !c.ty.array && c.ty.enum_idx.is_none() && c.path.is_empty() && c.source == Source::Field
}

/// The picker and chip settings of resource `i`, checked against the target resources.
pub(crate) fn build(schema: &Schema, resources: &[Resource], i: usize, config: &std::collections::BTreeMap<String, RelationConfig>) -> Result<Vec<Relation>, ConfigError> {
    let r = &resources[i];
    let model = schema.model(r.model);
    let mut out = Vec::new();
    for (name, c) in config {
        let at = format!("{}: relation {name:?}", r.name);
        let Ok((rel, target_idx)) = model.relation(name) else { return err(format!("{at} is not a relation of the model")) };
        let Some(target) = resources.iter().find(|o| o.model == target_idx) else { return err(format!("{at} leads to {}, which is not a resource", schema.model(target_idx).ir.name)) };
        let label = match &c.label {
            Some(l) => {
                let col = column(schema, target.model, l)?;
                if !plain_text(&col) {
                    return err(format!("{at}: label {l:?} is not a text column of {}", target.name));
                }
                col
            }
            None => match target.columns.iter().find(|c| plain_text(c) && !target.key_columns.iter().any(|k| k.label == c.label)) {
                Some(col) => col.clone(),
                None => return err(format!("{at}: {} has no text column to name a row; set label", target.name)),
            },
        };
        if !target.columns.iter().any(|c| c.label == label.label) {
            return err(format!("{at}: label {:?} is not a column of the list of {}", label.label, target.name));
        }
        if target.search.is_empty() && target.search_with.is_none() {
            return err(format!("{at}: {} has no search; set list.search or list.search_with on it", target.name));
        }
        if target.key_columns.len() != 1 {
            return err(format!("{at}: {} has a composite key, which a picker does not support", target.name));
        }
        let choices = queryset_of(schema, target.model, &format!("{at}: choices"), c.choices.as_ref())?;
        if c.create {
            if !target.allow.contains(&crate::permissions::Op::Create) {
                return err(format!("{at}: create needs {} to allow create", target.name));
            }
            if let Some(why) = crate::write::model_read_only_reason(schema, target.model) {
                return err(format!("{at}: create: {} is read-only in the admin: {why}", target.name));
            }
        }
        let mut relation = Relation { name: name.clone(), field: String::new(), target: target.name.clone(), label, choices, create: c.create, through: None };
        match (&rel.kind, &rel.through) {
            (RelKind::One, _) => {
                if rel.from.len() != 1 || !rel.foreign_key {
                    return err(format!("{at} needs a to-one relation that owns a single-column foreign key"));
                }
                let key = schema.model(target.model).key_fields();
                if !key.first().is_some_and(|k| rel.to[0] == k.column || rel.to[0] == k.name) {
                    return err(format!("{at}: the foreign key must lead to the key of {}", target.name));
                }
                let Some(field) = model.ir.fields.iter().find(|f| f.column == rel.from[0] || f.name == rel.from[0]) else { return err(format!("{at}: no field for the foreign key")) };
                if !r.detail.iter().any(|d| d.field == field.name && crate::write::writable(d)) {
                    return err(format!("{at}: the foreign key field {:?} is not a writable field of the form", field.name));
                }
                relation.field = field.name.clone();
            }
            (RelKind::Many, Some(t)) => {
                let join = schema.model_idx(&t.model).map_err(|_| ConfigError(format!("{at}: unknown join model {:?}", t.model)))?;
                let jm = schema.model(join);
                if let Some(why) = crate::write::model_read_only_reason(schema, join) {
                    return err(format!("{at}: the join model {} is read-only in the admin: {why}", t.model));
                }
                if model.key_fields().len() != 1 {
                    return err(format!("{at}: {} has a composite key, which chips do not support", r.name));
                }
                if let Some(f) = jm.ir.fields.iter().find(|f| f.name != t.source && f.name != t.target && !f.nullable && f.generated.is_none() && !crate::write::has_default(f)) {
                    return err(format!("{at}: the join model {} needs a value for {:?}, which chips do not set", t.model, f.name));
                }
                relation.through = Some(Through { model: t.model.clone(), source: t.source.clone(), target: t.target.clone(), scope_defaults: crate::site::scope_default_names(jm) });
            }
            (RelKind::Many, None) => return err(format!("{at} is a to-many relation without a join model; chips need a many-to-many relation")),
        }
        out.push(relation);
    }
    Ok(out)
}

/// The picker and the chips need the target resource: `build` checked that it exists.
fn target_of<'a>(s: &'a State_, relation: &Relation) -> &'a Resource {
    s.site.resource(&relation.target).expect("a relation leads to a resource: `build` checked it")
}

/// The rows of the target that the relation offers: the `queryset` of the target and, with `choices`, the `choices` of the relation.
/// The read defaults that read a scope value are off, as for every route. The admin ignores tenancy.
pub(crate) async fn offer(s: &State_, relation: &Relation, user: &User, scope: &Scoped, choices: bool) -> Result<RowScope, ListError> {
    let target = target_of(s, relation);
    let mut rows = RowScope { without: target.scope_defaults.clone(), ..RowScope::default() };
    if let Some(q) = &target.queryset {
        queryset_rows(s, target, q, user, scope, &mut rows).await?;
    }
    if let (true, Some(q)) = (choices, &relation.choices) {
        queryset_rows(s, target, q, user, scope, &mut rows).await?;
    }
    Ok(rows)
}

/// A key as the JSON of the stored value, so two spellings of one key compare equal.
pub(crate) fn canonical(s: &State_, relation: &Relation, v: &Value) -> Result<Value, String> {
    let target = target_of(s, relation);
    let ty = target.key_columns[0].ty;
    json::bind(ty.ty, v).map(|b| json::bind_json(&s.site.schema, ty, &b))
}

/// Which of `keys` (canonical) the rows offer, each with its label.
pub(crate) async fn offered(s: &State_, conn: &dyn Executor, relation: &Relation, rows: &RowScope, keys: &[Value]) -> Result<Vec<(Value, String)>, ListError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let target = target_of(s, relation);
    let key = &target.key_columns[0];
    let mut binds = Binds::default();
    let mut params = Vec::new();
    for k in keys {
        params.push(binds.push(json::bind(key.ty.ty, k).map_err(ListError::Bad)?));
    }
    let mut filters = vec![json!({"t": "in", "item": list::col(key), "values": params})];
    filters.extend(rows.push(&mut binds));
    let mut op = json!({
        "op": "select",
        "model": s.site.schema.model(target.model).ir.name,
        "columns": [{"t": "expr", "expr": list::col(key)}, {"t": "expr", "expr": list::col(&relation.label)}],
        "filters": filters,
        "limit": keys.len(),
    });
    rows.apply(&mut op);
    let selected = list::run_select(&s.site, conn, s.target, op, binds).await?;
    let mut out = Vec::new();
    for r in 0..selected.rows.len() {
        let mut cells = Vec::new();
        for (c, ty) in selected.plan.types.iter().enumerate() {
            let cell = selected.rows.cell(r, c, *ty).map_err(|e| ListError::Engine(e.to_string()))?;
            cells.push(json::cell(&s.site.schema, *ty, cell));
        }
        out.push((cells[0].clone(), cells[1].as_str().unwrap_or_default().to_string()));
    }
    Ok(out)
}

/// The keys (canonical) that the record with this source key links to, at most `among` when it is set, else `LINKS_CAP + 1`.
pub(crate) async fn current(s: &State_, conn: &dyn Executor, relation: &Relation, resource: &Resource, source: &Value, among: Option<&[Value]>) -> Result<Vec<Value>, ListError> {
    let through = relation.through.as_ref().expect("chips have a join model");
    let (from, to) = (&resource.key_columns[0], &target_of(s, relation).key_columns[0]);
    let mut binds = Binds::default();
    let param = binds.push(json::bind(from.ty.ty, source).map_err(ListError::Bad)?);
    let mut filters = vec![json!({"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": through.source}, "r": param})];
    if let Some(keys) = among {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        for k in keys {
            params.push(binds.push(json::bind(to.ty.ty, k).map_err(ListError::Bad)?));
        }
        filters.push(json!({"t": "in", "item": {"t": "col", "path": [], "name": through.target}, "values": params}));
    }
    let mut op = json!({
        "op": "select",
        "model": through.model,
        "columns": [{"t": "expr", "expr": {"t": "col", "path": [], "name": through.target}}],
        "filters": filters,
        "limit": among.map_or(LINKS_CAP + 1, <[Value]>::len),
    });
    if !through.scope_defaults.is_empty() {
        op["without_defaults"] = json!(through.scope_defaults);
    }
    let selected = list::run_select(&s.site, conn, s.target, op, binds).await?;
    let mut out = Vec::new();
    for r in 0..selected.rows.len() {
        let ty = selected.plan.types[0];
        let cell = selected.rows.cell(r, 0, ty).map_err(|e| ListError::Engine(e.to_string()))?;
        out.push(json::cell(&s.site.schema, ty, cell));
    }
    Ok(out)
}

struct Opened<'a> {
    user: User,
    scope: Scoped,
    grants: Grants,
    resource: &'a Resource,
    relation: &'a Relation,
    target: &'a Resource,
}

/// The preamble of the two routes: the user, the source resource and relation (404), the permission on the source
/// (`write`: create or update; else read) and the read permission on the target.
async fn open<'a>(s: &'a State_, user: Option<Extension<User>>, name: &str, rel: &str, write: bool) -> Result<Opened<'a>, Box<Response>> {
    let (user, scope, grants) = authorize(s, user).await?;
    let Some(resource) = s.site.resource(name) else {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", &format!("no resource {name:?}"))));
    };
    let Some(relation) = resource.relations.iter().find(|r| r.name == rel) else {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", &format!("no relation field {rel:?} on {name:?}"))));
    };
    let ops: &[Op] = if write { &[Op::Create, Op::Update] } else { &[Op::Read] };
    if !ops.iter().any(|&op| resource.allow.contains(&op)) {
        return Err(Box::new(deny(&s.site, scope, &user, &resource.name, json!({"reason": "allow"})).await));
    }
    if !ops.iter().any(|&op| resource.allow.contains(&op) && s.site.allows(&grants, resource, op)) {
        let needs: Vec<_> = ops.iter().filter(|op| resource.allow.contains(op)).flat_map(|&op| s.site.requires(resource, op)).collect();
        return Err(Box::new(deny(&s.site, scope, &user, &resource.name, json!({"needs": needs})).await));
    }
    let target = target_of(s, relation);
    if !target.allow.contains(&Op::Read) {
        return Err(Box::new(deny(&s.site, scope, &user, &target.name, json!({"reason": "allow"})).await));
    }
    if !s.site.allows(&grants, target, Op::Read) {
        return Err(Box::new(deny(&s.site, scope, &user, &target.name, json!({"needs": s.site.requires(target, Op::Read)})).await));
    }
    Ok(Opened { user, scope, grants, resource, relation, target })
}

#[derive(Deserialize)]
pub(crate) struct Pick {
    cursor: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
}

/// One page of the rows that a relation field offers: keyset, so a table of millions of rows costs one index range.
/// `items` hold the value that the field stores and the label; `next` is the cursor of the next call.
pub(crate) async fn autocomplete(axum::extract::State(s): Shared, user: Option<Extension<User>>, Path((name, rel)): Path<(String, String)>, q: Result<Query<Pick>, QueryRejection>) -> Response {
    let o = match open(&s, user, &name, &rel, true).await {
        Ok(o) => o,
        Err(r) => return *r,
    };
    let q = match q {
        Ok(Query(q)) => q,
        Err(e) => {
            o.scope.rollback().await;
            return reject(StatusCode::BAD_REQUEST, "BadRequest", &e.body_text());
        }
    };
    let (target, relation) = (o.target, o.relation);
    let Some(label_at) = target.columns.iter().position(|c| c.label == relation.label.label) else {
        o.scope.rollback().await;
        return reject(StatusCode::INTERNAL_SERVER_ERROR, "ServerError", "the label is not a column of the list");
    };
    let out = async {
        let rows = offer(&s, relation, &o.user, &o.scope, true).await?;
        let position = q.cursor.map_or(Position::First, Position::After);
        let named = target.named_filters.iter().map(|n| (n.name.clone(), "all".to_string())).collect();
        let mut request = ListRequest { limit: Some(q.limit.unwrap_or(PICK_PAGE).min(PICK_MAX)), q: q.q, named, scope: rows, ..ListRequest::new(position) };
        list::check_request(&s.site, target, &request)?;
        request.found = found_keys(&s, target, &o.scope, request.q.as_deref()).await?;
        list::list(&s.site, target, o.scope.executor(), s.target, &request).await
    }
    .await;
    finish(o.scope, out, |page| {
        let at = target.key_positions[0];
        let items: Vec<Value> = page.rows.iter().map(|r| json!({"value": r[at], "label": r[label_at]})).collect();
        Json(json!({"ok": true, "items": items, "next": page.next})).into_response()
    })
    .await
}

/// The labels of the records that one record links to through a many-to-many relation: the chips of its form.
pub(crate) async fn links(axum::extract::State(s): Shared, user: Option<Extension<User>>, Path((name, _, rel)): Path<(String, String, String)>, uri: axum::http::Uri) -> Response {
    let o = match open(&s, user, &name, &rel, false).await {
        Ok(o) => o,
        Err(r) => return *r,
    };
    let (resource, relation) = (o.resource, o.relation);
    if relation.through.is_none() {
        o.scope.rollback().await;
        return reject(StatusCode::NOT_FOUND, "NotFound", &format!("{rel:?} is not a many-to-many relation"));
    }
    let raw = crate::server::segment(&uri, 1);
    let types: Vec<_> = resource.key_columns.iter().map(|c| c.ty.ty).collect();
    let key = match key::decode(&types, raw) {
        Ok(k) => k,
        Err(m) => {
            o.scope.rollback().await;
            return reject(StatusCode::BAD_REQUEST, "BadRequest", &m);
        }
    };
    let user = o.user.clone();
    let (scope, reach) = match crate::server::reach(&s, resource, &user, &o.grants, Vec::new(), o.scope).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let out = async {
        if list::detail(&s.site, resource, scope.executor(), s.target, &key, &reach.rows).await?.is_none() {
            return Ok(None);
        }
        let linked = current(&s, scope.executor(), relation, resource, &key[0], None).await?;
        let truncated = linked.len() > LINKS_CAP;
        let rows = offer(&s, relation, &user, &scope, false).await?;
        let found = offered(&s, scope.executor(), relation, &rows, &linked[..linked.len().min(LINKS_CAP)]).await?;
        let items: Vec<Value> = found.into_iter().map(|(value, label)| json!({"value": value, "label": label})).collect();
        Ok::<_, ListError>(Some((items, truncated)))
    }
    .await;
    match out {
        Ok(Some((items, truncated))) => finish(scope, Ok(()), |()| Json(json!({"ok": true, "items": items, "truncated": truncated})).into_response()).await,
        Ok(None) => {
            scope.rollback().await;
            reject(StatusCode::NOT_FOUND, "NotFound", "no such record")
        }
        Err(e) => {
            scope.rollback().await;
            error_response(e)
        }
    }
}

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
use monorm_core::ir::{ColType, RelKind, RelationSide};
use monorm_core::schema::Schema;
use monorm_engine::db::Executor;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::json;
use crate::key;
use crate::list::{self, Binds, ListError, ListRequest, Position, RowScope};
use crate::permissions::{Grants, Op};
use crate::scope::Scoped;
use crate::server::{authorize, deny, error_response, finish, found_keys, queryset_rows, reject, user_key, Shared, State_, User};
use crate::sight::Sight;
use crate::error::Fault;
use crate::site::{config_err, column, err, queryset_of, Column, Queryset, Resource, Source};
use crate::site_config::{QuerysetConfig, RelationConfig};

/// The rows that one autocomplete call returns when the call sets no limit, and the most it returns.
const PICK_PAGE: usize = 20;
const PICK_MAX: usize = 50;
/// The most links that the chips of one record load, and the most keys in one side of a write.
pub(crate) const LINKS_CAP: usize = 200;
pub(crate) const LINKS_PER_WRITE: usize = 500;

/// A picker or chips named by the text column `label`, with the `choices` filter when there is one.
pub(crate) fn picker(label: &str, choices: Option<Value>) -> RelationConfig {
    RelationConfig { choices: choices.map(|filter| QuerysetConfig { filter: Some(filter), slots: Vec::new(), function: None }), create: false, label: Some(label.to_owned()) }
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

/// How a read-only link finds the related rows: a relation with fixed conditions or union members has no picker and no chips.
#[derive(Debug, Clone)]
pub enum Linked {
    /// A to-one union: the stored value of `type_column` names the member, and `key` holds the key of the related row.
    Union { type_column: Column, key: Column, members: Vec<UnionMember> },
    /// A to-many relation with fixed conditions on the target: the target rows whose `to` equals the key of the source.
    Many { to: Column, conditions: Vec<(Column, Value)>, label: Option<Column> },
}

/// One member of a union: the resource it leads to, and the value of the type column that selects it (as a row shows it).
#[derive(Debug, Clone)]
pub struct UnionMember {
    pub target: String,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub struct Relation {
    /// The relation name.
    pub name: String,
    /// The foreign key field of the source that a picker fills. Empty for chips.
    pub field: String,
    /// The name of the target resource.
    pub target: String,
    /// The column of the target that names a row. A link (`linked`) names its rows itself and ignores this one.
    pub label: Column,
    pub choices: Option<Queryset>,
    pub create: bool,
    /// Set for chips (many-to-many).
    pub through: Option<Through>,
    /// Set for a read-only link.
    pub linked: Option<Linked>,
}

impl Relation {
    /// What the form needs to draw the field: the name, the target resource and the label.
    pub fn meta(&self) -> Value {
        json!({"name": self.name, "target": self.target, "label": self.label.label, "create": self.create, "choices": self.choices.is_some()})
    }

    /// The columns of the source that the relation reads: a user who may not see one of them gets no relation.
    pub fn needs(&self) -> Vec<&str> {
        let mut out = Vec::new();
        if !self.field.is_empty() {
            out.push(self.field.as_str());
        }
        if let Some(Linked::Union { type_column, key, .. }) = &self.linked {
            out.extend([type_column.label.as_str(), key.label.as_str()]);
        }
        out
    }
}

fn plain_text(c: &Column) -> bool {
    matches!(c.ty.ty, ColType::String | ColType::Text) && !c.ty.array && c.ty.enum_idx.is_none() && c.path.is_empty() && c.source == Source::Field
}

/// The picker and chip settings of a resource, checked against the target resources.
pub(crate) fn build(schema: &Schema, resources: &[Resource], r: &Resource, config: &std::collections::BTreeMap<String, RelationConfig>) -> Result<Vec<Relation>, Fault> {
    let model = schema.model(r.model);
    let mut out = Vec::new();
    for (name, c) in config {
        let at = format!("{}: relation {name:?}", r.name);
        if let Some(rel) = model.ir.relations.iter().find(|x| x.name == *name).filter(|x| x.is_conditioned()) {
            out.push(link(schema, resources, r, &at, rel, c)?);
            continue;
        }
        let Ok((rel, target_idx)) = model.relation(name) else { return err(format!("{at} is not a relation of the model")) };
        if rel.is_conditioned() {
            return err(format!("{at} is one member of a union relation; name the union relation"));
        }
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
        let mut relation = Relation { name: name.clone(), field: String::new(), target: target.name.clone(), label, choices, create: c.create, through: None, linked: None };
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
                let join = schema.model_idx(&t.model).map_err(|_| config_err(format!("{at}: unknown join model {:?}", t.model)))?;
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

/// A relation with fixed conditions or union members, shown as a read-only link to the page of the related row.
fn link(schema: &Schema, resources: &[Resource], r: &Resource, at: &str, rel: &monorm_core::ir::RelationIr, c: &RelationConfig) -> Result<Relation, Fault> {
    if c.choices.is_some() || c.create {
        return err(format!("{at}: a relation with fixed conditions or union members is a read-only link: it takes no choices and no create"));
    }
    let [source_key] = &r.key_columns[..] else { return err(format!("{at}: {} has a composite key, which a link does not support", r.name)) };
    let model = schema.model(r.model);
    let mut relation = Relation { name: rel.name.clone(), field: String::new(), target: String::new(), label: source_key.clone(), choices: None, create: false, through: None, linked: None };
    if !rel.members.is_empty() {
        if c.label.is_some() {
            return err(format!("{at}: a union link takes no label: each row is named by its own resource"));
        }
        let mut fields = Vec::new();
        let mut members = Vec::new();
        let mut key = None;
        for member in &rel.members {
            let [cond] = &member.conditions[..] else { return err(format!("{at}: a union member needs one condition on a field of the source")) };
            if cond.side != RelationSide::Source || member.from.len() != 1 || member.to.len() != 1 {
                return err(format!("{at}: a union member needs one condition on a field of the source and one key field"));
            }
            fields.push(cond.field.clone());
            key.get_or_insert(&member.from[0]);
            let Ok((_, idx)) = model.relation(&member.name) else { return err(format!("{at}: {:?} is not a relation of the model", member.name)) };
            // A member whose model is no resource has no page to link to: its rows show no link.
            let Some(target) = resources.iter().find(|o| o.model == idx) else { continue };
            let [target_key] = &schema.model(idx).key_fields()[..] else { continue };
            if !(member.to[0] == target_key.column || member.to[0] == target_key.name) {
                return err(format!("{at}: the key field must lead to the key of {}", target.name));
            }
            members.push((target.name.clone(), cond.value.clone()));
        }
        let (Some(field), Some(key)) = (fields.first(), key) else { return err(format!("{at}: a union needs members")) };
        if fields.iter().any(|f| f != field) {
            return err(format!("{at}: the members of a union must read one type field"));
        }
        if members.is_empty() {
            return err(format!("{at}: no member of the union leads to a resource"));
        }
        let type_column = column(schema, r.model, field)?;
        let key = column(schema, r.model, key)?;
        let members = members.into_iter().map(|(target, value)| UnionMember { target, value: crate::json::shown(schema, type_column.ty, &value) }).collect();
        relation.linked = Some(Linked::Union { type_column, key, members });
        return Ok(relation);
    }
    let Ok((_, target_idx)) = model.relation(&rel.name) else { return err(format!("{at} is not a relation of the model")) };
    if rel.kind != RelKind::Many || rel.through.is_some() || !rel.path.is_empty() || rel.from.len() != 1 || rel.to.len() != 1 || rel.conditions.iter().any(|x| x.side != RelationSide::Target) {
        return err(format!("{at} has fixed conditions the admin cannot show as a link: it takes a to-one union or a to-many relation with conditions on the target"));
    }
    let Some(target) = resources.iter().find(|o| o.model == target_idx) else { return err(format!("{at} leads to {}, which is not a resource", schema.model(target_idx).ir.name)) };
    if !(rel.from[0] == source_key.field) {
        return err(format!("{at}: the relation must start at the key of {}", r.name));
    }
    let [_] = &target.key_columns[..] else { return err(format!("{at}: {} has a composite key, which a link does not support", target.name)) };
    let label = match &c.label {
        Some(l) => {
            let col = column(schema, target.model, l)?;
            if !plain_text(&col) {
                return err(format!("{at}: label {l:?} is not a text column of {}", target.name));
            }
            Some(col)
        }
        None => crate::site::name_of(target).cloned(),
    };
    let mut conditions = Vec::new();
    for x in &rel.conditions {
        conditions.push((column(schema, target.model, &x.field)?, x.value.clone()));
    }
    relation.target = target.name.clone();
    relation.linked = Some(Linked::Many { to: column(schema, target.model, &rel.to[0])?, conditions, label });
    Ok(relation)
}

/// The picker and the chips need the target resource: `build` checked that it exists.
fn target_of<'a>(s: &'a State_, relation: &Relation) -> &'a Resource {
    s.site.resource(&relation.target).expect("a relation leads to a resource: `build` checked it")
}

/// The target as the user may see it: a hidden field is not there.
fn target_view<'a>(s: &'a State_, relation: &Relation, grants: &Grants) -> Sight<'a> {
    s.site.sight(&relation.target, grants).expect("a relation leads to a resource: `build` checked it")
}

/// The rows of the target that the relation offers: the `queryset` of the target and, with `choices`, the `choices` of the relation.
/// The read defaults that read a scope value are off, as for every route. The admin ignores tenancy.
pub(crate) async fn offer(s: &State_, relation: &Relation, user: &User, scope: &Scoped, choices: bool) -> Result<RowScope, ListError> {
    let target = target_of(s, relation);
    let mut rows = visible(s, target, user, scope).await?;
    if let (true, Some(q)) = (choices, &relation.choices) {
        queryset_rows(s, target, q, user, scope, &mut rows).await?;
    }
    Ok(rows)
}

/// The rows of `target` that its `queryset` leaves: what a link may name.
async fn visible(s: &State_, target: &Resource, user: &User, scope: &Scoped) -> Result<RowScope, ListError> {
    let mut rows = RowScope { without: target.scope_defaults.clone(), user_key: user_key(user), ..RowScope::default() };
    if let Some(q) = &target.queryset {
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
pub(crate) async fn offered(s: &State_, conn: &dyn Executor, relation: &Relation, grants: &Grants, rows: &RowScope, keys: &[Value]) -> Result<Vec<(Value, String)>, ListError> {
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
    // A label that the user may not see falls back to the key.
    let label = if target_view(s, relation, grants).hidden.contains(&relation.label.label) { key } else { &relation.label };
    let mut filters = vec![json!({"t": "in", "item": list::col(key), "values": params})];
    filters.extend(rows.push(&mut binds));
    let mut op = json!({
        "op": "select",
        "model": s.site.schema.model(target.model).ir.name,
        "columns": [{"t": "expr", "expr": list::col(key)}, {"t": "expr", "expr": list::col(label)}],
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
        out.push((cells[0].clone(), cells[1].as_str().map_or_else(|| if cells[1].is_null() { String::new() } else { cells[1].to_string() }, str::to_owned)));
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
    resource: Sight<'a>,
    relation: Relation,
    /// The target resource as the user may see it. A union link has one for each member and none here.
    target: Option<Sight<'a>>,
}

/// The preamble of the two routes: the user, the source resource and relation (404), the permission on the source
/// (`write`: create or update; else read) and the read permission on the target.
async fn open<'a>(s: &'a State_, user: Option<Extension<User>>, name: &str, rel: &str, write: bool) -> Result<Opened<'a>, Box<Response>> {
    let (user, scope, grants) = authorize(s, user).await?;
    let Some(resource) = s.site.sight(name, &grants) else {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", &format!("no resource {name:?}"))));
    };
    let Some(relation) = resource.relations.iter().find(|r| r.name == rel).cloned() else {
        scope.rollback().await;
        return Err(Box::new(reject(StatusCode::NOT_FOUND, "NotFound", &format!("no relation field {rel:?} on {name:?}"))));
    };
    let ops: &[Op] = if write { &[Op::Create, Op::Update] } else { &[Op::Read] };
    if !ops.iter().any(|&op| resource.allow.contains(&op)) {
        return Err(Box::new(deny(&s.site, scope, &user, &resource.name, json!({"reason": "allow"})).await));
    }
    if !ops.iter().any(|&op| resource.allow.contains(&op) && s.site.allows(&grants, &resource, op)) {
        let needs: Vec<_> = ops.iter().filter(|op| resource.allow.contains(op)).flat_map(|&op| s.site.requires(&resource, op)).collect();
        return Err(Box::new(deny(&s.site, scope, &user, &resource.name, json!({"needs": needs})).await));
    }
    if matches!(relation.linked, Some(Linked::Union { .. })) {
        return Ok(Opened { user, scope, grants, resource, relation, target: None });
    }
    let target = target_view(s, &relation, &grants);
    if !target.allow.contains(&Op::Read) {
        return Err(Box::new(deny(&s.site, scope, &user, &target.name, json!({"reason": "allow"})).await));
    }
    if !s.site.allows(&grants, &target, Op::Read) {
        return Err(Box::new(deny(&s.site, scope, &user, &target.name, json!({"needs": s.site.requires(&target, Op::Read)})).await));
    }
    Ok(Opened { user, scope, grants, resource, relation, target: Some(target) })
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
    let (Some(target), relation) = (&o.target, &o.relation) else {
        o.scope.rollback().await;
        return reject(StatusCode::NOT_FOUND, "NotFound", &format!("no relation field {rel:?} on {name:?}"));
    };
    if relation.linked.is_some() {
        o.scope.rollback().await;
        return reject(StatusCode::NOT_FOUND, "NotFound", &format!("{rel:?} is a read-only link: it has no picker"));
    }
    // A label that the user may not see falls back to the key, so the picker still works and shows no hidden value.
    let label_at = target.columns.iter().position(|c| c.label == relation.label.label).unwrap_or(target.key_positions[0]);
    let out = async {
        let rows = offer(&s, relation, &o.user, &o.scope, true).await?;
        let position = q.cursor.map_or(Position::First, Position::After);
        let named = target.named_filters.iter().map(|n| (n.name.clone(), "all".to_string())).collect();
        let mut request = ListRequest { limit: Some(q.limit.unwrap_or(PICK_PAGE).min(PICK_MAX)), q: q.q, named, scope: rows, ..ListRequest::new(position) };
        list::check_request(&s.site, target, &request)?;
        request.found = found_keys(&s, target, &o.scope, request.q.as_deref()).await?;
        list::list(&s.site, target, o.scope.executor(), s.target, s.viewer(), &request).await
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
    let (resource, relation) = (&o.resource, &o.relation);
    if relation.linked.is_some() {
        return linked_rows(&s, o, &uri).await;
    }
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
        if list::detail(&s.site, resource, scope.executor(), s.target, s.viewer(), &key, &reach.rows).await?.is_none() {
            return Ok(None);
        }
        let linked = current(&s, scope.executor(), relation, resource, &key[0], None).await?;
        let truncated = linked.len() > LINKS_CAP;
        let rows = offer(&s, relation, &user, &scope, false).await?;
        let found = offered(&s, scope.executor(), relation, &o.grants, &rows, &linked[..linked.len().min(LINKS_CAP)]).await?;
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

/// The related rows of a read-only link: `items` hold the key, the label and the resource of each row.
/// A user who may not read the target gets no item. The cap is `LINKS_CAP`, as for chips.
async fn linked_rows(s: &State_, o: Opened<'_>, uri: &axum::http::Uri) -> Response {
    let resource = &o.resource;
    let raw = crate::server::segment(uri, 1);
    let types: Vec<_> = resource.key_columns.iter().map(|c| c.ty.ty).collect();
    let key = match key::decode(&types, raw) {
        Ok(k) => k,
        Err(m) => {
            o.scope.rollback().await;
            return reject(StatusCode::BAD_REQUEST, "BadRequest", &m);
        }
    };
    let user = o.user.clone();
    let (scope, reach) = match crate::server::reach(s, resource, &user, &o.grants, Vec::new(), o.scope).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let out = async {
        if list::detail(&s.site, resource, scope.executor(), s.target, s.viewer(), &key, &reach.rows).await?.is_none() {
            return Ok(None);
        }
        let Some(linked) = &o.relation.linked else { return Ok(Some((Vec::new(), false))) };
        let found = match linked {
            Linked::Many { to, conditions, label } => {
                let target = o.target.as_ref().expect("a to-many link has a target");
                let mut items = read_target(s, &user, &scope, &o.grants, target, label.as_ref(), vec![(to, &key[0])], conditions, LINKS_CAP + 1).await?;
                let truncated = items.len() > LINKS_CAP;
                items.truncate(LINKS_CAP);
                (items, truncated)
            }
            Linked::Union { type_column, key: key_column, members } => {
                let mut binds = Binds::default();
                let param = binds.push(json::bind(resource.key_columns[0].ty.ty, &key[0]).map_err(ListError::Bad)?);
                let op = json!({
                    "op": "select",
                    "model": s.site.schema.model(resource.model).ir.name,
                    "columns": [{"t": "expr", "expr": list::col(type_column)}, {"t": "expr", "expr": list::col(key_column)}],
                    "filters": [{"t": "cmp", "op": "eq", "l": list::col(&resource.key_columns[0]), "r": param}],
                    "limit": 1,
                });
                let selected = list::run_select(&s.site, scope.executor(), s.target, op, binds).await?;
                let row = cells(s, &selected)?.into_iter().next();
                let mut items = Vec::new();
                if let Some([kind, value]) = row.as_deref() {
                    let hit = members.iter().find(|m| m.value == *kind).and_then(|m| s.site.sight(&m.target, &o.grants));
                    if let (Some(target), false) = (hit, value.is_null()) {
                        if target.allow.contains(&Op::Read) && s.site.allows(&o.grants, &target, Op::Read) {
                            let at = &target.key_columns[0];
                            items = read_target(s, &user, &scope, &o.grants, &target, crate::site::name_of(&target), vec![(at, value)], &[], 1).await?;
                        }
                    }
                }
                (items, false)
            }
        };
        Ok::<_, ListError>(Some(found))
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

/// The rows of `cells` as JSON.
fn cells(s: &State_, selected: &monorm_engine::exec::Selected) -> Result<Vec<Vec<Value>>, ListError> {
    let mut out = Vec::new();
    for r in 0..selected.rows.len() {
        let mut row = Vec::new();
        for (c, ty) in selected.plan.types.iter().enumerate() {
            let cell = selected.rows.cell(r, c, *ty).map_err(|e| ListError::Engine(e.to_string()))?;
            row.push(json::cell(&s.site.schema, *ty, cell));
        }
        out.push(row);
    }
    Ok(out)
}

/// Rows of `target` where each `equal` column holds its value and each condition holds, as link items.
/// The `queryset` of the target applies. A label that the user may not see falls back to the key.
#[allow(clippy::too_many_arguments)]
async fn read_target(s: &State_, user: &User, scope: &Scoped, _grants: &Grants, target: &Sight<'_>, label: Option<&Column>, equal: Vec<(&Column, &Value)>, conditions: &[(Column, Value)], limit: usize) -> Result<Vec<Value>, ListError> {
    let rows = visible(s, target, user, scope).await?;
    let mut binds = Binds::default();
    let mut filters = Vec::new();
    for (c, v) in equal.into_iter().chain(conditions.iter().map(|(c, v)| (c, v))) {
        let param = binds.push(json::bind(c.ty.ty, v).map_err(ListError::Bad)?);
        filters.push(json!({"t": "cmp", "op": "eq", "l": list::col(c), "r": param}));
    }
    filters.extend(rows.push(&mut binds));
    let key = &target.key_columns[0];
    let label = label.filter(|l| !target.hidden.contains(&l.label)).unwrap_or(key);
    let mut op = json!({
        "op": "select",
        "model": s.site.schema.model(target.model).ir.name,
        "columns": [{"t": "expr", "expr": list::col(key)}, {"t": "expr", "expr": list::col(label)}],
        "filters": filters,
        "order": [{"expr": list::col(key), "desc": false}],
        "limit": limit,
    });
    rows.apply(&mut op);
    let selected = list::run_select(&s.site, scope.executor(), s.target, op, binds).await?;
    Ok(cells(s, &selected)?
        .into_iter()
        .map(|c| json!({"value": c[0], "label": c[1].as_str().map_or_else(|| if c[1].is_null() { c[0].to_string() } else { c[1].to_string() }, str::to_owned), "resource": target.name}))
        .collect())
}

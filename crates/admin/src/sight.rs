//! The view of one user on a resource: the page without the fields that the user may not see or change.
//!
//! `see` and `change` bind field permissions to fields. A user view is the resource with the hidden fields cut out of every part,
//! so a handler that reads the view cannot show, search, filter, sort, export or write a hidden field.
//! A view is made once for each set of granted names of a page and kept; a user who holds every name gets the page itself.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;
use std::sync::Arc;

use crate::meta;
use crate::permissions::Grants;
use crate::site::{field_columns, Column, FilterKind, Resource, Site};
use serde_json::Value;

/// The model of the admin log: its `changes` hold the values of the audited resources.
const LOG: &str = "AdminLogEntry";

/// The resource as one user may use it.
#[derive(Debug, Clone)]
pub enum Sight<'a> {
    /// The page as built: the user holds every name that binds a field, or no name binds one.
    Page(&'a Resource),
    View(Arc<Resource>),
    /// An inline of a view: the child page that the view already restricted.
    Inline(Arc<Resource>, usize),
}

impl Deref for Sight<'_> {
    type Target = Resource;

    fn deref(&self) -> &Resource {
        match self {
            Sight::Page(r) => r,
            Sight::View(r) => r,
            Sight::Inline(parent, at) => &parent.inlines[*at].resource,
        }
    }
}

/// The full names of the field permissions of `r` and its inlines that bind a field, sorted: the key of a view.
pub fn bound_names(r: &Resource) -> Vec<String> {
    let mut out = BTreeSet::new();
    for page in std::iter::once(r).chain(r.inlines.iter().map(|i| &i.resource)) {
        for p in page.see.values().chain(page.change.values()) {
            out.insert(format!("{}.{p}", page.label));
        }
    }
    out.into_iter().collect()
}

/// The log page binds the names of every other page: what a reader may see in `changes` depends on them.
pub(crate) fn bind_log(resources: &mut [Resource]) {
    let all: BTreeSet<String> = resources.iter().flat_map(|r| r.bound.iter().cloned()).collect();
    for r in resources.iter_mut().filter(|r| r.name == LOG) {
        r.bound = all.iter().cloned().collect();
    }
}

/// The fields of `page` and of its inlines that `grants` do not let the user see, by resource name.
fn hidden_by_resource(page: &Resource, grants: &Grants, out: &mut BTreeMap<String, BTreeSet<String>>) {
    for p in std::iter::once(page).chain(page.inlines.iter().map(|i| &i.resource)) {
        let hidden = p.see.iter().filter(|(_, perm)| !grants.has(&format!("{}.{perm}", p.label))).map(|(f, _)| f.clone());
        out.entry(p.name.clone()).or_default().extend(hidden);
    }
}

/// Drops from a `changes` value the fields in `hidden`: the keys of the `diff` and the values of the rows of the `inlines`.
fn redact_changes(changes: &mut Value, resource: &str, log_hidden: &BTreeMap<String, BTreeSet<String>>) {
    let Some(map) = changes.as_object_mut() else { return };
    if let Some(hidden) = log_hidden.get(resource).filter(|h| !h.is_empty()) {
        if let Some(diff) = map.get_mut("diff").and_then(Value::as_object_mut) {
            diff.retain(|field, _| !hidden.contains(field));
        }
    }
    let Some(sets) = map.get_mut("inlines").and_then(Value::as_object_mut) else { return };
    for set in sets.values_mut() {
        let Some(set) = set.as_object_mut() else { continue };
        let Some(hidden) = set.get("model").and_then(Value::as_str).and_then(|m| log_hidden.get(m)).filter(|h| !h.is_empty()).cloned() else { continue };
        for rows in ["create", "update", "delete"] {
            for row in set.get_mut(rows).and_then(Value::as_array_mut).into_iter().flatten() {
                for part in ["set", "loaded", "before"] {
                    if let Some(values) = row.get_mut(part).and_then(Value::as_object_mut) {
                        values.retain(|field, _| !hidden.contains(field));
                    }
                }
            }
        }
    }
}

/// Redacts the `changes` of the rows of a page of the log for the reader of `view`. `columns` are the columns of the rows.
pub(crate) fn redact_log(view: &Resource, columns: &[Column], rows: &mut [Value]) {
    for row in rows {
        if let Some(cells) = row.as_array_mut() {
            redact_row(view, columns, cells);
        }
    }
}

/// `redact_log` for one row: the cells of a record.
pub(crate) fn redact_row(view: &Resource, columns: &[Column], cells: &mut [Value]) {
    if view.name != LOG || view.log_hidden.values().all(BTreeSet::is_empty) {
        return;
    }
    let at = |field: &str| columns.iter().position(|c| c.field == field);
    let (Some(changes), Some(resource)) = (at("changes"), at("resource")) else { return };
    let name = cells[resource].as_str().unwrap_or_default().to_owned();
    redact_changes(&mut cells[changes], &name, &view.log_hidden);
}

impl Site {
    /// The resource `name` as the user with `grants` may use it. A handler reads this, never `resource`.
    pub fn sight<'a>(&'a self, name: &str, grants: &Grants) -> Option<Sight<'a>> {
        let at = self.resources.iter().position(|r| r.name == name)?;
        Some(self.sight_at(at, grants))
    }

    /// The view of a resource that the site already holds.
    pub fn sight_of<'a>(&'a self, resource: &Resource, grants: &Grants) -> Sight<'a> {
        let at = self.resources.iter().position(|r| r.name == resource.name).expect("a resource of the site");
        self.sight_at(at, grants)
    }

    fn sight_at(&self, at: usize, grants: &Grants) -> Sight<'_> {
        let page = &self.resources[at];
        if page.bound.is_empty() {
            return Sight::Page(page);
        }
        let held: Vec<bool> = page.bound.iter().map(|n| grants.has(n)).collect();
        if held.iter().all(|h| *h) {
            return Sight::Page(page);
        }
        let key = (at, held);
        let cached = self.views.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
        if let Some(view) = cached {
            return Sight::View(view);
        }
        // Built outside the lock: a build does not stop the requests that hit other views. Two builds of one key give equal views; the first insert wins.
        let mut view = self.restrict(page, grants);
        view.meta = meta::resource(&self.schema, &view, &self.meta_builds);
        let view = Arc::new(view);
        let mut views = self.views.lock().unwrap_or_else(|e| e.into_inner());
        Sight::View(views.entry(key).or_insert(view).clone())
    }

    /// `page` without what `grants` do not allow: the hidden fields leave every part, the locked fields turn read-only.
    /// It reads only the names in `bound_names(page)`, the key of the view cache: two users with the same held bits get the same result.
    fn restrict(&self, page: &Resource, grants: &Grants) -> Resource {
        let missing = |map: &std::collections::BTreeMap<String, String>| -> BTreeSet<String> {
            map.iter().filter(|(_, p)| !grants.has(&format!("{}.{p}", page.label))).map(|(f, _)| f.clone()).collect()
        };
        let hidden = missing(&page.see);
        let mut locked = missing(&page.change);
        locked.retain(|f| !hidden.contains(f));
        let mut v = page.clone();
        v.columns.retain(|c| !hidden.contains(&c.label));
        v.order.retain(|o| !hidden.contains(&o.column.label));
        let at = |label: &str| v.columns.iter().position(|c| c.label == label).expect("an order or key column is a column of the view");
        v.order_positions = v.order.iter().map(|o| at(&o.column.label)).collect();
        v.key_positions = v.key_columns.iter().map(|k| at(&k.label)).collect();
        v.detail.retain(|c| !hidden.contains(&c.label));
        v.links = page
            .links
            .iter()
            .filter(|l| l.from.iter().all(|&p| !hidden.contains(&page.detail[p].label)))
            .map(|l| {
                let mut l = l.clone();
                l.from = l.from.iter().map(|&p| v.detail.iter().position(|d| d.label == page.detail[p].label).expect("a link reads visible fields")).collect();
                l
            })
            .collect();
        v.refs.retain(|f| !hidden.contains(&f.column));
        v.search.retain(|s| !hidden.contains(&s.column.label));
        v.sortable.retain(|s| !hidden.contains(&s.column.label) && !hidden.contains(&s.column.order_by().label));
        v.filters.retain(|f| match &f.kind {
            FilterKind::Field(c) => !hidden.contains(&c.label),
            FilterKind::Relation { from, .. } => from.iter().all(|c| !hidden.contains(&c.label)),
        });
        v.files.retain(|f| !hidden.contains(f));
        v.editable.retain(|f| !hidden.contains(f) && !locked.contains(f));
        v.relations.retain(|r| r.field.is_empty() || !hidden.contains(&r.field));
        for sections in [&mut v.form.create, &mut v.form.edit] {
            for s in sections.iter_mut() {
                s.fields.retain(|f| !hidden.contains(f));
            }
            sections.retain(|s| !s.fields.is_empty());
        }
        v.form.readonly.extend(locked.iter().cloned());
        let model = self.schema.model(page.model);
        v.tabs.retain(|t| {
            let from = model.ir.relations.iter().find(|r| r.name == t.name).and_then(|r| field_columns(&self.schema, page.model, &r.from));
            from.is_some_and(|f| f.iter().all(|c| !hidden.contains(&c.label)))
        });
        v.inlines.retain(|i| i.parent_from.iter().all(|c| !hidden.contains(&c.label)));
        for i in &mut v.inlines {
            i.resource = self.restrict(&i.resource, grants);
        }
        v.hidden = hidden;
        v.locked = locked;
        if page.name == LOG {
            for other in &self.resources {
                hidden_by_resource(other, grants, &mut v.log_hidden);
            }
        }
        v
    }
}

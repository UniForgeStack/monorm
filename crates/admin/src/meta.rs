//! What the UI reads about a site: the brand and, for each resource, titles, widgets and links.
//! `Site::build` runs this once; a request only clones the result.

use crate::relations::Linked;
use std::sync::atomic::{AtomicUsize, Ordering};

use monorm_core::ir::ColType;
use monorm_core::schema::Schema;
use serde_json::Value;

use crate::list::{is_text, ranged};
use crate::error::{opt, Fault};
use crate::site::{invalid, Column, Filter, FilterKind, Resource, Source};
use crate::site_config::{BrandConfig, SwitcherBuild, SwitcherConfig, ThemeConfig};
use crate::wire;

/// A link target that a page may use: a path on this host or an `http` or `https` URL.
pub(crate) fn url_ok(url: &str) -> bool {
    let plain = url.bytes().all(|b| b.is_ascii_graphic());
    plain && ((url.starts_with('/') && !url.starts_with("//")) || url.starts_with("https://") || url.starts_with("http://"))
}

pub(crate) fn color_ok(c: &str) -> bool {
    let digits = c.strip_prefix('#').unwrap_or("");
    matches!(digits.len(), 3 | 6) && digits.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The brand as the UI reads it. A wrong value is a config error that names the setting.
pub fn brand(config: Option<&BrandConfig>, theme: Option<&ThemeConfig>, switcher: Option<&SwitcherConfig>) -> Result<Value, Fault> {
    let default = BrandConfig::default();
    let c = config.unwrap_or(&default);
    let no_theme = ThemeConfig::default();
    let t = theme.unwrap_or(&no_theme);
    for (set, key) in [(c.name.is_some() && t.title.is_some(), "title"), (c.logo.is_some() && t.logo.is_some(), "logo"), (c.colors.contains_key("primary") && t.color.is_some(), "color")] {
        if set {
            let twin = match key {
                "title" => "brand.name",
                "logo" => "brand.logo",
                _ => "brand.colors.primary",
            };
            return invalid(format!("theme.{key} and {twin} both set the same value: set one of them"));
        }
    }
    let mut colors = c.colors.clone();
    let switcher = match switcher {
        Some(s) => switcher_view(s, colors.contains_key("primary") || t.color.is_some())?,
        None => None,
    };
    if let Some(color) = switcher.as_ref().and_then(|v| v.color.as_ref()) {
        colors.insert("primary".to_owned(), color.clone());
    }
    if let Some(color) = &t.color {
        if !color_ok(color) {
            return invalid(format!("theme.color {color:?} must be #rgb or #rrggbb"));
        }
        colors.insert("primary".to_owned(), color.clone());
    }
    let logo = t.logo.clone().or_else(|| c.logo.clone());
    let name = t.title.clone().or_else(|| c.name.clone()).unwrap_or_else(|| "Admin".to_owned());
    if name.trim().is_empty() || name.chars().count() > 80 {
        return invalid(format!("{} must have 1 to 80 characters", if t.title.is_some() { "theme.title" } else { "brand.name" }));
    }
    if let Some(logo) = &logo {
        // The CSP allows images from this origin and from https only.
        if !url_ok(logo) || logo.starts_with("http://") {
            let setting = if t.logo.is_some() { "theme.logo" } else { "brand.logo" };
            return invalid(format!("{setting} {logo:?} must be a path that starts with / or an https URL"));
        }
    }
    for (key, value) in &colors {
        if !matches!(key.as_str(), "primary" | "accent") {
            return invalid(format!("brand.colors has {key:?}, not one of primary, accent"));
        }
        if !color_ok(value) {
            return invalid(format!("brand.colors.{key} {value:?} must be #rgb or #rrggbb"));
        }
    }
    let mut links = Vec::new();
    for l in &c.links {
        if l.label.trim().is_empty() || l.label.chars().count() > 40 {
            return invalid("brand.links: a label has 1 to 40 characters".into());
        }
        if !url_ok(&l.url) {
            return invalid(format!("brand.links: {:?} must be a path that starts with / or an http or https URL", l.url));
        }
        links.push(wire::BrandLink { label: l.label.clone(), url: l.url.clone() });
    }
    if c.environment.as_ref().is_some_and(|e| e.trim().is_empty() || e.chars().count() > 32) {
        return invalid("brand.environment must have 1 to 32 characters".into());
    }
    let level = environment_level(c)?;
    let colors = wire::BrandColors { primary: colors.remove("primary"), accent: colors.remove("accent") };
    let brand = wire::Brand { name, logo, colors, links, environment: c.environment.clone(), environment_level: level, switcher: switcher.map(|v| v.meta) };
    Ok(serde_json::to_value(brand).expect("the brand is JSON"))
}

/// The colour of the environment label: the set level, else the one the label implies.
fn environment_level(c: &BrandConfig) -> Result<Option<String>, Fault> {
    let Some(env) = &c.environment else {
        return match c.environment_level {
            Some(_) => invalid(format!("brand.{} needs brand.environment", opt("environment_level"))),
            None => Ok(None),
        };
    };
    if let Some(level) = &c.environment_level {
        if !matches!(level.as_str(), "info" | "warning" | "danger") {
            return invalid(format!("brand.{} {level:?} must be info, warning or danger", opt("environment_level")));
        }
        return Ok(Some(level.clone()));
    }
    Ok(Some(match env.trim().to_lowercase().as_str() {
        "production" | "prod" => "danger",
        "staging" => "warning",
        _ => "info",
    }.to_owned()))
}

/// A lucide icon name in kebab case. The UI decides which names it knows.
pub(crate) fn icon_ok(icon: &str) -> bool {
    icon.len() <= 64 && icon.starts_with(|c: char| c.is_ascii_lowercase()) && icon.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The value of a build setting. An empty string is no value: an unset `${VAR}` of `monorm.toml` gives one.
fn build_value(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// The `build` part of the switcher: the value sent, the rows and the subheader.
struct BuildView {
    meta: wire::SwitcherBuildMeta,
    rows: Vec<String>,
    subheader: String,
}

fn build_view(b: &SwitcherBuild) -> BuildView {
    let (commit, built, deployed, tree) = (build_value(&b.commit), build_value(&b.built_at), build_value(&b.deployed_at), build_value(&b.tree));
    let mut rows = Vec::new();
    match (built, commit) {
        (Some(t), Some(c)) => rows.push(format!("built {t} · commit {}", short(c))),
        (Some(t), None) => rows.push(format!("built {t}")),
        (None, Some(c)) => rows.push(format!("commit {}", short(c))),
        (None, None) => {}
    }
    if let Some(t) = deployed {
        rows.push(format!("deployed {t}"));
    }
    if let Some(t) = tree {
        rows.push(format!("tree {}", short(t)));
    }
    let head: Vec<String> = deployed.map(str::to_owned).into_iter().chain(tree.map(short)).collect();
    let subheader = if head.is_empty() { "dev".to_owned() } else { head.join(" · ") };
    let own = |v: Option<&str>| v.map(str::to_owned);
    BuildView { meta: wire::SwitcherBuildMeta { commit: own(commit), built_at: own(built), deployed_at: own(deployed), tree: own(tree) }, rows, subheader }
}

/// The switcher as sent, and the colour of the current service.
struct SwitcherView {
    meta: wire::Switcher,
    color: Option<String>,
}

/// Build the switcher view. `s` has passed `SwitcherConfig::check`. `brand_primary`: the brand already sets the primary colour.
fn switcher_view(s: &SwitcherConfig, brand_primary: bool) -> Result<Option<SwitcherView>, Fault> {
    s.check()?;
    let services = s.services.as_deref().unwrap_or(&[]);
    let current = s.current.as_deref();
    if current.is_none() && services.is_empty() && s.build == SwitcherBuild::default() {
        return Ok(None);
    }
    let mut out = Vec::new();
    let mut current_color = None;
    for svc in services {
        let is_current = current == Some(svc.key.as_str());
        let url = if is_current { None } else { svc.url.clone() };
        if is_current {
            current_color = svc.color.clone();
        }
        out.push(wire::SwitcherService { key: svc.key.clone(), title: svc.title.clone(), url, icon: svc.icon.clone(), color: svc.color.clone() });
    }
    if current_color.is_some() && brand_primary {
        return invalid("the colour of the current switcher service and the brand set the primary colour: set one of them (brand.colors.primary, theme.color or the service colour)".to_owned());
    }
    let build = build_view(&s.build);
    let meta = wire::Switcher { current: current.map(str::to_owned), services: out, subheader: build.subheader, rows: build.rows, build: build.meta };
    Ok(Some(SwitcherView { meta, color: current_color }))
}

/// `created_at` and `BlogPost` become `Created at` and `Blog post`; a `.` of a path becomes a space.
pub fn humanize(name: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for ch in name.chars() {
        if ch == '_' || ch == '.' {
            out.push(' ');
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
        out.extend(ch.to_lowercase());
    }
    let mut chars = out.trim().chars();
    chars.next().map(|f| f.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

pub(crate) fn widget(c: &Column) -> &'static str {
    if c.ty.enum_idx.is_some() {
        return "select";
    }
    if c.ty.array {
        return "list";
    }
    match c.ty.ty {
        ColType::Bool => "checkbox",
        ColType::Int | ColType::BigInt | ColType::Float | ColType::Decimal => "number",
        ColType::DateTime => "datetime",
        ColType::Time | ColType::Duration => "text",
        ColType::Date => "date",
        ColType::Text => "textarea",
        ColType::Json => "json",
        ColType::String | ColType::Uuid | ColType::Bytes => "text",
    }
}

/// One column or field: the key in a row, the title, the type and the widget.
pub fn column(schema: &Schema, c: &Column) -> wire::ColumnMeta {
    wire::ColumnMeta {
        label: c.label.clone(),
        title: humanize(&c.label),
        ty: c.ty.ty,
        nullable: c.nullable,
        array: c.ty.array,
        widget: widget(c).to_owned(),
        choices: enum_names(schema, c),
        empty: None,
        computed: None,
        ref_: None,
        ref_by: None,
        badge: None,
    }
}

fn enum_names(schema: &Schema, c: &Column) -> Option<Vec<String>> {
    let e = c.ty.enum_idx.and_then(|i| schema.enums.get(i as usize))?;
    Some(e.values.iter().map(|v| v.name.clone()).collect())
}

/// A column of a list: the title and the empty text that the config gives, and whether the value is computed.
fn list_column(schema: &Schema, r: &Resource, c: &Column) -> wire::ColumnMeta {
    let mut out = column(schema, c);
    if let Some(title) = r.labels.get(&c.label) {
        out.title = title.clone();
    }
    out.empty = r.empty_fields.get(&c.label).or(r.empty.as_ref()).cloned();
    if !matches!(c.source, Source::Field) {
        out.computed = Some(true);
    }
    out.ref_ = r.refs.iter().find(|f| f.column == c.label).map(|f| wire::ColumnRef { resource: f.target.clone() });
    out.badge = r.badges.get(&c.label).cloned();
    // The key of a union link names its record by the value of the type column.
    if let Some(Linked::Union { type_column, members, .. }) = r.relations.iter().filter_map(|x| x.linked.as_ref()).find(|l| matches!(l, Linked::Union { key, .. } if key.label == c.label)) {
        let targets = members.iter().map(|m| (m.value.as_str().map_or_else(|| m.value.to_string(), str::to_owned), m.target.clone())).collect();
        out.ref_by = Some(wire::ColumnRefBy { column: type_column.label.clone(), targets });
    }
    out
}

/// A column of the list: a foreign key shows the name of its relation ("Author") as the title, unless the config gives one.
fn list_header(schema: &Schema, r: &Resource, c: &Column) -> wire::ColumnMeta {
    let mut out = list_column(schema, r, c);
    if !r.labels.contains_key(&c.label) && c.path.is_empty() && c.source == Source::Field {
        if let Some(link) = r.links.iter().find(|l| l.from.len() == 1 && r.detail.get(l.from[0]).is_some_and(|d| d.label == c.label)) {
            out.title = humanize(&link.name);
        }
    }
    out
}

/// One filter of the filter bar: the name in the URL, the kind of value, and the operators that it takes.
fn filter(schema: &Schema, r: &Resource, f: &Filter) -> wire::FilterMeta {
    let title = r.labels.get(&f.name).cloned().unwrap_or_else(|| humanize(&f.name));
    match &f.kind {
        FilterKind::Field(c) => {
            let kind = if c.ty.enum_idx.is_some() {
                "enum"
            } else {
                match c.ty.ty {
                    ColType::Bool => "bool",
                    ColType::Int | ColType::BigInt | ColType::Float => "number",
                    ColType::DateTime => "datetime",
                    ColType::Time => "text",
                    ColType::Date => "date",
                    ColType::Uuid => "uuid",
                    _ => "text",
                }
            };
            let mut operators = vec!["eq"];
            if ranged(c.ty) {
                operators.extend(["gt", "gte", "lt", "lte"]);
            }
            if c.nullable || is_text(c.ty) {
                operators.extend(["empty", "notempty"]);
            }
            wire::FilterMeta { name: f.name.clone(), title, kind, ty: Some(c.ty.ty), nullable: c.nullable, operators, choices: enum_names(schema, c), target: None }
        }
        FilterKind::Relation { target, from, .. } => {
            let nullable = from.iter().all(|c| c.nullable);
            let operators = if nullable { vec!["eq", "empty", "notempty"] } else { vec!["eq"] };
            wire::FilterMeta { name: f.name.clone(), title, kind: "relation", ty: None, nullable, operators, choices: None, target: Some(target.clone()) }
        }
    }
}

/// For "Add" on a to-many tab: the field of the new child record and the field of the open record that gives its value.
fn prefill(schema: &Schema, parent: &Resource, tab: &crate::site::Tab) -> Vec<wire::PrefillMeta> {
    let model = schema.model(parent.model);
    let Some(rel) = model.ir.relations.iter().find(|x| x.name == tab.name) else { return Vec::new() };
    let Some(child) = schema.models.iter().position(|m| m.ir.name == tab.target) else { return Vec::new() };
    let name_of = |m: usize, n: &String| schema.model(m).ir.fields.iter().find(|f| f.name == *n || f.column == *n).map(|f| f.name.clone());
    rel.from.iter().zip(&rel.to).filter_map(|(from, to)| Some(wire::PrefillMeta { parent: name_of(parent.model, from)?, child: name_of(child, to)? })).collect()
}

/// The metadata of one inline: how it looks and saves, and the form of its rows without the fields that the parent fixes.
fn inline(schema: &Schema, inline: &crate::site::Inline) -> wire::InlineInfo {
    let r = &inline.resource;
    let pins: Vec<String> = inline.child_to.iter().map(|c| c.label.clone()).collect();
    let mut form = crate::write::form_meta(schema, r);
    form.form.retain(|f| !pins.contains(&f.name));
    for sections in [&mut form.sections.create, &mut form.sections.edit] {
        for s in sections.iter_mut() {
            s.fields.retain(|f| !pins.contains(f));
        }
        sections.retain(|s| !s.fields.is_empty());
    }
    wire::InlineInfo {
        name: inline.name.clone(),
        label: inline.label.clone(),
        style: if inline.style == "stacked" { "stacked" } else { "table" },
        max: inline.max,
        collapsed: inline.collapsed,
        delete: inline.delete,
        save: match inline.save { crate::site::InlineSave::WithParent => "with_parent", crate::site::InlineSave::Row => "row" },
        link: inline.link.clone(),
        model: r.name.clone(),
        fields: r.detail.iter().map(|c| list_column(schema, r, c)).collect(),
        pins,
        key: r.key_columns.iter().map(|c| c.label.clone()).collect(),
        can: r.can.iter().map(|(n, _)| n.clone()).collect(),
        form,
        user: None,
    }
}

/// The metadata of one resource that does not depend on the user.
pub fn resource(schema: &Schema, r: &Resource, builds: &AtomicUsize) -> Value {
    builds.fetch_add(1, Ordering::SeqCst);
    let meta = wire::ResourceMeta {
        name: r.name.clone(),
        title: r.title.clone(),
        group: r.group.clone(),
        icon: r.icon.clone(),
        columns: r.columns.iter().map(|c| list_header(schema, r, c)).collect(),
        order: r.order.iter().map(|o| wire::OrderMeta { column: o.column.label.clone(), desc: o.desc }).collect(),
        page_size: r.page_size,
        max_page_size: crate::site::MAX_PAGE_SIZE,
        sortable: r.sortable.iter().map(|c| wire::SortableMeta { column: c.column.label.clone(), indexed: c.indexed }).collect(),
        sortable_explicit: r.sortable_explicit,
        editable: r.editable.clone(),
        search: wire::SearchMeta { fields: r.search.iter().map(|f| f.column.label.clone()).collect(), custom: r.search_with.is_some() },
        permissions: vec!["read"],
        table: schema.model(r.model).table().to_owned(),
        key: r.key_columns.iter().map(|c| c.label.clone()).collect(),
        name_column: crate::site::name_of(r).map(|c| c.label.clone()),
        fields: r.detail.iter().map(|c| list_column(schema, r, c)).collect(),
        linked: r.relations.iter().filter_map(|x| x.linked.as_ref().map(|l| wire::LinkedMeta { name: x.name.clone(), title: humanize(&x.name), many: matches!(l, Linked::Many { .. }) })).collect(),
        links: r.links.iter().map(|l| wire::LinkMeta { name: l.name.clone(), title: humanize(&l.name), target: l.target.clone() }).collect(),
        filters: r.filters.iter().map(|f| filter(schema, r, f)).collect(),
        named_filters: r
            .named_filters
            .iter()
            .map(|n| wire::NamedFilterMeta { name: n.name.clone(), title: n.label.clone(), default: n.default.clone(), choices: n.choices.iter().map(|c| wire::NamedChoiceMeta { name: c.name.clone(), title: c.label.clone() }).collect() })
            .collect(),
        date_hierarchy: r.date_hierarchy.as_ref().map(|d| wire::DateHierarchyMeta { field: d.field.clone(), counts: d.counts, kind: if d.column.ty.ty == ColType::Date { "date" } else { "datetime" } }),
        tabs: r.tabs.iter().map(|t| wire::TabMeta { name: t.name.clone(), title: r.labels.get(&t.name).cloned().unwrap_or_else(|| humanize(&t.name)), target: t.target.clone(), prefill: prefill(schema, r, t) }).collect(),
        fieldsets: r.fieldsets.iter().map(|f| wire::FieldsetMeta { name: f.name.clone(), title: f.title.clone().unwrap_or_else(|| humanize(&f.name)), fields: f.fields.clone() }).collect(),
        package_tabs: r.package_tabs.iter().map(|t| wire::PackageTabMeta { name: t.name.clone(), title: t.title.clone(), module: t.module.clone() }).collect(),
        inlines: r.inlines.iter().map(|i| inline(schema, i)).collect(),
        empty: r.empty.clone(),
        can: r.can.iter().map(|(n, _)| n.clone()).collect(),
        form: crate::write::form_meta(schema, r),
        user: None,
    };
    serde_json::to_value(meta).expect("the resource metadata is JSON")
}

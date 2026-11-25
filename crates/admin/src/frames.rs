//! Frame configuration is trusted code. Row data can fill only a checked segment or query value.

use std::collections::{BTreeMap, BTreeSet};

use monorm_core::ir::{ColType, ConstraintIr};
use monorm_core::schema::Schema;
use serde_json::{json, Value};
use url::{Origin, Url};

use crate::error::{code, Fault};
use crate::site::{check_name, column, Column, Resource, Site, err_code};
use crate::site_config::{FrameConfig, Fieldset, EmbedConfig};

#[derive(Debug)]
pub(crate) struct Embed {
    pub resource: String,
    pub lookup: Column,
    pub tab: EmbedTab,
    pub csp: axum::http::HeaderValue,
    pub self_only: bool,
}

#[derive(Debug)]
pub(crate) enum EmbedTab {
    Fields(Fieldset),
    Package { name: String, title: String },
}

#[derive(Debug, Clone)]
pub(crate) struct Frame {
    name: String,
    title: String,
    sandbox: String,
    height: String,
    template: Template,
}

#[derive(Debug, Clone)]
enum Part {
    Fixed(String),
    Segment(Column),
    Query(Column),
}

#[derive(Debug, Clone)]
struct Template {
    parts: Vec<Part>,
    relative: bool,
    origin: Origin,
    // A dynamic segment can change only its own slot.
    path: Vec<Option<String>>,
}

pub(crate) fn origins(values: &[String]) -> Result<Vec<String>, Fault> {
    values
        .iter()
        .map(|value| {
            if value == "'self'" {
                return Ok(value.clone());
            }
            let bad = || {
                Fault::new(code::FRAME, format!(
                    "invalid frame origin {value:?}: use 'self' or an exact HTTPS origin (HTTP only for loopback)"
                ))
            };
            let (scheme, authority) = value.split_once("://").ok_or_else(bad)?;
            if authority.is_empty() || !authority.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b)) {
                return Err(bad());
            }
            let url = Url::parse(value).map_err(|_| bad())?;
            let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
            // Only the literal loopback hosts are accepted for HTTP; URL parsing can normalize other IP spellings.
            let literal_local = ["localhost", "127.0.0.1", "[::1]"].iter().any(|host| {
                authority.strip_prefix(host).is_some_and(|rest| {
                    rest.is_empty()
                        || rest
                            .strip_prefix(':')
                            .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
                })
            });
            if !(scheme == "https" || scheme == "http" && local && literal_local) || authority.ends_with(':') {
                return Err(bad());
            }
            Ok(url.origin().ascii_serialization())
        })
        .collect()
}

fn parse(url: &str, relative: bool) -> Option<Url> {
    if relative {
        if !url.starts_with('/') || url.starts_with("//") {
            return None;
        }
        Url::parse(&format!("https://frame.invalid{url}")).ok()
    } else {
        Url::parse(url).ok()
    }
}

fn allowed_path(path: &str, base: &str) -> bool {
    let decoded = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    let Some(rest) = decoded.strip_prefix(base) else { return true };
    if !rest.is_empty() && !rest.starts_with('/') {
        return true;
    }
    rest.strip_prefix("/-/embed/").is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

impl Template {
    fn build(raw: &str, origins: &[String], base: &str, model: Option<(&Schema, usize)>) -> Result<Self, Fault> {
        let bad = || Fault::new(code::FRAME, format!("invalid frame URL template {raw:?}"));
        if raw.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control() || b == b'\\') {
            return Err(bad());
        }
        let relative = raw.starts_with('/');
        let end = raw.find(['?', '#']).unwrap_or(raw.len());
        let path_start = if relative {
            0
        } else {
            let (scheme, _) = raw.split_once("://").ok_or_else(bad)?;
            if !matches!(scheme, "https" | "http") {
                return Err(bad());
            }
            let start = scheme.len() + 3;
            if start >= end || raw[start..end].starts_with('/') {
                return Err(bad());
            }
            raw[start..end].find('/').map_or(end, |at| start + at)
        };
        let query = raw[end..].strip_prefix('?').map(|q| q.split('#').next().unwrap_or_default());
        let mut fields = BTreeMap::new();
        let mut add = |offset: usize, value: &str, segment: bool| -> Result<(), Fault> {
            if !value.contains(['{', '}']) {
                return Ok(());
            }
            let field = value
                .strip_prefix('{')
                .and_then(|v| v.strip_suffix('}'))
                .filter(|v| !v.contains(['{', '}']))
                .ok_or_else(bad)?;
            let (schema, model) = model.ok_or_else(|| Fault::new(code::FRAME, "frame cards cannot have placeholders"))?;
            let c = column(schema, model, field)?;
            if c.path.len() > 1 || c.ty.array || c.ty.ty == ColType::Json {
                return err_code(code::FRAME, format!("frame field {field:?} must be scalar and may use at most one relation hop"));
            }
            fields.insert(offset, (value.len(), if segment { Part::Segment(c) } else { Part::Query(c) }));
            Ok(())
        };
        let mut offset = path_start;
        for segment in raw[path_start..end].split('/') {
            add(offset, segment, true)?;
            offset += segment.len() + 1;
        }
        if let Some(query) = query {
            let mut offset = end + 1;
            for pair in query.split('&') {
                if let Some((name, value)) = pair.split_once('=') {
                    add(offset + name.len() + 1, value, false)?;
                }
                offset += pair.len() + 1;
            }
        }
        let mut marker = "monorm-frame-value".to_string();
        while raw.contains(&marker) {
            marker.push('-');
        }
        let mut probe = String::new();
        let mut parts = Vec::new();
        let mut from = 0;
        let mut segment_count = 0;
        for (start, (len, part)) in fields {
            let fixed = &raw[from..start];
            if fixed.contains(['{', '}']) {
                return Err(bad());
            }
            probe.push_str(fixed);
            probe.push_str(&marker);
            segment_count += usize::from(matches!(part, Part::Segment(_)));
            parts.extend([Part::Fixed(fixed.to_owned()), part]);
            from = start + len;
        }
        let fixed = &raw[from..];
        if fixed.contains(['{', '}']) {
            return Err(bad());
        }
        probe.push_str(fixed);
        parts.push(Part::Fixed(fixed.to_owned()));
        let checked = parse(&probe, relative).ok_or_else(bad)?;
        let origin = checked.origin();
        let source = if relative { "'self'".to_owned() } else { origin.ascii_serialization() };
        if !origins.contains(&source)
            || !checked.username().is_empty()
            || checked.password().is_some()
            || (!relative && raw[..path_start].contains('@'))
        {
            return err_code(code::FRAME, format!(
                "frame URL {raw:?} requires {source} in frame_origins and cannot contain credentials"
            ));
        }
        if relative && !allowed_path(checked.path(), base) {
            return err_code(code::FRAME, format!("frame URL {raw:?} points into this admin outside /-/embed/"));
        }
        let path: Vec<_> = checked
            .path_segments()
            .ok_or_else(bad)?
            .map(|part| (part != marker).then(|| part.to_owned()))
            .collect();
        if path.iter().filter(|p| p.is_none()).count() != segment_count {
            return Err(bad());
        }
        Ok(Self {
            parts,
            relative,
            origin,
            path,
        })
    }

    fn render<'a>(&self, base: &str, value: impl Fn(&Column) -> Option<&'a Value>) -> Option<String> {
        let mut raw = String::new();
        for part in &self.parts {
            let (col, segment) = match part {
                Part::Fixed(s) => {
                    raw.push_str(s);
                    continue;
                }
                Part::Segment(c) => (c, true),
                Part::Query(c) => (c, false),
            };
            let v = value(col)?;
            if v.is_null() {
                return None;
            }
            let text = crate::key::text(v);
            if segment {
                if matches!(text.as_str(), "." | "..") {
                    return None;
                }
                raw.push_str(&crate::key::escape(&text));
            } else {
                raw.extend(form_urlencoded::byte_serialize(text.as_bytes()));
            }
        }
        let url = parse(&raw, self.relative)?;
        let segments: Vec<_> = url.path_segments()?.collect();
        (url.origin() == self.origin
            && (!self.relative || allowed_path(url.path(), base))
            && segments.len() == self.path.len()
            && self
                .path
                .iter()
                .zip(segments)
                .all(|(fixed, actual)| fixed.as_deref().is_none_or(|fixed| fixed == actual)))
        .then_some(raw)
    }
}

impl Frame {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn columns(&self) -> impl Iterator<Item = &Column> {
        self.template.parts.iter().filter_map(|p| match p {
            Part::Fixed(_) => None,
            Part::Segment(c) | Part::Query(c) => Some(c),
        })
    }

    pub(crate) fn payload<'a>(&self, base: &str, value: impl Fn(&Column) -> Option<&'a Value>) -> Value {
        json!({"name": self.name, "title": self.title, "url": self.template.render(base, value), "sandbox": self.sandbox, "height": self.height,
            "referrer_policy": if self.template.relative { "same-origin" } else { "no-referrer" }})
    }
}

pub(crate) fn build(configs: &[FrameConfig], origins: &[String], base: &str, model: Option<(&Schema, usize)>) -> Result<Vec<Frame>, Fault> {
    let mut seen = BTreeSet::new();
    configs
        .iter()
        .map(|c| {
            check_name("frame name", &c.name)?;
            if !seen.insert(&c.name) || c.title.trim().is_empty() {
                return err_code(code::FRAME, format!("frame {:?} needs a unique name and a title", c.name));
            }
            let mut sandbox = Vec::new();
            if c.scripts.unwrap_or(true) {
                sandbox.push("allow-scripts");
            }
            if c.same_origin.unwrap_or(true) {
                sandbox.push("allow-same-origin");
            }
            for flag in &c.sandbox {
                if !matches!(
                    flag.as_str(),
                    "allow-forms" | "allow-popups" | "allow-popups-to-escape-sandbox" | "allow-downloads"
                ) {
                    return err_code(code::FRAME, format!("frame {:?}: sandbox flag {flag:?} is not allowed", c.name));
                }
                if !sandbox.contains(&flag.as_str()) {
                    sandbox.push(flag);
                }
            }
            let height = c.height.as_deref().unwrap_or("70vh");
            Ok(Frame {
                name: c.name.clone(),
                title: c.title.clone(),
                sandbox: sandbox.join(" "),
                height: height.to_owned(),
                template: Template::build(&c.url, origins, base, model)?,
            })
        })
        .collect()
}

pub(crate) fn check_fieldsets(resource: &Resource) -> Result<(), Fault> {
    let mut names = BTreeSet::new();
    for set in &resource.fieldsets {
        check_name("fieldset name", &set.name)?;
        if !names.insert(&set.name) || set.fields.is_empty() || set.title.as_ref().is_some_and(|title| title.trim().is_empty()) {
            return err_code(code::FRAME, format!(
                "{}: fieldset {:?} needs a unique name and at least one field",
                resource.name, set.name
            ));
        }
        let mut fields = BTreeSet::new();
        for name in &set.fields {
            if !fields.insert(name) || !resource.detail.iter().any(|c| &c.label == name) {
                return err_code(code::FRAME, format!(
                    "{}: fieldset {:?} has an unknown or repeated field {name:?}",
                    resource.name, set.name
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn build_embeds(configs: &BTreeMap<String, EmbedConfig>, site: &Site) -> Result<BTreeMap<String, Embed>, Fault> {
    configs.iter().map(|(name, c)| {
        check_name("embed name", name)?;
        let invalid = |message: &str| Fault::new(code::FRAME, format!("embed {name:?}: {message}"));
        // The prepared schema has one model namespace. The optional app prefix is a config label.
        let model_name = match c.resource.split_once('.') {
            Some((app, model)) => {
                if app.is_empty() || model.contains('.') {
                    return Err(invalid("unknown resource"));
                }
                model
            }
            None => &c.resource,
        };
        let model = site.schema.model_idx(model_name).map_err(|_| invalid("unknown resource"))?;
        let resource = site.resources.iter().find(|r| r.model == model).ok_or_else(|| invalid("unknown resource"))?;
        let model = site.schema.model(model);
        let lookup = column(&site.schema, resource.model, &c.lookup)?;
        let f = model.field(&c.lookup).map_err(|_| invalid("lookup must be a unique field of the resource"))?;
        let unique = f.unique
            || (resource.key_columns.len() == 1 && resource.key_columns[0].field == f.name)
            || model.ir.constraints.iter().any(|constraint| matches!(constraint, ConstraintIr::Unique { fields, .. } if fields.len() == 1 && (fields[0] == f.name || fields[0] == f.column)));
        if !unique || !lookup.path.is_empty() || lookup.ty.array || lookup.ty.ty == ColType::Json {
            return Err(invalid("lookup must be a unique scalar field of the resource"));
        }
        let tab = if let Some(set) = resource.fieldsets.iter().find(|set| set.name == c.tab) {
            EmbedTab::Fields(set.clone())
        } else if let Some(tab) = resource.package_tabs.iter().find(|t| t.name == c.tab) {
            if !site.packages.embeddable(&tab.name) {
                return Err(invalid("package tab must declare embeddable: true"));
            }
            EmbedTab::Package { name: tab.name.clone(), title: tab.title.clone() }
        } else {
            return Err(invalid("unknown detail tab"));
        };
        let ancestors = origins(&c.ancestors)?;
        if ancestors.is_empty() {
            return Err(invalid("ancestors must list at least one origin"));
        }
        let csp = crate::ui::with_ancestors(&site.ui_csp, &ancestors.join(" "))?;
        Ok((name.clone(), Embed { resource: resource.name.clone(), lookup, tab, csp, self_only: ancestors.iter().all(|a| a == "'self'") }))
    }).collect()
}

/// A field can be restricted on the page as a dotted column, or on the registered target page.
pub(crate) fn visible(site: &Site, resource: &Resource, grants: &crate::permissions::Grants, column: &Column) -> bool {
    if resource.hidden.contains(&column.label) {
        return false;
    }
    let Some(hop) = column.path.first() else { return true };
    let Ok((_, target)) = site.schema.model(resource.model).relation(hop) else {
        return false;
    };
    // A target that no page registers cannot be checked: hide it.
    site.resources.iter().find(|r| r.model == target).is_some_and(|page| {
        site.allows(grants, page, crate::permissions::Op::Read)
            && page
                .see
                .get(&column.field)
                .is_none_or(|permission| grants.has(&format!("{}.{permission}", page.label)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(parts: Vec<Part>, path: &[Option<&str>]) -> Template {
        Template {
            parts,
            relative: true,
            origin: Url::parse("https://frame.invalid").unwrap().origin(),
            path: path.iter().map(|p| p.map(str::to_owned)).collect(),
        }
    }

    fn field() -> Column {
        Column {
            label: "domain".into(),
            field: "domain".into(),
            path: Vec::new(),
            ty: monorm_core::ir::ValueType::scalar(ColType::String),
            nullable: true,
            source: crate::site::Source::Field,
            view: None,
            sort: None,
        }
    }

    #[test]
    fn encodes_segments_and_query_values_with_their_own_rules() {
        let template = template(
            vec![
                Part::Fixed("/other/".into()),
                Part::Segment(field()),
                Part::Fixed("?domain=".into()),
                Part::Query(field()),
            ],
            &[Some("other"), None],
        );
        let value = json!("/?#% é&=+");
        assert_eq!(
            template.render("/admin", |_| Some(&value)).as_deref(),
            Some("/other/%2F%3F%23%25%20%C3%A9%26%3D%2B?domain=%2F%3F%23%25+%C3%A9%26%3D%2B"),
        );
        let empty = json!("");
        assert_eq!(template.render("/admin", |_| Some(&empty)).as_deref(), Some("/other/?domain="));
        for value in [json!("."), json!(".."), Value::Null] {
            assert_eq!(template.render("/admin", |_| Some(&value)), None);
        }
        assert_eq!(template.render("/admin", |_| None), None);
    }

    #[test]
    fn own_api_prefix_is_never_an_allowed_frame_path() {
        // `Site` always puts the API prefix under the base path.
        for path in ["/admin/api/resources/X", "/admin/api", "/admin/v1/export/X", "/admin/%61pi/site"] {
            assert!(!allowed_path(path, "/admin"), "{path}");
        }
        assert!(allowed_path("/admin/-/embed/health", "/admin"));
        assert!(allowed_path("/administrator/x", "/admin"));
    }

    #[test]
    fn rechecks_the_origin_fixed_path_and_own_admin() {
        let mut url = template(vec![Part::Fixed("/other/health".into())], &[Some("other"), Some("health")]);
        assert_eq!(url.render("/admin", |_| None).as_deref(), Some("/other/health"));
        url.origin = Url::parse("https://changed.invalid").unwrap().origin();
        assert_eq!(url.render("/admin", |_| None), None);
        url.origin = Url::parse("https://frame.invalid").unwrap().origin();
        url.path[1] = Some("changed".into());
        assert_eq!(url.render("/admin", |_| None), None);

        let dynamic = template(
            vec![Part::Fixed("/".into()), Part::Segment(field()), Part::Fixed("/resource".into())],
            &[None, Some("resource")],
        );
        let empty = json!("");
        assert_eq!(dynamic.render("/admin", |_| Some(&empty)), None); // an empty first segment would produce //resource
        let own = json!("admin");
        assert_eq!(dynamic.render("/admin", |_| Some(&own)), None);
        assert!(Template::build("//host", &["'self'".into()], "/admin", None).is_err());
    }
}

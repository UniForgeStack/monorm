//! Offline Rust generation from the prepared normalized schema.
use crate::{
    ir::{ColType, EnumStorage, FieldIr, SchemaIr},
    schema::Schema,
};
use std::{collections::BTreeSet, fmt::Write};

/// Validate the language/profile before producing a self-contained consumer module.
pub fn generate(ir: &SchemaIr, schema: &Schema) -> Result<String, String> {
    crate::behavior::validate_requirements(ir, crate::behavior::manifests(), Some("rust"))?;
    crate::behavior::validate_declarations(ir, crate::behavior::manifests(), Some("rust"))?;
    if ir.dialect != schema.dialect {
        return Err("prepared schema dialect mismatch".into());
    }
    // Do not accept an unrelated prepared schema and silently emit mismatched codecs.
    let prepared = Schema::from_ir(SchemaIr::from_json(&super::embedded_schema_json(ir)?)?)?;
    if prepared.models.len() != schema.models.len()
        || prepared.models.iter().zip(&schema.models).any(|(a, b)| {
            a.ir.name != b.ir.name
                || a.fields().len() != b.fields().len()
                || a.fields().iter().zip(b.fields()).any(|(a, b)| {
                    a.name != b.name
                        || a.column != b.column
                        || a.value_type() != b.value_type()
                        || a.hints != b.hints
                })
        })
    {
        return Err("prepared schema does not match generation input".into());
    }
    let schema = &prepared;
    let mut names: BTreeSet<String> = [
        "Schema",
        "Database",
        "Loaded",
        "Unloaded",
        "std",
        "orm_client",
        "i32",
        "i64",
        "f64",
        "bool",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for m in &schema.models {
        for n in [
            m.ir.name.clone(),
            format!("{}Fields", m.ir.name),
            format!("{}Source", m.ir.name),
            format!("{}Path", m.ir.name),
        ] {
            ident(&n)?;
            if !names.insert(n.clone()) {
                return Err(format!("Rust model/helper normalization collision: {n}"));
            }
        }
        let mut fields = BTreeSet::new();
        let mut tokens: BTreeSet<String> = ["METADATA"].into_iter().map(str::to_owned).collect();
        for f in m.fields() {
            let n = snake(&f.name);
            ident(&n)?;
            if n.starts_with("__orm_") {
                return Err(format!(
                    "{}.{}: reserved Rust helper name",
                    m.ir.name, f.name
                ));
            }
            if !fields.insert(n.clone()) {
                return Err(format!(
                    "{}.{}: Rust field normalization collision",
                    m.ir.name, f.name
                ));
            }
            if !tokens.insert(n.to_ascii_uppercase()) {
                return Err(format!(
                    "{}.{}: Rust token normalization collision",
                    m.ir.name, f.name
                ));
            }
            if !f.hints.is_empty() && !f.hints.contains_key("rust") {
                return Err(format!(
                    "{}.{}: extension type has no Rust language support",
                    m.ir.name, f.name
                ));
            }
            if f.hints.contains_key("rust") {
                return Err(format!(
                    "{}.{}: custom Rust extension codecs require a compiled contribution",
                    m.ir.name, f.name
                ));
            }
        }
        for r in &m.ir.relations {
            let n = snake(&r.name);
            ident(&n)?;
            if matches!(n.as_str(), "objects" | "query") {
                return Err(format!(
                    "{}.{n}: Rust helper normalization collision",
                    m.ir.name
                ));
            }
            if !fields.insert(n.clone()) || !tokens.insert(n.to_ascii_uppercase()) {
                return Err(format!(
                    "{}.{n}: Rust relation/token normalization collision",
                    m.ir.name
                ));
            }
        }
        for f in m.fields().iter().filter(|f| f.unique || f.primary_key) {
            let n = format!("{}_KEY", snake(&f.name).to_ascii_uppercase());
            if !tokens.insert(n.clone()) {
                return Err(format!("{}: Rust unique token collision {n}", m.ir.name));
            }
        }
        for columns in unique_columns(m) {
            let helper = key_type(&m.ir.name, &columns);
            ident(&helper)?;
            if !names.insert(helper.clone()) {
                return Err(format!(
                    "Rust unique key/helper normalization collision: {helper}"
                ));
            }
            let n = key_token(&columns);
            if !tokens.insert(n.clone()) {
                return Err(format!("{}: Rust unique token collision {n}", m.ir.name));
            }
        }
    }
    for e in &schema.enums {
        ident(&e.name)?;
        if !names.insert(e.name.clone()) {
            return Err(format!("Rust enum/helper collision: {}", e.name));
        }
        let mut variants = BTreeSet::new();
        for v in &e.values {
            ident(&variant(&v.name))?;
            if !variants.insert(variant(&v.name)) {
                return Err(format!("{}: Rust enum variant collision", e.name));
            }
        }
    }
    let mut out=String::from("// Generated by orm-core; do not edit.\nuse orm_client::Loaded;\npub enum Schema {}\nimpl orm_client::SchemaSpec for Schema {\n");
    writeln!(
        out,
        "type Dialect = orm_client::{};",
        if schema.dialect.is_postgres() {
            "Postgres"
        } else {
            "Sqlite"
        }
    )
    .unwrap();
    writeln!(
        out,
        "const JSON: &'static str = {:?};\n}}\npub type Database = orm_client::Database<Schema>;",
        super::embedded_schema_json(ir)?
    )
    .unwrap();
    for e in &schema.enums {
        writeln!(
            out,
            "#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]\npub enum {} {{ {} }}",
            ident(&e.name)?,
            e.values
                .iter()
                .map(|v| ident(&variant(&v.name)))
                .collect::<Result<Vec<_>, _>>()?
                .join(",")
        )
        .unwrap();
        writeln!(out,"impl orm_client::Codec for {} {{ fn decode(cell:orm_client::Cell<'_>)->orm_client::Result<Self> {{ match cell {{",ident(&e.name)?).unwrap();
        for v in &e.values {
            match e.storage {
                EnumStorage::Int => {
                    let value = v.value.as_i64().ok_or_else(|| {
                        format!("{}.{}: integer enum mapping must fit i64", e.name, v.name)
                    })?;
                    writeln!(
                        out,
                        "orm_client::Cell::Int(value) if i64::from(value)=={value} =>Ok(Self::{}),",
                        ident(&variant(&v.name))?
                    )
                    .unwrap();
                    writeln!(
                        out,
                        "orm_client::Cell::BigInt({value})=>Ok(Self::{}),",
                        ident(&variant(&v.name))?
                    )
                    .unwrap();
                }
                _ => writeln!(
                    out,
                    "orm_client::Cell::Text({:?})=>Ok(Self::{}),",
                    v.value.as_str().ok_or("enum label must be text")?,
                    ident(&variant(&v.name))?
                )
                .unwrap(),
            }
        }
        writeln!(out,"_=>Err(orm_client::Error::Decode({:?}.into())) }} }} fn encode(self)->orm_client::BindValue {{ match self {{",format!("unknown {} enum value",e.name)).unwrap();
        for v in &e.values {
            let value = match e.storage {
                EnumStorage::Int => format!("{}i64", v.value),
                _ => format!("{:?}.to_owned()", v.value.as_str().unwrap()),
            };
            writeln!(out, "Self::{}=>{value}.into(),", ident(&variant(&v.name))?).unwrap();
        }
        out.push_str("} } }\n");
    }
    for m in &schema.models {
        let name = ident(&m.ir.name)?;
        let fs = m.fields();
        let vars: Vec<_> = (0..fs.len()).map(|i| format!("S{i}")).collect();
        let decl = fs
            .iter()
            .enumerate()
            .map(|(i, f)| format!("S{i}:orm_client::State<{}>", ty(f)))
            .collect::<Vec<_>>()
            .join(",");
        let defaults = fs
            .iter()
            .enumerate()
            .map(|(i, f)| format!("S{i}:orm_client::State<{}>=Loaded", ty(f)))
            .collect::<Vec<_>>()
            .join(",");
        let args = vars.join(",");
        for columns in unique_columns(m) {
            let key = ident(&key_type(&m.ir.name, &columns))?;
            let ts = columns
                .iter()
                .map(|n| m.field(n).map(ty))
                .collect::<Result<Vec<_>, _>>()?;
            let tuple = format!("({},)", ts.join(","));
            writeln!(out,"pub enum {key} {{}}\nimpl orm_client::KeyValues for {key} {{ type Input={tuple}; fn values(value:Self::Input)->std::vec::Vec<orm_client::BindValue> {{ vec![{}] }} }}",(0..ts.len()).map(|i|format!("orm_client::Codec::encode(value.{i})")).collect::<Vec<_>>().join(",")).unwrap();
        }
        writeln!(
            out,
            "pub enum {name}Source {{}}\npub struct {name}Fields<{decl}> {{"
        )
        .unwrap();
        for (i, f) in fs.iter().enumerate() {
            writeln!(out, "pub {}:S{i}::Value,", ident(&snake(&f.name))?).unwrap();
        }
        writeln!(out,"}}\npub struct {name}<{defaults}> {{ __orm_values:{name}Fields<{args}>, __orm_identity:orm_client::Identity }}").unwrap();
        writeln!(out,"impl<{decl}> std::ops::Deref for {name}<{args}> {{ type Target={name}Fields<{args}>; fn deref(&self)->&Self::Target {{ &self.__orm_values }} }}").unwrap();
        let rhs = fs
            .iter()
            .enumerate()
            .map(|(i, f)| format!("R{i}:orm_client::State<{}>", ty(f)))
            .collect::<Vec<_>>()
            .join(",");
        let rargs = (0..fs.len())
            .map(|i| format!("R{i}"))
            .collect::<Vec<_>>()
            .join(",");
        writeln!(out,"impl<{decl},{rhs}> std::cmp::PartialEq<{name}<{rargs}>> for {name}<{args}> {{ fn eq(&self,other:&{name}<{rargs}>)->bool {{ self.__orm_identity==other.__orm_identity }} }}\nimpl<{decl}> std::cmp::Eq for {name}<{args}> {{}}\nimpl<{decl}> std::hash::Hash for {name}<{args}> {{ fn hash<H:std::hash::Hasher>(&self,h:&mut H) {{ std::hash::Hash::hash(&std::any::TypeId::of::<{name}Source>(),h); std::hash::Hash::hash(&self.__orm_identity,h); }} }}").unwrap();
        writeln!(out,"impl<{decl}> {name}<{args}> where {} {{ pub fn query()->orm_client::Query<Self> {{ orm_client::Query::new() }} }}",vars.iter().map(|v|format!("{v}:'static,{v}::Value:Send")).collect::<Vec<_>>().join(",")).unwrap();
        writeln!(out,"impl<{decl}> orm_client::Model for {name}<{args}> where {} {{ type Schema=Schema; type Source={name}Source; const NAME:&'static str={:?}; fn fields()->std::vec::Vec<std::string::String> {{ let mut fields=std::vec::Vec::new();",vars.iter().map(|v|format!("{v}:'static,{v}::Value:Send")).collect::<Vec<_>>().join(","),m.ir.name).unwrap();
        for (i, f) in fs.iter().enumerate() {
            writeln!(
                out,
                "if S{i}::LOADED {{ fields.push({:?}.into()); }}",
                f.name
            )
            .unwrap();
        }
        out.push_str("fields } fn decode(row:&orm_client::DecodedRow<'_>)->orm_client::Result<Self> { Ok(Self { __orm_values:");
        writeln!(out, "{name}Fields {{").unwrap();
        for (i, f) in fs.iter().enumerate() {
            writeln!(
                out,
                "{}:S{i}::read(row,{:?})?,",
                ident(&snake(&f.name))?,
                f.name
            )
            .unwrap();
        }
        writeln!(out, "}},__orm_identity:row.identity()? }}) }} }}").unwrap();
        writeln!(out,"impl {name} {{ pub fn objects()->orm_client::Query<Self> {{ orm_client::Query::new() }}").unwrap();
        for f in fs {
            writeln!(
                out,
                "pub const {}:orm_client::Column<{name}Source,{}>=orm_client::Column::field({:?});",
                snake(&f.name).to_ascii_uppercase(),
                ty(f),
                f.name
            )
            .unwrap();
            if f.primary_key || f.unique {
                writeln!(out,"pub const {}_KEY:orm_client::Unique<{name}Source,{}>=orm_client::Unique::generated(&[{:?}]);",snake(&f.name).to_ascii_uppercase(),ty(f),f.name).unwrap();
            }
        }
        for columns in unique_columns(m) {
            let key = ident(&key_type(&m.ir.name, &columns))?;
            writeln!(out,"pub const {}:orm_client::Unique<{name}Source,{key}>=orm_client::Unique::generated(&{:?});",key_token(&columns),columns).unwrap();
        }
        out.push_str(
            "pub const METADATA:orm_client::ModelMetadata=orm_client::ModelMetadata { name:",
        );
        writeln!(out, "{:?},table:{:?},fields:&[", m.ir.name, m.ir.table).unwrap();
        for f in fs {
            writeln!(
                out,
                "orm_client::FieldMetadata {{ name:{:?},column:{:?},nullable:{},primary_key:{} }},",
                f.name, f.column, f.nullable, f.primary_key
            )
            .unwrap();
        }
        out.push_str("] };\n");
        for r in &m.ir.relations {
            writeln!(out,"pub fn {}()->{}Path<{name}Source,orm_client::{}> {{ {}Path {{ path:vec![{:?}.into()],_type:std::marker::PhantomData }} }}",ident(&snake(&r.name))?,ident(&r.target)?,if r.kind==crate::ir::RelKind::One {"One"}else{"Many"},ident(&r.target)?,r.name).unwrap();
        }
        out.push_str("}\n");
        writeln!(out,"pub struct {name}Path<R,C> {{ path:std::vec::Vec<std::string::String>,_type:std::marker::PhantomData<(R,C)> }}\nimpl<R,C> {name}Path<R,C> {{").unwrap();
        for f in fs {
            writeln!(out,"pub fn {}(&self)->orm_client::Column<R,{},C> {{ orm_client::Column::path({:?},self.path.clone()) }}",ident(&snake(&f.name))?,ty(f),f.name).unwrap();
        }
        for r in &m.ir.relations {
            let c = if r.kind == crate::ir::RelKind::One {
                "One"
            } else {
                "Many"
            };
            writeln!(out,"pub fn {}(&self)->{}Path<R,<C as orm_client::Combine<orm_client::{c}>>::Output> where C:orm_client::Combine<orm_client::{c}> {{ let mut path=self.path.clone(); path.push({:?}.into()); {}Path {{path,_type:std::marker::PhantomData}} }}",ident(&snake(&r.name))?,ident(&r.target)?,r.name,ident(&r.target)?).unwrap();
        }
        out.push_str("}\n");
    }
    Ok(out)
}
fn unique_columns(m: &crate::schema::Model) -> Vec<Vec<&str>> {
    let mut keys: Vec<_> =
        m.ir.constraints
            .iter()
            .filter_map(|c| match c {
                crate::ir::ConstraintIr::Unique { fields, .. } => {
                    Some(fields.iter().map(String::as_str).collect())
                }
                _ => None,
            })
            .collect();
    keys.extend(
        m.ir.indexes
            .iter()
            .filter(|i| {
                i.unique && i.where_.is_none() && i.columns.iter().all(|c| c.field.is_some())
            })
            .map(|i| {
                i.columns
                    .iter()
                    .map(|c| c.field.as_deref().unwrap())
                    .collect()
            }),
    );
    keys
}
fn key_token(columns: &[&str]) -> String {
    format!(
        "{}_KEY",
        columns
            .iter()
            .map(|s| snake(s))
            .collect::<Vec<_>>()
            .join("_")
            .to_ascii_uppercase()
    )
}
fn ty(f: &FieldIr) -> String {
    let base = f
        .enum_name
        .as_ref()
        .map(|n| format!("self::{}", ident(n).expect("validated enum name")))
        .unwrap_or_else(|| {
            match f.ty {
                ColType::Int => "i32",
                ColType::BigInt => "i64",
                ColType::Float => "f64",
                ColType::Bool => "bool",
                ColType::String | ColType::Text => "std::string::String",
                ColType::DateTime => "orm_client::chrono::DateTime<orm_client::chrono::Utc>",
                ColType::Date => "orm_client::chrono::NaiveDate",
                ColType::Uuid => "orm_client::uuid::Uuid",
                ColType::Json => "orm_client::serde_json::Value",
                ColType::Decimal => "orm_client::bigdecimal::BigDecimal",
            }
            .into()
        });
    let base = if f.array {
        format!("std::vec::Vec<{base}>")
    } else {
        base
    };
    if f.nullable {
        format!("std::option::Option<{base}>")
    } else {
        base
    }
}
fn ident(s: &str) -> Result<String, String> {
    if !super::is_ident(s) || matches!(s, "self" | "Self" | "super" | "crate" | "_") {
        return Err(format!("{s:?} is not a supported Rust identifier"));
    }
    Ok(
        if [
            "as", "async", "await", "break", "const", "continue", "dyn", "else", "enum", "extern",
            "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
            "pub", "ref", "return", "static", "struct", "trait", "true", "type", "unsafe", "use",
            "where", "while", "abstract", "become", "box", "do", "final", "macro", "override",
            "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
        ]
        .contains(&s)
        {
            format!("r#{s}")
        } else {
            s.into()
        },
    )
}
fn snake(s: &str) -> String {
    let cs: Vec<_> = s.chars().collect();
    let mut out = String::new();
    for (i, &c) in cs.iter().enumerate() {
        if c.is_ascii_uppercase()
            && i > 0
            && (cs[i - 1].is_ascii_lowercase()
                || cs[i - 1].is_ascii_digit()
                || cs.get(i + 1).is_some_and(|n| n.is_ascii_lowercase())
                    && cs[i - 1].is_ascii_uppercase())
        {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn variant(s: &str) -> String {
    snake(s)
        .split('_')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut chars = s.chars();
            chars.next().unwrap().to_ascii_uppercase().to_string() + chars.as_str()
        })
        .collect()
}

fn key_type(model: &str, columns: &[&str]) -> String {
    format!(
        "{}{}Key",
        model,
        columns.iter().map(|c| variant(c)).collect::<String>()
    )
}

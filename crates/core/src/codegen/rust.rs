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
    let prepared = Schema::from_ir_unbound(SchemaIr::from_json(&super::embedded_schema_json(ir)?)?)?;
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
        "monorm_client",
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
            format!("{}Shape", m.ir.name),
        ] {
            ident(&n)?;
            if !names.insert(n.clone()) {
                return Err(format!("Rust model/helper normalization collision: {n}"));
            }
        }
        for helper in (0..m.fields().len())
            .map(|i| format!("{}Select{i}", m.ir.name))
            .chain((0..m.ir.relations.len()).map(|i| format!("{}Relation{i}", m.ir.name)))
        {
            if !names.insert(helper.clone()) {
                return Err(format!("Rust helper normalization collision: {helper}"));
            }
        }
        let mut fields = BTreeSet::new();
        let mut associated = BTreeSet::new();
        let mut tokens: BTreeSet<String> = ["METADATA"].into_iter().map(str::to_owned).collect();
        for f in m.fields() {
            let n = snake(&f.name);
            ident(&n)?;
            if matches!(n.as_str(), "objects" | "query")
                || m.ir
                    .relations
                    .iter()
                    .any(|r| format!("{}_path", snake(&r.name)) == n)
            {
                return Err(format!(
                    "{}.{}: Rust helper normalization collision",
                    m.ir.name, f.name
                ));
            }
            if !associated.insert(variant(&f.name)) {
                return Err(format!(
                    "{}.{}: Rust shape associated type collision (normalization collision)",
                    m.ir.name, f.name
                ));
            }
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
            if !associated.insert(variant(&r.name)) {
                return Err(format!(
                    "{}.{}: Rust shape associated type collision (normalization collision)",
                    m.ir.name, r.name
                ));
            }
            if n.starts_with("__orm_")
                || matches!(n.as_str(), "objects" | "query")
                || m.ir.relations.iter().any(|other| {
                    other.name != r.name && n == format!("{}_path", snake(&other.name))
                })
            {
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
        for f in m
            .fields()
            .iter()
            .filter(|f| f.unique || (f.primary_key && m.key.len() == 1))
        {
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
    let mut out=String::from("// Generated by monorm-core; do not edit.\nuse monorm_client::Loaded;\npub enum Schema {}\nimpl monorm_client::SchemaSpec for Schema {\n");
    writeln!(
        out,
        "type Dialect = monorm_client::{};",
        if schema.dialect.is_postgres() {
            "Postgres"
        } else {
            "Sqlite"
        }
    )
    .unwrap();
    writeln!(
        out,
        "const JSON: &'static str = {:?};\n}}\npub type Database = monorm_client::Database<Schema>;",
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
        writeln!(
            out,
            "impl monorm_client::NullableCodec for {} {{type Nullable=std::option::Option<Self>;}}",
            ident(&e.name)?
        )
        .unwrap();
        writeln!(out,"impl monorm_client::Codec for {} {{ fn decode(cell:monorm_client::Cell<'_>)->monorm_client::Result<Self> {{ match cell {{",ident(&e.name)?).unwrap();
        for v in &e.values {
            match e.storage {
                EnumStorage::Int => {
                    let value = v.value.as_i64().ok_or_else(|| {
                        format!("{}.{}: integer enum mapping must fit i64", e.name, v.name)
                    })?;
                    writeln!(
                        out,
                        "monorm_client::Cell::Int(value) if i64::from(value)=={value} =>Ok(Self::{}),",
                        ident(&variant(&v.name))?
                    )
                    .unwrap();
                    writeln!(
                        out,
                        "monorm_client::Cell::BigInt({value})=>Ok(Self::{}),",
                        ident(&variant(&v.name))?
                    )
                    .unwrap();
                }
                _ => writeln!(
                    out,
                    "monorm_client::Cell::Text({:?})=>Ok(Self::{}),",
                    v.value.as_str().ok_or("enum label must be text")?,
                    ident(&variant(&v.name))?
                )
                .unwrap(),
            }
        }
        writeln!(out,"_=>Err(monorm_client::Error::Decode({:?}.into())) }} }} fn encode(self)->monorm_client::BindValue {{ match self {{",format!("unknown {} enum value",e.name)).unwrap();
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
        let vars: Vec<_> = (0..fs.len() + m.ir.relations.len())
            .map(|i| format!("S{i}"))
            .collect();
        let scalar_decl = fs
            .iter()
            .enumerate()
            .map(|(i, f)| format!("S{i}:monorm_client::State<{}>", ty(f)))
            .collect::<Vec<_>>()
            .join(",");
        let relation_decls =
            m.ir.relations
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    format!(
                        "S{}:monorm_client::RelationState<self::{},monorm_client::{}>",
                        fs.len() + i,
                        ident(&r.target).unwrap(),
                        if r.kind == crate::ir::RelKind::One {
                            "One"
                        } else {
                            "Many"
                        }
                    )
                })
                .collect::<Vec<_>>();
        let decl = std::iter::once(scalar_decl)
            .chain(relation_decls.iter().cloned())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        let scalar_defaults = fs
            .iter()
            .enumerate()
            .map(|(i, f)| {
                format!(
                    "S{i}:monorm_client::State<{}>={}",
                    ty(f),
                    if default_fields(schema, &m.ir.name)
                        .is_none_or(|fields| fields.iter().any(|n| n == &f.name))
                    {
                        "Loaded"
                    } else {
                        "monorm_client::Unloaded"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let defaults = std::iter::once(scalar_defaults)
            .chain(relation_decls.iter().enumerate().map(|(i, d)| {
                format!(
                    "{d}={}",
                    normal_relation(schema, &m.ir.name, i, &[])
                        .expect("validated finite defaults")
                )
            }))
            .filter(|s| !s.is_empty())
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
            writeln!(out,"pub enum {key} {{}}\nimpl monorm_client::KeyValues for {key} {{ type Input={tuple}; fn values(value:Self::Input)->std::vec::Vec<monorm_client::BindValue> {{ vec![{}] }} }}",(0..ts.len()).map(|i|format!("monorm_client::Codec::encode(value.{i})")).collect::<Vec<_>>().join(",")).unwrap();
        }
        writeln!(
            out,
            "pub enum {name}Source {{}}\npub struct {name}Fields<{decl}> {{"
        )
        .unwrap();
        for (i, f) in fs.iter().enumerate() {
            writeln!(out, "pub {}:S{i}::Value,", ident(&snake(&f.name))?).unwrap();
        }
        for (i, r) in m.ir.relations.iter().enumerate() {
            writeln!(
                out,
                "pub {}:S{}::Value,",
                ident(&snake(&r.name))?,
                fs.len() + i
            )
            .unwrap();
        }
        writeln!(out,"}}\npub struct {name}<{defaults}> {{ __orm_values:{name}Fields<{args}>, __orm_identity:monorm_client::Identity, __orm_data:monorm_client::RowData }}").unwrap();
        writeln!(out,"impl<{decl}> std::ops::Deref for {name}<{args}> {{ type Target={name}Fields<{args}>; fn deref(&self)->&Self::Target {{ &self.__orm_values }} }}").unwrap();
        let scalar_rhs = fs
            .iter()
            .enumerate()
            .map(|(i, f)| format!("R{i}:monorm_client::State<{}>", ty(f)))
            .collect::<Vec<_>>()
            .join(",");
        let rhs = std::iter::once(scalar_rhs)
            .chain(relation_decls.iter().enumerate().map(|(i, d)| {
                d.replacen(
                    &format!("S{}:", fs.len() + i),
                    &format!("R{}:", fs.len() + i),
                    1,
                )
            }))
            .collect::<Vec<_>>()
            .join(",");
        let rargs = (0..vars.len())
            .map(|i| format!("R{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut union_args = vec![];
        let mut union_bounds = vec![];
        for (i, f) in fs.iter().enumerate() {
            union_args.push(format!(
                "<S{i} as monorm_client::UnionField<{},R{i}>>::Output",
                ty(f)
            ));
            union_bounds.push(format!("S{i}:monorm_client::UnionField<{},R{i}>", ty(f)));
        }
        for (i, r) in m.ir.relations.iter().enumerate() {
            let index = fs.len() + i;
            let target = format!("self::{}", ident(&r.target)?);
            let card = if r.kind == crate::ir::RelKind::One {
                "One"
            } else {
                "Many"
            };
            union_args.push(format!("<S{index} as monorm_client::UnionRelation<{target},monorm_client::{card},R{index}>>::Output"));
            union_bounds.push(format!("S{index}:monorm_client::UnionRelation<{target},monorm_client::{card},R{index}>,R{index}:'static"));
        }
        writeln!(out,"impl<{decl},{rhs}> monorm_client::Unify<{name}<{rargs}>> for {name}<{args}> where {},{name}<{args}>:monorm_client::Model<Schema=Schema,Source={name}Source>,{name}<{rargs}>:monorm_client::Model<Schema=Schema,Source={name}Source> {{type Output={name}<{}>;}}",union_bounds.join(","),union_args.join(",")).unwrap();
        writeln!(out,"impl<{decl},{rhs}> std::cmp::PartialEq<{name}<{rargs}>> for {name}<{args}> {{ fn eq(&self,other:&{name}<{rargs}>)->bool {{ self.__orm_identity==other.__orm_identity }} }}\nimpl<{decl}> std::cmp::Eq for {name}<{args}> {{}}\nimpl<{decl}> std::hash::Hash for {name}<{args}> {{ fn hash<H:std::hash::Hasher>(&self,h:&mut H) {{ std::hash::Hash::hash(&std::any::TypeId::of::<{name}Source>(),h); std::hash::Hash::hash(&self.__orm_identity,h); }} }}").unwrap();
        writeln!(out,"impl<{decl}> {name}<{args}> where {} {{ pub fn query()->monorm_client::Query<Self,monorm_client::Optional,monorm_client::Unsliced,monorm_client::ExplicitSelection> {{ monorm_client::Query::<Self>::new().explicit_shape() }} }}",vars.iter().map(|v|format!("{v}:'static,{v}::Value:Send")).collect::<Vec<_>>().join(",")).unwrap();
        writeln!(out,"impl<{decl}> monorm_client::Model for {name}<{args}> where {} {{ type Schema=Schema; type Source={name}Source; type Normal={name}; const NAME:&'static str={:?}; fn fields()->std::vec::Vec<std::string::String> {{ let mut fields=std::vec::Vec::new();",vars.iter().map(|v|format!("{v}:'static,{v}::Value:Send")).collect::<Vec<_>>().join(","),m.ir.name).unwrap();
        for (i, f) in fs.iter().enumerate() {
            writeln!(
                out,
                "if S{i}::LOADED {{ fields.push({:?}.into()); }}",
                f.name
            )
            .unwrap();
        }
        let configure =
            m.ir.relations
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    format!(
                        "q.model_helpers.extend({:?}.iter().map(|name| (*name).into())); S{}::configure(q,{:?});",
                        r.from,
                        fs.len() + i,
                        r.name
                    )
                })
                .collect::<Vec<_>>()
                .join("");
        let mut selection = String::from("q.model_fields=Some(Self::fields());");
        for (i, r) in m.ir.relations.iter().enumerate() {
            let index = fs.len() + i;
            writeln!(selection,"if S{index}::LOADED {{S{index}::configure(q,{:?});S{index}::merge_shape(q,{:?});}} else {{q.prefetch.retain(|p|p.attr.is_some()||p.relation!={:?});}}",r.name,r.name,r.name).unwrap();
        }
        out.push_str(&"fields } fn data(&self)->&monorm_client::RowData { &self.__orm_data } fn configure(q:&mut monorm_client::SelectionPlan) { q.without_related=true; CONFIGURE_PLACEHOLDER } fn selection(q:&mut monorm_client::SelectionPlan) { SELECTION_PLACEHOLDER } fn decode(row:&monorm_client::DecodedRow<'_>)->monorm_client::Result<Self> { row.check_model(Self::NAME)?; Ok(Self { __orm_values:".replace("CONFIGURE_PLACEHOLDER",&configure).replace("SELECTION_PLACEHOLDER",&selection));
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
        for (i, r) in m.ir.relations.iter().enumerate() {
            writeln!(
                out,
                "{}:S{}::read(row,{:?},&{:?},&{:?})?,",
                ident(&snake(&r.name))?,
                fs.len() + i,
                r.name,
                r.from,
                r.to
            )
            .unwrap();
        }
        writeln!(
            out,
            "}},__orm_identity:row.identity()?,__orm_data:row.data() }}) }} }}"
        )
        .unwrap();
        writeln!(out, "pub trait {name}Shape {{").unwrap();
        for f in fs {
            writeln!(
                out,
                "type {};fn {}(&self)->&Self::{};",
                variant(&f.name),
                ident(&snake(&f.name))?,
                variant(&f.name)
            )
            .unwrap();
        }
        for r in &m.ir.relations {
            writeln!(
                out,
                "type {};fn {}(&self)->&Self::{};",
                variant(&r.name),
                ident(&snake(&r.name))?,
                variant(&r.name)
            )
            .unwrap();
        }
        writeln!(out, "}} impl<{decl}> {name}Shape for {name}<{args}> {{").unwrap();
        for (i, f) in fs.iter().enumerate() {
            writeln!(
                out,
                "type {}=S{i}::Value;fn {}(&self)->&Self::{} {{&self.__orm_values.{}}}",
                variant(&f.name),
                ident(&snake(&f.name))?,
                variant(&f.name),
                ident(&snake(&f.name))?
            )
            .unwrap();
        }
        for (i, r) in m.ir.relations.iter().enumerate() {
            writeln!(
                out,
                "type {}=S{}::Value;fn {}(&self)->&Self::{} {{&self.__orm_values.{}}}",
                variant(&r.name),
                fs.len() + i,
                ident(&snake(&r.name))?,
                variant(&r.name),
                ident(&snake(&r.name))?
            )
            .unwrap();
        }
        out.push_str("}\n");
        writeln!(out,"impl<{decl}> std::fmt::Debug for {name}<{args}> {{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {{f.debug_struct({:?}).field(\"loaded\",&self.__orm_data.public_json()).finish()}}}}",m.ir.name).unwrap();
        writeln!(out,"impl<{decl}> monorm_client::serde::Serialize for {name}<{args}> {{fn serialize<Serializer:monorm_client::serde::Serializer>(&self,s:Serializer)->std::result::Result<Serializer::Ok,Serializer::Error> {{monorm_client::serde::Serialize::serialize(&self.__orm_data,s)}}}}").unwrap();
        let send = vars
            .iter()
            .map(|v| format!("{v}:'static,{v}::Value:Send"))
            .collect::<Vec<_>>()
            .join(",");
        let empty = vec!["monorm_client::Unloaded".to_owned(); fs.len()]
            .into_iter()
            .chain(vars[fs.len()..].iter().cloned())
            .collect::<Vec<_>>()
            .join(",");
        let full = vec!["Loaded".to_owned(); fs.len()]
            .into_iter()
            .chain(vars[fs.len()..].iter().cloned())
            .collect::<Vec<_>>()
            .join(",");
        writeln!(out,"impl<{decl}> monorm_client::ResetScalars for {name}<{args}> where {send} {{ type Empty={name}<{empty}>; type Full={name}<{full}>; }}").unwrap();
        let reset_args = vars[..fs.len()]
            .iter()
            .cloned()
            .chain(m.ir.relations.iter().enumerate().map(|(i, r)| {
                format!(
                    "<S{} as monorm_client::StripDefault<self::{},monorm_client::{}>>::Output",
                    fs.len() + i,
                    ident(&r.target).unwrap(),
                    if r.kind == crate::ir::RelKind::One {
                        "One"
                    } else {
                        "Many"
                    }
                )
            }))
            .collect::<Vec<_>>()
            .join(",");
        let reset_bounds =
            m.ir.relations
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    format!(
                        "S{}:monorm_client::StripDefault<self::{},monorm_client::{}>",
                        fs.len() + i,
                        ident(&r.target).unwrap(),
                        if r.kind == crate::ir::RelKind::One {
                            "One"
                        } else {
                            "Many"
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
        writeln!(out,"impl<{decl}> monorm_client::ResetRelated for {name}<{args}> where {send}{} {{type WithoutRelated={name}<{reset_args}>;fn default_relations()->std::vec::Vec<&'static str> {{#[allow(unused_mut)] let mut result=vec![];",if reset_bounds.is_empty(){String::new()}else{format!(",{reset_bounds}")}).unwrap();
        for (i, r) in m.ir.relations.iter().enumerate() {
            writeln!(
                out,
                "if S{}::DEFAULT {{result.push({:?});}}",
                fs.len() + i,
                r.name
            )
            .unwrap();
        }
        out.push_str("result}}\n");
        let bypass_args = vars[..fs.len()]
            .iter()
            .cloned()
            .chain(m.ir.relations.iter().enumerate().map(|(i, r)| {
                format!(
                    "<S{} as monorm_client::BypassRelation<self::{},monorm_client::{}>>::Output",
                    fs.len() + i,
                    ident(&r.target).unwrap(),
                    cardinality(r.kind)
                )
            }))
            .collect::<Vec<_>>()
            .join(",");
        let bypass_bounds =
            m.ir.relations
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    format!(
                        "S{}:monorm_client::BypassRelation<self::{},monorm_client::{}>",
                        fs.len() + i,
                        ident(&r.target).unwrap(),
                        cardinality(r.kind)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
        writeln!(out,"impl<{decl}> monorm_client::BypassShape for {name}<{args}> where {send}{} {{type Output={name}<{bypass_args}>;}}",if bypass_bounds.is_empty(){String::new()}else{format!(",{bypass_bounds}")}).unwrap();
        for (i, _) in fs.iter().enumerate() {
            let mut selected = vars.clone();
            selected[i] = "Loaded".into();
            let selected = selected.join(",");
            writeln!(out,"#[derive(Clone,Copy)] pub struct {name}Select{i}; impl monorm_client::Descriptor for {name}Select{i} {{type Kind=monorm_client::Scalars;}} impl<{decl}> monorm_client::Apply<{name}<{args}>> for {name}Select{i} where {send} {{type Output={name}<{selected}>;fn needs_data(&self,row:&monorm_client::RowData)->bool {{!row.loaded({:?})}} fn apply(self,q:&mut monorm_client::SelectionPlan,_:&mut std::vec::Vec<monorm_client::Tree>){{ q.model_fields.get_or_insert_with(std::vec::Vec::new).push({:?}.into()); }} }}",fs[i].name,fs[i].name).unwrap();
        }
        for (i, f) in fs.iter().enumerate() {
            let ty = ty(f);
            writeln!(
                out,
                "monorm_client::impl_projection_column!({name}Select{i},{name}Source,{ty},{:?});",
                f.name
            )
            .unwrap();
            writeln!(out,"impl From<{name}Select{i}> for monorm_client::Column<{name}Source,{ty}> {{fn from(_:{name}Select{i})->Self {{Self::field({:?})}}}} impl {name}Select{i} {{",f.name).unwrap();
            for method in ["eq", "ne", "lt", "le", "gt", "ge"] {
                writeln!(out,"pub fn {method}(self,value:{ty})->monorm_client::Predicate<{name}Source> {{monorm_client::Column::<{name}Source,{ty}>::from(self).{method}(value)}}").unwrap();
            }
            for method in ["asc", "desc"] {
                writeln!(out,"pub fn {method}(self)->monorm_client::Ordering<{name}Source> {{monorm_client::Column::<{name}Source,{ty}>::from(self).{method}()}}").unwrap();
            }
            writeln!(out,"pub fn in_values(self,values:impl IntoIterator<Item={ty}>)->monorm_client::Predicate<{name}Source> {{monorm_client::Column::<{name}Source,{ty}>::from(self).in_values(values)}} pub fn eq_field(self,other:impl Into<monorm_client::Column<{name}Source,{ty}>>)->monorm_client::Predicate<{name}Source> {{monorm_client::Column::<{name}Source,{ty}>::from(self).eq_field(other.into())}}").unwrap();
            if f.nullable {
                writeln!(out,"pub fn is_null(self)->monorm_client::Predicate<{name}Source> {{monorm_client::Column::<{name}Source,{ty}>::from(self).is_null()}}").unwrap();
            }
            out.push_str("}\n");
        }
        for (i, r) in m.ir.relations.iter().enumerate() {
            writeln!(out, "pub enum {name}Relation{i} {{}}").unwrap();
            writeln!(out,"impl monorm_client::PathDescriptor for {name}Relation{i} {{type Path={}Path<{name}Source,monorm_client::{}>;fn path()->Self::Path {{{}Path {{path:vec![{:?}.into()],_type:std::marker::PhantomData}}}}}}",ident(&r.target)?,path_cardinality(schema,m,r),ident(&r.target)?,r.name).unwrap();
            let target = format!("self::{}", ident(&r.target)?);
            let card = if r.kind == crate::ir::RelKind::One {
                "One"
            } else {
                "Many"
            };
            let index = fs.len() + i;
            let source = format!("{}Source", ident(&r.target)?);
            let mut selected = vars.clone();
            selected[index] = format!(
                "<S{index} as monorm_client::MergeRequested<{target},monorm_client::{card},P,B>>::Output"
            );
            let selected = selected.join(",");
            writeln!(out,"impl<{decl},P:monorm_client::Model<Schema=Schema,Source=self::{source}>,B> monorm_client::Apply<{name}<{args}>> for monorm_client::Relation<{name}Source,P,monorm_client::{card},{name}Relation{i},B> where {send},S{index}:monorm_client::MergeRequested<{target},monorm_client::{card},P,B> {{type Output={name}<{selected}>;fn needs_data(&self,row:&monorm_client::RowData)->bool {{!row.cache_satisfies::<P>({:?},{})}} fn apply(self,q:&mut monorm_client::SelectionPlan,f:&mut std::vec::Vec<monorm_client::Tree>){{self.install(q,f,!S{index}::DEFAULT); <<S{index} as monorm_client::MergeRequested<{target},monorm_client::{card},P,B>>::Output as monorm_client::RelationState<{target},monorm_client::{card}>>::merge_shape(q,{:?});}} }}",r.name,r.kind!=crate::ir::RelKind::One,r.name).unwrap();
        }
        writeln!(out,"impl {name} {{ #[allow(non_upper_case_globals)] pub const objects:monorm_client::Query<Self>=monorm_client::Query::new();").unwrap();
        for (i, f) in fs.iter().enumerate() {
            writeln!(
                out,
                "#[allow(non_upper_case_globals)] pub const {}:{name}Select{i}={name}Select{i};",
                ident(&snake(&f.name))?
            )
            .unwrap();
        }
        for f in fs {
            writeln!(
                out,
                "pub const {}:monorm_client::Column<{name}Source,{}>=monorm_client::Column::field({:?});",
                snake(&f.name).to_ascii_uppercase(),
                ty(f),
                f.name
            )
            .unwrap();
            if f.unique || (f.primary_key && m.key.len() == 1) {
                writeln!(out,"pub const {}_KEY:monorm_client::Unique<{name}Source,{}>=monorm_client::Unique::generated(&[{:?}]);",snake(&f.name).to_ascii_uppercase(),ty(f),f.name).unwrap();
            }
        }
        for columns in unique_columns(m) {
            let key = ident(&key_type(&m.ir.name, &columns))?;
            writeln!(out,"pub const {}:monorm_client::Unique<{name}Source,{key}>=monorm_client::Unique::generated(&{:?});",key_token(&columns),columns).unwrap();
        }
        out.push_str(
            "pub const METADATA:monorm_client::ModelMetadata=monorm_client::ModelMetadata { name:",
        );
        writeln!(out, "{:?},table:{:?},fields:&[", m.ir.name, m.ir.table).unwrap();
        for f in fs {
            writeln!(
                out,
                "monorm_client::FieldMetadata {{ name:{:?},column:{:?},nullable:{},primary_key:{} }},",
                f.name, f.column, f.nullable, f.primary_key
            )
            .unwrap();
        }
        out.push_str("] };\n");
        for (i, r) in m.ir.relations.iter().enumerate() {
            writeln!(out,"#[allow(non_upper_case_globals)] pub const {}:monorm_client::Relation<{name}Source,{},monorm_client::{},{name}Relation{i}>=monorm_client::Relation::generated({:?});",ident(&snake(&r.name))?,ident(&r.target)?,if r.kind==crate::ir::RelKind::One {"One"}else{"Many"},r.name).unwrap();
            writeln!(out,"pub fn {}_path()->{}Path<{name}Source,monorm_client::{}> {{ {}Path {{ path:vec![{:?}.into()],_type:std::marker::PhantomData }} }}",snake(&r.name),ident(&r.target)?,path_cardinality(schema,m,r),ident(&r.target)?,r.name).unwrap();
        }
        out.push_str("}\n");
        writeln!(out,"pub struct {name}Path<R,C> {{ path:std::vec::Vec<std::string::String>,_type:std::marker::PhantomData<(R,C)> }}\nimpl<R,C> {name}Path<R,C> {{").unwrap();
        for f in fs {
            writeln!(out,"pub fn {}(&self)->monorm_client::Column<R,{},C> {{ monorm_client::Column::path({:?},self.path.clone()) }}",ident(&snake(&f.name))?,ty(f),f.name).unwrap();
        }
        for r in &m.ir.relations {
            let c = path_cardinality(schema, m, r);
            writeln!(out,"pub fn {}(&self)->{}Path<R,<C as monorm_client::Combine<monorm_client::{c}>>::Output> where C:monorm_client::Combine<monorm_client::{c}> {{ let mut path=self.path.clone(); path.push({:?}.into()); {}Path {{path,_type:std::marker::PhantomData}} }}",ident(&snake(&r.name))?,ident(&r.target)?,r.name,ident(&r.target)?).unwrap();
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
    if m.key.len() > 1 {
        keys.push(
            m.key
                .iter()
                .map(|&pos| m.fields()[pos].name.as_str())
                .collect(),
        );
    }
    keys.sort();
    keys.dedup();
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
                ColType::DateTime => "monorm_client::chrono::DateTime<monorm_client::chrono::Utc>",
                ColType::Date => "monorm_client::chrono::NaiveDate",
                ColType::Uuid => "monorm_client::uuid::Uuid",
                ColType::Json => "monorm_client::serde_json::Value",
                ColType::Decimal => "monorm_client::bigdecimal::BigDecimal",
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

fn default_fields<'a>(schema: &'a Schema, name: &str) -> Option<&'a Vec<String>> {
    schema.models.iter().find(|m| m.ir.name == name).and_then(|m| m.read_flags.fields.as_ref())
}
fn normal_relation(
    schema: &Schema,
    name: &str,
    index: usize,
    extra: &[Vec<String>],
) -> Result<String, String> {
    let m = schema
        .models
        .iter()
        .find(|m| m.ir.name == name)
        .ok_or("unknown default model")?;
    let r = &m.ir.relations[index];
    let mut paths = extra.to_vec();
    paths.extend(m.read_flags.related.clone());
    let children: Vec<_> = paths
        .iter()
        .filter(|p| p.first() == Some(&r.name))
        .map(|p| p[1..].to_vec())
        .filter(|p| !p.is_empty())
        .collect();
    if !paths.iter().any(|p| p.first() == Some(&r.name)) {
        return Ok("monorm_client::NotCached".into());
    }
    let target = schema
        .models
        .iter()
        .find(|m| m.ir.name == r.target)
        .ok_or("unknown default target")?;
    let mut args = target
        .fields()
        .iter()
        .map(|f| {
            if default_fields(schema, &r.target)
                .is_none_or(|fields| fields.iter().any(|n| n == &f.name))
            {
                "Loaded".to_owned()
            } else {
                "monorm_client::Unloaded".to_owned()
            }
        })
        .collect::<Vec<_>>();
    for i in 0..target.ir.relations.len() {
        args.push(normal_relation(schema, &r.target, i, &children)?);
    }
    Ok(format!(
        "monorm_client::DefaultCached<{}<{}>>",
        ident(&r.target)?,
        args.join(",")
    ))
}

fn path_cardinality(
    schema: &Schema,
    model: &crate::schema::Model,
    relation: &crate::ir::RelationIr,
) -> &'static str {
    let filtered = super::reference_target_filtered(schema, &relation.target);
    if relation.kind != crate::ir::RelKind::One {
        "Many"
    } else if !relation.foreign_key
        || filtered
        || relation
            .from
            .iter()
            .any(|name| model.fields().iter().any(|f| f.name == *name && f.nullable))
    {
        "OptionalOne"
    } else {
        "One"
    }
}

fn cardinality(kind: crate::ir::RelKind) -> &'static str {
    if kind == crate::ir::RelKind::One {
        "One"
    } else {
        "Many"
    }
}

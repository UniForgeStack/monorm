//! Owned values and loaded flags shared by static transitions and runtime rows.
use crate::*;
use orm_core::{behavior::ResultShape, ir::ValueType};
use orm_engine::{db::RowSet, exec::Fetched, plan::Output};
use std::collections::BTreeMap;

/// Internal row storage has no public constructor or mutable escape.
#[doc(hidden)]
#[derive(Clone)]
pub struct RowData {
    pub(crate) model: String,
    pub(crate) identity: Identity,
    pub(crate) values: BTreeMap<String, BindValue>,
    pub(crate) helpers: BTreeMap<String, BindValue>,
    pub(crate) relations: BTreeMap<String, CacheData>,
}
#[derive(Clone)]
pub(crate) enum CacheData {
    One(Option<Box<RowData>>),
    Many(Vec<RowData>),
}
impl serde::Serialize for RowData {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.public_json()
            .map_err(serde::ser::Error::custom)?
            .serialize(s)
    }
}

pub(crate) fn cell(value: &BindValue) -> Result<Cell<'_>> {
    use BindValue as V;
    if *value == value.as_null() {
        return Ok(Cell::Null);
    }
    Ok(match value {
        V::Bool(Some(v)) => Cell::Bool(*v),
        V::Int(Some(v)) => Cell::Int(*v),
        V::BigInt(Some(v)) => Cell::BigInt(*v),
        V::Double(Some(v)) => Cell::Float(*v),
        V::String(Some(v)) => Cell::Text(v),
        V::ChronoDate(Some(v)) => Cell::Date(*v),
        V::ChronoDateTimeUtc(Some(v)) => Cell::DateTime(*v),
        V::ChronoDateTimeWithTimeZone(Some(v)) => Cell::DateTime(v.with_timezone(&chrono::Utc)),
        V::Uuid(Some(v)) => Cell::Uuid(*v),
        V::Json(Some(v)) => Cell::Json((**v).clone()),
        V::BigDecimal(Some(v)) => Cell::Decimal(v.to_string()),
        V::Array(_, Some(v)) => Cell::Array(v.iter().map(cell).collect::<Result<_>>()?),
        _ => return Err(Error::Decode("unsupported stored value".into())),
    })
}
fn store(value: Cell<'_>, ty: ValueType) -> Result<BindValue> {
    Ok(match value {
        Cell::Null => orm_engine::params::null_of(Some(ty)),
        Cell::Bool(v) => v.into(),
        Cell::Int(v) => v.into(),
        Cell::BigInt(v) => v.into(),
        Cell::Float(v) => v.into(),
        Cell::Text(v) => v.to_owned().into(),
        Cell::DateTime(v) => v.fixed_offset().into(),
        Cell::Date(v) => v.into(),
        Cell::Uuid(v) => v.into(),
        Cell::Json(v) => v.into(),
        Cell::Decimal(v) => v
            .parse::<bigdecimal::BigDecimal>()
            .map_err(|e| Error::Decode(format!("invalid decimal: {e}")))?
            .into(),
        Cell::Array(values) => BindValue::Array(
            orm_engine::params::array_type(ty.ty),
            Some(Box::new(
                values
                    .into_iter()
                    .map(|v| store(v, ValueType { array: false, ..ty }))
                    .collect::<Result<_>>()?,
            )),
        ),
    })
}
impl RowData {
    #[doc(hidden)]
    pub fn loaded(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }
    #[doc(hidden)]
    pub fn cache_satisfies<T: Model>(&self, name: &str, many: bool) -> bool {
        let valid = |data: &RowData| T::decode(&DecodedRow { data }).is_ok();
        match self.relations.get(name) {
            Some(CacheData::One(row)) if !many => row.as_deref().is_none_or(valid),
            Some(CacheData::Many(rows)) if many => rows.iter().all(valid),
            _ => false,
        }
    }
    pub(crate) fn read<T: Codec>(&self, name: &str) -> Result<T> {
        T::decode(cell(
            self.values
                .get(name)
                .ok_or_else(|| Error::Unloaded(name.into()))?,
        )?)
    }
    pub(crate) fn key(&self, name: &str) -> Result<&BindValue> {
        self.values
            .get(name)
            .or_else(|| self.helpers.get(name))
            .ok_or_else(|| Error::Decode(format!("missing private key {name}")))
    }
    pub(crate) fn merge(&mut self, other: Self, reload: bool) {
        for (k, v) in other.values {
            if reload || !self.values.contains_key(&k) {
                self.values.insert(k, v);
            }
        }
        for (k, v) in other.helpers {
            if reload || !self.helpers.contains_key(&k) {
                self.helpers.insert(k, v);
            }
        }
        for (k, v) in other.relations {
            if reload {
                self.relations.insert(k, v);
                continue;
            }
            match (self.relations.get_mut(&k), v) {
                (Some(CacheData::One(Some(old))), CacheData::One(Some(new)))
                    if old.identity == new.identity =>
                {
                    old.merge(*new, false)
                }
                (Some(CacheData::Many(old)), CacheData::Many(new)) => {
                    let new: std::collections::HashMap<_, _> = new
                        .into_iter()
                        .map(|row| (row.identity.clone(), row))
                        .collect();
                    for row in old {
                        if let Some(next) = new.get(&row.identity) {
                            row.merge(next.clone(), false);
                        }
                    }
                }
                (None, v) => {
                    self.relations.insert(k, v);
                }
                _ => {}
            }
        }
    }
    pub fn public_json(&self) -> Result<serde_json::Value> {
        let mut result = serde_json::Map::new();
        for (name, value) in &self.values {
            result.insert(name.clone(), json_cell(cell(value)?));
        }
        for (name, cache) in &self.relations {
            let value = match cache {
                CacheData::One(None) => serde_json::Value::Null,
                CacheData::One(Some(row)) => row.public_json()?,
                CacheData::Many(rows) => serde_json::Value::Array(
                    rows.iter()
                        .map(RowData::public_json)
                        .collect::<Result<_>>()?,
                ),
            };
            result.insert(name.clone(), value);
        }
        Ok(result.into())
    }
}
fn json_cell(c: Cell<'_>) -> serde_json::Value {
    match c {
        Cell::Null => serde_json::Value::Null,
        Cell::Bool(v) => v.into(),
        Cell::Int(v) => v.into(),
        Cell::BigInt(v) => v.into(),
        Cell::Float(v) => serde_json::json!(v),
        Cell::Text(v) => v.into(),
        Cell::DateTime(v) => v.to_rfc3339().into(),
        Cell::Date(v) => v.to_string().into(),
        Cell::Uuid(v) => v.to_string().into(),
        Cell::Json(v) => v,
        Cell::Decimal(v) => v.into(),
        Cell::Array(v) => v.into_iter().map(json_cell).collect(),
    }
}

fn instance(
    schema: &orm_core::schema::Schema,
    model: usize,
    shape: Option<&ResultShape>,
    rows: &dyn RowSet,
    types: &[ValueType],
    index: usize,
    start: usize,
) -> Result<RowData> {
    let m = &schema.models[model];
    let positions: Vec<_> = shape.map_or_else(
        || (0..m.fields().len()).map(|p| (p, true)).collect(),
        |s| {
            s.fields
                .iter()
                .map(|f| (f.field.position, f.public))
                .collect()
        },
    );
    let mut data = RowData {
        model: m.ir.name.clone(),
        identity: Identity {
            value: BindValue::Int(None),
        },
        values: BTreeMap::new(),
        helpers: BTreeMap::new(),
        relations: BTreeMap::new(),
    };
    for (slot, (pos, public)) in positions.into_iter().enumerate() {
        let value = store(
            rows.cell(index, start + slot, types[start + slot])?,
            types[start + slot],
        )?;
        if pos == m.pk {
            data.identity = Identity {
                value: value.clone(),
            };
        }
        if public {
            data.values.insert(m.fields()[pos].name.clone(), value);
        } else {
            data.helpers.insert(m.fields()[pos].name.clone(), value);
        }
    }
    Ok(data)
}
fn instances(
    schema: &orm_core::schema::Schema,
    output: &Output,
    rows: &dyn RowSet,
    types: &[ValueType],
) -> Result<Vec<RowData>> {
    let Output::Instances {
        model,
        shape,
        joins,
    } = output
    else {
        return Err(Error::Decode("expected model rows".into()));
    };
    let mut result = Vec::new();
    for index in 0..rows.len() {
        let mut root = instance(schema, *model, shape.as_ref(), rows, types, index, 0)?;
        let mut joined: Vec<Option<RowData>> = Vec::new();
        for j in joins {
            joined.push(
                if rows.cell(index, j.start + j.pk_pos, types[j.start + j.pk_pos])? == Cell::Null {
                    None
                } else {
                    Some(instance(
                        schema,
                        j.model,
                        j.shape.as_ref(),
                        rows,
                        types,
                        index,
                        j.start,
                    )?)
                },
            );
        }
        for (i, j) in joins.iter().enumerate().rev() {
            let value = CacheData::One(joined[i].take().map(Box::new));
            let parent = match j.parent {
                None => Some(&mut root),
                Some(p) => joined[p].as_mut(),
            };
            if let Some(parent) = parent {
                parent.relations.insert(j.attr.clone(), value);
            }
        }
        result.push(root);
    }
    Ok(result)
}
fn attach(
    schema: &orm_core::schema::Schema,
    parents: &mut [RowData],
    parent_rows: &dyn RowSet,
    fetched: &[Fetched],
) -> Result<()> {
    for fetched in fetched {
        let p = &fetched.plan;
        let mut children = instances(schema, &p.output, fetched.rows.as_ref(), &p.types)?;
        attach(
            schema,
            &mut children,
            fetched.rows.as_ref(),
            &fetched.children,
        )?;
        let mut groups: std::collections::HashMap<BindValue, Vec<RowData>> =
            std::collections::HashMap::new();
        for (i, row) in children.into_iter().enumerate() {
            groups
                .entry(
                    fetched
                        .rows
                        .value(i, p.child_key_pos, p.types[p.child_key_pos])?,
                )
                .or_default()
                .push(row);
        }
        for (i, parent) in parents.iter_mut().enumerate() {
            let key = parent_rows.value(i, p.key_pos, p.key_type)?;
            let rows = if key == key.as_null() {
                vec![]
            } else {
                groups.get(&key).cloned().unwrap_or_default()
            };
            let cache = if p.many {
                CacheData::Many(rows)
            } else {
                CacheData::One(rows.into_iter().next().map(Box::new))
            };
            parent.relations.insert(p.attr.clone(), cache);
        }
    }
    Ok(())
}
pub(crate) fn snapshot(
    schema: &orm_core::schema::Schema,
    s: &orm_engine::exec::Selected,
) -> Result<Vec<RowData>> {
    let mut rows = instances(schema, &s.plan.output, s.rows.as_ref(), &s.plan.types)?;
    attach(schema, &mut rows, s.rows.as_ref(), &s.prefetched)?;
    Ok(rows)
}

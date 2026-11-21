//! Application checks for writes to tables and read-only views.

use crate::error::{Error, Result, WriteProtection};
use crate::exec::Conflict;
use crate::prepared_write::{RowsKind, RowsSpec};
use monorm_core::ir::{Operation, TriggerEvent};
use monorm_core::schema::{Model, Schema};

/// A prepared check keeps only rules that can match the planned write.
pub struct Writable {
    view: Option<String>,
    protected: Vec<(String, String)>,
}

#[derive(Default)]
struct WriteOptions {
    hard_delete: bool,
    upsert: bool,
}

impl Writable {
    fn new(
        schema: &Schema,
        name: &str,
        event: TriggerEvent,
        fields: &[&str],
        options: WriteOptions,
    ) -> Result<Self> {
        use monorm::writes::WriteKind;
        use TriggerEvent::{Delete, Insert, Update};
        let index = schema.model_idx(name).map_err(Error::Query)?;
        let model = schema.model(index);
        let view = model.ir.sql_view.is_some().then(|| name.to_owned());
        if schema.write_rules.is_empty() {
            return Ok(Self {
                view,
                protected: vec![],
            });
        }
        let matches = |owner, field: &str| {
            model.field_pos(field).ok().is_some_and(|position| {
                let storage = model.resolved_fields[position].storage;
                storage.owner == owner
            })
        };
        let mut protected = vec![];
        for table in schema.storage_chain(index) {
            let owner = table.owner;
            let has_field = |field: &str| matches(owner, field);
            let assignment_kind = match event {
                Insert => WriteKind::Insert,
                Update if options.upsert => WriteKind::Upsert,
                Update => WriteKind::Update,
                Delete => WriteKind::DeleteRewrite,
                TriggerEvent::Truncate => unreachable!(),
            };
            let assignments = model
                .assignments
                .iter()
                .filter(|a| a.applies_to(assignment_kind))
                .map(|a| model.fields()[a.field].name.as_str());
            let rewrite = (event == Delete && !options.hard_delete)
                .then_some(model.delete_rewrite.as_ref())
                .flatten();
            let rewritten_field = rewrite.map(|rule| model.fields()[rule.field].name.as_str());
            let assigned: Vec<&str> = fields
                .iter()
                .copied()
                .chain(assignments)
                .chain(rewritten_field)
                .filter(|f| has_field(f))
                .collect();
            if event == Update && assigned.is_empty() {
                continue;
            }
            for rule in schema
                .write_rules
                .iter()
                .filter(|rule| rule.model == table.ir.name && rule.layer.app())
            {
                let regular = rule.operations.contains(&event)
                    && (event != Update
                        || rule.fields.is_empty()
                        || rule
                            .fields
                            .iter()
                            .any(|f| field_is_assigned(schema, table, model, f, &assigned)));
                let rewritten = rewrite.is_some()
                    && !assigned.is_empty()
                    && rule.operations.contains(&Update)
                    && (rule.fields.is_empty()
                        || rule
                            .fields
                            .iter()
                            .any(|f| field_is_assigned(schema, table, model, f, &assigned)));
                if (regular || rewritten) && !protected.iter().any(|(t, _)| t == table.table()) {
                    protected.push((table.table().to_owned(), table.ir.name.clone()));
                }
            }
        }
        Ok(Self { view, protected })
    }

    pub fn operation(schema: &Schema, op: &Operation) -> Result<Self> {
        match op {
            Operation::Update(update) => Self::new(
                schema,
                &update.model,
                TriggerEvent::Update,
                &update
                    .set
                    .iter()
                    .map(|a| a.field.as_str())
                    .collect::<Vec<_>>(),
                WriteOptions::default(),
            ),
            Operation::Delete(delete) => Self::new(
                schema,
                &delete.model,
                TriggerEvent::Delete,
                &[],
                WriteOptions {
                    hard_delete: delete.hard,
                    ..Default::default()
                },
            ),
            _ => Ok(Self {
                view: None,
                protected: vec![],
            }),
        }
    }

    pub fn insert(schema: &Schema, model: &str, conflict: Option<&Conflict>) -> Result<Self> {
        let mut check = Self::new(
            schema,
            model,
            TriggerEvent::Insert,
            &[],
            WriteOptions::default(),
        )?;
        if let Some(Conflict::Update { update, set, .. }) = conflict {
            let fields = update
                .iter()
                .map(String::as_str)
                .chain(set.iter().map(|a| a.field.as_str()))
                .collect::<Vec<_>>();
            check.protected.extend(
                Self::new(
                    schema,
                    model,
                    TriggerEvent::Update,
                    &fields,
                    WriteOptions {
                        upsert: true,
                        ..Default::default()
                    },
                )?
                .protected,
            );
        }
        Ok(check)
    }

    pub fn update_many(schema: &Schema, model: &str, fields: &[String]) -> Result<Self> {
        let fields = fields
            .iter()
            .skip(1)
            .map(String::as_str)
            .collect::<Vec<_>>();
        Self::new(
            schema,
            model,
            TriggerEvent::Update,
            &fields,
            WriteOptions::default(),
        )
    }

    pub fn rows(schema: &Schema, spec: &RowsSpec) -> Result<Self> {
        match &spec.kind {
            RowsKind::Insert(conflict) => Self::insert(schema, &spec.model, conflict.as_ref()),
            RowsKind::UpdateMany(_) => Self::update_many(schema, &spec.model, &spec.fields),
        }
    }

    pub fn check(&self, schema: &Schema, allowed: &[String]) -> Result<()> {
        if let Some(view) = &self.view {
            return Err(Error::ReadOnlyModel(view.clone()));
        }
        for (table, owner) in &self.protected {
            let permitted = allowed
                .iter()
                .filter_map(|name| schema.model_idx(name).ok())
                .any(|m| schema.model(m).table() == table);
            if !permitted {
                return Err(Error::WriteProtected(WriteProtection::Application {
                    model: owner.clone(),
                }));
            }
        }
        Ok(())
    }
}

fn field_is_assigned(
    schema: &Schema,
    table: &Model,
    model: &Model,
    field: &str,
    assigned: &[&str],
) -> bool {
    let Ok(column) = table.field_pos(field) else {
        return false;
    };
    assigned
        .iter()
        .filter_map(|field| model.field_pos(field).ok())
        .any(|pos| {
            let storage = model.resolved_fields[pos].storage;
            schema.physical().model(storage.owner.0).table() == table.table()
                && storage.column == column
        })
}

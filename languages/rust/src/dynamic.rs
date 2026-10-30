use crate::*;
use monorm_core::{ir, schema::Schema};

fn schema<M: Model>() -> Result<Schema> {
    Schema::from_ir(ir::SchemaIr::from_json(M::Schema::JSON).map_err(Error::Compatibility)?)
        .map_err(Error::Compatibility)
}
fn path(schema: &Schema, root: usize, name: &str, order: bool) -> Result<(Vec<String>, String)> {
    let parts: Vec<_> = name.split('.').collect();
    let mut model = root;
    let mut hops = Vec::new();
    for hop in &parts[..parts.len() - 1] {
        let (relation, target) = schema.models[model]
            .relation(hop)
            .map_err(Error::Compatibility)?;
        if order && relation.kind == ir::RelKind::Many {
            return Err(Error::Compatibility(
                "cannot order by a to-many path".into(),
            ));
        }
        hops.push((*hop).into());
        model = target;
    }
    let field = parts[parts.len() - 1];
    schema.models[model]
        .field(field)
        .map_err(Error::Compatibility)?;
    Ok((hops, field.into()))
}
fn normal_query(schema: &Schema, model: usize) -> Result<SelectionPlan> {
    let mut q: SelectionPlan = serde_json::from_value(
        serde_json::json!({"model":schema.models[model].ir.name,"without_related":true}),
    )
    .map_err(|e| Error::Compatibility(e.to_string()))?;
    for r in &schema.models[model].ir.relations {
        q.model_helpers.extend(r.from.iter().cloned());
    }
    for path in &schema.models[model].read_flags.related {
        add(
            schema,
            model,
            &mut q,
            &path.iter().map(String::as_str).collect::<Vec<_>>(),
        )?;
    }
    Ok(q)
}
fn add(schema: &Schema, model: usize, q: &mut SelectionPlan, parts: &[&str]) -> Result<()> {
    if parts.len() == 1 && schema.models[model].field(parts[0]).is_ok() {
        let fields = q.model_fields.get_or_insert_with(Vec::new);
        if !fields.iter().any(|f| f == parts[0]) {
            fields.push(parts[0].into());
        }
        return Ok(());
    }
    let (relation, target) = schema.models[model]
        .relation(parts[0])
        .map_err(Error::Compatibility)?;
    let index = if let Some(index) = q
        .prefetch
        .iter()
        .position(|p| p.relation == relation.name && p.attr.is_none())
    {
        index
    } else {
        let query = normal_query(schema, target)?;
        q.prefetch.push(ir::Prefetch {
            relation: relation.name.clone(),
            attr: None,
            one: false,
            names: Vec::new(),
            query,
        });
        q.prefetch.len() - 1
    };
    if parts.len() > 1 {
        add(schema, target, &mut q.prefetch[index].query, &parts[1..])?;
    }
    Ok(())
}
impl<M: Model, R, L, D> Query<M, R, L, D> {
    /// Runtime strings always yield the same RuntimeRow type. Invalid columns and
    /// relation paths fail validation; strings never synthesize Rust fields.
    pub fn runtime(
        mut self,
        fields: &[&str],
    ) -> Result<Query<RuntimeRow<M::Normal>, R, L, ExplicitSelection>> {
        self = self.initialized();
        let schema = schema::<M>()?;
        let root = schema
            .models
            .iter()
            .position(|m| m.ir.name == M::NAME)
            .ok_or_else(|| Error::Compatibility("unknown model".into()))?;
        if fields
            .iter()
            .any(|field| !field.contains('.') && schema.models[root].field(field).is_ok())
        {
            self.select.model_fields = Some(vec![]);
        }
        for field in fields {
            add(
                &schema,
                root,
                &mut self.select,
                &field.split('.').collect::<Vec<_>>(),
            )?;
        }
        Ok(Query::from_parts(self.select, self.filters))
    }
}
impl<M: Model, R, L, D> Query<RuntimeRow<M>, R, L, D> {
    pub fn filter_runtime<T: Codec>(mut self, name: &str, value: T) -> Result<Self> {
        let schema = schema::<M>()?;
        let root = schema
            .models
            .iter()
            .position(|m| m.ir.name == M::NAME)
            .ok_or_else(|| Error::Compatibility("unknown model".into()))?;
        let (path, name) = path(&schema, root, name, false)?;
        self.filters.push(Tree::Compare(
            ir::Expr::Col { path, name, sub: vec![], alias: vec![] },
            ir::CmpOp::Eq,
            value.encode(),
        ));
        Ok(self)
    }
    pub fn order_runtime(mut self, name: &str, descending: bool) -> Result<Self> {
        let schema = schema::<M>()?;
        let root = schema
            .models
            .iter()
            .position(|m| m.ir.name == M::NAME)
            .ok_or_else(|| Error::Compatibility("unknown model".into()))?;
        let (path, name) = path(&schema, root, name, true)?;
        self.select.order = vec![ir::Order {
            expr: ir::Expr::Col { path, name, sub: vec![], alias: vec![] },
            desc: descending,
            nulls: None,
        }];
        Ok(self)
    }
}

//! Reusable literal-name machinery for immutable named relation views.
use crate::*;

pub struct NamedRelation<R, P, C, K, B = NormalTarget> {
    relation: Relation<R, P, C, K, B>,
    label: &'static str,
    one: bool,
}
impl<R, P: Model, C, K, B> Relation<R, P, C, K, B> {
    /// Relation-only builder. Runtime labels do not create fields; use
    /// relation_views! with literal identifiers for an immutable named view.
    pub fn label(self, label: &'static str) -> NamedRelation<R, P, C, K, B> {
        NamedRelation {
            relation: self,
            label,
            one: false,
        }
    }
}
impl<R, P, C, K, B> Clone for Relation<R, P, C, K, B> {
    fn clone(&self) -> Self {
        Self {
            name: self.name,
            query: self.query.clone(),
            filters: self.filters.clone(),
            _type: std::marker::PhantomData,
        }
    }
}
impl<R, P, C, K, B> Clone for NamedRelation<R, P, C, K, B> {
    fn clone(&self) -> Self {
        Self {
            relation: self.relation.clone(),
            label: self.label,
            one: self.one,
        }
    }
}
impl<R, P, C, K, B> Descriptor for NamedRelation<R, P, C, K, B> {
    type Kind = Relations;
}
impl<M: Model<Source = R>, R, P: Model<Schema = M::Schema>, C, K, B> Apply<M>
    for NamedRelation<R, P, C, K, B>
{
    type Output = M;
    fn apply(self, q: &mut SelectionPlan, f: &mut Vec<Tree>) {
        let mut node = SelectionPlan::clone(q);
        node.prefetch.clear();
        node.select_related.clear();
        node.related_fields.clear();
        let mut filters = vec![];
        self.relation.install(&mut node, &mut filters, false);
        let mut prefetch = node.prefetch.pop().expect("installed relation");
        prefetch.attr = Some(self.label.into());
        prefetch.one = self.one;
        q.prefetch.push(prefetch);
        for tree in filters {
            if let Tree::Nested(_, trees) = tree {
                f.push(Tree::Nested(self.label.into(), trees));
            }
        }
        // Literal/root/schema collisions are checked before planning, including
        // collisions with unloaded fields and canonical relation names.
    }
}
impl<R, P: Model, C, K, B> NamedRelation<R, P, C, K, B> {
    pub fn label(mut self, label: &'static str) -> Self {
        self.label = label;
        self
    }
    pub fn as_prefetch<T: Model<Schema = P::Schema, Source = P::Source>, L, D: TargetSelection>(
        self,
        query: Query<T, Optional, L, D>,
    ) -> NamedRelation<R, T, C, K, D::Target> {
        NamedRelation {
            relation: self.relation.as_prefetch(query),
            label: self.label,
            one: self.one,
        }
    }
}
impl<R, P: Model, K, B> NamedRelation<R, P, Many, K, B> {
    /// An optional single named view of a to-many relation, ordered per parent.
    pub fn one(self) -> NamedRelation<R, P, One, K, B> {
        let relation = Relation {
            name: self.relation.name,
            query: self.relation.query,
            filters: self.relation.filters,
            _type: std::marker::PhantomData,
        };
        NamedRelation {
            relation,
            label: self.label,
            one: true,
        }
    }
}
impl<R, P: Model, C, K, B> NamedRelation<R, P, C, K, B>
where
    Cached<P>: RelationState<P, C>,
{
    #[doc(hidden)]
    pub fn extract<M: Model<Source = R, Schema = P::Schema>>(
        &self,
        row: &M,
    ) -> Result<<Cached<P> as RelationState<P, C>>::Value> {
        let ir =
            orm_core::ir::SchemaIr::from_json(M::Schema::JSON).map_err(Error::Compatibility)?;
        let root = ir
            .models
            .iter()
            .find(|m| m.name == M::NAME)
            .ok_or_else(|| Error::Compatibility("unknown view root".into()))?;
        let relation = root
            .relations
            .iter()
            .find(|r| r.name == self.relation.name)
            .ok_or_else(|| Error::Compatibility("unknown view relation".into()))?;
        let to: Vec<_> = relation.to.iter().map(String::as_str).collect();
        let from: Vec<_> = relation.from.iter().map(String::as_str).collect();
        Cached::<P>::read(&DecodedRow { data: row.data() }, self.label, &from, &to)
    }
}

/// Owns a macro-generated public field view and exposes immutable field syntax.
pub struct NamedViews<T> {
    values: T,
}
impl<T> NamedViews<T> {
    #[doc(hidden)]
    pub fn generated(values: T) -> Self {
        Self { values }
    }
}
impl<T> std::ops::Deref for NamedViews<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.values
    }
}

/// Named results remain local generic definitions. Duplicate literal identifiers
/// are rejected by Rust; schema and canonical-cache collisions fail validation.
#[macro_export]
macro_rules! relation_views {
    ($row:expr,$db:expr; $($label:ident => $descriptor:expr),+ $(,)?) => {
        async {
            use $crate::Instance as _;
            $(let $label=($descriptor).label(stringify!($label));)+
            let row=($row).load(($($label.clone(),)+),$db).await?;
            $(let $label=match $label.extract(&row) {Ok(value)=>value,Err(error)=>return Err($crate::TransitionError {error,original:Box::new(row)})};)+
            #[allow(non_camel_case_types)]
            struct Fields<Root,$($label),+> {pub row:Root,$(pub $label:$label),+}
            Ok::<_, $crate::TransitionError<_>>($crate::NamedViews::generated(Fields {row,$($label),+}))
        }
    };
}

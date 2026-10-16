//! Selection descriptors carry finite output types; execution still uses shared IR.
use crate::*;
use orm_core::ir;
use std::marker::PhantomData;

pub enum DefaultSelection {}
pub enum ExplicitSelection {}
pub enum BypassedDefault {}
pub enum BypassedExplicit {}
pub trait TargetSelection {
    type Target;
}
impl TargetSelection for DefaultSelection {
    type Target = NormalTarget;
}
impl TargetSelection for ExplicitSelection {
    type Target = SelectedTarget;
}
impl TargetSelection for BypassedDefault {
    type Target = NormalTarget;
}
impl TargetSelection for BypassedExplicit {
    type Target = SelectedTarget;
}
pub trait AdjustOutput<M: Model> {
    type Output: Model<Schema = M::Schema, Source = M::Source>;
}
impl<M: Model> AdjustOutput<M> for DefaultSelection {
    type Output = M;
}
impl<M: Model> AdjustOutput<M> for ExplicitSelection {
    type Output = M;
}
impl<M: BypassShape> AdjustOutput<M> for BypassedDefault {
    type Output = M::Output;
}
impl<M: BypassShape> AdjustOutput<M> for BypassedExplicit {
    type Output = M::Output;
}

pub enum Scalars {}
pub enum Relations {}
pub trait NextSelection<K> {
    type Mode;
}
impl NextSelection<Scalars> for DefaultSelection {
    type Mode = ExplicitSelection;
}
impl NextSelection<Relations> for DefaultSelection {
    type Mode = DefaultSelection;
}
impl<K> NextSelection<K> for ExplicitSelection {
    type Mode = ExplicitSelection;
}
pub trait SelectionKind<K> {
    type Output;
}
impl SelectionKind<Scalars> for Scalars {
    type Output = Scalars;
}
impl SelectionKind<Relations> for Scalars {
    type Output = Scalars;
}
impl SelectionKind<Scalars> for Relations {
    type Output = Scalars;
}
impl SelectionKind<Relations> for Relations {
    type Output = Relations;
}

pub trait ResetScalars: Model {
    type Empty: Model<Schema = Self::Schema, Source = Self::Source>;
    type Full: Model<Schema = Self::Schema, Source = Self::Source>;
}
pub trait ResetRelated: Model {
    type WithoutRelated: Model<Schema = Self::Schema, Source = Self::Source>;
    fn default_relations() -> Vec<&'static str>;
}
pub trait BypassDefaults<M: Model> {
    type Mode;
    type Output: Model<Schema = M::Schema, Source = M::Source>;
}
impl<M: ResetScalars> BypassDefaults<M> for DefaultSelection
where
    M::Full: BypassShape,
{
    type Output = <M::Full as BypassShape>::Output;
    type Mode = BypassedDefault;
}
impl<M: BypassShape> BypassDefaults<M> for ExplicitSelection {
    type Output = M::Output;
    type Mode = BypassedExplicit;
}

impl<M: ResetScalars> BypassDefaults<M> for BypassedDefault
where
    M::Full: BypassShape,
{
    type Output = <M::Full as BypassShape>::Output;
    type Mode = Self;
}
impl<M: BypassShape> BypassDefaults<M> for BypassedExplicit {
    type Output = M::Output;
    type Mode = Self;
}
impl NextSelection<Scalars> for BypassedDefault {
    type Mode = BypassedExplicit;
}
impl NextSelection<Relations> for BypassedDefault {
    type Mode = BypassedDefault;
}
impl<K> NextSelection<K> for BypassedExplicit {
    type Mode = BypassedExplicit;
}
pub trait SelectionBase<M: Model, K> {
    type Base: Model<Schema = M::Schema, Source = M::Source>;
}
impl<M: ResetScalars> SelectionBase<M, Scalars> for DefaultSelection {
    type Base = M::Empty;
}
impl<M: Model> SelectionBase<M, Relations> for DefaultSelection {
    type Base = M;
}
impl<M: Model, K> SelectionBase<M, K> for ExplicitSelection {
    type Base = M;
}

impl<M: ResetScalars> SelectionBase<M, Scalars> for BypassedDefault {
    type Base = M::Empty;
}
impl<M: Model> SelectionBase<M, Relations> for BypassedDefault {
    type Base = M;
}
impl<M: Model, K> SelectionBase<M, K> for BypassedExplicit {
    type Base = M;
}
/// Implemented by generated root descriptors. Paths and wrong-model descriptors
/// have no implementation and cannot upgrade a root row.
/// Output selected by a descriptor after applying first-selection policy.
pub type Requested<M, A, D = DefaultSelection> = <D as AdjustOutput<
    <A as Apply<<D as SelectionBase<M, <A as Descriptor>::Kind>>::Base>>::Output,
>>::Output;
pub type RequestedQuery<M, A, R = Optional, L = Unsliced, D = DefaultSelection> =
    Query<Requested<M, A, D>, R, L, <D as NextSelection<<A as Descriptor>::Kind>>::Mode>;
pub trait Descriptor {
    type Kind;
}
pub trait Apply<M: Model>: Descriptor {
    type Output: Model<Schema = M::Schema, Source = M::Source>;
    fn apply(self, query: &mut ir::Select, filters: &mut Vec<Tree>);
    #[doc(hidden)]
    fn needs_data(&self, _: &RowData) -> bool {
        true
    }
}
impl<A: Descriptor, B: Descriptor> Descriptor for (A, B)
where
    <A as Descriptor>::Kind: SelectionKind<B::Kind>,
{
    type Kind = <<A as Descriptor>::Kind as SelectionKind<B::Kind>>::Output;
}
impl<M: Model, A: Apply<M>, B: Apply<A::Output>> Apply<M> for (A, B)
where
    <A as Descriptor>::Kind: SelectionKind<B::Kind>,
{
    type Output = B::Output;
    fn needs_data(&self, row: &RowData) -> bool {
        self.0.needs_data(row) || self.1.needs_data(row)
    }
    fn apply(self, q: &mut ir::Select, f: &mut Vec<Tree>) {
        self.0.apply(q, f);
        self.1.apply(q, f);
    }
}
// Nested tuples support arbitrary descriptor counts without subset enumeration.
macro_rules! tuple {
    ($a:ident,$b:ident,$c:ident) => {
        impl<$a: Descriptor, $b: Descriptor, $c: Descriptor> Descriptor for ($a, $b, $c)
        where
            ($a, $b): Descriptor,
            <($a, $b) as Descriptor>::Kind: SelectionKind<$c::Kind>,
        {
            type Kind = <<($a, $b) as Descriptor>::Kind as SelectionKind<$c::Kind>>::Output;
        }
        impl<M: Model, $a: Apply<M>, $b: Apply<$a::Output>, $c: Apply<$b::Output>> Apply<M>
            for ($a, $b, $c)
        where
            ($a, $b): Descriptor,
            <($a, $b) as Descriptor>::Kind: SelectionKind<$c::Kind>,
        {
            type Output = $c::Output;
            fn needs_data(&self, row: &RowData) -> bool {
                self.0.needs_data(row) || self.1.needs_data(row) || self.2.needs_data(row)
            }
            fn apply(self, q: &mut ir::Select, f: &mut Vec<Tree>) {
                self.0.apply(q, f);
                self.1.apply(q, f);
                self.2.apply(q, f);
            }
        }
    };
}
tuple!(A, B, C);

impl<M: Model, R, L, D> Query<M, R, L, D> {
    #[doc(hidden)]
    pub fn explicit_shape(mut self) -> Query<M, R, L, ExplicitSelection> {
        self = self.initialized();
        Query::from_parts(self.select, self.filters)
    }
    pub fn without_related(mut self) -> Query<M::WithoutRelated, R, L, D>
    where
        M: ResetRelated,
    {
        self = self.initialized();
        let defaults = M::default_relations();
        self.select
            .prefetch
            .retain(|p| p.attr.is_some() || !defaults.contains(&p.relation.as_str()));
        self.select.select_related.clear();
        self.select.related_fields.clear();
        self.select.without_related = true;
        Query::from_parts(self.select, self.filters)
    }
    pub fn without_defaults(
        mut self,
    ) -> Query<<D as BypassDefaults<M>>::Output, R, L, <D as BypassDefaults<M>>::Mode>
    where
        D: BypassDefaults<M>,
        M: ResetRelated,
    {
        self = self.initialized();
        let defaults = M::default_relations();
        self.select
            .prefetch
            .retain(|p| p.attr.is_some() || !defaults.contains(&p.relation.as_str()));
        self.select.select_related.clear();
        self.select.related_fields.clear();
        self.select.without_defaults = true;
        self.select.without_related = true;
        <D as BypassDefaults<M>>::Output::selection(&mut self.select);
        Query::from_parts(self.select, self.filters)
    }
    pub fn load<A>(mut self, descriptor: A) -> RequestedQuery<M, A, R, L, D>
    where
        A: Descriptor,
        D: SelectionBase<M, <A as Descriptor>::Kind> + NextSelection<<A as Descriptor>::Kind>,
        A: Apply<<D as SelectionBase<M, <A as Descriptor>::Kind>>::Base>,
        D: AdjustOutput<
            <A as Apply<<D as SelectionBase<M, <A as Descriptor>::Kind>>::Base>>::Output,
        >,
    {
        self = self.initialized();
        self.select.model_fields =
            Some(<D as SelectionBase<M, <A as Descriptor>::Kind>>::Base::fields());
        descriptor.apply(&mut self.select, &mut self.filters);
        Requested::<M, A, D>::selection(&mut self.select);
        Query::from_parts(self.select, self.filters)
    }
    pub fn all_scalars(mut self) -> Query<M::Full, R, L, <D as NextSelection<Scalars>>::Mode>
    where
        M: ResetScalars,
        D: NextSelection<Scalars>,
    {
        self = self.initialized();
        self.select.model_fields = Some(M::Full::fields());
        Query::from_parts(self.select, self.filters)
    }
    pub(crate) fn from_parts(select: ir::Select, filters: Vec<crate::query::Tree>) -> Self {
        Self {
            select,
            filters,
            _type: PhantomData,
        }
    }
}

impl<A: Descriptor> Descriptor for (A,) {
    type Kind = A::Kind;
}
impl<M: Model, A: Apply<M>> Apply<M> for (A,) {
    type Output = A::Output;
    fn apply(self, q: &mut SelectionPlan, f: &mut Vec<Tree>) {
        self.0.apply(q, f);
    }
}
impl<A, B, C, D> Descriptor for (A, B, C, D)
where
    (A, (B, (C, D))): Descriptor,
{
    type Kind = <(A, (B, (C, D))) as Descriptor>::Kind;
}
impl<M: Model, A, B, C, D> Apply<M> for (A, B, C, D)
where
    (A, (B, (C, D))): Apply<M>,
{
    type Output = <(A, (B, (C, D))) as Apply<M>>::Output;
    fn apply(self, q: &mut SelectionPlan, f: &mut Vec<Tree>) {
        (self.0, (self.1, (self.2, self.3))).apply(q, f);
    }
}
impl<A, B, C, D, E> Descriptor for (A, B, C, D, E)
where
    (A, (B, (C, (D, E)))): Descriptor,
{
    type Kind = <(A, (B, (C, (D, E)))) as Descriptor>::Kind;
}
impl<M: Model, A, B, C, D, E> Apply<M> for (A, B, C, D, E)
where
    (A, (B, (C, (D, E)))): Apply<M>,
{
    type Output = <(A, (B, (C, (D, E)))) as Apply<M>>::Output;
    fn apply(self, q: &mut SelectionPlan, f: &mut Vec<Tree>) {
        (self.0, (self.1, (self.2, (self.3, self.4)))).apply(q, f);
    }
}
impl<A, B, C, D, E, F> Descriptor for (A, B, C, D, E, F)
where
    (A, (B, (C, (D, (E, F))))): Descriptor,
{
    type Kind = <(A, (B, (C, (D, (E, F))))) as Descriptor>::Kind;
}
impl<M: Model, A, B, C, D, E, F> Apply<M> for (A, B, C, D, E, F)
where
    (A, (B, (C, (D, (E, F))))): Apply<M>,
{
    type Output = <(A, (B, (C, (D, (E, F))))) as Apply<M>>::Output;
    fn apply(self, q: &mut SelectionPlan, f: &mut Vec<Tree>) {
        (self.0, (self.1, (self.2, (self.3, (self.4, self.5))))).apply(q, f);
    }
}
impl<A, B, C, D, E, F, G> Descriptor for (A, B, C, D, E, F, G)
where
    (A, (B, (C, (D, (E, (F, G)))))): Descriptor,
{
    type Kind = <(A, (B, (C, (D, (E, (F, G)))))) as Descriptor>::Kind;
}
impl<M: Model, A, B, C, D, E, F, G> Apply<M> for (A, B, C, D, E, F, G)
where
    (A, (B, (C, (D, (E, (F, G)))))): Apply<M>,
{
    type Output = <(A, (B, (C, (D, (E, (F, G)))))) as Apply<M>>::Output;
    fn apply(self, q: &mut SelectionPlan, f: &mut Vec<Tree>) {
        (
            self.0,
            (self.1, (self.2, (self.3, (self.4, (self.5, self.6))))),
        )
            .apply(q, f);
    }
}
impl<A, B, C, D, E, F, G, H> Descriptor for (A, B, C, D, E, F, G, H)
where
    (A, (B, (C, (D, (E, (F, (G, H))))))): Descriptor,
{
    type Kind = <(A, (B, (C, (D, (E, (F, (G, H))))))) as Descriptor>::Kind;
}
impl<M: Model, A, B, C, D, E, F, G, H> Apply<M> for (A, B, C, D, E, F, G, H)
where
    (A, (B, (C, (D, (E, (F, (G, H))))))): Apply<M>,
{
    type Output = <(A, (B, (C, (D, (E, (F, (G, H))))))) as Apply<M>>::Output;
    fn apply(self, q: &mut SelectionPlan, f: &mut Vec<Tree>) {
        (
            self.0,
            (
                self.1,
                (self.2, (self.3, (self.4, (self.5, (self.6, self.7))))),
            ),
        )
            .apply(q, f);
    }
}

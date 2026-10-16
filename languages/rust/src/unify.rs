use crate::*;
pub trait Unify<Other: Model>: Model {
    type Output: Model<Schema = Self::Schema, Source = Self::Source>;
}
pub trait UnionField<T: Codec, Other: State<T>>: State<T> {
    type Output: State<T> + 'static;
}
impl<T: Codec + Send + Sync, O: State<T>> UnionField<T, O> for Loaded {
    type Output = Loaded;
}
impl<T: Codec + Send + Sync> UnionField<T, Loaded> for Unloaded {
    type Output = Loaded;
}
impl<T: Codec> UnionField<T, Unloaded> for Unloaded {
    type Output = Unloaded;
}

pub trait UnionRelation<T, C, Other: RelationState<T, C>>: RelationState<T, C> {
    type Output: RelationState<T, C> + 'static;
}
impl<T, C> UnionRelation<T, C, NotCached> for NotCached {
    type Output = NotCached;
}
impl<T, C, P: 'static> UnionRelation<T, C, Cached<P>> for NotCached
where
    Cached<P>: RelationState<T, C>,
{
    type Output = Cached<P>;
}
impl<T, C, P: 'static> UnionRelation<T, C, NotCached> for Cached<P>
where
    Self: RelationState<T, C>,
{
    type Output = Self;
}
impl<T, C, P, Q> UnionRelation<T, C, Cached<Q>> for Cached<P>
where
    P: Model + Unify<Q>,
    Q: Model,
    Cached<P>: RelationState<T, C>,
    Cached<Q>: RelationState<T, C>,
    Cached<<P as Unify<Q>>::Output>: RelationState<T, C>,
{
    type Output = Cached<<P as Unify<Q>>::Output>;
}

impl<T, C, P: 'static> UnionRelation<T, C, DefaultCached<P>> for NotCached
where
    DefaultCached<P>: RelationState<T, C>,
{
    type Output = DefaultCached<P>;
}
impl<T, C, P: 'static> UnionRelation<T, C, NotCached> for DefaultCached<P>
where
    Self: RelationState<T, C>,
{
    type Output = Self;
}
impl<T, C, P: 'static> UnionRelation<T, C, BareCached<P>> for NotCached
where
    BareCached<P>: RelationState<T, C>,
{
    type Output = BareCached<P>;
}
impl<T, C, P: 'static> UnionRelation<T, C, NotCached> for BareCached<P>
where
    Self: RelationState<T, C>,
{
    type Output = Self;
}
macro_rules! cache_union {
    ($left:ident,$right:ident,$output:ident) => {
        impl<T, C, P, Q> UnionRelation<T, C, $right<Q>> for $left<P>
        where
            P: Model + Unify<Q>,
            Q: Model,
            $left<P>: RelationState<T, C>,
            $right<Q>: RelationState<T, C>,
            $output<<P as Unify<Q>>::Output>: RelationState<T, C>,
        {
            type Output = $output<<P as Unify<Q>>::Output>;
        }
    };
}
cache_union!(DefaultCached, Cached, Cached);
cache_union!(Cached, DefaultCached, Cached);
cache_union!(DefaultCached, DefaultCached, DefaultCached);
cache_union!(BareCached, BareCached, BareCached);
cache_union!(BareCached, Cached, Cached);
cache_union!(Cached, BareCached, BareCached);
cache_union!(DefaultCached, BareCached, BareCached);
cache_union!(BareCached, DefaultCached, BareCached);

pub trait TargetCache<T, C, P: Model> {
    type State: RelationState<T, C> + 'static;
}
impl<T, C, P: Model> TargetCache<T, C, P> for NormalTarget
where
    BareCached<P>: RelationState<T, C>,
{
    type State = BareCached<P>;
}
impl<T, C, P: Model> TargetCache<T, C, P> for SelectedTarget
where
    Cached<P>: RelationState<T, C>,
{
    type State = Cached<P>;
}
pub trait MergeRequested<T, C, P: Model, B = SelectedTarget>: RelationState<T, C> {
    type Output: RelationState<T, C> + 'static;
}
impl<T, C, P: Model, B: TargetCache<T, C, P>> MergeRequested<T, C, P, B> for NotCached {
    type Output = B::State;
}
impl<T, C, Old, P: Model, B: TargetCache<T, C, P>> MergeRequested<T, C, P, B> for DefaultCached<Old>
where
    Self: RelationState<T, C>,
{
    type Output = B::State;
}
macro_rules! merge_cached {
    ($cache:ident) => {
        impl<T, C, Old, P, B> MergeRequested<T, C, P, B> for $cache<Old>
        where
            Old: Model + Unify<P>,
            P: Model,
            Self: RelationState<T, C>,
            B: TargetCache<T, C, <Old as Unify<P>>::Output>,
        {
            type Output = B::State;
        }
    };
}
merge_cached!(Cached);
merge_cached!(BareCached);

pub trait BypassShape: Model {
    type Output: Model<Schema = Self::Schema, Source = Self::Source>;
}
pub trait BypassRelation<T, C>: RelationState<T, C> {
    type Output: RelationState<T, C> + 'static;
}
impl<T, C> BypassRelation<T, C> for NotCached {
    type Output = Self;
}
impl<T, C, P> BypassRelation<T, C> for DefaultCached<P>
where
    Self: RelationState<T, C>,
{
    type Output = NotCached;
}
impl<T, C, P: Model + BypassShape> BypassRelation<T, C> for Cached<P>
where
    Self: RelationState<T, C>,
    Cached<P::Output>: RelationState<T, C>,
{
    type Output = Cached<P::Output>;
}
impl<T, C, P: ResetScalars> BypassRelation<T, C> for BareCached<P>
where
    P::Full: BypassShape,
    Self: RelationState<T, C>,
    BareCached<<P::Full as BypassShape>::Output>: RelationState<T, C>,
{
    type Output = BareCached<<P::Full as BypassShape>::Output>;
}

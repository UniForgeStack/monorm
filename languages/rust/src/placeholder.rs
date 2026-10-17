//! Typed placeholders for prepared templates. A placeholder names a slot; the template
//! carries the slots it uses as a type-level list, and `bind` needs a pack that has all of them.
use crate::*;
use std::any::TypeId;

pub struct Nil;
pub struct Cons<S, T>(PhantomData<(S, T)>);
/// Concatenates two slot lists. Repeated slots stay in the list: a pack that has the slot once serves every use.
pub trait Join<B> {
    type Out;
}
impl<B> Join<B> for Nil {
    type Out = B;
}
impl<S, T: Join<B>, B> Join<B> for Cons<S, T> {
    type Out = Cons<S, T::Out>;
}

/// A named parameter with one value type. Declare slots with `params!`.
pub trait Slot: 'static {
    const NAME: &'static str;
    type Value: Codec + 'static;
}
pub struct Param<S: Slot>(PhantomData<S>);
impl<S: Slot> Param<S> {
    #[doc(hidden)]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}
impl<S: Slot> Clone for Param<S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S: Slot> Copy for Param<S> {}
impl<S: Slot> Default for Param<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// A pack that has a value for slot `S`.
pub trait Has<S: Slot> {}
/// The values a pack carries: slot name, value type and encoded value.
pub trait Pack {
    fn values(&self) -> Vec<(&'static str, TypeId, sea_query::Value)>;
}
/// A pack that has every slot in the list `K`.
pub trait Provides<K> {}
impl<A> Provides<Nil> for A {}
impl<A: Has<S> + Provides<T>, S: Slot, T> Provides<Cons<S, T>> for A {}

#[derive(Clone, Copy, Debug)]
pub struct SlotRef {
    pub(crate) name: &'static str,
    pub(crate) ty: TypeId,
}
impl SlotRef {
    pub(crate) fn of<S: Slot>() -> Self {
        Self {
            name: S::NAME,
            ty: TypeId::of::<S::Value>(),
        }
    }
}

/// Replaces every placeholder with its bound value. A slot used with two value types, a
/// missing slot, a value of another type, or a NULL value is an error.
pub(crate) fn substitute(
    trees: &mut [Tree],
    pack: &[(&'static str, TypeId, sea_query::Value)],
) -> Result<()> {
    for tree in trees {
        match tree {
            Tree::Param(l, op, slot) => {
                let SlotRef { name, ty } = *slot;
                let mut found = pack.iter().filter(|(n, _, _)| *n == name);
                let (_, have, value) = found.next().ok_or_else(|| {
                    Error::Compatibility(format!("missing value for parameter {name}"))
                })?;
                if found.next().is_some() || *have != ty {
                    return Err(Error::Compatibility(format!(
                        "conflicting types for parameter {name}"
                    )));
                }
                if *value == value.as_null() {
                    return Err(Error::Engine(orm_engine::Error::value(format!(
                        "parameter {name} can't be None"
                    ))));
                }
                *tree = Tree::Compare(l.clone(), *op, value.clone());
            }
            Tree::And(items) | Tree::Or(items) | Tree::Nested(_, items) => substitute(items, pack)?,
            Tree::Not(item) => substitute(std::slice::from_mut(&mut **item), pack)?,
            _ => {}
        }
    }
    Ok(())
}

/// Two uses of one slot name must agree on the value type, whatever the pack holds.
pub(crate) fn check_slots(trees: &[Tree], seen: &mut Vec<SlotRef>) -> Result<()> {
    for tree in trees {
        match tree {
            Tree::Param(_, _, slot) => {
                if seen.iter().any(|s| s.name == slot.name && s.ty != slot.ty) {
                    return Err(Error::Compatibility(format!(
                        "conflicting types for parameter {}",
                        slot.name
                    )));
                }
                seen.push(*slot);
            }
            Tree::And(items) | Tree::Or(items) | Tree::Nested(_, items) => {
                check_slots(items, seen)?
            }
            Tree::Not(item) => check_slots(std::slice::from_ref(&**item), seen)?,
            _ => {}
        }
    }
    Ok(())
}

/// Declares the placeholders of a template and the pack that binds them:
/// `params! { pub mod range { min: i64, max: i64 } }` gives `range::min()`, `range::max()`
/// and `range::Args { min, max }`.
#[macro_export]
macro_rules! params {
    ($vis:vis mod $module:ident { $($name:ident : $ty:ty),* $(,)? }) => {
        #[allow(non_camel_case_types, dead_code)]
        $vis mod $module {
            use super::*;
            $(
                pub struct $name {}
                impl $crate::Slot for $name {
                    const NAME: &'static str = stringify!($name);
                    type Value = $ty;
                }
                pub fn $name() -> $crate::Param<$name> {
                    $crate::Param::new()
                }
            )*
            pub struct Args {
                $(pub $name: $ty,)*
            }
            $(impl $crate::Has<$name> for Args {})*
            impl $crate::Pack for Args {
                fn values(&self) -> Vec<(&'static str, ::std::any::TypeId, $crate::BindValue)> {
                    vec![$((
                        stringify!($name),
                        ::std::any::TypeId::of::<$ty>(),
                        $crate::Codec::encode(::std::clone::Clone::clone(&self.$name)),
                    )),*]
                }
            }
        }
    };
}

//! Per-thread, lazily initialised values that spend no pthread key of their own: the
//! [`thread_slot!`](crate::thread_slot) macro and the [`ThreadSlot`] it declares.
//!
//! On Android every `thread_local!` static takes one of the process's 128 pthread keys, and an
//! app that runs out aborts (limitation `android-thread-local-key-limit`). A [`ThreadSlot`] is an
//! ordinary static holding two function pointers; every slot in the process shares ONE per-thread
//! map ([`SlotMap`]), which lives in the facade's single per-thread struct in `back_glue`, so
//! declaring a slot costs no key at all.
//!
//! # The map and its borrow
//!
//! The map is keyed by the `TypeId` of a zero-sized marker type the macro declares for each item,
//! and holds each value behind an `Rc`. No borrow of the map is held while user code runs: an
//! access looks the entry up and clones the `Rc`, and a miss releases the borrow before running the
//! init expression, then inserts only if the entry is still absent. So an init expression, an
//! accessor's closure or a value's `Drop` may use any slot, the same one included, without a
//! `RefCell` panic.
//!
//! # Teardown
//!
//! The map is dropped with the rest of the facade's per-thread state when the thread exits, which
//! drops every value in it. A `Drop` that uses a slot then reaches a destroyed map: `with` and
//! `get` run the init expression and hand over the fresh value without storing it, `set` drops the
//! value it was given, and `try_with` returns `None`.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::{DefaultHasher, Entry};
use std::hash::BuildHasherDefault;
use std::rc::Rc;

use crate::back_glue::with_slot_map;

/// The per-thread store behind every [`ThreadSlot`]: one entry per slot used on this thread.
///
/// A fixed-key hasher rather than `RandomState`, so the map can be built in a `const` context
/// (the per-thread struct it lives in is `const`-initialised); its keys are `TypeId`s, never
/// attacker-chosen input.
pub(crate) struct SlotMap {
    entries: RefCell<HashMap<TypeId, Rc<dyn Any>, BuildHasherDefault<DefaultHasher>>>,
}

impl SlotMap {
    pub(crate) const fn new() -> Self {
        Self {
            entries: RefCell::new(HashMap::with_hasher(BuildHasherDefault::new())),
        }
    }

    /// The value stored under `key`, running `init` (with no borrow held) on a miss.
    fn get_or_init(&self, key: TypeId, init: impl FnOnce() -> Rc<dyn Any>) -> Rc<dyn Any> {
        let found = self.entries.borrow().get(&key).cloned();
        if let Some(found) = found {
            return found;
        }
        let fresh = init();
        let (value, spare) = match self.entries.borrow_mut().entry(key) {
            // The init expression stored a value of its own under this key: keep that one.
            Entry::Occupied(occupied) => (Rc::clone(occupied.get()), Some(fresh)),
            Entry::Vacant(vacant) => (Rc::clone(vacant.insert(fresh)), None),
        };
        drop(spare);
        value
    }

    /// Store `value` under `key`, handing back the value it replaced so the caller drops it with
    /// no borrow held.
    fn replace(&self, key: TypeId, value: Rc<dyn Any>) -> Option<Rc<dyn Any>> {
        self.entries.borrow_mut().insert(key, value)
    }
}

/// A per-thread, lazily initialised value that costs no pthread key of its own, declared with
/// [`thread_slot!`](crate::thread_slot).
///
/// It is for per-thread state in app or plugin code: on Android every `thread_local!` static
/// takes one of the process's 128 pthread keys and running out aborts the app (limitation
/// `android-thread-local-key-limit`), while every `ThreadSlot` in the process shares one per-thread
/// map, so a slot spends none. Migrating a `thread_local!` block is a one-word change.
///
/// ```
/// use frust::{GetUntracked, RwSignal, Set};
///
/// frust::thread_slot! {
///     /// How many times the refresh button was pressed on this thread.
///     static REFRESHES: RwSignal<u32> = RwSignal::new(0);
/// }
///
/// let refreshes = REFRESHES.get();
/// refreshes.set(refreshes.get_untracked() + 1);
/// assert_eq!(REFRESHES.get().get_untracked(), 1);
/// ```
///
/// # Cost
///
/// Every access is one hash lookup in the shared map plus an `Rc` clone, so a slot does not
/// belong on a per-frame hot path: take the value once with [`get`](Self::get) (an
/// [`RwSignal`](crate::RwSignal) is `Copy`) and hold it.
///
/// # Lifetime
///
/// Values are per thread: another thread's first access runs the init expression again. A value
/// is dropped when its thread exits; a `Drop` that uses a slot then gets the fallbacks described
/// on each method ([`try_with`](Self::try_with) is the one that can tell).
pub struct ThreadSlot<T: 'static> {
    key: fn() -> TypeId,
    init: fn() -> T,
}

impl<T: 'static> ThreadSlot<T> {
    /// The constructor [`thread_slot!`](crate::thread_slot) expands to. `key` must return the
    /// `TypeId` of a type no other slot uses: the macro declares one marker type per item.
    #[doc(hidden)]
    pub const fn new(key: fn() -> TypeId, init: fn() -> T) -> Self {
        Self { key, init }
    }

    /// This thread's value, initialised on first use; `None` once the thread's teardown has
    /// destroyed the slot map.
    fn stored(&self) -> Option<Rc<T>> {
        let key = (self.key)();
        let value = with_slot_map(|map| {
            map.get_or_init(key, || -> Rc<dyn Any> { Rc::new((self.init)()) })
        })?;
        // Only this slot's marker keys the entry and it only ever stores a `T`.
        Some(
            value
                .downcast::<T>()
                .expect("a ThreadSlot's marker keys only its own value type"),
        )
    }

    /// Run `f` on this thread's value, initialising it with the init expression on first use.
    ///
    /// `f` runs with no borrow of the shared map held, so it may use any slot, this one included.
    /// During thread teardown, once the map is gone, `f` gets a fresh value from the init
    /// expression that is dropped afterwards rather than stored.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        match self.stored() {
            Some(value) => f(&value),
            None => f(&(self.init)()),
        }
    }

    /// A clone of this thread's value, initialising it on first use (see [`with`](Self::with),
    /// including its teardown fallback).
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.with(T::clone)
    }

    /// Replace this thread's value with `value`, whether or not it was initialised. The old value
    /// is dropped after the shared map's borrow is released. During thread teardown, once the map
    /// is gone, `value` is dropped instead.
    pub fn set(&self, value: T) {
        let key = (self.key)();
        let value: Rc<dyn Any> = Rc::new(value);
        let replaced = with_slot_map(|map| map.replace(key, value));
        drop(replaced);
    }

    /// Like [`with`](Self::with), but `None` once the thread's teardown has destroyed the shared
    /// map, rather than running `f` on an unstored fresh value.
    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        self.stored().map(|value| f(&value))
    }
}

/// Declares per-thread, lazily initialised [`ThreadSlot`] statics, with the same syntax as
/// `thread_local!`, that spend no pthread key of their own.
///
/// For per-thread state in app or plugin code on Android, where each `thread_local!` static
/// takes one of the process's 128 pthread keys (limitation `android-thread-local-key-limit`):
/// every slot shares one per-thread map instead. Attributes and visibility pass through, and one
/// invocation may declare several items:
///
/// ```
/// use frust::{GetUntracked, RwSignal};
///
/// frust::thread_slot! {
///     /// The page's counter.
///     static COUNT: RwSignal<u32> = RwSignal::new(0);
///     pub(crate) static NAME: String = String::from("inbox");
/// }
///
/// assert_eq!(COUNT.get().get_untracked(), 0);
/// assert_eq!(NAME.with(|name| name.len()), 5);
/// ```
///
/// Each item expands to a plain `static NAME: frust::ThreadSlot<T>` (an ordinary static, not a
/// thread-local) plus a zero-sized marker type, private to the item, whose `TypeId` keys its entry
/// in the map. Each access costs one hash lookup, so take a value once with
/// [`ThreadSlot::get`] and hold it rather than reading a slot on a per-frame hot path. Values are
/// per thread and dropped when the thread exits; see [`ThreadSlot`] for what an access during
/// that teardown does.
#[macro_export]
macro_rules! thread_slot {
    () => {};
    ($(#[$attr:meta])* $vis:vis static $name:ident : $t:ty = $init:expr; $($rest:tt)*) => {
        $(#[$attr])*
        $vis static $name: $crate::ThreadSlot<$t> = {
            fn __key() -> ::core::any::TypeId {
                struct __Marker;
                ::core::any::TypeId::of::<__Marker>()
            }
            fn __init() -> $t {
                $init
            }
            $crate::ThreadSlot::new(__key, __init)
        };
        $crate::thread_slot!($($rest)*);
    };
    ($(#[$attr:meta])* $vis:vis static $name:ident : $t:ty = $init:expr) => {
        $crate::thread_slot!($(#[$attr])* $vis static $name: $t = $init;);
    };
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU32, Ordering};

    crate::thread_slot! {
        static FIRST: u32 = 1;
        static SECOND: u32 = 2;
        static DEPENDENT: u32 = FIRST.get() + 10;
        static TEXT: String = String::from("fresh");
        static COUNTER: Cell<u32> = Cell::new(0);
        static READ_ON_DROP: Option<ReadsOnDrop> = None;
    }

    #[test]
    fn two_slots_of_one_type_are_independent() {
        FIRST.set(7);
        assert_eq!(FIRST.get(), 7);
        assert_eq!(SECOND.get(), 2);
    }

    #[test]
    fn an_init_expression_may_read_another_slot() {
        assert_eq!(DEPENDENT.get(), FIRST.get() + 10);
    }

    #[test]
    fn with_may_reenter_the_same_slot() {
        COUNTER.with(|outer| {
            outer.set(outer.get() + 1);
            COUNTER.with(|inner| inner.set(inner.get() + 1));
            COUNTER.set(Cell::new(40));
        });
        assert_eq!(COUNTER.with(Cell::get), 40);
    }

    #[test]
    fn set_replaces_and_get_sees_it() {
        assert_eq!(TEXT.get(), "fresh");
        TEXT.set(String::from("stored"));
        assert_eq!(TEXT.get(), "stored");
        assert_eq!(TEXT.try_with(String::len), Some(6));
    }

    #[test]
    fn values_are_per_thread() {
        TEXT.set(String::from("main"));
        let other = std::thread::spawn(|| TEXT.get())
            .join()
            .expect("thread joins");
        assert_eq!(other, "fresh");
        assert_eq!(TEXT.get(), "main");
    }

    /// What the last [`ReadsOnDrop`] saw: `SECOND`'s value through `with`, plus one when
    /// `try_with` found the map already gone.
    static DROP_READ: AtomicU32 = AtomicU32::new(0);

    /// A value whose `Drop` reads another slot, as app state tearing down might.
    struct ReadsOnDrop;

    impl Drop for ReadsOnDrop {
        fn drop(&mut self) {
            let value = SECOND.with(|second| *second);
            let gone = u32::from(SECOND.try_with(|_| ()).is_none());
            DROP_READ.store(value * 10 + gone, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_thread_whose_slot_values_read_slots_on_drop_exits_cleanly() {
        std::thread::spawn(|| {
            FIRST.set(5);
            SECOND.set(9);
            TEXT.set(String::from("spawned"));
            READ_ON_DROP.set(Some(ReadsOnDrop));
            assert_eq!(FIRST.get() + SECOND.get(), 14);
        })
        .join()
        .expect("a thread tearing its slots down does not panic");
        // The drop ran during teardown, once the map was already destroyed: `with` got a fresh
        // 2 rather than the stored 9, and `try_with` reported the map gone.
        assert_eq!(DROP_READ.load(Ordering::SeqCst), 21);
    }
}

//! [`Samples`]: a shared, fixed-length set of current values (per-core load, meter values),
//! and [`SampleSlot`], a reference to one of them.
//!
//! Writes follow [`Series`](crate::Series)' rule: a content change bumps the revision and
//! requests one repaint, never a rebuild.

use std::fmt;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// A cheap-to-clone, `Send + Sync` handle to a fixed-length set of current values.
///
/// Each value is an `Option<f32>`: `None` is "no value" (a gap), and a non-finite input is
/// stored as a gap. Clones share one set. A call that changes at least one value bumps
/// [`revision`](Self::revision) once and calls [`frust::request_repaint`] once; a call that
/// writes the values already held changes nothing and requests nothing. Reads take a short
/// read lock, and a poisoned lock stays usable (the set is always left consistent).
#[derive(Clone)]
pub struct Samples {
    inner: Arc<RwLock<Values>>,
}

struct Values {
    values: Box<[Option<f32>]>,
    revision: u64,
}

/// A non-finite value is a gap.
fn sanitize(value: Option<f32>) -> Option<f32> {
    value.filter(|v| v.is_finite())
}

impl Samples {
    /// `len` values, every one a gap.
    pub fn new(len: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Values {
                values: vec![None; len].into_boxed_slice(),
                revision: 0,
            })),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Values> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Values> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `mutate`; when it reports a change, bump the revision and request one repaint
    /// (after the lock is released).
    fn mutate(&self, mutate: impl FnOnce(&mut [Option<f32>]) -> bool) {
        let changed = {
            let mut set = self.write();
            let changed = mutate(&mut set.values);
            if changed {
                set.revision = set.revision.wrapping_add(1);
            }
            changed
        };
        if changed {
            frust::request_repaint();
        }
    }

    /// How many values the set holds; fixed at creation.
    pub fn len(&self) -> usize {
        self.read().values.len()
    }

    /// Whether the set holds no values (`Samples::new(0)`).
    pub fn is_empty(&self) -> bool {
        self.read().values.is_empty()
    }

    /// Set the value at `index` (a non-finite value is stored as a gap).
    ///
    /// An out-of-range `index` is ignored in release builds and fails a debug assertion.
    pub fn set(&self, index: usize, value: f32) {
        self.set_slot(index, Some(value));
    }

    /// Make the value at `index` a gap; an out-of-range `index` is treated as in
    /// [`set`](Self::set).
    pub fn set_gap(&self, index: usize) {
        self.set_slot(index, None);
    }

    fn set_slot(&self, index: usize, value: Option<f32>) {
        debug_assert!(
            index < self.len(),
            "Samples: index {index} out of range for {} values",
            self.len()
        );
        let value = sanitize(value);
        self.mutate(|values| match values.get_mut(index) {
            Some(slot) if *slot != value => {
                *slot = value;
                true
            }
            _ => false,
        });
    }

    /// Set every value from index 0 in order, in one revision bump and one repaint request.
    ///
    /// Items past [`len`](Self::len) are ignored; indices `values` does not reach become gaps.
    pub fn set_all(&self, values: impl IntoIterator<Item = Option<f32>>) {
        let mut incoming = values.into_iter();
        self.mutate(|slots| {
            let mut changed = false;
            for slot in slots.iter_mut() {
                let value = sanitize(incoming.next().flatten());
                if *slot != value {
                    *slot = value;
                    changed = true;
                }
            }
            changed
        });
    }

    /// The value at `index`: `None` for a gap or an out-of-range index.
    pub fn get(&self, index: usize) -> Option<f32> {
        self.read().values.get(index).copied().flatten()
    }

    /// A counter that moves on every content change: compare it to skip work.
    pub fn revision(&self) -> u64 {
        self.read().revision
    }

    /// Read every value in index order without copying, under a short read lock.
    ///
    /// Keep `f` brief (a paint): a writer on another thread waits for it to return, and a
    /// write to this same set from inside `f` deadlocks.
    pub fn with_values<R>(&self, f: impl FnOnce(&[Option<f32>]) -> R) -> R {
        let set = self.read();
        f(&set.values)
    }

    /// Whether `self` and `other` share one set (a clone of the same handle).
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// A clonable reference to the value at `index`, for a widget that shows one value.
    ///
    /// # Panics
    ///
    /// If `index` is not below [`len`](Self::len).
    pub fn slot(&self, index: usize) -> SampleSlot {
        let len = self.len();
        assert!(
            index < len,
            "Samples::slot: index {index} out of range for {len} values"
        );
        SampleSlot {
            samples: self.clone(),
            index,
        }
    }
}

impl fmt::Debug for Samples {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let set = self.read();
        f.debug_struct("Samples")
            .field("values", &set.values)
            .field("revision", &set.revision)
            .finish()
    }
}

/// One index of a [`Samples`] set, from [`Samples::slot`]: reads and writes go to the shared
/// set, so a meter built over a slot repaints when that set is written.
#[derive(Clone, Debug)]
pub struct SampleSlot {
    samples: Samples,
    index: usize,
}

impl SampleSlot {
    /// The current value: `None` for a gap.
    pub fn get(&self) -> Option<f32> {
        self.samples.get(self.index)
    }

    /// Set the value (a non-finite value is stored as a gap).
    pub fn set(&self, value: f32) {
        self.samples.set(self.index, value);
    }

    /// Make the value a gap.
    pub fn set_gap(&self) {
        self.samples.set_gap(self.index);
    }

    /// The index this slot refers to.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The set this slot belongs to.
    pub fn samples(&self) -> &Samples {
        &self.samples
    }

    /// The owning set's [`revision`](Samples::revision).
    pub fn revision(&self) -> u64 {
        self.samples.revision()
    }
}

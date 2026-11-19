//! [`Series`]: a shared, fixed-capacity ring of evenly spaced samples, newest last.
//!
//! A write bumps the handle's revision and asks every live root to repaint
//! ([`frust::request_repaint`]); it never rebuilds or lays out, and needs no frame loop.

use std::fmt;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// A cheap-to-clone, `Send + Sync` handle to a fixed-capacity ring of evenly spaced slots.
///
/// The oldest slot is dropped when a push would exceed the capacity. A slot is an
/// `Option<f32>`: `None` is "no sample" (a gap), and a non-finite input is stored as a gap.
/// Clones share one ring, so a background thread can push while a chart paints from it.
///
/// Every call that changes the content bumps [`revision`](Self::revision) once and calls
/// [`frust::request_repaint`] once, however many slots it wrote. Reads take a short read lock;
/// a paint holds it only for the [`with_slots`](Self::with_slots) closure. A lock poisoned by a
/// panicking writer stays readable and writable: the ring is always left consistent.
#[derive(Clone)]
pub struct Series {
    inner: Arc<RwLock<Ring>>,
}

/// The ring itself: `slots[head]` is the oldest of `len` live slots.
struct Ring {
    slots: Box<[Option<f32>]>,
    head: usize,
    len: usize,
    revision: u64,
}

impl Ring {
    fn new(capacity: usize) -> Self {
        Self {
            slots: vec![None; capacity].into_boxed_slice(),
            head: 0,
            len: 0,
            revision: 0,
        }
    }

    fn push(&mut self, slot: Option<f32>) {
        let cap = self.slots.len();
        if self.len < cap {
            self.slots[(self.head + self.len) % cap] = slot;
            self.len += 1;
        } else {
            self.slots[self.head] = slot;
            self.head = (self.head + 1) % cap;
        }
    }

    fn view(&self) -> SlotsView<'_> {
        let cap = self.slots.len();
        let end = self.head + self.len;
        if end <= cap {
            SlotsView {
                first: &self.slots[self.head..end],
                second: &[],
            }
        } else {
            SlotsView {
                first: &self.slots[self.head..],
                second: &self.slots[..end - cap],
            }
        }
    }
}

/// A non-finite value is a gap.
fn sanitize(slot: Option<f32>) -> Option<f32> {
    slot.filter(|v| v.is_finite())
}

impl Series {
    /// An empty series holding at most `capacity` slots.
    ///
    /// # Panics
    ///
    /// If `capacity` is 0.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 1, "Series::new: capacity must be at least 1");
        Self {
            inner: Arc::new(RwLock::new(Ring::new(capacity))),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Ring> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Ring> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `mutate`; when it reports a change, bump the revision and request one repaint
    /// (after the lock is released).
    fn mutate(&self, mutate: impl FnOnce(&mut Ring) -> bool) {
        let changed = {
            let mut ring = self.write();
            let changed = mutate(&mut ring);
            if changed {
                ring.revision = ring.revision.wrapping_add(1);
            }
            changed
        };
        if changed {
            frust::request_repaint();
        }
    }

    /// The most slots this series holds.
    pub fn capacity(&self) -> usize {
        self.read().slots.len()
    }

    /// How many slots are filled (gaps included), at most [`capacity`](Self::capacity).
    pub fn len(&self) -> usize {
        self.read().len
    }

    /// Whether no slot has been pushed since creation or the last [`clear`](Self::clear).
    pub fn is_empty(&self) -> bool {
        self.read().len == 0
    }

    /// Append a sample (a non-finite value is stored as a gap), dropping the oldest when full.
    pub fn push(&self, value: f32) {
        self.push_slot(Some(value));
    }

    /// Append a gap: a slot with no sample.
    pub fn push_gap(&self) {
        self.push_slot(None);
    }

    /// Append one slot: `None` is a gap, a non-finite value is stored as a gap.
    pub fn push_slot(&self, slot: Option<f32>) {
        self.mutate(|ring| {
            ring.push(sanitize(slot));
            true
        });
    }

    /// Append every slot in order: one revision bump and one repaint request for the whole
    /// call, none when `slots` is empty.
    pub fn extend(&self, slots: impl IntoIterator<Item = Option<f32>>) {
        self.mutate(|ring| {
            let mut changed = false;
            for slot in slots {
                ring.push(sanitize(slot));
                changed = true;
            }
            changed
        });
    }

    /// Clear, then fill with `slots`, keeping the newest [`capacity`](Self::capacity) of them
    /// (loading a history range). A call that leaves the content unchanged is a no-op.
    pub fn replace(&self, slots: impl IntoIterator<Item = Option<f32>>) {
        let mut fresh = Ring::new(self.capacity());
        for slot in slots {
            fresh.push(sanitize(slot));
        }
        self.mutate(|ring| {
            if ring.view().iter().eq(fresh.view().iter()) {
                return false;
            }
            fresh.revision = ring.revision;
            *ring = fresh;
            true
        });
    }

    /// Remove every slot; a no-op on an empty series.
    pub fn clear(&self) {
        self.mutate(|ring| {
            let changed = ring.len > 0;
            ring.head = 0;
            ring.len = 0;
            changed
        });
    }

    /// The newest slot's value: `None` when the series is empty or the newest slot is a gap.
    pub fn latest(&self) -> Option<f32> {
        let ring = self.read();
        ring.view().iter().next_back().flatten()
    }

    /// The largest sample, ignoring gaps; `None` when there is none. Feeds
    /// [`YDomain::resolve`](crate::YDomain::resolve).
    pub fn max(&self) -> Option<f32> {
        let ring = self.read();
        ring.view().iter().flatten().reduce(f32::max)
    }

    /// A counter that moves on every content change: compare it to skip work.
    pub fn revision(&self) -> u64 {
        self.read().revision
    }

    /// Read the slots oldest to newest without copying, under a short read lock.
    ///
    /// Keep `f` brief (a paint): a writer on another thread waits for it to return, and a
    /// write to this same series from inside `f` deadlocks.
    pub fn with_slots<R>(&self, f: impl FnOnce(SlotsView<'_>) -> R) -> R {
        let ring = self.read();
        f(ring.view())
    }

    /// Whether `self` and `other` share one ring (a clone of the same handle).
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl fmt::Debug for Series {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ring = self.read();
        f.debug_struct("Series")
            .field("capacity", &ring.slots.len())
            .field("len", &ring.len)
            .field("revision", &ring.revision)
            .finish()
    }
}

/// Borrowed read access to a [`Series`]' slots, oldest to newest, from
/// [`Series::with_slots`].
///
/// The ring may wrap, so the slots are two contiguous runs; [`get`](Self::get) and
/// [`iter`](Self::iter) hide the seam.
#[derive(Clone, Copy, Debug)]
pub struct SlotsView<'a> {
    first: &'a [Option<f32>],
    second: &'a [Option<f32>],
}

impl<'a> SlotsView<'a> {
    /// How many slots there are, gaps included.
    pub fn len(&self) -> usize {
        self.first.len() + self.second.len()
    }

    /// Whether there are no slots.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The slot at `index` (0 is the oldest): `None` past the end, `Some(None)` for a gap.
    pub fn get(&self, index: usize) -> Option<Option<f32>> {
        match index.checked_sub(self.first.len()) {
            None => self.first.get(index).copied(),
            Some(rest) => self.second.get(rest).copied(),
        }
    }

    /// Every slot, oldest to newest.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = Option<f32>> + 'a {
        self.first.iter().chain(self.second.iter()).copied()
    }
}

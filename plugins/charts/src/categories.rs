//! [`Categories`]: a shared, ordered list of labelled rows of values (one bar group, pie
//! section or radar axis per row), and [`CategoryRow`], one such row.
//!
//! Writes follow [`Samples`](crate::Samples)' rule: a content change bumps the revision and
//! requests one repaint, never a rebuild. Label text is shaped in layout, so a widget also
//! watches [`Categories::label_revision`], which moves only when the labels change, and
//! requests a layout when it does.

use std::fmt;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// One labelled row of a [`Categories`] list: a label and its values, `None` a gap.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CategoryRow {
    /// The row's label (a bar group's category, a pie section's name, a radar axis' title).
    pub label: String,
    /// The row's values in column order; `None` is a gap. A row may hold any number of
    /// values, and rows need not hold the same number.
    pub values: Vec<Option<f32>>,
}

impl CategoryRow {
    /// A row labelled `label` holding `values`; a non-finite value is stored as a gap.
    pub fn new(label: impl Into<String>, values: impl IntoIterator<Item = f32>) -> Self {
        Self::with_gaps(label, values.into_iter().map(Some))
    }

    /// A row labelled `label` holding `values`, `None` a gap; a non-finite value is stored as
    /// a gap too.
    pub fn with_gaps(
        label: impl Into<String>,
        values: impl IntoIterator<Item = Option<f32>>,
    ) -> Self {
        Self {
            label: label.into(),
            values: values.into_iter().map(sanitize).collect(),
        }
    }

    /// The row with every non-finite value made a gap.
    fn sanitized(mut self) -> Self {
        for value in &mut self.values {
            *value = sanitize(*value);
        }
        self
    }
}

impl<S, I> From<(S, I)> for CategoryRow
where
    S: Into<String>,
    I: IntoIterator<Item = f32>,
{
    /// [`CategoryRow::new`] from a `(label, values)` pair.
    fn from((label, values): (S, I)) -> Self {
        Self::new(label, values)
    }
}

/// A cheap-to-clone, `Send + Sync` handle to an ordered list of labelled rows of values.
///
/// Each value is an `Option<f32>`: `None` is a gap, and a non-finite input is stored as a
/// gap. Clones share one list. A call that changes the list bumps
/// [`revision`](Self::revision) once and calls [`frust::request_repaint`] once; a call that
/// writes what the list already holds changes nothing and requests nothing. A call that also
/// changes the labels (a label's text, or the number of rows) bumps
/// [`label_revision`](Self::label_revision) once too: label text is shaped in layout, so a
/// widget that sees it move requests a layout, where a values-only change needs just the
/// repaint. Reads take a short read lock, and a poisoned lock stays usable (the list is always
/// left consistent).
///
/// ```
/// use frust_charts::Categories;
///
/// let sales = Categories::from_rows([("Q1", [12.0, 8.0]), ("Q2", [15.0, 9.5])]);
/// let labels = sales.label_revision();
/// sales.set_value(1, 0, 16.0);
/// assert_eq!(sales.value(1, 0), Some(16.0));
/// assert_eq!(sales.label_revision(), labels, "a value write leaves the labels alone");
/// assert_eq!(sales.value_range(), Some((8.0, 16.0)));
/// ```
#[derive(Clone)]
pub struct Categories {
    inner: Arc<RwLock<Table>>,
}

#[derive(Default)]
struct Table {
    rows: Vec<CategoryRow>,
    revision: u64,
    label_revision: u64,
}

/// What a mutation changed.
#[derive(Clone, Copy, Default)]
struct Change {
    content: bool,
    labels: bool,
}

/// A non-finite value is a gap.
fn sanitize(value: Option<f32>) -> Option<f32> {
    value.filter(|v| v.is_finite())
}

impl Categories {
    /// An empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// A list holding `rows`, in order (non-finite values become gaps).
    pub fn from_rows<R: Into<CategoryRow>>(rows: impl IntoIterator<Item = R>) -> Self {
        let rows = rows.into_iter().map(|r| r.into().sanitized()).collect();
        Self {
            inner: Arc::new(RwLock::new(Table {
                rows,
                revision: 0,
                label_revision: 0,
            })),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Table> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Table> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `mutate`; when it reports a change, bump the revisions it names and request one
    /// repaint (after the lock is released).
    fn mutate(&self, mutate: impl FnOnce(&mut Vec<CategoryRow>) -> Change) {
        let change = {
            let mut table = self.write();
            let change = mutate(&mut table.rows);
            if change.content || change.labels {
                table.revision = table.revision.wrapping_add(1);
            }
            if change.labels {
                table.label_revision = table.label_revision.wrapping_add(1);
            }
            change
        };
        if change.content || change.labels {
            frust::request_repaint();
        }
    }

    /// How many rows the list holds.
    pub fn len(&self) -> usize {
        self.read().rows.len()
    }

    /// Whether the list holds no rows.
    pub fn is_empty(&self) -> bool {
        self.read().rows.is_empty()
    }

    /// Replace every row with `rows`, in order, in one revision bump and one repaint request
    /// (non-finite values become gaps).
    pub fn set_rows<R: Into<CategoryRow>>(&self, rows: impl IntoIterator<Item = R>) {
        let incoming: Vec<CategoryRow> = rows.into_iter().map(|r| r.into().sanitized()).collect();
        self.mutate(|rows| {
            let labels = rows.len() != incoming.len()
                || rows.iter().zip(&incoming).any(|(a, b)| a.label != b.label);
            let content = labels
                || rows
                    .iter()
                    .zip(&incoming)
                    .any(|(a, b)| a.values != b.values);
            if content {
                *rows = incoming;
            }
            Change { content, labels }
        });
    }

    /// Set the value at column `col` of row `row` (a non-finite value is stored as a gap).
    ///
    /// An out-of-range `row` or `col` is ignored in release builds and fails a debug
    /// assertion.
    pub fn set_value(&self, row: usize, col: usize, value: f32) {
        self.set_cell(row, col, Some(value));
    }

    /// Make the value at column `col` of row `row` a gap; an out-of-range position is treated
    /// as in [`set_value`](Self::set_value).
    pub fn set_gap(&self, row: usize, col: usize) {
        self.set_cell(row, col, None);
    }

    fn set_cell(&self, row: usize, col: usize, value: Option<f32>) {
        debug_assert!(
            self.with_rows(|rows| rows.get(row).is_some_and(|r| col < r.values.len())),
            "Categories: cell ({row}, {col}) out of range"
        );
        let value = sanitize(value);
        self.mutate(|rows| {
            let slot = rows.get_mut(row).and_then(|r| r.values.get_mut(col));
            match slot {
                Some(slot) if *slot != value => {
                    *slot = value;
                    Change {
                        content: true,
                        labels: false,
                    }
                }
                _ => Change::default(),
            }
        });
    }

    /// Set the label of row `row`. A new text bumps both revisions; the same text changes
    /// nothing.
    ///
    /// An out-of-range `row` is ignored in release builds and fails a debug assertion.
    pub fn set_label(&self, row: usize, label: impl Into<String>) {
        debug_assert!(
            row < self.len(),
            "Categories: row {row} out of range for {} rows",
            self.len()
        );
        let label = label.into();
        self.mutate(|rows| match rows.get_mut(row) {
            Some(r) if r.label != label => {
                r.label = label;
                Change {
                    content: true,
                    labels: true,
                }
            }
            _ => Change::default(),
        });
    }

    /// The value at column `col` of row `row`: `None` for a gap or an out-of-range position.
    pub fn value(&self, row: usize, col: usize) -> Option<f32> {
        self.read()
            .rows
            .get(row)
            .and_then(|r| r.values.get(col).copied().flatten())
    }

    /// The label of row `row`, or `None` for an out-of-range row.
    pub fn label(&self, row: usize) -> Option<String> {
        self.read().rows.get(row).map(|r| r.label.clone())
    }

    /// The smallest and largest value over every row (gaps skipped), or `None` when there is
    /// none: the input [`YDomain::resolve_range`](crate::YDomain::resolve_range) takes.
    pub fn value_range(&self) -> Option<(f32, f32)> {
        self.read()
            .rows
            .iter()
            .flat_map(|r| r.values.iter().flatten().copied())
            .fold(None, |range, v| match range {
                None => Some((v, v)),
                Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
            })
    }

    /// A counter that moves on every content change, labels included: compare it to skip a
    /// repaint's work.
    pub fn revision(&self) -> u64 {
        self.read().revision
    }

    /// A counter that moves only when the labels change (a label's text, or the number of
    /// rows): compare it to know when a layout is needed to reshape them.
    pub fn label_revision(&self) -> u64 {
        self.read().label_revision
    }

    /// Read every row in order without copying, under a short read lock.
    ///
    /// Keep `f` brief (a paint): a writer on another thread waits for it to return, and a
    /// write to this same list from inside `f` deadlocks.
    pub fn with_rows<R>(&self, f: impl FnOnce(&[CategoryRow]) -> R) -> R {
        let table = self.read();
        f(&table.rows)
    }

    /// Whether `self` and `other` share one list (a clone of the same handle).
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Default for Categories {
    /// [`Categories::new`]: an empty list.
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Table::default())),
        }
    }
}

impl fmt::Debug for Categories {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let table = self.read();
        f.debug_struct("Categories")
            .field("rows", &table.rows)
            .field("revision", &table.revision)
            .field("label_revision", &table.label_revision)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::Categories;

    #[test]
    fn a_poisoned_lock_stays_usable() {
        let c = Categories::from_rows([("a", [1.0])]);
        let writer = c.clone();
        let result = std::thread::spawn(move || {
            let _guard = writer.inner.write().unwrap();
            panic!("poison the lock");
        })
        .join();
        assert!(result.is_err());
        assert!(c.inner.is_poisoned());
        c.set_value(0, 0, 2.0);
        c.set_label(0, "b");
        assert_eq!(
            (c.value(0, 0), c.label(0).as_deref()),
            (Some(2.0), Some("b"))
        );
        assert_eq!((c.revision(), c.label_revision()), (2, 1));
    }
}

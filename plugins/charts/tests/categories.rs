//! The `Categories` handle: labelled rows of values that repaint on write.
//!
//! Every write requests a process-wide repaint, so each test that writes or reads the repaint
//! generation holds `STATICS` for its whole run.

use std::sync::{Mutex, MutexGuard, PoisonError};

use frust_charts::{Categories, CategoryRow};
use frust_core::repaint_generation;

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How many repaint requests `f` made.
fn repaints(f: impl FnOnce()) -> u64 {
    let before = repaint_generation();
    f();
    repaint_generation() - before
}

fn snapshot(categories: &Categories) -> Vec<CategoryRow> {
    categories.with_rows(<[CategoryRow]>::to_vec)
}

#[test]
fn the_handle_is_send_sync_and_shared_by_clones() {
    fn check<T: Send + Sync + Clone>() {}
    check::<Categories>();

    let _g = hold();
    let a = Categories::new();
    assert!(a.is_empty());
    let b = a.clone();
    assert!(a.ptr_eq(&b));
    assert!(!a.ptr_eq(&Categories::new()));
    b.set_rows([("x", [1.0])]);
    assert_eq!(a.len(), 1);
    assert_eq!(a.value(0, 0), Some(1.0));
}

#[test]
fn rows_keep_order_labels_and_gaps() {
    let _g = hold();
    let c = Categories::from_rows([
        CategoryRow::new("apples", [3.0, f32::NAN]),
        CategoryRow::with_gaps("pears", [None, Some(2.5), Some(f32::INFINITY)]),
        ("plums", vec![]).into(),
    ]);
    assert_eq!(c.len(), 3);
    assert_eq!(
        snapshot(&c),
        [
            CategoryRow {
                label: "apples".into(),
                values: vec![Some(3.0), None],
            },
            CategoryRow {
                label: "pears".into(),
                values: vec![None, Some(2.5), None],
            },
            CategoryRow {
                label: "plums".into(),
                values: vec![],
            },
        ]
    );
    assert_eq!(c.label(1).as_deref(), Some("pears"));
    assert_eq!(c.label(3), None);
    assert_eq!(c.value(0, 0), Some(3.0));
    assert_eq!(c.value(0, 1), None);
    assert_eq!(c.value(9, 0), None);
    assert_eq!(c.value_range(), Some((2.5, 3.0)));
    assert_eq!(Categories::new().value_range(), None);

    // A row given directly with a non-finite value stores it as a gap.
    c.set_rows([CategoryRow {
        label: "raw".into(),
        values: vec![Some(f32::NAN), Some(-1.0)],
    }]);
    assert_eq!(snapshot(&c)[0].values, [None, Some(-1.0)]);
    assert_eq!(c.value_range(), Some((-1.0, -1.0)));
}

#[test]
fn each_changing_call_bumps_the_revision_and_repaints_once() {
    let _g = hold();
    let c = Categories::from_rows([("a", [1.0, 2.0]), ("b", [3.0, 4.0])]);
    let (rev, labels) = (c.revision(), c.label_revision());

    assert_eq!(repaints(|| c.set_value(0, 1, 5.0)), 1);
    assert_eq!(c.revision(), rev + 1);
    assert_eq!(
        c.label_revision(),
        labels,
        "a value write leaves the labels alone"
    );
    assert_eq!(repaints(|| c.set_gap(1, 0)), 1);
    assert_eq!(c.value(1, 0), None);
    assert_eq!(repaints(|| c.set_value(1, 0, f32::NAN)), 0, "already a gap");
    assert_eq!(
        repaints(|| c.set_rows([("a", [9.0, 9.0]), ("b", [9.0, 9.0])])),
        1
    );
    assert_eq!(c.revision(), rev + 3);
    assert_eq!(c.label_revision(), labels);

    // No-op writes change nothing and request nothing.
    assert_eq!(repaints(|| c.set_value(0, 0, 9.0)), 0);
    assert_eq!(repaints(|| c.set_label(1, "b")), 0);
    assert_eq!(
        repaints(|| c.set_rows([("a", [9.0, 9.0]), ("b", [9.0, 9.0])])),
        0
    );
    assert_eq!(c.revision(), rev + 3);
}

#[test]
fn label_changes_move_the_label_revision() {
    let _g = hold();
    let c = Categories::from_rows([("a", [1.0]), ("b", [2.0])]);
    let (rev, labels) = (c.revision(), c.label_revision());

    assert_eq!(repaints(|| c.set_label(0, "alpha")), 1);
    assert_eq!((c.revision(), c.label_revision()), (rev + 1, labels + 1));
    assert_eq!(c.label(0).as_deref(), Some("alpha"));

    // A renamed row through set_rows, even with the same values.
    assert_eq!(
        repaints(|| c.set_rows([("alpha", [1.0]), ("beta", [2.0])])),
        1
    );
    assert_eq!((c.revision(), c.label_revision()), (rev + 2, labels + 2));

    // A changed row count changes the labels.
    assert_eq!(repaints(|| c.set_rows([("alpha", [1.0])])), 1);
    assert_eq!((c.revision(), c.label_revision()), (rev + 3, labels + 3));
    assert_eq!(repaints(|| c.set_rows(Vec::<CategoryRow>::new())), 1);
    assert_eq!(c.label_revision(), labels + 4);
    assert!(c.is_empty());
}

#[test]
fn a_reader_that_panics_leaves_the_list_usable() {
    let _g = hold();
    let c = Categories::from_rows([("a", [1.0])]);
    let reader = c.clone();
    let result = std::thread::spawn(move || {
        reader.with_rows(|_| panic!("a reader panics"));
    })
    .join();
    assert!(result.is_err());
    c.set_value(0, 0, 2.0);
    assert_eq!(c.value(0, 0), Some(2.0));
    assert_eq!(c.len(), 1);
    assert!(format!("{c:?}").contains("Categories"));
}

#[test]
fn writes_from_another_thread_are_visible() {
    let _g = hold();
    let c = Categories::from_rows([("a", [0.0])]);
    let writer = c.clone();
    let before = repaint_generation();
    std::thread::spawn(move || writer.set_value(0, 0, 42.0))
        .join()
        .unwrap();
    assert_eq!(c.value(0, 0), Some(42.0));
    assert!(repaint_generation() > before);
}

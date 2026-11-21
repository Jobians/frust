//! Every per-thread table and revision counter this crate compiles on Android, as the fields of one
//! [`PluginLocals`] behind one `thread_local!` static: on Android each static takes one of 128
//! process-wide pthread keys, and a field costs per access what its own static did.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;

#[cfg(feature = "frust-api")]
use crate::component::Staged;
use crate::controls::{image, tab_bar};
use crate::registry::SlotId;
use crate::runtime::NativeRuntime;

/// One field per table; the owning modules keep the accessors. No outer cell wraps it, so nested
/// access to two fields behaves as nested access to two statics. A table whose value cannot be
/// built in a `const` sits in a [`OnceCell`] its accessor fills on first touch, as its static's
/// lazy initialiser did. Fields drop in declaration order, so the tables holding platform handles
/// and app values come first; a `Drop` of theirs that reaches back in finds [`try_with`] `None`.
pub(crate) struct PluginLocals {
    /// The main thread's runtime (`crate::runtime`'s *main-thread confinement*), seeded with the
    /// platform backend's own control kinds by `with_runtime` on first touch.
    pub(crate) runtime: OnceCell<RefCell<NativeRuntime>>,

    /// Per-slot staged component values and props (`crate::component`'s staging table).
    #[cfg(feature = "frust-api")]
    pub(crate) staged: OnceCell<RefCell<HashMap<SlotId, Staged>>>,

    /// The `Image` publish table (`crate::controls::image`'s module doc).
    pub(crate) image_bytes: OnceCell<RefCell<HashMap<SlotId, image::Published>>>,

    /// The `TabBar` icon-bytes side table (`crate::controls::tab_bar`).
    pub(crate) tab_icon_bytes: OnceCell<RefCell<HashMap<SlotId, tab_bar::Published>>>,

    /// The next `Image` publish revision; starts at `1`.
    pub(crate) image_next_rev: Cell<u64>,

    /// The next `TabBar` icon publish revision; starts at `1`.
    pub(crate) tab_icon_next_rev: Cell<u64>,
}

impl PluginLocals {
    /// Every field at the initial value its static had, the lazy tables not yet built.
    const fn new() -> Self {
        Self {
            runtime: OnceCell::new(),
            #[cfg(feature = "frust-api")]
            staged: OnceCell::new(),
            image_bytes: OnceCell::new(),
            tab_icon_bytes: OnceCell::new(),
            image_next_rev: Cell::new(1),
            tab_icon_next_rev: Cell::new(1),
        }
    }
}

thread_local! {
    static PLUGIN: PluginLocals = const { PluginLocals::new() };
}

/// Run `f` on this thread's state; panics once the thread's teardown has destroyed it, so a path
/// that can run then (a `Drop`, a teardown-time cleanup) uses [`try_with`].
pub(crate) fn with<R>(f: impl FnOnce(&PluginLocals) -> R) -> R {
    PLUGIN.with(f)
}

/// Run `f` on this thread's state, or return `None` once the thread's teardown has destroyed it.
pub(crate) fn try_with<R>(f: impl FnOnce(&PluginLocals) -> R) -> Option<R> {
    PLUGIN.try_with(f).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;
    use crate::controls::tab_bar::ItemIconBytes;

    /// A staged component whose drop reaches back into every table, as an app value may.
    #[cfg(feature = "frust-api")]
    struct Teardown;

    #[cfg(feature = "frust-api")]
    impl crate::component::NativeComponent for Teardown {
        type Props = ();
        type State = ();

        fn create(
            &self,
            _ctx: &mut crate::component::ComponentCtx<'_, '_, '_>,
            _props: &(),
        ) -> Option<(crate::component::NativeRoot, ())> {
            None
        }

        fn update(
            &self,
            _ctx: &mut crate::component::ComponentCtx<'_, '_, '_>,
            _state: &mut (),
            _old: &(),
            _new: &(),
        ) {
        }
    }

    #[cfg(feature = "frust-api")]
    impl Drop for Teardown {
        fn drop(&mut self) {
            let _ = crate::runtime::with_runtime(|runtime| runtime.live_count());
            image::retire(1);
            tab_bar::retire_icon_bytes(2);
        }
    }

    #[test]
    fn a_thread_exits_cleanly_with_every_table_holding_a_value() {
        thread::spawn(|| {
            assert_eq!(
                crate::runtime::with_runtime(|runtime| runtime.live_count()),
                Some(0),
                "the runtime seeds on first touch"
            );
            #[cfg(feature = "frust-api")]
            crate::component::publish(3, std::rc::Rc::new(Teardown), ());
            let bytes: Arc<[u8]> = Arc::from(&[1u8, 2, 3][..]);
            assert_eq!(image::publish_bytes(1, Arc::clone(&bytes)), 1);
            let icons = vec![ItemIconBytes {
                icon: Some(bytes),
                selected_icon: None,
            }];
            assert_eq!(tab_bar::publish_icon_bytes(2, icons), 1);
        })
        .join()
        .expect("the thread's state tears down without a panic");
    }
}

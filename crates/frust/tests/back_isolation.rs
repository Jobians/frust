//! Under `test-support`, a system Back press is observed only by the navigators on the thread
//! that pushed it, so tests running on separate threads do not pop each other's pages.
#![cfg(feature = "test-support")]

use std::sync::{Arc, Barrier};
use std::time::Duration;

use frust::testing::Tester;
use frust::{AnyView, NavigatorController, any, navigator, push_back_press, text};
use kurbo::Size;

const FRAME: Duration = Duration::from_millis(16);

fn page() -> AnyView<()> {
    any(text("x"))
}

/// A tester over a navigator that has been pushed one page deep.
fn two_deep() -> (NavigatorController<()>, Tester<(), impl frust::View<()>>) {
    let controller: NavigatorController<()> = NavigatorController::new();
    let build = {
        let controller = controller.clone();
        move |_: &mut ()| navigator(&controller, page)
    };
    let mut tester = Tester::new((), build, Size::new(300.0, 200.0));
    controller.push(page);
    tester.pump(FRAME);
    assert_eq!(controller.depth(), 2, "pushed to depth 2");
    (controller, tester)
}

#[test]
fn a_press_pops_only_the_pushing_threads_navigator() {
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [true, false]
        .into_iter()
        .map(|pushes| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let (controller, mut tester) = two_deep();
                barrier.wait();
                if pushes {
                    push_back_press();
                }
                barrier.wait();
                tester.pump(FRAME);
                tester.pump(FRAME);
                controller.depth()
            })
        })
        .collect();
    let depths: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        depths,
        vec![1, 2],
        "only the pushing thread's navigator popped"
    );
}

#[test]
fn a_press_on_one_thread_still_pops() {
    let (controller, mut tester) = two_deep();
    push_back_press();
    tester.pump(FRAME);
    tester.pump(FRAME);
    assert_eq!(controller.depth(), 1);
}

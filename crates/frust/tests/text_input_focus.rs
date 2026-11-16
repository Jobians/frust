//! A text field's focus from app code: `autofocus`, a `FocusHandle`, `on_focus_change`, and the
//! `keep_focus` wrapper that lets a sibling control take a press without ending the session.
//!
//! Needs the facade's `test-support` feature (`cargo test -p frust-ui --features test-support`);
//! without it the file compiles to nothing.

#![cfg(feature = "test-support")]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use frust::testing::Tester;
use frust::{
    Component, FocusHandle, View, button, column, component, either, keep_focus, text_input,
};
use kurbo::{Point, Size};

const FRAME: Duration = Duration::from_millis(16);

/// What the button does when tapped.
#[derive(Clone, Copy, PartialEq)]
enum Tap {
    Count,
    Request,
    Reveal,
}

struct Model {
    value: String,
    focus_log: Vec<bool>,
    clicks: u32,
    tap: Tap,
    keep: bool,
    autofocus: bool,
    show_field: bool,
}

type Shared = Rc<RefCell<Model>>;

fn model(tap: Tap) -> Shared {
    Rc::new(RefCell::new(Model {
        value: String::new(),
        focus_log: Vec::new(),
        clicks: 0,
        tap,
        keep: false,
        autofocus: false,
        show_field: true,
    }))
}

struct Page {
    model: Shared,
    handle: FocusHandle,
}

impl Page {
    fn tap_button(&self) -> impl View<Shared> + use<> {
        let handle = self.handle.clone();
        button("Tap", move |model: &mut Shared| {
            let mut model = model.borrow_mut();
            model.clicks += 1;
            match model.tap {
                Tap::Count => {}
                Tap::Request => handle.request(),
                Tap::Reveal => model.show_field = true,
            }
        })
    }
}

impl Component for Page {
    type State = Shared;

    fn init(&self) -> Shared {
        self.model.clone()
    }

    fn build(&self, model: &mut Shared) -> impl View<Shared> {
        let (value, keep, autofocus, show_field) = {
            let model = model.borrow();
            (
                model.value.clone(),
                model.keep,
                model.autofocus,
                model.show_field,
            )
        };
        column()
            .child(either(
                keep,
                || keep_focus(self.tap_button()),
                || self.tap_button(),
            ))
            .when(show_field, |col| {
                col.child(
                    text_input(value, |model: &mut Shared, v: String| {
                        model.borrow_mut().value = v;
                    })
                    .focus_handle(self.handle.clone())
                    .autofocus(autofocus)
                    .on_focus_change(|model: &mut Shared, focused: bool| {
                        model.borrow_mut().focus_log.push(focused);
                    }),
                )
            })
    }
}

fn page(model: &Shared, handle: &FocusHandle) -> Tester<(), impl View<()>> {
    let (model, handle) = (model.clone(), handle.clone());
    Tester::new(
        (),
        move |_: &mut ()| {
            component(Page {
                model: model.clone(),
                handle: handle.clone(),
            })
        },
        Size::new(400.0, 300.0),
    )
}

/// A few frames: enough for a deferred focus change and its notification. A focused field blinks
/// its caret, so the tree never goes idle and `pump_and_settle` cannot be used.
fn settle<S: 'static, V: View<S>>(tester: &mut Tester<S, V>) {
    for _ in 0..4 {
        tester.pump(FRAME);
    }
}

fn center_of<S: 'static, V: View<S>>(tester: &Tester<S, V>, type_suffix: &str) -> Point {
    tester
        .inspect()
        .into_iter()
        .find(|node| node.type_name.ends_with(type_suffix))
        .unwrap_or_else(|| panic!("the page holds a {type_suffix}"))
        .bounds
        .center()
}

fn tap_on<S: 'static, V: View<S>>(tester: &mut Tester<S, V>, type_suffix: &str) {
    let at = center_of(tester, type_suffix);
    tester.tap(at);
    settle(tester);
}

#[test]
fn an_autofocus_field_is_focused_after_the_first_pump_with_no_input() {
    let (model, handle) = (model(Tap::Count), FocusHandle::new());
    model.borrow_mut().autofocus = true;
    let mut tester = page(&model, &handle);
    tester.pump(FRAME);
    settle(&mut tester);

    assert!(tester.focus_info().is_some(), "the field took focus");
    assert!(handle.is_focused(), "and the handle reports it");
    assert_eq!(model.borrow().focus_log, [true]);
}

#[test]
fn a_button_requesting_the_handle_focuses_the_field() {
    let (model, handle) = (model(Tap::Request), FocusHandle::new());
    let mut tester = page(&model, &handle);
    assert!(tester.focus_info().is_none());

    tap_on(&mut tester, "ButtonWidget");

    assert_eq!(model.borrow().clicks, 1);
    assert!(
        tester.focus_info().is_some(),
        "the request focused the field"
    );
    assert!(handle.is_focused());
    assert_eq!(model.borrow().focus_log, [true]);
}

#[test]
fn a_press_on_a_keep_focus_button_keeps_the_field_focused() {
    let (model, handle) = (model(Tap::Count), FocusHandle::new());
    model.borrow_mut().keep = true;
    let mut tester = page(&model, &handle);
    tap_on(&mut tester, "TextInputWidget");
    let field = tester.focus_info().expect("the tap focused the field").node;

    tap_on(&mut tester, "ButtonWidget");

    assert_eq!(model.borrow().clicks, 1, "the button still ran its tap");
    assert_eq!(
        tester.focus_info().map(|info| info.node),
        Some(field),
        "the field kept its session"
    );
    assert!(handle.is_focused());
    assert_eq!(
        model.borrow().focus_log,
        [true],
        "and was never told it lost focus"
    );
}

#[test]
fn the_same_button_without_keep_focus_blurs_the_field() {
    let (model, handle) = (model(Tap::Count), FocusHandle::new());
    let mut tester = page(&model, &handle);
    tap_on(&mut tester, "TextInputWidget");
    let field = tester.focus_info().expect("the tap focused the field").node;

    tap_on(&mut tester, "ButtonWidget");

    assert_eq!(model.borrow().clicks, 1);
    assert_ne!(
        tester.focus_info().map(|info| info.node),
        Some(field),
        "the press ended the field's session"
    );
    assert!(!handle.is_focused());
}

#[test]
fn on_focus_change_records_one_true_and_one_false_for_tap_in_then_tap_out() {
    let (model, handle) = (model(Tap::Count), FocusHandle::new());
    let mut tester = page(&model, &handle);
    assert!(model.borrow().focus_log.is_empty());

    tap_on(&mut tester, "TextInputWidget");
    assert_eq!(model.borrow().focus_log, [true]);

    tap_on(&mut tester, "ButtonWidget");
    assert_eq!(model.borrow().focus_log, [true, false]);
}

#[test]
fn a_field_that_mounts_later_takes_a_pending_handle_request() {
    let (model, handle) = (model(Tap::Reveal), FocusHandle::new());
    model.borrow_mut().show_field = false;
    let mut tester = page(&model, &handle);
    handle.request();
    settle(&mut tester);
    assert!(
        tester.focus_info().is_none(),
        "nothing is mounted to take it"
    );

    tap_on(&mut tester, "ButtonWidget");

    assert!(
        tester.focus_info().is_some(),
        "the field took the pending request"
    );
    assert!(handle.is_focused());
    assert_eq!(model.borrow().focus_log, [true]);
}

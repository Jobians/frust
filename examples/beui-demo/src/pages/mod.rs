//! One module per gallery page.
//!
//! [`home`] and [`theming`] carry their interactive state on
//! [`crate::AppState`]; [`content`] and every section page under [`motion`],
//! [`agents`], and [`blocks`] host their own state in a page-local
//! `frust::component`, so the shell's page functions stay stateless and a
//! page starts fresh each time it is navigated to. [`gpu_effects`] needs no retained state of its own — every
//! specimen on it is either self-contained or deliberately frozen — so its
//! `page()` is a plain function, like [`motion::shader`]'s.

pub mod agents;
pub mod blocks;
pub mod content;
pub mod gpu_effects;
pub mod home;
pub mod motion;
pub mod theming;

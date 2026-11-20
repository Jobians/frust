//! `frust-charts`: live charts for Frust apps, platform-free and built on the facade alone.
//!
//! Data lives in shared handles, not in view state: a [`Series`] is a fixed-capacity ring of
//! evenly spaced samples (newest last, `None` a gap) and [`Samples`] a fixed set of current
//! values. Both are cheap to clone and `Send + Sync`, so a background thread writes while a
//! chart paints. Charts repaint when a handle is written, with no rebuild and no frame loop:
//! each write bumps the handle's revision and calls [`frust::request_repaint`] once.
//!
//! Charts are display-only and tap-transparent: they take no input, so a tap reaches whatever
//! lies beneath. A chart's colours come from the caller first, then the installed theme, then
//! the fallback constants on [`Tone`]. [`YDomain`] and [`nice_max`] pick the y range.
//!
//! ```
//! use frust_charts::{Samples, Series, YDomain};
//!
//! let load = Series::new(120); // two minutes at one sample a second
//! let worker = load.clone();
//! std::thread::spawn(move || worker.push(42.0)).join().unwrap();
//! assert_eq!(load.latest(), Some(42.0));
//! assert_eq!(YDomain::auto_from_zero().resolve(load.max()), (0.0, 50.0));
//!
//! let cores = Samples::new(8);
//! cores.slot(3).set(97.5);
//! ```

mod axis;
mod geometry;
mod heat;
mod meter;
mod samples;
mod scale;
mod scrub;
mod series;
mod sparkline;
mod time_series;
mod timed;
mod tone;

pub use samples::{SampleSlot, Samples};
pub use scale::{YDomain, nice_max};
pub use series::{Series, SlotsView};
pub use tone::Tone;

pub use geometry::*;
pub use heat::*;
pub use meter::*;
pub use sparkline::*;
pub use time_series::*;

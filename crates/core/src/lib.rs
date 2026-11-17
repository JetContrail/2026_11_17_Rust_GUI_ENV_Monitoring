//! GUI-independent domain logic of the integrated access-control and monitoring system.
//!
//! Nothing here touches the network, the GUI or the OS clock: callers pass the
//! wall-clock time (`NaiveDateTime`) and the monotonic time (`Instant`) in, so every
//! rule can be unit tested deterministically on any host.

pub mod demo;
pub mod directory;
pub mod door;
pub mod env;
pub mod model;
pub mod objects;
pub mod rules;
pub mod store;
pub mod system;

pub use env::{InvalidLimits, Level, Limits, Reading, Thresholds};

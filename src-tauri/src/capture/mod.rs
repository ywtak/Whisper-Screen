//! Visual and selection capture.
//!
//! `screen` is the full-screen path; `selection` reads the user's highlighted
//! text for the Transform intent. A window-scoped capture may still land here
//! as `window.rs`, which is why this is a directory rather than a single file.

pub mod screen;
pub mod selection;

pub use screen::{capture_primary, CaptureFrame};
pub use selection::read_selection;

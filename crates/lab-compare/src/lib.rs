//! Frame comparison.
//!
//! The perceptual compare comes from a production render pipeline's test bench,
//! where its thresholds were set and proven against two render backends. Here
//! they are only the defaults: an app's adapter can set its own, and a
//! scenario step can override them again.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod checks;
mod compare;
mod image;

pub use checks::{differs, is_blank, BLANK_LEVEL, CHANGE_TOLERANCE};
pub use compare::{compare, diff_image, mean_ssim, Comparison, SizeMismatch, Thresholds};
pub use image::{load_png, save_png, ImageError};

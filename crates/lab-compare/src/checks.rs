//! Checks that need no expected picture.
//!
//! These are what the lab can say about any app, with no help from it: is
//! anything on the screen, and did the screen change.

use lab_core::Frame;

/// A channel at or below this counts as black. Real capture paths are not
/// perfectly black on a black frame (dithering, colour management), so exact
/// zero would call a blank screen "content".
pub const BLANK_LEVEL: u8 = 8;

/// Two frames whose channels never differ by more than this are the same
/// picture, for change detection. Same reason as [`BLANK_LEVEL`].
pub const CHANGE_TOLERANCE: u8 = 2;

/// Nothing but black (alpha ignored).
pub fn is_blank(frame: &Frame) -> bool {
    frame
        .rgba
        .chunks_exact(4)
        .all(|p| p[0] <= BLANK_LEVEL && p[1] <= BLANK_LEVEL && p[2] <= BLANK_LEVEL)
}

/// Whether `b` is a different picture from `a`. A size change is always a
/// change.
pub fn differs(a: &Frame, b: &Frame) -> bool {
    if a.width != b.width || a.height != b.height {
        return true;
    }
    a.rgba
        .chunks_exact(4)
        .zip(b.rgba.chunks_exact(4))
        .any(|(p, q)| (0..3).any(|c| p[c].abs_diff(q[c]) > CHANGE_TOLERANCE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_black_is_blank() {
        assert!(is_blank(&Frame::solid(4, 4, [0, 0, 0, 255])));
        assert!(is_blank(&Frame::solid(4, 4, [8, 3, 0, 255])));
        assert!(!is_blank(&Frame::solid(4, 4, [9, 0, 0, 255])));
    }

    #[test]
    fn one_bright_pixel_is_content() {
        let mut f = Frame::solid(4, 4, [0, 0, 0, 255]);
        f.rgba[20] = 200;
        assert!(!is_blank(&f));
    }

    #[test]
    fn dither_is_not_a_change() {
        let a = Frame::solid(4, 4, [100, 100, 100, 255]);
        let b = Frame::solid(4, 4, [102, 99, 100, 255]);
        assert!(!differs(&a, &b));
        let c = Frame::solid(4, 4, [110, 100, 100, 255]);
        assert!(differs(&a, &c));
        assert!(differs(&a, &Frame::solid(4, 5, [100, 100, 100, 255])));
    }
}

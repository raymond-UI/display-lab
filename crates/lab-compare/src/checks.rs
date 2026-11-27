//! Checks that need no expected picture.
//!
//! These are what the lab can say about any app, with no help from it: is
//! anything on the screen, and did the screen change.
//!
//! Both ignore marks too small to be an app's picture. The OS draws some on
//! every screen: macOS puts a screen-recording dot, a few pixels across, on
//! each screen being captured. One such dot must not make a black screen
//! "content", nor its arrival a "change".

use lab_core::Frame;

/// A channel at or below this counts as black. Real capture paths are not
/// perfectly black on a black frame (dithering, colour management), so exact
/// zero would call a blank screen "content".
pub const BLANK_LEVEL: u8 = 8;

/// Two pixels whose channels never differ by more than this are the same,
/// for change detection. Same reason as [`BLANK_LEVEL`].
pub const CHANGE_TOLERANCE: u8 = 2;

/// The smallest mark that counts, as one pixel in this many: 1 in 2000 is
/// about 1000 pixels on a 1080p screen and 390 on a 1024x768 one. The macOS
/// recording dot is 32 pixels; one line of slide text is thousands.
pub const MIN_MARK_DIVISOR: usize = 2000;

/// How many pixels a mark on `frame` must cover to count. At least one, so
/// a tiny frame still has content when anything is lit.
pub fn min_mark_pixels(frame: &Frame) -> usize {
    (frame.width as usize * frame.height as usize / MIN_MARK_DIVISOR).max(1)
}

/// How many pixels are not black (alpha ignored).
pub fn lit_pixels(frame: &Frame) -> usize {
    frame
        .rgba
        .chunks_exact(4)
        .filter(|p| p[0] > BLANK_LEVEL || p[1] > BLANK_LEVEL || p[2] > BLANK_LEVEL)
        .count()
}

/// Black, apart from marks too small to be an app's picture.
pub fn is_blank(frame: &Frame) -> bool {
    lit_pixels(frame) < min_mark_pixels(frame)
}

/// Whether `b` is a different picture from `a`: enough pixels changed to be
/// more than an OS mark. A size change is always a change.
pub fn differs(a: &Frame, b: &Frame) -> bool {
    if a.width != b.width || a.height != b.height {
        return true;
    }
    // Stops counting once enough have changed: this runs on every captured
    // frame while the lab watches for a change.
    let min = min_mark_pixels(a);
    a.rgba
        .chunks_exact(4)
        .zip(b.rgba.chunks_exact(4))
        .filter(|(p, q)| (0..3).any(|c| p[c].abs_diff(q[c]) > CHANGE_TOLERANCE))
        .take(min)
        .count()
        >= min
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1080p black frame with a `side` x `side` square of `colour` at the
    /// top-right, where macOS puts its recording dot.
    fn black_with_square(side: u32, colour: [u8; 4]) -> Frame {
        let mut f = Frame::solid(1920, 1080, [0, 0, 0, 255]);
        for y in 4..4 + side {
            for x in 1906..1906 + side {
                let i = ((y * 1920 + x) * 4) as usize;
                f.rgba[i..i + 4].copy_from_slice(&colour);
            }
        }
        f
    }

    #[test]
    fn near_black_is_blank() {
        assert!(is_blank(&Frame::solid(4, 4, [0, 0, 0, 255])));
        assert!(is_blank(&Frame::solid(4, 4, [8, 3, 0, 255])));
        assert!(!is_blank(&Frame::solid(4, 4, [9, 0, 0, 255])));
    }

    #[test]
    fn one_bright_pixel_on_a_tiny_frame_is_content() {
        let mut f = Frame::solid(4, 4, [0, 0, 0, 255]);
        f.rgba[20] = 200;
        assert!(!is_blank(&f));
    }

    #[test]
    fn an_os_dot_on_black_is_still_blank() {
        // The macOS recording dot fills a 6 x 6 box on a captured 1080p
        // frame (32 lit pixels); a full square is the worst case.
        let dot = black_with_square(6, [90, 90, 240, 255]);
        assert_eq!(lit_pixels(&dot), 36);
        assert!(is_blank(&dot));
    }

    #[test]
    fn a_mark_the_size_of_text_is_content() {
        // 40 x 40: about a word of slide text on a 1080p screen.
        assert!(!is_blank(&black_with_square(40, [255, 255, 255, 255])));
    }

    #[test]
    fn an_os_dot_appearing_is_not_a_change() {
        let black = Frame::solid(1920, 1080, [0, 0, 0, 255]);
        assert!(!differs(&black, &black_with_square(6, [90, 90, 240, 255])));
        assert!(differs(
            &black,
            &black_with_square(40, [255, 255, 255, 255])
        ));
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

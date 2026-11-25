//! What the fake app draws.
//!
//! Every slide is a different picture with real structure in it (SSIM means
//! nothing on a flat field), scaled to fit the screen with letterboxing, the
//! way a 16:9 presentation lands on a 4:3 projector.

use lab_core::Frame;

/// The app's canvas aspect ratio.
const CANVAS: (u32, u32) = (16, 9);

/// Slide `n` drawn to fit `width` x `height`, letterboxed in black.
pub fn render_slide(n: u32, width: u32, height: u32) -> Frame {
    let mut frame = Frame::solid(width, height, [0, 0, 0, 255]);
    let (cw, ch) = fit(width, height);
    let (ox, oy) = ((width - cw) / 2, (height - ch) / 2);

    // A background tint per slide, a diagonal stripe pattern, and a bright
    // marker whose position moves with the slide number.
    let tint = [
        (40 + (n * 53) % 160) as u8,
        (40 + (n * 97) % 160) as u8,
        (40 + (n * 31) % 160) as u8,
    ];
    let marker_x = (n % 8) * cw / 8;
    let marker_w = (cw / 8).max(1);
    let (marker_y, marker_h) = (ch / 3, (ch / 3).max(1));

    for y in 0..ch {
        for x in 0..cw {
            let stripe = ((x + y + n * 7) / 12).is_multiple_of(2);
            let mut p = if stripe {
                tint
            } else {
                [tint[0] / 2, tint[1] / 2, tint[2] / 2]
            };
            let in_marker = x >= marker_x
                && x < marker_x + marker_w
                && y >= marker_y
                && y < marker_y + marker_h;
            if in_marker {
                p = [250, 250, 250];
            }
            let i = (((oy + y) * width + ox + x) * 4) as usize;
            frame.rgba[i..i + 3].copy_from_slice(&p);
        }
    }
    frame
}

/// The operator's console, drawn on the host screen when no slide is there:
/// a dark panel with a light toolbar along the top.
pub fn render_console(width: u32, height: u32) -> Frame {
    let mut frame = Frame::solid(width, height, [30, 30, 34, 255]);
    let bar = (height / 12).max(1);
    for y in 0..bar.min(height) {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            frame.rgba[i..i + 3].copy_from_slice(&[200, 200, 210]);
        }
    }
    frame
}

/// The largest 16:9 rectangle that fits.
fn fit(width: u32, height: u32) -> (u32, u32) {
    let by_width = (width, width * CANVAS.1 / CANVAS.0);
    if by_width.1 <= height {
        (by_width.0.max(1), by_width.1.max(1))
    } else {
        ((height * CANVAS.0 / CANVAS.1).max(1), height.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slides_differ() {
        assert!(!render_slide(1, 64, 36).same_pixels(&render_slide(2, 64, 36)));
    }

    #[test]
    fn a_4_3_screen_is_letterboxed() {
        let f = render_slide(3, 160, 120);
        // 160x90 content, 15 px black bands above and below.
        assert_eq!(f.pixel(80, 5), [0, 0, 0, 255]);
        assert_eq!(f.pixel(80, 114), [0, 0, 0, 255]);
        assert_ne!(f.pixel(80, 60), [0, 0, 0, 255]);
    }

    #[test]
    fn console_is_not_a_slide() {
        assert!(!render_console(64, 36).same_pixels(&render_slide(0, 64, 36)));
    }
}

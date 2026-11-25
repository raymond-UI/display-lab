//! A captured (or expected) picture of one screen.

/// Tightly packed 8-bit RGBA, row-major, no padding between rows.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// When the frame reached the screen, on the lab clock ([`crate::now_ns`]).
    /// Zero for a frame that was never presented (an expected frame, a
    /// reference image).
    pub present_ns: u64,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The pixels are megabytes; a Debug that prints them is unusable.
        f.debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("present_ns", &self.present_ns)
            .finish_non_exhaustive()
    }
}

impl Frame {
    /// A frame filled with one colour.
    pub fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Frame {
        let count = width as usize * height as usize;
        let mut pixels = Vec::with_capacity(count * 4);
        for _ in 0..count {
            pixels.extend_from_slice(&rgba);
        }
        Frame {
            width,
            height,
            rgba: pixels,
            present_ns: 0,
        }
    }

    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// True when the buffer length matches the stated size. Backends and
    /// adapters build frames from outside data, so this is checked rather
    /// than assumed wherever a frame enters the lab.
    pub fn is_well_formed(&self) -> bool {
        self.rgba.len() == self.pixel_count() * 4
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// Same size and same pixels. Timestamps are ignored.
    pub fn same_pixels(&self, other: &Frame) -> bool {
        self.width == other.width && self.height == other.height && self.rgba == other.rgba
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_is_well_formed() {
        let f = Frame::solid(3, 2, [1, 2, 3, 4]);
        assert!(f.is_well_formed());
        assert_eq!(f.pixel(2, 1), [1, 2, 3, 4]);
    }

    #[test]
    fn same_pixels_ignores_time() {
        let mut a = Frame::solid(2, 2, [0, 0, 0, 255]);
        let b = a.clone();
        a.present_ns = 99;
        assert!(a.same_pixels(&b));
    }
}

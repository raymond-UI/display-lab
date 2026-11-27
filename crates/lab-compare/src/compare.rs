//! Perceptual comparison of two frames.
//!
//! A frame matches when mean SSIM is at least the minimum **and** no more than
//! a budget of its pixels differ by more than a CIE76 ΔE threshold. Both halves
//! are needed: SSIM alone tolerates a uniform colour shift that is obvious on
//! a projector; a ΔE budget alone tolerates a structural error confined to a
//! small area, such as a line of text landing 4px low.
//!
//! The defaults (SSIM ≥ 0.99, ≤ 0.5% of pixels beyond ΔE 3) come from a
//! production render pipeline, where they separated real regressions from
//! harmless differences between two GPU backends.
//! For the lab they are only defaults: captured frames have been through the
//! compositor and a colour profile, so an app's adapter may loosen them.

use lab_core::Frame;

/// RGBA of pixel `i` in raster order.
fn px(f: &Frame, i: usize) -> [u8; 4] {
    let o = i * 4;
    [f.rgba[o], f.rgba[o + 1], f.rgba[o + 2], f.rgba[o + 3]]
}

/// The lab's default thresholds.
pub const DEFAULT_MIN_SSIM: f64 = 0.99;
pub const DEFAULT_MAX_DELTA_E: f64 = 3.0;
pub const DEFAULT_MAX_BAD_FRACTION: f64 = 0.005;

/// Alpha is compared alongside colour rather than ignored: a compositor that
/// drops an alpha channel would otherwise pass every golden. One 8-bit step of
/// slack absorbs encoder rounding. Deliberately not a CLI knob: if a fixture
/// ever needs to tune it, add the flag then.
const MAX_ALPHA_DELTA: u8 = 1;

/// SSIM window, the standard 11x11 Gaussian with σ = 1.5 (Wang et al. 2004).
const SSIM_RADIUS: usize = 5;
const SSIM_SIGMA: f64 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub min_ssim: f64,
    pub max_delta_e: f64,
    pub max_bad_fraction: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            min_ssim: DEFAULT_MIN_SSIM,
            max_delta_e: DEFAULT_MAX_DELTA_E,
            max_bad_fraction: DEFAULT_MAX_BAD_FRACTION,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub width: u32,
    pub height: u32,
    pub ssim: f64,
    /// Fraction of pixels beyond `max_delta_e` (or beyond the alpha slack).
    pub bad_fraction: f64,
    pub bad_pixels: usize,
    pub max_delta_e: f64,
    pub thresholds: Thresholds,
}

impl Comparison {
    pub fn ssim_ok(&self) -> bool {
        self.ssim >= self.thresholds.min_ssim
    }

    pub fn delta_e_ok(&self) -> bool {
        self.bad_fraction <= self.thresholds.max_bad_fraction
    }

    pub fn passed(&self) -> bool {
        self.ssim_ok() && self.delta_e_ok()
    }

    /// The one-line summary the issue asks `compare` to print.
    pub fn summary(&self, name: &str) -> String {
        format!(
            "{} {}: ssim {:.5} (min {:.5}), {:.3}% of pixels over deltaE {:.1} \
             (max {:.3}%), peak deltaE {:.2}, {}x{}",
            if self.passed() { "PASS" } else { "FAIL" },
            name,
            self.ssim,
            self.thresholds.min_ssim,
            self.bad_fraction * 100.0,
            self.thresholds.max_delta_e,
            self.thresholds.max_bad_fraction * 100.0,
            self.max_delta_e,
            self.width,
            self.height,
        )
    }

    /// Why it failed, one reason per line. Empty when it passed.
    pub fn reasons(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.ssim_ok() {
            out.push(format!(
                "structural: ssim {:.5} is below {:.5}",
                self.ssim, self.thresholds.min_ssim
            ));
        }
        if !self.delta_e_ok() {
            out.push(format!(
                "colour: {} of {} pixels ({:.3}%) exceed deltaE {:.1}, budget {:.3}%",
                self.bad_pixels,
                self.width as usize * self.height as usize,
                self.bad_fraction * 100.0,
                self.thresholds.max_delta_e,
                self.thresholds.max_bad_fraction * 100.0,
            ));
        }
        out
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct SizeMismatch {
    pub actual: (u32, u32),
    pub expected: (u32, u32),
}

impl std::fmt::Display for SizeMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "size mismatch: actual is {}x{}, expected is {}x{}",
            self.actual.0, self.actual.1, self.expected.0, self.expected.1
        )
    }
}

impl std::error::Error for SizeMismatch {}

/// Compare two frames. A size mismatch is not a threshold failure: nothing
/// meaningful can be said about the pixels, so it is reported separately.
pub fn compare(
    actual: &Frame,
    expected: &Frame,
    thresholds: Thresholds,
) -> Result<Comparison, SizeMismatch> {
    if actual.width != expected.width || actual.height != expected.height {
        return Err(SizeMismatch {
            actual: (actual.width, actual.height),
            expected: (expected.width, expected.height),
        });
    }

    let deltas = delta_e_map(actual, expected);
    let mut bad_pixels = 0usize;
    let mut peak = 0.0f64;
    for (i, &d) in deltas.iter().enumerate() {
        if d > peak {
            peak = d;
        }
        let alpha_delta = px(actual, i)[3].abs_diff(px(expected, i)[3]);
        if d > thresholds.max_delta_e || alpha_delta > MAX_ALPHA_DELTA {
            bad_pixels += 1;
        }
    }

    let total = actual.pixel_count();
    Ok(Comparison {
        width: actual.width,
        height: actual.height,
        ssim: mean_ssim(actual, expected),
        bad_fraction: bad_pixels as f64 / total as f64,
        bad_pixels,
        max_delta_e: peak,
        thresholds,
    })
}

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// sRGB 8-bit to linear light, as a lookup table: the transfer function is the
/// hottest thing in the comparator and there are only 256 inputs.
fn srgb_to_linear_lut() -> [f64; 256] {
    let mut lut = [0.0; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let c = i as f64 / 255.0;
        *slot = if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        };
    }
    lut
}

/// CIE L*a*b* under D65, from linear-light RGB.
fn lab(rgb: [u8; 3], lut: &[f64; 256]) -> [f64; 3] {
    let (r, g, b) = (
        lut[rgb[0] as usize],
        lut[rgb[1] as usize],
        lut[rgb[2] as usize],
    );

    // sRGB primaries to CIE XYZ, D65 white.
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;

    const WHITE: [f64; 3] = [0.950_489, 1.0, 1.088_840];
    let f = |t: f64| {
        const DELTA: f64 = 6.0 / 29.0;
        if t > DELTA * DELTA * DELTA {
            t.cbrt()
        } else {
            t / (3.0 * DELTA * DELTA) + 4.0 / 29.0
        }
    };
    let (fx, fy, fz) = (f(x / WHITE[0]), f(y / WHITE[1]), f(z / WHITE[2]));

    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Per-pixel CIE76 ΔE between two same-sized images.
fn delta_e_map(a: &Frame, b: &Frame) -> Vec<f64> {
    let lut = srgb_to_linear_lut();
    (0..a.pixel_count())
        .map(|i| {
            let (pa, pb) = (px(a, i), px(b, i));
            let la = lab([pa[0], pa[1], pa[2]], &lut);
            let lb = lab([pb[0], pb[1], pb[2]], &lut);
            let d = [la[0] - lb[0], la[1] - lb[1], la[2] - lb[2]];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// SSIM
// ---------------------------------------------------------------------------

/// Rec.601 luma, the channel SSIM is conventionally computed on.
fn luma(img: &Frame) -> Vec<f64> {
    (0..img.pixel_count())
        .map(|i| {
            let p = px(img, i);
            0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64
        })
        .collect()
}

fn gaussian_kernel(radius: usize, sigma: f64) -> Vec<f64> {
    let mut k: Vec<f64> = (0..=2 * radius)
        .map(|i| {
            let x = i as f64 - radius as f64;
            (-(x * x) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let sum: f64 = k.iter().sum();
    for v in &mut k {
        *v /= sum;
    }
    k
}

/// Separable Gaussian blur with edge reflection. Reflected samples only ever
/// reach positions within `radius` of an edge, and `mean_ssim` drops that band
/// before averaging, so the result over the kept region is the exact "valid"
/// convolution.
fn blur(src: &[f64], width: usize, height: usize, kernel: &[f64], radius: usize) -> Vec<f64> {
    let reflect = |i: isize, n: usize| -> usize {
        let n = n as isize;
        let mut i = i;
        while i < 0 || i >= n {
            if i < 0 {
                i = -i;
            }
            if i >= n {
                i = 2 * n - 2 - i;
            }
        }
        i as usize
    };

    let mut tmp = vec![0.0; src.len()];
    for y in 0..height {
        for x in 0..width {
            let mut acc = 0.0;
            for (k, &w) in kernel.iter().enumerate() {
                let sx = reflect(x as isize + k as isize - radius as isize, width);
                acc += w * src[y * width + sx];
            }
            tmp[y * width + x] = acc;
        }
    }

    let mut out = vec![0.0; src.len()];
    for y in 0..height {
        for x in 0..width {
            let mut acc = 0.0;
            for (k, &w) in kernel.iter().enumerate() {
                let sy = reflect(y as isize + k as isize - radius as isize, height);
                acc += w * tmp[sy * width + x];
            }
            out[y * width + x] = acc;
        }
    }
    out
}

/// Mean SSIM over every window that fits inside the image.
pub fn mean_ssim(a: &Frame, b: &Frame) -> f64 {
    let (width, height) = (a.width as usize, a.height as usize);
    // Shrink the window rather than refuse: a 4x4 fixture is still comparable.
    let radius = SSIM_RADIUS.min((width.min(height).saturating_sub(1)) / 2);
    let kernel = gaussian_kernel(radius, SSIM_SIGMA);

    let (x, y) = (luma(a), luma(b));
    let xx: Vec<f64> = x.iter().map(|v| v * v).collect();
    let yy: Vec<f64> = y.iter().map(|v| v * v).collect();
    let xy: Vec<f64> = x.iter().zip(&y).map(|(u, v)| u * v).collect();

    let mu_x = blur(&x, width, height, &kernel, radius);
    let mu_y = blur(&y, width, height, &kernel, radius);
    let s_xx = blur(&xx, width, height, &kernel, radius);
    let s_yy = blur(&yy, width, height, &kernel, radius);
    let s_xy = blur(&xy, width, height, &kernel, radius);

    // Stabilising constants for 8-bit dynamic range (L = 255).
    const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
    const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);

    let mut total = 0.0;
    let mut count = 0usize;
    for row in radius..height - radius {
        for col in radius..width - radius {
            let i = row * width + col;
            let (mx, my) = (mu_x[i], mu_y[i]);
            let vx = (s_xx[i] - mx * mx).max(0.0);
            let vy = (s_yy[i] - my * my).max(0.0);
            let vxy = s_xy[i] - mx * my;

            let numerator = (2.0 * mx * my + C1) * (2.0 * vxy + C2);
            let denominator = (mx * mx + my * my + C1) * (vx + vy + C2);
            total += numerator / denominator;
            count += 1;
        }
    }

    if count == 0 {
        // Degenerate: nothing to compare structurally. Identical images should
        // still score 1.0, so fall back to an exact-match answer.
        return if a.rgba == b.rgba { 1.0 } else { 0.0 };
    }
    total / count as f64
}

// ---------------------------------------------------------------------------
// Diff heatmap
// ---------------------------------------------------------------------------

/// A heatmap of per-pixel ΔE, scaled so the threshold sits at the blue/yellow
/// boundary: anything warm is over budget and anything red is twice over.
pub fn diff_image(actual: &Frame, expected: &Frame, max_delta_e: f64) -> Frame {
    let deltas = delta_e_map(actual, expected);
    let scale = if max_delta_e > 0.0 {
        max_delta_e
    } else {
        DEFAULT_MAX_DELTA_E
    };

    let mut pixels = Vec::with_capacity(deltas.len() * 4);
    for (i, &d) in deltas.iter().enumerate() {
        let alpha_delta = px(actual, i)[3].abs_diff(px(expected, i)[3]);
        let t = if alpha_delta > MAX_ALPHA_DELTA {
            // An alpha-only difference is invisible in ΔE; show it as over
            // budget so the heatmap never looks clean on a failing frame.
            2.0
        } else {
            (d / scale).min(2.0)
        };
        let rgb = if t <= 1.0 {
            // Under budget: black through blue to cyan.
            [0, (t * 128.0).round() as u8, (t * 255.0).round() as u8]
        } else {
            // Over budget: yellow through red.
            let u = t - 1.0;
            [255, (255.0 * (1.0 - u)).round() as u8, 0]
        };
        pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }

    Frame {
        width: actual.width,
        height: actual.height,
        rgba: pixels,
        present_ns: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic image with structure in it: SSIM is meaningless on a
    /// flat field, so every fixture here has gradients and edges.
    fn textured(width: u32, height: u32) -> Frame {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let checker = if (x / 8 + y / 8) % 2 == 0 { 40 } else { 0 };
                let r = (x * 255 / width.max(1)) as u8;
                let g = (y * 255 / height.max(1)) as u8;
                let b = ((x * y) % 251) as u8;
                pixels.extend_from_slice(&[
                    r.saturating_add(checker),
                    g.saturating_add(checker),
                    b,
                    255,
                ]);
            }
        }
        Frame {
            width,
            height,
            rgba: pixels,
            present_ns: 0,
        }
    }

    fn map_rgb(img: &Frame, f: impl Fn([u8; 3]) -> [u8; 3]) -> Frame {
        let mut pixels = Vec::with_capacity(img.rgba.len());
        for i in 0..img.pixel_count() {
            let p = px(img, i);
            let out = f([p[0], p[1], p[2]]);
            pixels.extend_from_slice(&[out[0], out[1], out[2], p[3]]);
        }
        Frame {
            width: img.width,
            height: img.height,
            rgba: pixels,
            present_ns: 0,
        }
    }

    #[test]
    fn identical_images_pass() {
        let img = textured(64, 64);
        let result = compare(&img, &img, Thresholds::default()).expect("same size");
        assert!(result.passed(), "{}", result.summary("identical"));
        assert_eq!(result.bad_pixels, 0);
        assert_eq!(result.max_delta_e, 0.0);
        assert!((result.ssim - 1.0).abs() < 1e-9, "ssim {}", result.ssim);
        assert!(result.reasons().is_empty());
    }

    #[test]
    fn a_slight_gamma_shift_passes() {
        // The case this tolerance exists for: two backends with marginally
        // different output transfer curves rendering the same scene.
        let img = textured(64, 64);
        let shifted = map_rgb(&img, |p| {
            let g = |c: u8| {
                let v = c as f64 / 255.0;
                (v.powf(1.0 / 1.01) * 255.0).round().clamp(0.0, 255.0) as u8
            };
            [g(p[0]), g(p[1]), g(p[2])]
        });

        assert!(
            !img.same_pixels(&shifted),
            "the fixture must actually differ"
        );
        let result = compare(&shifted, &img, Thresholds::default()).expect("same size");
        assert!(result.passed(), "{}", result.summary("gamma"));
        assert!(result.ssim >= 0.99, "ssim {}", result.ssim);
    }

    #[test]
    fn one_percent_of_pixels_beyond_the_threshold_fails() {
        // The ΔE budget is 0.5% of pixels; corrupting 1% must fail it.
        let img = textured(100, 100);
        let mut broken = img.clone();
        for i in (0..broken.pixel_count()).step_by(100) {
            let o = i * 4;
            broken.rgba[o] = 255 - broken.rgba[o];
            broken.rgba[o + 1] = 255 - broken.rgba[o + 1];
            broken.rgba[o + 2] = 255 - broken.rgba[o + 2];
        }

        let result = compare(&broken, &img, Thresholds::default()).expect("same size");
        assert!(!result.passed(), "{}", result.summary("one-percent"));
        assert!(!result.delta_e_ok(), "{}", result.summary("one-percent"));
        assert!(
            result.bad_fraction >= 0.01,
            "bad fraction {}",
            result.bad_fraction
        );
        assert!(result.reasons().iter().any(|r| r.starts_with("colour:")));
    }

    #[test]
    fn a_one_percent_geometric_shift_fails() {
        // The other reading of "1% shifted": the whole frame translated by 1%
        // of its width, which is the shape of a text-layout regression. SSIM is
        // the half of the threshold that catches this.
        let img = textured(100, 100);
        let shift = (img.width / 100).max(1) as usize;
        let mut pixels = vec![0u8; img.rgba.len()];
        for y in 0..img.height as usize {
            for x in 0..img.width as usize {
                let src = y * img.width as usize + x;
                let dst = y * img.width as usize + (x + shift).min(img.width as usize - 1);
                pixels[dst * 4..dst * 4 + 4].copy_from_slice(&px(&img, src));
            }
        }
        let shifted = Frame {
            width: img.width,
            height: img.height,
            rgba: pixels,
            present_ns: 0,
        };

        let result = compare(&shifted, &img, Thresholds::default()).expect("same size");
        assert!(!result.passed(), "{}", result.summary("shifted"));
        assert!(
            !result.ssim_ok(),
            "ssim {} should be below 0.99",
            result.ssim
        );
        assert!(result
            .reasons()
            .iter()
            .any(|r| r.starts_with("structural:")));
    }

    #[test]
    fn a_dropped_alpha_channel_fails() {
        let mut img = textured(32, 32);
        for i in 0..img.pixel_count() {
            img.rgba[i * 4 + 3] = 128;
        }
        let opaque = Frame {
            width: img.width,
            height: img.height,
            rgba: img
                .rgba
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            present_ns: 0,
        };

        let result = compare(&opaque, &img, Thresholds::default()).expect("same size");
        assert!(!result.passed(), "an alpha-only difference must not pass");
        assert_eq!(result.bad_pixels, img.pixel_count());
    }

    #[test]
    fn a_size_mismatch_is_reported_separately() {
        let a = textured(16, 16);
        let b = textured(16, 32);
        let err = compare(&a, &b, Thresholds::default()).expect_err("different sizes");
        assert_eq!(err.actual, (16, 16));
        assert_eq!(err.expected, (16, 32));
    }

    #[test]
    fn thresholds_are_honoured() {
        let img = textured(64, 64);
        let broken = map_rgb(&img, |p| [p[0].saturating_add(20), p[1], p[2]]);

        let strict = compare(&broken, &img, Thresholds::default()).expect("same size");
        assert!(!strict.passed());

        let loose = Thresholds {
            min_ssim: 0.0,
            max_delta_e: 100.0,
            max_bad_fraction: 1.0,
        };
        let relaxed = compare(&broken, &img, loose).expect("same size");
        assert!(relaxed.passed(), "{}", relaxed.summary("relaxed"));
    }

    #[test]
    fn the_diff_heatmap_is_cold_where_the_frames_agree() {
        let img = textured(32, 32);
        let mut broken = img.clone();
        broken.rgba[0] = 255 - broken.rgba[0];
        broken.rgba[1] = 255 - broken.rgba[1];
        broken.rgba[2] = 255 - broken.rgba[2];

        let diff = diff_image(&broken, &img, DEFAULT_MAX_DELTA_E);
        assert_eq!(diff.width, img.width);
        assert_eq!(diff.height, img.height);
        // The corrupted pixel is red-hot; an untouched one is pure black.
        assert_eq!(px(&diff, 0), [255, 0, 0, 255]);
        assert_eq!(px(&diff, diff.pixel_count() - 1), [0, 0, 0, 255]);
    }

    #[test]
    fn summary_states_the_verdict_first() {
        let img = textured(16, 16);
        let result = compare(&img, &img, Thresholds::default()).expect("same size");
        assert!(result.summary("fixture").starts_with("PASS fixture:"));
    }
}

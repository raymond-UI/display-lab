//! PNG in and out, as [`Frame`]s.
//!
//! Reference images can be any PNG; everything is normalised to 8-bit RGBA so
//! the comparator has one shape to reason about.

use lab_core::Frame;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("{path}: not a readable PNG: {source}")]
    Decode {
        path: String,
        source: png::DecodingError,
    },
    #[error("{path}: could not encode PNG: {source}")]
    Encode {
        path: String,
        source: png::EncodingError,
    },
    #[error("{0}")]
    Shape(String),
}

pub fn load_png(path: &Path) -> Result<Frame, ImageError> {
    let shown = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|source| ImageError::Io {
        path: shown.clone(),
        source,
    })?;
    decode_png(&bytes, &shown)
}

/// A PNG held in memory, such as an image an app sent back over MCP.
/// `label` names it in errors.
pub fn decode_png(bytes: &[u8], label: &str) -> Result<Frame, ImageError> {
    let shown = label.to_string();
    let decode = |source| ImageError::Decode {
        path: shown.clone(),
        source,
    };
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(decode)?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(decode)?;
    buf.truncate(info.buffer_size());

    let channels = info.color_type.samples();
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => expand(&buf, channels, |s| [s[0], s[1], s[2], 255]),
        png::ColorType::Grayscale => expand(&buf, channels, |s| [s[0], s[0], s[0], 255]),
        png::ColorType::GrayscaleAlpha => expand(&buf, channels, |s| [s[0], s[0], s[0], s[1]]),
        png::ColorType::Indexed => {
            return Err(ImageError::Shape(format!(
                "{shown}: indexed PNG was not expanded by the decoder"
            )))
        }
    };
    let frame = Frame {
        width: info.width,
        height: info.height,
        rgba,
        present_ns: 0,
    };
    if frame.width == 0 || frame.height == 0 || !frame.is_well_formed() {
        return Err(ImageError::Shape(format!("{shown}: malformed image")));
    }
    Ok(frame)
}

/// `frame` as PNG bytes.
pub fn encode_png(frame: &Frame) -> Result<Vec<u8>, ImageError> {
    if !frame.is_well_formed() {
        return Err(ImageError::Shape(
            "frame buffer does not match its size".into(),
        ));
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let encode = |source| ImageError::Encode {
        path: "in-memory PNG".into(),
        source,
    };
    encoder
        .write_header()
        .map_err(encode)?
        .write_image_data(&frame.rgba)
        .map_err(encode)?;
    Ok(out)
}

pub fn save_png(frame: &Frame, path: &Path) -> Result<(), ImageError> {
    let shown = path.display().to_string();
    let io = |source| ImageError::Io {
        path: shown.clone(),
        source,
    };
    if !frame.is_well_formed() {
        return Err(ImageError::Shape(format!(
            "{shown}: frame buffer does not match its size"
        )));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
    }
    let file = std::fs::File::create(path).map_err(io)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let encode = |source| ImageError::Encode {
        path: shown.clone(),
        source,
    };
    encoder
        .write_header()
        .map_err(encode)?
        .write_image_data(&frame.rgba)
        .map_err(encode)?;
    Ok(())
}

fn expand(buf: &[u8], channels: usize, to_rgba: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len() / channels * 4);
    for sample in buf.chunks_exact(channels) {
        out.extend_from_slice(&to_rgba(sample));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lab-compare-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_through_png() {
        let dir = tempdir();
        let path = dir.join("a.png");
        let original = Frame::solid(7, 5, [12, 240, 3, 200]);
        save_png(&original, &path).unwrap();
        let loaded = load_png(&path).unwrap();
        assert!(original.same_pixels(&loaded));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_in_memory() {
        let original = Frame::solid(4, 3, [9, 8, 7, 255]);
        let bytes = encode_png(&original).unwrap();
        assert!(decode_png(&bytes, "x").unwrap().same_pixels(&original));
        assert!(decode_png(b"not a png", "x").is_err());
    }

    #[test]
    fn rgb_gets_full_alpha() {
        let dir = tempdir();
        let path = dir.join("rgb.png");
        let file = std::fs::File::create(&path).unwrap();
        let mut encoder = png::Encoder::new(file, 2, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[1, 2, 3, 4, 5, 6])
            .unwrap();
        let loaded = load_png(&path).unwrap();
        assert_eq!(loaded.pixel(0, 0), [1, 2, 3, 255]);
        assert_eq!(loaded.pixel(1, 0), [4, 5, 6, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

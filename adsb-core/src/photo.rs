//! Photo decode (feature `photo`): JPEG (baseline or progressive) to RGB8, centre-cropped to the
//! panel's 3:2 aspect and scaled to [`WIDTH`] x [`HEIGHT`].

use crate::meta::Photo;
use alloc::vec;
use alloc::vec::Vec;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;

pub const WIDTH: usize = 336;
pub const HEIGHT: usize = 224;
/// Larger downloads are refused (the device buffer is 96 KB).
pub const MAX_JPEG: usize = 96 * 1024;
/// Decoded size limit (width x height): bounds the RGB buffer to 3 MB.
pub const MAX_PIXELS: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhotoError {
    TooLarge,
    Decode,
}

/// Decodes `jpeg` and returns a [`WIDTH`] x [`HEIGHT`] photo. The decoder's buffer is freed
/// before this returns.
pub fn decode(jpeg: &[u8]) -> Result<Photo, PhotoError> {
    if jpeg.len() > MAX_JPEG {
        return Err(PhotoError::TooLarge);
    }
    let mut dec = JpegDecoder::new(ZCursor::new(jpeg));
    dec.decode_headers().map_err(|_| PhotoError::Decode)?;
    let info = dec.info().ok_or(PhotoError::Decode)?;
    let (w, h) = (usize::from(info.width), usize::from(info.height));
    if w == 0 || h == 0 || w * h > MAX_PIXELS {
        return Err(PhotoError::TooLarge);
    }
    if dec.output_buffer_size() != Some(w * h * 3) {
        return Err(PhotoError::Decode); // not 3 channels (e.g. grayscale or CMYK)
    }
    let mut src = vec![0u8; w * h * 3];
    dec.decode_into(&mut src).map_err(|_| PhotoError::Decode)?;
    Ok(Photo {
        width: WIDTH as u16,
        height: HEIGHT as u16,
        rgb: scale(&src, w, h),
    })
}

/// Centre crop to 3:2, then bilinear resample (sample positions in 1/256 px, integer maths).
fn scale(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let (cw, ch) = if w * HEIGHT > h * WIDTH {
        (h * WIDTH / HEIGHT, h) // too wide: crop the sides
    } else {
        (w, w * HEIGHT / WIDTH) // too tall: crop top and bottom
    };
    let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
    let pos = |o: usize, n: usize, len: usize| -> (usize, usize, i64) {
        // Source coordinate of the centre of output pixel `o`, in 1/256 px.
        let c = ((2 * o as i64 + 1) * len as i64 * 256) / (2 * n as i64) - 128;
        let c = c.clamp(0, (len as i64 - 1) * 256);
        let i = (c >> 8) as usize;
        (i, (i + 1).min(len - 1), c & 255)
    };
    let mut out = Vec::with_capacity(WIDTH * HEIGHT * 3);
    for oy in 0..HEIGHT {
        let (ya, yb, fy) = pos(oy, HEIGHT, ch);
        for ox in 0..WIDTH {
            let (xa, xb, fx) = pos(ox, WIDTH, cw);
            for c in 0..3 {
                let p = |x: usize, y: usize| i64::from(src[((y0 + y) * w + x0 + x) * 3 + c]);
                let top = p(xa, ya) * (256 - fx) + p(xb, ya) * fx;
                let bot = p(xa, yb) * (256 - fx) + p(xb, yb) * fx;
                out.push(((top * (256 - fy) + bot * fy + 32768) >> 16) as u8);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jpeg_encoder::{ColorType, Encoder};

    /// Smooth colour field: red follows x, green follows y, blue is flat.
    fn field(w: usize, h: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                v.extend([(x * 255 / w) as u8, (y * 255 / h) as u8, 128]);
            }
        }
        v
    }

    fn encode(w: usize, h: usize, progressive: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = Encoder::new(&mut out, 90);
        e.set_progressive(progressive);
        e.encode(&field(w, h), w as u16, h as u16, ColorType::Rgb)
            .unwrap();
        out
    }

    /// Mean absolute error per channel against the analytic field at the crop/scale position.
    fn mean_err(p: &Photo, w: usize, h: usize) -> [f64; 3] {
        let (cw, ch) = if w * HEIGHT > h * WIDTH {
            (h * WIDTH / HEIGHT, h)
        } else {
            (w, w * HEIGHT / WIDTH)
        };
        let (x0, y0) = ((w - cw) as f64 / 2.0, (h - ch) as f64 / 2.0);
        let mut e = [0.0; 3];
        for oy in 0..HEIGHT {
            for ox in 0..WIDTH {
                let sx = x0 + (ox as f64 + 0.5) * cw as f64 / WIDTH as f64;
                let sy = y0 + (oy as f64 + 0.5) * ch as f64 / HEIGHT as f64;
                let want = [sx * 255.0 / w as f64, sy * 255.0 / h as f64, 128.0];
                for c in 0..3 {
                    e[c] += (f64::from(p.rgb[(oy * WIDTH + ox) * 3 + c]) - want[c]).abs();
                }
            }
        }
        e.map(|v| v / (WIDTH * HEIGHT) as f64)
    }

    #[test]
    fn round_trip_baseline_and_progressive() {
        // 422x280 is the planespotters large thumbnail (too wide: sides cropped); 300x300 is too tall.
        for (w, h) in [(422, 280), (336, 224), (300, 300)] {
            for progressive in [false, true] {
                let p = decode(&encode(w, h, progressive)).unwrap();
                assert_eq!(
                    (usize::from(p.width), usize::from(p.height)),
                    (WIDTH, HEIGHT)
                );
                assert_eq!(p.rgb.len(), WIDTH * HEIGHT * 3);
                let e = mean_err(&p, w, h);
                assert!(
                    e.iter().all(|&v| v < 8.0),
                    "{w}x{h} progressive={progressive}: {e:?}"
                );
            }
        }
    }

    /// Manual check against a real downloaded photo: `PHOTO=/path/x.jpg cargo test ... real_photo`.
    #[test]
    fn real_photo_if_given() {
        let Ok(path) = std::env::var("PHOTO") else {
            return;
        };
        let t = std::time::Instant::now();
        let p = decode(&std::fs::read(path).unwrap()).unwrap();
        println!(
            "decoded in {:?}, mean {:?}",
            t.elapsed(),
            p.rgb.iter().map(|&v| v as u64).sum::<u64>() / p.rgb.len() as u64
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(decode(&vec![0xFF; MAX_JPEG + 1]), Err(PhotoError::TooLarge));
        assert_eq!(decode(&[]), Err(PhotoError::Decode));
        assert_eq!(decode(b"not a jpeg at all"), Err(PhotoError::Decode));
        // A frame bigger than MAX_PIXELS is refused before the buffer is allocated.
        let mut e = Vec::new();
        Encoder::new(&mut e, 50)
            .encode(&vec![0u8; 1100 * 1000 * 3], 1100, 1000, ColorType::Rgb)
            .unwrap();
        assert!(e.len() < MAX_JPEG);
        assert_eq!(decode(&e), Err(PhotoError::TooLarge));
    }
}

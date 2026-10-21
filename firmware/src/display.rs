//! 800x480 RGB565 scanout: static test pattern and a looped DMA buffer over a PSRAM framebuffer.

use esp_hal::dma::aligned::DmaAlignedMut;
use esp_hal::dma::{
    BurstConfig, DmaDescriptor, DmaTxBuffer, ExternalBurstConfig, Owner, Preparation,
};

pub const WIDTH: usize = 800;
pub const HEIGHT: usize = 480;
pub const FB_BYTES: usize = WIDTH * HEIGHT * 2;
/// Framebuffer alignment: covers the data-cache line (32/64 B) and the 64 B external burst.
pub const FB_ALIGN: usize = 64;
pub const BURST: ExternalBurstConfig = ExternalBurstConfig::Size64;
/// Bytes per descriptor: 4095 rounded down to a burst multiple so every chunk starts burst-aligned.
const CHUNK: usize = 4032;
/// `FB_BYTES.div_ceil(CHUNK)` plus one for the split at the ring's start; the descriptors must
/// live in internal RAM.
pub const DESCRIPTORS: usize = FB_BYTES.div_ceil(CHUNK) + 1;
/// Where in the framebuffer the ring starts, in bytes (multiple of `BURST`, below `FB_BYTES`).
///
/// CALIBRATED WORKAROUND: esp-hal `Dpi::send` starts the DMA, then resets the LCD afifo and starts
/// the LCD, so the stream is offset by a fixed amount when the first pixel is clocked out. Measured
/// on the board with the 64 B-granular ring start; the photo of the first pattern showed the stream
/// 564 B early, so start 9 * 64 B later. Re-measure with the ruler (a ring start S shifts the image
/// by S bytes = S / 2 px). TODO: replace once the upstream cause is fixed (esp-rs/esp-hal DPI
/// `send` ordering, no issue filed yet).
pub const START_OFFSET: usize = 9 * 64;

const R: u16 = 0xF800;
const G: u16 = 0x07E0;
const B: u16 = 0x001F;
const Y: u16 = R | G;
const COLS_H: usize = 80;
/// Colour per hundred (0..=7); yellow is reserved for the 50 px ticks.
const HUNDREDS: [u16; 8] = [R, G, B, G | B, R | B, 0xFD20, 0x8010, 0x8410];

/// Calibration ruler (temporary diagnostic for the scanout offset, step 1.3).
///
/// * 1 px white border on all four edges.
/// * y 1..80: column ruler, alternating 10 px black/white blocks; 2 px full-height ticks at every
///   50 px (yellow) and every 100 px (red, green, blue, cyan, magenta, orange, purple, grey for
///   x = 0, 100 .. 700).
/// * y >= 80: row ruler, 10 row bands alternating black/dark grey; 2 row ticks every 50 rows
///   (yellow) and 100 rows (same colour table by y / 100), full width.
pub fn pattern_pixel(x: usize, y: usize) -> u16 {
    if x == 0 || y == 0 || x == WIDTH - 1 || y == HEIGHT - 1 {
        return 0xFFFF;
    }
    let tick = |v: usize| match (v % 100, v % 50) {
        (0..=1, _) => Some(HUNDREDS[v / 100 % 8]),
        (_, 0..=1) => Some(Y),
        _ => None,
    };
    if y < COLS_H {
        tick(x).unwrap_or(if (x / 10).is_multiple_of(2) {
            0xFFFF
        } else {
            0
        })
    } else {
        tick(y).unwrap_or(if (y / 10).is_multiple_of(2) {
            0x4208
        } else {
            0
        })
    }
}

pub fn draw_pattern(fb: &mut [u16]) {
    for (i, px) in fb.iter_mut().enumerate() {
        *px = pattern_pixel(i % WIDTH, i / WIDTH);
    }
}

/// A circular descriptor chain over a PSRAM framebuffer, so the LCD DMA never stops.
///
/// esp-hal's `DmaTxBuf` ends the chain with EOF and `DmaLoopBuf` is limited to one 4095 byte
/// descriptor, so neither can loop a full frame with `Dpi::send(true, ..)`.
pub struct LoopedFb {
    descriptors: DmaAlignedMut<'static, [DmaDescriptor]>,
    fb: DmaAlignedMut<'static, [u8]>,
}

impl LoopedFb {
    /// `fb` must be in PSRAM, `FB_ALIGN`-aligned and `FB_BYTES` long (checked by `DmaAlignedMut`).
    pub fn new(
        mut descriptors: DmaAlignedMut<'static, [DmaDescriptor]>,
        mut fb: DmaAlignedMut<'static, [u8]>,
    ) -> Self {
        assert_eq!(fb.len(), FB_BYTES);
        assert!(descriptors.len() >= DESCRIPTORS);
        const { assert!(START_OFFSET < FB_BYTES && START_OFFSET.is_multiple_of(64)) };
        let base = fb.as_mut_ptr();
        let head = descriptors.as_mut_ptr();
        // Walk the framebuffer circularly from START_OFFSET; chunks also break at the wrap point.
        let (mut off, mut left, mut n) = (START_OFFSET, FB_BYTES, 0);
        while left > 0 {
            let len = CHUNK.min(left).min(FB_BYTES - off);
            let next = if left == len {
                head
            } else {
                unsafe { head.add(n + 1) }
            };
            let d = &mut descriptors[n];
            d.buffer = unsafe { base.add(off) };
            d.next = next;
            d.set_size(len);
            d.set_length(len);
            d.set_suc_eof(false);
            d.set_owner(Owner::Dma);
            off = (off + len) % FB_BYTES;
            left -= len;
            n += 1;
        }
        assert!(n <= DESCRIPTORS);
        Self { descriptors, fb }
    }
}

// SAFETY: descriptors and framebuffer are 'static and owned by this value.
unsafe impl DmaTxBuffer for LoopedFb {
    type View = Self;
    type Final = Self;

    fn prepare(&mut self) -> Preparation {
        // DMA reads PSRAM directly, so dirty cache lines must reach it before the transfer starts.
        self.fb.writeback();
        Preparation {
            start: self.descriptors.as_mut_ptr(),
            accesses_psram: true,
            burst_transfer: BurstConfig::from(BURST),
            // Owner bit is never cleared, so the DMA must not check it on the wrap-around.
            check_owner: Some(false),
            auto_write_back: false,
        }
    }

    fn into_view(self) -> Self {
        self
    }

    fn from_view(view: Self) -> Self {
        view
    }
}

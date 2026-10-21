//! 800x480 RGB565 scanout: static test pattern and a looped DMA buffer over a PSRAM framebuffer.

use esp_hal::dma::aligned::DmaAlignedMut;
use esp_hal::dma::{
    BurstConfig, DmaDescriptor, DmaTxBuffer, ExternalBurstConfig, Owner, Preparation,
};

pub const WIDTH: usize = 800;
pub const HEIGHT: usize = 480;
pub const FB_BYTES: usize = WIDTH * HEIGHT * 2;
/// Pixel clock. 10 MHz is an exact divide of the 160 MHz PLL and gives the bounce-buffer refill
/// headroom (see docs/design/adsb-display.md, step 1.5 result); the panel was verified at 16 MHz
/// (feature `pclk-16mhz`).
pub const PCLK_MHZ: u32 = if cfg!(feature = "pclk-16mhz") { 16 } else { 10 };
/// Frame rate for the 820 x 500 total frame.
pub const REFRESH_HZ: u32 = PCLK_MHZ * 1_000_000 / (820 * 500);
/// Framebuffer alignment: covers the data-cache line (32/64 B) and the 64 B external burst.
pub const FB_ALIGN: usize = 64;
pub const BURST: ExternalBurstConfig = ExternalBurstConfig::Size64;
/// Bytes per descriptor: 4095 rounded down to a burst multiple so every chunk starts burst-aligned.
const CHUNK: usize = 4032;
/// `FB_BYTES.div_ceil(CHUNK)`; the descriptors must live in internal RAM.
pub const DESCRIPTORS: usize = FB_BYTES.div_ceil(CHUNK);

const R: u16 = 0xF800;
const G: u16 = 0x07E0;
const B: u16 = 0x001F;
const Y: u16 = R | G;
const COLS_H: usize = 80;
const BARS_H: usize = 224;
const STRIPE_H: usize = 16;
/// Bar order, left to right.
const BARS: [u16; 8] = [R, G, B, G | B, R | B, R | G, R | G | B, 0];

/// Which test pattern `draw_pattern` paints.
#[allow(dead_code)] // the unselected variant is kept as a diagnostic
pub enum Pattern {
    Bars,
    Ruler,
}
pub const PATTERN: Pattern = if cfg!(feature = "diag-ruler") {
    Pattern::Ruler
} else {
    Pattern::Bars
};

/// Pixel of the bars pattern (RGB565: R = bits 15..11, G = 10..5, B = 4..0).
///
/// * y < 224: eight 100 px bars: red, green, blue, cyan, magenta, yellow, white, black.
/// * y >= 224: sixteen 16 px stripes, top to bottom R0..R4, G0..G5, B0..B4, each with only that one
///   bit set across the full width. Brightness doubles per bit within a colour, so each colour's
///   stripes must ramp up monotonically (R0/G0/B0 are near black, R4/G5/B4 are half-bright).
/// * 1 px white border on all four edges (overrides the above).
fn bars_pixel(x: usize, y: usize) -> u16 {
    if x == 0 || y == 0 || x == WIDTH - 1 || y == HEIGHT - 1 {
        return 0xFFFF;
    }
    if y < BARS_H {
        return BARS[x / (WIDTH / 8)];
    }
    match (y - BARS_H) / STRIPE_H {
        k @ 0..=4 => 1 << (11 + k),
        k @ 5..=10 => 1 << k, // G0 is bit 5
        k => 1 << (k - 11),
    }
}

/// Colour per hundred (0..=7); yellow is reserved for the 50 px ticks.
const HUNDREDS: [u16; 8] = [R, G, B, G | B, R | B, 0xFD20, 0x8010, 0x8410];

/// Calibration ruler for checking scanout alignment and offsets.
///
/// * 1 px white border on all four edges.
/// * y 1..80: column ruler, alternating 10 px black/white blocks; 2 px full-height ticks at every
///   50 px (yellow) and every 100 px (red, green, blue, cyan, magenta, orange, purple, grey for
///   x = 0, 100 .. 700).
/// * y >= 80: row ruler, 10 row bands alternating black/dark grey; 2 row ticks every 50 rows
///   (yellow) and 100 rows (same colour table by y / 100), full width.
fn ruler_pixel(x: usize, y: usize) -> u16 {
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
        let (x, y) = (i % WIDTH, i / WIDTH);
        *px = match PATTERN {
            Pattern::Bars => bars_pixel(x, y),
            Pattern::Ruler => ruler_pixel(x, y),
        };
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
        let base = fb.as_mut_ptr();
        let head = descriptors.as_mut_ptr();
        for i in 0..DESCRIPTORS {
            let off = i * CHUNK;
            let len = CHUNK.min(FB_BYTES - off);
            let next = if i + 1 == DESCRIPTORS {
                head
            } else {
                unsafe { head.add(i + 1) }
            };
            let d = &mut descriptors[i];
            d.buffer = unsafe { base.add(off) };
            d.next = next;
            d.set_size(len);
            d.set_length(len);
            d.set_suc_eof(false);
            d.set_owner(Owner::Dma);
        }
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

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
/// `FB_BYTES.div_ceil(CHUNK)`; the descriptors must live in internal RAM.
pub const DESCRIPTORS: usize = FB_BYTES.div_ceil(CHUNK);

const R: u16 = 0xF800;
const G: u16 = 0x07E0;
const B: u16 = 0x001F;
const BARS_H: usize = 224;
const STRIPE_H: usize = 16;
/// Bar order, left to right.
const BARS: [u16; 8] = [R, G, B, G | B, R | B, R | G, R | G | B, 0];

/// Pixel of the test pattern (RGB565: R = bits 15..11, G = 10..5, B = 4..0).
///
/// * y < 224: eight 100 px bars: red, green, blue, cyan, magenta, yellow, white, black.
/// * y >= 224: sixteen 16 px stripes, top to bottom R0..R4, G0..G5, B0..B4, each with only that one
///   bit set across the full width. Brightness doubles per bit within a colour, so each colour's
///   stripes must ramp up monotonically (R0/G0/B0 are near black, R4/G5/B4 are half-bright).
/// * 1 px white border on all four edges (overrides the above).
pub fn pattern_pixel(x: usize, y: usize) -> u16 {
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

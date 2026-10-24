//! 800x480 RGB565 scanout: static test pattern and a looped DMA buffer over a PSRAM framebuffer with a restart descriptor.

use esp_hal::dma::aligned::DmaAlignedMut;
use esp_hal::dma::{
    BurstConfig, DmaDescriptor, DmaTxBuffer, ExternalBurstConfig, Owner, Preparation,
};

pub const WIDTH: usize = 800;
pub const HEIGHT: usize = 480;
pub const FB_BYTES: usize = WIDTH * HEIGHT * 2;
/// Pixel clock: 12 MHz (29 Hz refresh). At 16 MHz flash cache refills starved the PSRAM scanout reads and
/// glitched ~30 % of frames; 12 MHz gave 0 in 464 frames, 13 MHz still 21 % (radar-ui K4). The old
/// bounce-buffer path (feature `bounce`) runs at 10 MHz unless `pclk-16mhz` is also set.
pub const PCLK_MHZ: u32 = if cfg!(feature = "bounce") && !cfg!(feature = "pclk-16mhz") {
    10
} else {
    12
};
/// Frame rate for the 820 x 500 total frame.
pub const REFRESH_HZ: u32 = PCLK_MHZ * 1_000_000 / (820 * 500);
/// Framebuffer alignment: covers the data-cache line (32/64 B) and the 64 B external burst.
pub const FB_ALIGN: usize = 64;
pub const BURST: ExternalBurstConfig = ExternalBurstConfig::Size64;
/// Bytes per descriptor: 4095 rounded down to a burst multiple so every chunk starts burst-aligned.
const CHUNK: usize = 4032;
/// Continuous scanout (`next_frame_en` = 1, circular ring) for the old `restart-vsync` and `no-resync`
/// variants. Default: stopped panel, one frame per start from a linear chain (see `restart`).
pub const CONTINUOUS: bool = cfg!(any(feature = "restart-vsync", feature = "no-resync"));
/// Ring length: `FB_BYTES.div_ceil(CHUNK)`.
const RING: usize = FB_BYTES.div_ceil(CHUNK);
/// Ring plus the restart descriptor; the descriptors must live in internal RAM.
pub const DESCRIPTORS: usize = RING + 1;
/// The LCD async FIFO keeps 16 + 1 pixels across its reset, so a restart skips them (ESP-IDF
/// `LCD_FIFO_PRESERVE_SIZE_PX`, esp_lcd_panel_rgb.c).
const RESTART_SKIP_BYTES: usize = 17 * 2;

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

/// Pixel-aspect check image (RD.14.1): 1 px white border on the 800x480 edge; left a 300x300 px square
/// with its inscribed circle (square-pixel assumption), right the `round(300 / aspect)` x 300 px
/// rectangle with its inscribed ellipse. On a panel with the given aspect the right shape looks round.
pub fn aspect_pixel(x: usize, y: usize, aspect: f64) -> u16 {
    let edge = x == 0 || y == 0 || x == WIDTH - 1 || y == HEIGHT - 1;
    let w = (300.0 / aspect + 0.5) as usize;
    let outline = |x0: usize, w: usize| {
        let y0 = 90;
        let (dx, dy) = (
            x as f64 + 0.5 - x0 as f64 - w as f64 / 2.0,
            y as f64 + 0.5 - y0 as f64 - 150.0,
        );
        let rect = x >= x0 && x < x0 + w && y >= y0 && y < y0 + 300;
        let on_rect = rect && (x < x0 + 2 || x >= x0 + w - 2 || y < y0 + 2 || y >= y0 + 298);
        // Ellipse outline about 2 px thick: |d^2 - 1| ~ 2 |d - 1|, times the mean semi-axis, < 2.
        let (ex, ey) = (dx / (w as f64 / 2.0), dy / 150.0);
        let d2 = ex * ex + ey * ey;
        on_rect
            || (d2 - 1.0) * (w as f64 / 4.0 + 75.0) < 1.0
                && (1.0 - d2) * (w as f64 / 4.0 + 75.0) < 1.0
    };
    if edge || outline(50, 300) || outline(450, w) {
        0xFFFF
    } else {
        0
    }
}

pub fn draw_aspect(fb: &mut [u16], aspect: f64) {
    for (i, px) in fb.iter_mut().enumerate() {
        *px = aspect_pixel(i % WIDTH, i / WIDTH, aspect);
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

/// A descriptor chain over a PSRAM framebuffer: circular if `CONTINUOUS` (the DMA never stops), else one
/// linear frame ending in EOF.
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
        for i in 0..RING {
            let off = i * CHUNK;
            let len = CHUNK.min(FB_BYTES - off);
            let last = i + 1 == RING;
            let next = if last && !CONTINUOUS {
                core::ptr::null_mut() // linear chain: the DMA stops after the frame
            } else if last {
                head
            } else {
                unsafe { head.add(i + 1) }
            };
            let d = &mut descriptors[i];
            d.buffer = unsafe { base.add(off) };
            d.next = next;
            d.set_size(len);
            d.set_length(len);
            d.set_suc_eof(last && !CONTINUOUS);
            d.set_owner(Owner::Dma);
        }
        // Restart descriptor (used by `restart`): the rest of ring chunk 0 from 17 px in, then the
        // normal ring. Like IDF's, its start is not burst-aligned (IDF bypasses the alignment check
        // for it: "it doesn't really matter in this case").
        let r = &mut descriptors[RING];
        r.buffer = unsafe { base.add(RESTART_SKIP_BYTES) };
        r.next = unsafe { head.add(1) };
        r.set_size(CHUNK - RESTART_SKIP_BYTES);
        r.set_length(CHUNK - RESTART_SKIP_BYTES);
        r.set_suc_eof(false);
        r.set_owner(Owner::Dma);
        let restart = if CONTINUOUS {
            r as *mut DmaDescriptor
        } else {
            head
        };
        crate::restart::set_desc(restart as usize);
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

/// Writes the cache lines of rows `y0..y1` of the framebuffer at `fb` back to PSRAM so the scanout
/// DMA sees them (the single cache-maintenance point of the UI, parent R3). Rows are 1600 B, a
/// multiple of the 64 B cache line.
///
/// # Safety
/// `fb` must point to the WIDTH x HEIGHT framebuffer.
pub unsafe fn writeback_rows(fb: *const u16, y0: usize, y1: usize) {
    assert!(y0 <= y1 && y1 <= HEIGHT);
    // SAFETY: inside the framebuffer; a shared slice is only used to name the range to write back.
    let rows = unsafe { core::slice::from_raw_parts(fb.add(y0 * WIDTH), (y1 - y0) * WIDTH) };
    unsafe { esp_hal::dma::aligned::DmaAlignedRef::new_unchecked(rows) }.writeback();
}

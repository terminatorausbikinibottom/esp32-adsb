//! Bounce-buffer scanout (design M2): the LCD DMA reads two small internal-SRAM buffers in a ring,
//! and a GDMA out-EOF handler on core 1 refills the buffer that just finished from the PSRAM
//! framebuffer. The CPU reads PSRAM through the (shared) data cache and writes uncached SRAM, so no
//! cache writeback is needed on this path.
//!
//! Frame sync: both buffers are prefilled with rows 0..2*BB_ROWS before the DMA starts. Every
//! EOF then refills the finished buffer with chunk `fill` (BB_ROWS rows) and `fill` counts mod
//! `CHUNKS`.
//!
//! The S3 LCD_CAM/GDMA pair can lose its frame alignment for good (ESP-IDF works around it in
//! `lcd_rgb_panel_try_restart_transmission`), so the VSYNC interrupt restarts the DMA at the start of
//! buffer 0 every frame (`resync`). Feature `no-resync` keeps only the old counter correction.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed};

use esp_hal::dma::{
    BurstConfig, DmaDescriptor, DmaTxBuffer, InternalBurstConfig, Owner, Preparation,
};
use esp_hal::interrupt;
use esp_hal::peripherals::Interrupt;
use esp_hal::time::Instant;

use crate::display::{FB_BYTES, HEIGHT, WIDTH};

/// Rows per bounce buffer. Does not change the CPU load (it is set by PCLK), only how much
/// interrupt-latency jitter a refill tolerates: slack = buffer time - copy time.
pub const BB_ROWS: usize = 10;
pub const BB_BYTES: usize = WIDTH * 2 * BB_ROWS;
pub const CHUNKS: usize = HEIGHT / BB_ROWS;
/// Max bytes per descriptor (hardware limit 4095), a multiple of 4.
const DESC_BYTES: usize = 4000;
const BB_DESCS: usize = BB_BYTES.div_ceil(DESC_BYTES);
/// Two buffers plus the restart descriptor.
pub const DESCRIPTORS: usize = 2 * BB_DESCS + 1;
/// The async FIFO keeps LCD_FIFO_DEPTH (16) + 1 pixels of the next frame across a reset, so the
/// restart skips them (ESP-IDF `LCD_FIFO_PRESERVE_SIZE_PX`).
const RESTART_SKIP_BYTES: usize = 17 * 2;

// Even chunk count: the frame then starts on the same buffer every time.
const _: () = assert!(
    HEIGHT.is_multiple_of(BB_ROWS) && CHUNKS.is_multiple_of(2) && BB_BYTES.is_multiple_of(32)
);

/// GDMA channel the LCD uses (`DMA_CH0` in main, interrupt `DMA_OUT_CH0`).
const CH: usize = 0;

struct State {
    fb: AtomicUsize,
    /// Address of each buffer's last (EOF) descriptor; identifies which buffer an EOF belongs to.
    eof_desc: [AtomicUsize; 2],
    bufs: [AtomicUsize; 2],
    /// Chunk currently held by each buffer.
    loaded: [AtomicU32; 2],
    /// Descriptor that restarts the DMA inside buffer 0.
    restart_desc: AtomicUsize,
    /// Buffer the next EOF is expected from, and the chunk to load into it.
    next_buf: AtomicU32,
    fill: AtomicU32,
    handlers_ready: AtomicBool,
    // Statistics, reset by `take_stats`. Written only from the two handlers (same priority level,
    // so they never preempt each other).
    refills: AtomicU32,
    busy_us: AtomicU32,
    max_us: AtomicU32,
    late: AtomicU32,
    seq_err: AtomicU32,
    vsyncs: AtomicU32,
    vsync_fix: AtomicU32,
}

#[allow(clippy::declare_interior_mutable_const)]
const Z: AtomicU32 = AtomicU32::new(0);
#[allow(clippy::declare_interior_mutable_const)]
const ZU: AtomicUsize = AtomicUsize::new(0);

static STATE: State = State {
    fb: ZU,
    eof_desc: [ZU; 2],
    bufs: [ZU; 2],
    loaded: [Z; 2],
    restart_desc: ZU,
    next_buf: Z,
    fill: Z,
    handlers_ready: AtomicBool::new(false),
    refills: Z,
    busy_us: Z,
    max_us: Z,
    late: Z,
    seq_err: Z,
    vsyncs: Z,
    vsync_fix: Z,
};

/// Counters since the previous call.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub refills: u32,
    pub busy_us: u32,
    pub max_us: u32,
    /// Another EOF arrived while a refill was still copying (the DMA overtook the CPU).
    pub late: u32,
    /// The EOF came from a different buffer than expected (an EOF was missed).
    pub seq_err: u32,
    pub vsyncs: u32,
    /// VSYNC found the fill counter off and corrected it.
    pub vsync_fix: u32,
}

pub fn take_stats() -> Stats {
    Stats {
        refills: STATE.refills.swap(0, Relaxed),
        busy_us: STATE.busy_us.swap(0, Relaxed),
        max_us: STATE.max_us.swap(0, Relaxed),
        late: STATE.late.swap(0, Relaxed),
        seq_err: STATE.seq_err.swap(0, Relaxed),
        vsyncs: STATE.vsyncs.swap(0, Relaxed),
        vsync_fix: STATE.vsync_fix.swap(0, Relaxed),
    }
}

pub fn handlers_ready() -> bool {
    STATE.handlers_ready.load(Relaxed)
}

/// Binds both handlers to the *calling* core's interrupt map; call from the core that should
/// service them (core 1).
pub fn bind_handlers() {
    interrupt::bind_handler(Interrupt::DMA_OUT_CH0, dma_out_eof);
    interrupt::bind_handler(Interrupt::LCD_CAM, lcd_vsync);
    STATE.handlers_ready.store(true, Relaxed);
}

/// ESP-IDF's RGB panel sets `lcd_always_out_en` (the DOUT phase then lasts until LCD_START is cleared,
/// not `lcd_dout_cyclelen` cycles); esp-hal does not. Call before `Dpi::send` (it latches with
/// `lcd_update`).
pub fn configure_lcd() {
    if cfg!(feature = "exp-no-always-out") {
        return;
    }
    // SAFETY: only this bit is touched, once, before the transfer starts.
    unsafe {
        (*esp32s3::LCD_CAM::ptr())
            .lcd_user()
            .modify(|_, w| w.lcd_always_out_en().set_bit());
    }
}

/// Unmasks the out-EOF and VSYNC sources. Call before `Dpi::send` (its DMA reset only clears raw
/// status, not the enables).
pub fn enable_interrupts() {
    // SAFETY: only these two enable bits are touched, once, before the transfer starts.
    unsafe {
        (*esp32s3::DMA::ptr())
            .ch(CH)
            .out_int()
            .ena()
            .modify(|_, w| w.out_eof().set_bit());
        (*esp32s3::LCD_CAM::ptr())
            .lc_dma_int_ena()
            .modify(|_, w| w.lcd_vsync_int_ena().set_bit());
    }
}

fn now_us() -> u32 {
    Instant::now().duration_since_epoch().as_micros() as u32
}

/// Copies `words` u32 from PSRAM to SRAM, 8 loads then 8 stores per iteration. Written out so LLVM
/// does not turn it into a call to a generic `memcpy`.
///
/// # Safety
/// Both pointers 4-aligned and valid for `words` u32; `words` a multiple of 8.
#[esp_hal::ram]
unsafe fn copy_words(mut dst: *mut u32, mut src: *const u32, words: usize) {
    for _ in 0..words / 8 {
        unsafe {
            let (a, b, c, d) = (
                src.read(),
                src.add(1).read(),
                src.add(2).read(),
                src.add(3).read(),
            );
            let (e, f, g, h) = (
                src.add(4).read(),
                src.add(5).read(),
                src.add(6).read(),
                src.add(7).read(),
            );
            dst.write(a);
            dst.add(1).write(b);
            dst.add(2).write(c);
            dst.add(3).write(d);
            dst.add(4).write(e);
            dst.add(5).write(f);
            dst.add(6).write(g);
            dst.add(7).write(h);
            src = src.add(8);
            dst = dst.add(8);
        }
    }
}

#[esp_hal::ram]
fn load_chunk(buf: usize, chunk: u32) {
    let src = STATE.fb.load(Relaxed) + chunk as usize * BB_BYTES;
    // SAFETY: `buf` is a bounce buffer, `chunk < CHUNKS` keeps `src` inside the framebuffer.
    unsafe { copy_words(buf as *mut u32, src as *const u32, BB_BYTES / 4) };
}

#[esp_hal::handler(priority = esp_hal::interrupt::Priority::Priority2)]
fn dma_out_eof() {
    on_eof();
}

// The handler stub is in flash; the refill itself runs from IRAM.
#[esp_hal::ram]
fn on_eof() {
    let t0 = now_us();
    // SAFETY: shared register blocks; this handler only touches this channel's EOF bits.
    let ch = unsafe { (*esp32s3::DMA::ptr()).ch(CH) };
    ch.out_int().clr().write(|w| w.out_eof().clear_bit_by_one());
    let eof_addr = ch.out_eof_des_addr().read().out_eof_des_addr().bits() as usize;

    let mut buf = STATE.next_buf.load(Relaxed);
    let mut fill = STATE.fill.load(Relaxed);
    let done = (0..2).find(|&b| STATE.eof_desc[b].load(Relaxed) == eof_addr);
    if let Some(done) = done
        && done as u32 != buf
    {
        // An EOF was missed (the refill for the other buffer never ran): skip its chunk.
        STATE.seq_err.fetch_add(1, Relaxed);
        buf = done as u32;
        fill = (fill + 1) % CHUNKS as u32;
    }
    // diag-norefill: leave the buffers as prefilled (rows 0..2*BB_ROWS repeated).
    if !cfg!(feature = "diag-norefill") {
        load_chunk(STATE.bufs[buf as usize].load(Relaxed), fill);
    }
    STATE.loaded[buf as usize].store(fill, Relaxed);
    STATE.fill.store((fill + 1) % CHUNKS as u32, Relaxed);
    STATE.next_buf.store(buf ^ 1, Relaxed);

    // A second EOF while copying means the DMA already moved on into the buffer being refilled.
    if ch.out_int().raw().read().out_eof().bit_is_set() {
        STATE.late.fetch_add(1, Relaxed);
    }
    let dt = now_us().wrapping_sub(t0);
    STATE.refills.fetch_add(1, Relaxed);
    STATE.busy_us.fetch_add(dt, Relaxed);
    if dt > STATE.max_us.load(Relaxed) {
        STATE.max_us.store(dt, Relaxed);
    }
}

#[esp_hal::handler(priority = esp_hal::interrupt::Priority::Priority2)]
fn lcd_vsync() {
    on_vsync();
}

#[esp_hal::ram]
fn on_vsync() {
    // SAFETY: shared register block; only the VSYNC bits are touched.
    let lcd = unsafe { &*esp32s3::LCD_CAM::ptr() };
    lcd.lc_dma_int_clr()
        .write(|w| w.lcd_vsync_int_clr().set_bit());
    STATE.vsyncs.fetch_add(1, Relaxed);
    if cfg!(feature = "no-resync") {
        // Between frames the DMA is stalled inside buffer 0 with no EOF yet: the next EOF refills
        // buffer 0 with chunk 2. Only correct the chunk counter, and only when the buffer agrees;
        // otherwise an EOF is probably still pending and the state is not ours to judge.
        if STATE.next_buf.load(Relaxed) == 0 && STATE.fill.load(Relaxed) != 2 {
            STATE.fill.store(2, Relaxed);
            STATE.vsync_fix.fetch_add(1, Relaxed);
        }
    } else {
        resync(lcd);
    }
}

/// Restarts the DMA at the start of buffer 0 inside the vertical blanking, as ESP-IDF's RGB panel
/// does on the S3: FIFO reset, GDMA reset, restart descriptor that skips the pixels the FIFO kept.
/// If this runs after the LCD took the first pixels the frame shifts for one frame (IDF has the
/// same limit).
#[esp_hal::ram]
fn resync(lcd: &esp32s3::lcd_cam::RegisterBlock) {
    // Normally both buffers already hold chunks 0 and 1; only copy if a refill is still pending.
    for b in 0..2 {
        if STATE.loaded[b].load(Relaxed) != b as u32 {
            load_chunk(STATE.bufs[b].load(Relaxed), b as u32);
            STATE.loaded[b].store(b as u32, Relaxed);
        }
    }
    // SAFETY: shared register block; only this channel's out registers and its EOF bit are touched.
    let ch = unsafe { (*esp32s3::DMA::ptr()).ch(CH) };
    lcd.lcd_misc().modify(|_, w| w.lcd_afifo_reset().set_bit());
    ch.out_conf0().modify(|_, w| w.out_rst().set_bit());
    ch.out_conf0().modify(|_, w| w.out_rst().clear_bit());
    ch.out_link().modify(|_, w| unsafe {
        w.outlink_addr()
            .bits(STATE.restart_desc.load(Relaxed) as u32)
    });
    ch.out_link().modify(|_, w| w.outlink_start().set_bit());
    ch.out_int().clr().write(|w| w.out_eof().clear_bit_by_one());
    STATE.next_buf.store(0, Relaxed);
    STATE.fill.store(2, Relaxed);
}

/// Two bounce buffers in a circular descriptor chain, EOF on the last descriptor of each buffer.
pub struct BounceFb {
    descriptors: &'static mut [DmaDescriptor; DESCRIPTORS],
    _bufs: &'static mut [[u8; BB_BYTES]; 2],
}

impl BounceFb {
    /// `bufs` and `descriptors` must be in internal RAM; `fb` is the `FB_BYTES` PSRAM framebuffer.
    pub fn new(
        descriptors: &'static mut [DmaDescriptor; DESCRIPTORS],
        bufs: &'static mut [[u8; BB_BYTES]; 2],
        fb: &'static [u8],
    ) -> Self {
        assert_eq!(fb.len(), FB_BYTES);
        assert!(
            bufs.as_ptr().cast::<u32>().is_aligned(),
            "bounce buffers must be 4-aligned"
        );
        STATE.fb.store(fb.as_ptr() as usize, Relaxed);
        let head = descriptors.as_mut_ptr();
        for (i, d) in descriptors[..2 * BB_DESCS].iter_mut().enumerate() {
            let (b, k) = (i / BB_DESCS, i % BB_DESCS);
            let len = DESC_BYTES.min(BB_BYTES - k * DESC_BYTES);
            d.buffer = unsafe { bufs[b].as_mut_ptr().add(k * DESC_BYTES) };
            d.next = unsafe { head.add((i + 1) % (2 * BB_DESCS)) };
            d.set_size(len);
            d.set_length(len);
            d.set_suc_eof(k == BB_DESCS - 1 && !cfg!(feature = "exp-solid-noeof"));
            d.set_owner(Owner::Dma);
        }
        // Restart descriptor: rest of buffer 0's first descriptor, then the normal chain (EOF at its end).
        let r = &mut descriptors[2 * BB_DESCS];
        r.buffer = unsafe { bufs[0].as_mut_ptr().add(RESTART_SKIP_BYTES) };
        r.next = unsafe { head.add(1) };
        r.set_size(DESC_BYTES - RESTART_SKIP_BYTES);
        r.set_length(DESC_BYTES - RESTART_SKIP_BYTES);
        r.set_suc_eof(false);
        r.set_owner(Owner::Dma);
        STATE
            .restart_desc
            .store(r as *mut DmaDescriptor as usize, Relaxed);
        for (b, buf) in bufs.iter().enumerate() {
            STATE.bufs[b].store(buf.as_ptr() as usize, Relaxed);
            STATE.eof_desc[b].store(
                unsafe { head.add(b * BB_DESCS + BB_DESCS - 1) } as usize,
                Relaxed,
            );
        }
        Self {
            descriptors,
            _bufs: bufs,
        }
    }
}

// SAFETY: descriptors and buffers are 'static, internal RAM and owned by this value; the handlers
// only write the buffer the DMA has finished with.
unsafe impl DmaTxBuffer for BounceFb {
    type View = Self;
    type Final = Self;

    fn prepare(&mut self) -> Preparation {
        // Called by `Dpi::send` after the LCD reset and right before the DMA starts, so the first
        // bytes are already in SRAM when the DMA begins: no PSRAM latency at start-up.
        if cfg!(feature = "exp-solid") {
            // Test ring: buffer 0 red, buffer 1 blue (RGB565, little endian), never refilled.
            for (b, px) in [(0, 0xF800u16), (1, 0x001F)] {
                let p = STATE.bufs[b].load(Relaxed) as *mut u16;
                for i in 0..BB_BYTES / 2 {
                    unsafe { p.add(i).write(px) };
                }
            }
        } else {
            load_chunk(STATE.bufs[0].load(Relaxed), 0);
            load_chunk(STATE.bufs[1].load(Relaxed), 1);
        }
        STATE.loaded[0].store(0, Relaxed);
        STATE.loaded[1].store(1, Relaxed);
        STATE.next_buf.store(0, Relaxed);
        STATE.fill.store(2, Relaxed);
        Preparation {
            start: self.descriptors.as_mut_ptr(),
            accesses_psram: false,
            // IDF enables data burst for internal memory too; esp-hal defaults to off.
            burst_transfer: BurstConfig::from(if cfg!(feature = "exp-no-burst") {
                InternalBurstConfig::Disabled
            } else {
                InternalBurstConfig::Enabled
            }),
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

impl BounceFb {
    /// SRAM taken by the buffers and descriptors (statics, not heap).
    pub const SRAM_BYTES: usize = 2 * BB_BYTES + DESCRIPTORS * size_of::<DmaDescriptor>();
}

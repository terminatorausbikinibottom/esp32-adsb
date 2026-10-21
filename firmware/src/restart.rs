//! Per-VBLANK DMA restart for the direct PSRAM ring, as ESP-IDF does on the S3
//! (`lcd_rgb_panel_try_restart_transmission`, esp_lcd_panel_rgb.c): on the VSYNC interrupt reset the
//! LCD async FIFO and the GDMA out channel, then start the DMA at the restart descriptor (the
//! framebuffer start plus the 17 pixels the FIFO keeps), which chains into the normal ring. This
//! pins the frame start phase every frame, so a start-up latency race or a lost beat cannot persist.
//! If the interrupt runs after the LCD took the first pixels, one frame shifts (IDF has the same limit).

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed};

use esp_hal::interrupt;
use esp_hal::peripherals::Interrupt;
use esp_hal::time::Instant;

/// GDMA channel the LCD uses (`DMA_CH0` in main).
const CH: usize = 0;

static RESTART_DESC: AtomicUsize = AtomicUsize::new(0);
static READY: AtomicBool = AtomicBool::new(false);
// Written only by the handler; read and cleared by `take_stats` (a torn pair costs one sample).
static RESTARTS: AtomicU32 = AtomicU32::new(0);
static BUSY_US: AtomicU32 = AtomicU32::new(0);
static MAX_US: AtomicU32 = AtomicU32::new(0);

/// Counters since the previous call.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub restarts: u32,
    pub busy_us: u32,
    pub max_us: u32,
}

pub fn take_stats() -> Stats {
    Stats {
        restarts: RESTARTS.swap(0, Relaxed),
        busy_us: BUSY_US.swap(0, Relaxed),
        max_us: MAX_US.swap(0, Relaxed),
    }
}

pub(crate) fn set_desc(addr: usize) {
    RESTART_DESC.store(addr, Relaxed);
}

pub fn handler_ready() -> bool {
    READY.load(Relaxed)
}

/// Binds the handler to the *calling* core's interrupt map; call from the core that should service it.
pub fn bind_handler() {
    interrupt::bind_handler(Interrupt::LCD_CAM, lcd_vsync);
    READY.store(true, Relaxed);
}

/// Unmasks the VSYNC source. Call before `Dpi::send` (its resets only clear raw status).
pub fn enable_interrupt() {
    // SAFETY: only this enable bit is touched, once, before the transfer starts.
    unsafe {
        (*esp32s3::LCD_CAM::ptr())
            .lc_dma_int_ena()
            .modify(|_, w| w.lcd_vsync_int_ena().set_bit());
    }
}

fn now_us() -> u32 {
    Instant::now().duration_since_epoch().as_micros() as u32
}

#[esp_hal::handler(priority = esp_hal::interrupt::Priority::Priority2)]
fn lcd_vsync() {
    restart();
}

// The handler stub is in flash; the restart itself runs from IRAM.
#[esp_hal::ram]
fn restart() {
    let t0 = now_us();
    // SAFETY: shared register blocks; only the VSYNC bit, the FIFO reset bit and this channel's out
    // registers are touched, and the DMA is owned by the running scanout.
    let (lcd, ch) = unsafe { (&*esp32s3::LCD_CAM::ptr(), (*esp32s3::DMA::ptr()).ch(CH)) };
    lcd.lc_dma_int_clr()
        .write(|w| w.lcd_vsync_int_clr().set_bit());
    lcd.lcd_misc().modify(|_, w| w.lcd_afifo_reset().set_bit());
    ch.out_conf0().modify(|_, w| w.out_rst().set_bit());
    ch.out_conf0().modify(|_, w| w.out_rst().clear_bit());
    ch.out_link()
        .modify(|_, w| unsafe { w.outlink_addr().bits(RESTART_DESC.load(Relaxed) as u32) });
    ch.out_link().modify(|_, w| w.outlink_start().set_bit());

    let dt = now_us().wrapping_sub(t0);
    RESTARTS.fetch_add(1, Relaxed);
    BUSY_US.fetch_add(dt, Relaxed);
    if dt > MAX_US.load(Relaxed) {
        MAX_US.store(dt, Relaxed);
    }
}

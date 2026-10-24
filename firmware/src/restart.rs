//! Per-frame restart of the scanout from the VSYNC interrupt (core 1). Two modes, see `display::CONTINUOUS`:
//!
//! * Stopped panel (default): the LCD runs one frame per start (`next_frame_en` = 0) over a linear
//!   one-frame descriptor chain. The VSYNC interrupt fires inside the vertical blank, so the handler
//!   waits for the LCD to finish the frame (`lcd_start` self-clears), then does exactly what ESP-IDF's
//!   `esp_lcd_rgb_panel_refresh` (refresh on demand, `lcd_rgb_panel_start_transmission`) does: GDMA
//!   reset, LCD stop + reset, async FIFO reset, GDMA start at the frame head, 1 us delay, LCD start.
//!   Every frame therefore starts from a known empty FIFO.
//! * Continuous (`restart-vsync`): ESP-IDF's stream-mode `lcd_rgb_panel_try_restart_transmission`:
//!   reset the FIFO and GDMA while the LCD runs on and restart at a descriptor 17 px into the
//!   framebuffer (the pixels the FIFO keeps). Measured: the kept count varies, so the frame start
//!   shifts by 14..34 px from frame to frame.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed};

use esp_hal::interrupt;
use esp_hal::peripherals::Interrupt;
use esp_hal::time::Instant;

use crate::display;

/// GDMA channel the LCD uses (`DMA_CH0` in main).
const CH: usize = 0;

static RESTART_DESC: AtomicUsize = AtomicUsize::new(0);
static READY: AtomicBool = AtomicBool::new(false);
static FIRST_RESTART: AtomicBool = AtomicBool::new(false);
// Written only by the handler; read and cleared by `take_stats` (a torn pair costs one sample).
static RESTARTS: AtomicU32 = AtomicU32::new(0);
static BUSY_US: AtomicU32 = AtomicU32::new(0);
static WAIT_US: AtomicU32 = AtomicU32::new(0);
static MAX_US: AtomicU32 = AtomicU32::new(0);

/// Counters since the previous call.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub restarts: u32,
    pub busy_us: u32,
    /// Part of `busy_us` spent waiting for the LCD to finish the frame (stopped-panel mode).
    pub wait_us: u32,
    pub max_us: u32,
}

pub fn take_stats() -> Stats {
    Stats {
        restarts: RESTARTS.swap(0, Relaxed),
        busy_us: BUSY_US.swap(0, Relaxed),
        wait_us: WAIT_US.swap(0, Relaxed),
        max_us: MAX_US.swap(0, Relaxed),
    }
}

pub(crate) fn set_desc(addr: usize) {
    RESTART_DESC.store(addr, Relaxed);
}

pub fn handler_ready() -> bool {
    READY.load(Relaxed)
}

/// True once the handler has started the first restarted frame (the one after `Dpi::send`'s own).
pub fn first_frame_started() -> bool {
    FIRST_RESTART.load(Relaxed)
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

fn reset_dma(ch: &esp32s3::dma::ch::CH) {
    ch.out_conf0().modify(|_, w| w.out_rst().set_bit());
    ch.out_conf0().modify(|_, w| w.out_rst().clear_bit());
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
    let mut wait = 0;
    if display::CONTINUOUS {
        reset_dma(ch);
    } else {
        // The frame ends after the vertical blank (12 lines: ~0.8 ms at 12 MHz, ~0.6 ms at 16 MHz); the bound only guards a hung LCD.
        let tw = now_us();
        while lcd.lcd_user().read().lcd_start().bit_is_set() && now_us().wrapping_sub(tw) < 3000 {}
        wait = now_us().wrapping_sub(tw);
        reset_dma(ch);
        lcd.lcd_user().modify(|_, w| w.lcd_start().clear_bit());
        lcd.lcd_user().modify(|_, w| w.lcd_reset().set_bit());
        lcd.lcd_misc().modify(|_, w| w.lcd_afifo_reset().set_bit());
    }
    ch.out_link()
        .modify(|_, w| unsafe { w.outlink_addr().bits(RESTART_DESC.load(Relaxed) as u32) });
    ch.out_link().modify(|_, w| w.outlink_start().set_bit());
    if !display::CONTINUOUS {
        // IDF: "delay 1us is sufficient for DMA to pass data to LCD FIFO".
        let t = now_us();
        while now_us().wrapping_sub(t) < 1 {}
        lcd.lcd_user().modify(|_, w| {
            w.lcd_update().set_bit();
            w.lcd_start().set_bit()
        });
    }

    FIRST_RESTART.store(true, Relaxed);
    let dt = now_us().wrapping_sub(t0);
    RESTARTS.fetch_add(1, Relaxed);
    BUSY_US.fetch_add(dt, Relaxed);
    WAIT_US.fetch_add(wait, Relaxed);
    if dt > MAX_US.load(Relaxed) {
        MAX_US.store(dt, Relaxed);
    }
}

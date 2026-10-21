#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use adsb_core as _; // linked for now; used from Phase 3
use core::alloc::Layout;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_alloc::MemoryCapability;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::psram::{Psram, PsramConfig, PsramMode, SpiRamFreq};
use esp_hal::time::Instant;
use esp_hal::timer::timg::TimerGroup;
use log::info;
use modes as _;

extern crate alloc;

/// Data-bus window the S3 maps external RAM into (up to 32 MB).
const PSRAM_VADDR: core::ops::Range<usize> = 0x3C00_0000..0x3E00_0000;
/// One RGB565 800x480 framebuffer.
const FB_BYTES: usize = 800 * 480 * 2;

fn log_heap(tag: &str) {
    let internal = esp_alloc::HEAP.free_caps(MemoryCapability::Internal.into());
    let external = esp_alloc::HEAP.free_caps(MemoryCapability::External.into());
    let used = esp_alloc::HEAP.used();
    info!("{tag}: heap internal_free={internal} psram_free={external} used_total={used}");
}

/// Address-dependent word pattern XOR a walking one, over the whole buffer.
fn psram_pattern_test(buf: *mut u8, len: usize) -> bool {
    let words = buf.cast::<u32>();
    let n = len / 4;
    let expect = |i: usize| (words as usize + i * 4) as u32 ^ (1u32 << (i % 32));
    let t = Instant::now();
    for i in 0..n {
        unsafe { words.add(i).write_volatile(expect(i)) };
    }
    let write_ms = t.elapsed().as_millis();
    let t = Instant::now();
    let mut bad = 0usize;
    for i in 0..n {
        let got = unsafe { words.add(i).read_volatile() };
        if got != expect(i) {
            if bad == 0 {
                info!(
                    "psram: first mismatch word={i} got={got:#010x} want={:#010x}",
                    expect(i)
                );
            }
            bad += 1;
        }
    }
    let read_ms = t.elapsed().as_millis();
    info!(
        "psram: pattern {} bytes write={write_ms}ms read={read_ms}ms mismatches={bad}",
        n * 4
    );
    info!(
        "psram: pattern test {}",
        if bad == 0 { "PASS" } else { "FAIL" }
    );
    bad == 0
}

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.4.0
    // generator parameters: -o esp32s3 -o esp32s3-wroom-1-octal-psram -o embassy -o unstable-hal -o alloc -o wifi -o log -o esp-backtrace

    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // The following pins are used to bootstrap the chip. They are available
    // for use, but check the datasheet of the module for more information on them.
    // - GPIO0
    // - GPIO3
    // - GPIO45
    // - GPIO46
    // These GPIO pins are in use by some feature of the module and should not be used.
    let _gpio27 = peripherals.GPIO27;
    let _gpio28 = peripherals.GPIO28;
    let _gpio29 = peripherals.GPIO29;
    let _gpio30 = peripherals.GPIO30;
    let _gpio31 = peripherals.GPIO31;
    let _gpio32 = peripherals.GPIO32;
    let _gpio33 = peripherals.GPIO33;
    let _gpio34 = peripherals.GPIO34;
    let _gpio35 = peripherals.GPIO35;
    let _gpio36 = peripherals.GPIO36;
    let _gpio37 = peripherals.GPIO37;

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    log_heap("boot(internal only)");

    // Octal mode forced (auto-detect is "not entirely reliable"); 80 MHz is the module's max rated
    // octal speed. The PSRAM heap region is added after the internal one, which stays the default.
    let psram = Psram::new(
        peripherals.PSRAM,
        PsramConfig {
            mode: PsramMode::OctalSpi,
            ram_frequency: SpiRamFreq::Freq80m,
            ..Default::default()
        },
    );
    esp_alloc::psram_allocator!(&psram);
    let (psram_start, psram_size) = psram.raw_parts();
    info!(
        "psram: start={psram_start:p} size={} MB ({psram_size} bytes)",
        psram_size / 1024 / 1024
    );
    log_heap("psram");

    let layout = Layout::from_size_align(FB_BYTES, 32).unwrap();
    let fb = unsafe { esp_alloc::HEAP.alloc_caps(MemoryCapability::External.into(), layout) };
    assert!(!fb.is_null(), "psram: {FB_BYTES} byte alloc failed");
    let in_psram = PSRAM_VADDR.contains(&(fb as usize));
    info!("psram: fb ptr={fb:p} len={FB_BYTES} in_psram_range={in_psram}");
    assert!(in_psram, "psram: allocation did not land in PSRAM");
    log_heap("psram(fb allocated)");
    psram_pattern_test(fb, FB_BYTES);
    unsafe { core::alloc::GlobalAlloc::dealloc(&esp_alloc::HEAP, fb, layout) };
    log_heap("psram(fb freed)");

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    info!("Embassy initialized!");

    let _wifi_controller =
        esp_radio::wifi::WifiController::new(peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");
    let _wifi_interface = esp_radio::wifi::Interface::station();

    // TODO: Spawn some tasks
    let _ = spawner;

    loop {
        info!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.2.2/examples
}

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
use esp_hal::Async;
use esp_hal::clock::CpuClock;
use esp_hal::dma::DmaDescriptor;
use esp_hal::dma::aligned::{DmaAlignedMut, InternalMemory};
use esp_hal::gpio::Level;
use esp_hal::i2c::master::{Config as I2cConfig, I2c, SoftwareTimeout};
use esp_hal::lcd_cam::LcdCam;
use esp_hal::lcd_cam::lcd::dpi::{self, Dpi, Format, FrameTiming};
use esp_hal::lcd_cam::lcd::{ClockMode, Phase, Polarity};
use esp_hal::psram::{Psram, PsramConfig, PsramMode, SpiRamFreq};
use esp_hal::time::{Duration as HalDuration, Rate};
use esp_hal::timer::timg::TimerGroup;
use log::info;
use modes as _;

use firmware::ch422g::{self, Ch422g};
use firmware::display::{self, LoopedFb};
use static_cell::StaticCell;

extern crate alloc;

/// Data-bus window the S3 maps external RAM into (up to 32 MB).
const PSRAM_VADDR: core::ops::Range<usize> = 0x3C00_0000..0x3E00_0000;

fn log_heap(tag: &str) {
    let internal = esp_alloc::HEAP.free_caps(MemoryCapability::Internal.into());
    let external = esp_alloc::HEAP.free_caps(MemoryCapability::External.into());
    let used = esp_alloc::HEAP.used();
    info!("{tag}: heap internal_free={internal} psram_free={external} used_total={used}");
}

/// Probes 0x08..=0x77 with an empty write; CH422G ACKs on several addresses in 0x20..0x3F.
#[allow(
    clippy::large_stack_frames,
    reason = "esp-hal's inlined async I2C transaction inflates the future, which lives in task storage"
)]
async fn i2c_scan(i2c: &mut I2c<'_, Async>) {
    for addr in 0x08u8..=0x77 {
        if i2c.write_async(addr, &[]).await.is_ok() {
            info!("i2c: found {addr:#04x}");
        }
    }
    info!("i2c: scan done");
}

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
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
    let (psram_start, psram_size) = psram.raw_parts();
    assert!(psram_size > 0, "psram: init failed, size 0");
    esp_alloc::psram_allocator!(&psram);
    info!(
        "psram: start={psram_start:p} size={} MB ({psram_size} bytes)",
        psram_size / 1024 / 1024
    );
    log_heap("psram");

    let layout = Layout::from_size_align(display::FB_BYTES, display::FB_ALIGN).unwrap();
    let fb_ptr = unsafe { esp_alloc::HEAP.alloc_caps(MemoryCapability::External.into(), layout) };
    assert!(!fb_ptr.is_null(), "psram: framebuffer alloc failed");
    assert!(
        PSRAM_VADDR.contains(&(fb_ptr as usize)),
        "psram: allocation did not land in PSRAM"
    );
    // Never freed: the LCD DMA reads it for the life of the program.
    let fb: &'static mut [u8] =
        unsafe { core::slice::from_raw_parts_mut(fb_ptr, display::FB_BYTES) };
    info!("psram: fb ptr={fb_ptr:p} len={}", display::FB_BYTES);
    // SAFETY: 64-byte aligned, 2-byte multiple length, exclusively owned.
    display::draw_pattern(unsafe {
        core::slice::from_raw_parts_mut(fb_ptr.cast::<u16>(), display::WIDTH * display::HEIGHT)
    });
    log_heap("psram(fb drawn)");

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    info!("Embassy initialized!");
    log_heap("rtos started");

    let _wifi_controller =
        esp_radio::wifi::WifiController::new(peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");
    let _wifi_interface = esp_radio::wifi::Interface::station();
    log_heap("wifi init");

    // Async I2C0; touch_task will take over the bus (and the expander) in step 1.6.
    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default()
            .with_frequency(Rate::from_khz(400))
            // A stuck bus would otherwise hang silently.
            .with_software_timeout(SoftwareTimeout::Transaction(HalDuration::from_millis(50))),
    )
    .expect("i2c config")
    .with_sda(peripherals.GPIO8)
    .with_scl(peripherals.GPIO9)
    .into_async();
    i2c_scan(&mut i2c).await;

    let mut expander = Ch422g::new(i2c);
    // EXIO5 low = USB stays selected. Backlight stays off until scanout runs (no garbage flash).
    let initial = (1 << ch422g::TP_RST) | (1 << ch422g::LCD_RST) | (1 << ch422g::SD_CS);
    expander.set_mode_output().await.expect("ch422g: set mode");
    expander
        .write_outputs(initial)
        .await
        .expect("ch422g: write outputs");
    info!("ch422g: outputs={initial:#04x} (EXIO5 low = USB)");

    // Panel reset: low 10 ms, high, then 100 ms before the first frame.
    expander
        .set_pin(ch422g::LCD_RST, false)
        .await
        .expect("lcd reset low");
    Timer::after(Duration::from_millis(10)).await;
    expander
        .set_pin(ch422g::LCD_RST, true)
        .await
        .expect("lcd reset high");
    Timer::after(Duration::from_millis(100)).await;

    // Waveshare ESP-IDF values: HSYNC/VSYNC pulse 4, back porch 8, front porch 8. esp-hal's
    // "front porch" fields are really line start to first active pixel, i.e. pulse + back porch.
    let lcd_cam = LcdCam::new(peripherals.LCD_CAM);
    let dpi_config = dpi::Config::default()
        .with_frequency(Rate::from_mhz(16))
        // IDF pclk_active_neg=1 sets LCD_CK_OUT_EDGE=1, which esp-hal calls ShiftHigh.
        .with_clock_mode(ClockMode {
            polarity: Polarity::IdleLow,
            phase: Phase::ShiftHigh,
        })
        .with_format(Format {
            enable_2byte_mode: true,
            ..Default::default()
        })
        .with_timing(FrameTiming {
            horizontal_active_width: 800,
            horizontal_total_width: 800 + 4 + 8 + 8,
            horizontal_blank_front_porch: 4 + 8,
            vertical_active_height: 480,
            vertical_total_height: 480 + 4 + 8 + 8,
            vertical_blank_front_porch: 4 + 8,
            hsync_width: 4,
            vsync_width: 4,
            hsync_position: 0,
        })
        .with_vsync_idle_level(Level::High)
        .with_hsync_idle_level(Level::High)
        .with_de_idle_level(Level::Low)
        .with_disable_black_region(false);
    let dpi = Dpi::new(lcd_cam.lcd, peripherals.DMA_CH0, dpi_config)
        .expect("dpi config")
        .with_vsync(peripherals.GPIO3)
        .with_hsync(peripherals.GPIO46)
        .with_de(peripherals.GPIO5)
        .with_pclk(peripherals.GPIO7)
        // Blue D0..D4
        .with_data0(peripherals.GPIO14)
        .with_data1(peripherals.GPIO38)
        .with_data2(peripherals.GPIO18)
        .with_data3(peripherals.GPIO17)
        .with_data4(peripherals.GPIO10)
        // Green D5..D10
        .with_data5(peripherals.GPIO39)
        .with_data6(peripherals.GPIO0)
        .with_data7(peripherals.GPIO45)
        .with_data8(peripherals.GPIO48)
        .with_data9(peripherals.GPIO47)
        .with_data10(peripherals.GPIO21)
        // Red D11..D15
        .with_data11(peripherals.GPIO1)
        .with_data12(peripherals.GPIO2)
        .with_data13(peripherals.GPIO42)
        .with_data14(peripherals.GPIO41)
        .with_data15(peripherals.GPIO40);

    static DESCRIPTORS: StaticCell<InternalMemory<[DmaDescriptor; display::DESCRIPTORS]>> =
        StaticCell::new();
    let descriptors = DESCRIPTORS
        .init(InternalMemory::new(
            [DmaDescriptor::EMPTY; display::DESCRIPTORS],
        ))
        .get_mut()
        .unsize();
    let fb = DmaAlignedMut::new(fb).expect("framebuffer not DMA-aligned");
    let buf = LoopedFb::new(descriptors, fb);
    // Looped transfer: kept alive forever, never dropped (drop would stop the DMA).
    let _transfer = dpi
        .send(true, buf)
        .map_err(|e| e.0)
        .expect("dpi: send failed");
    info!(
        "lcd: scanout started pclk=16MHz 800x480 total=820x500 (~39 Hz) burst=64B descriptors={}",
        display::DESCRIPTORS
    );
    expander
        .set_pin(ch422g::BACKLIGHT, true)
        .await
        .expect("backlight on");
    info!("ch422g: backlight on");

    let mut n = 0u32;
    loop {
        info!("alive {n}s");
        log_heap("heartbeat");
        n += 1;
        Timer::after(Duration::from_secs(1)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.2.2/examples
}

# ADS-B display on Waveshare ESP32-S3-Touch-LCD-7

Status: **draft, awaiting user review**. Nothing below is implemented yet.
Date: 2026-10-21. Branch: `feature/adsb-display`.

Conventions used in this doc:

- **[VERIFY]** marks a crate version, API name, pin or register value that I could not confirm
  from inside the container (no network access during design). Confirm it at implementation time
  against crates.io / docs.rs / the Waveshare wiki and schematic. Don't treat it as fact.
- Step tags: **[code]** = firmware or library code with automated tests where possible;
  **[devops]** = repo, CI, tooling or infra; **[hw]** = needs manual verification on the board
  (no device in CI; flashing happens from the host per README).

---

## Decisions needed from user

These are open. I give a recommendation for each, but the plan only depends on them from the step
noted.

| # | Decision | Options | Recommendation | Blocks |
|---|----------|---------|----------------|--------|
| D1 | **Where do "source / destination" (route) and "airplane" (type, registration) come from?** ADS-B doesn't carry them. | (a) Defer: show callsign + ICAO24 only. (b) Device calls a public API (e.g. adsbdb.com) over **HTTPS**. (c) A small **LAN helper service** that the device calls over plain HTTP; it proxies/caches a public API or a local DB. (d) Offline DB in flash. | Phase 1 ships with (a). Then (b) via a pure-Rust TLS stack, if you accept a third-party API dependency and its rate limits/terms. Pick (c) if you'd rather keep TLS off the device. (d) isn't viable for routes. Details in [Route and aircraft metadata](#route-and-aircraft-metadata). | Phase 5 |
| D2 | **Feed format.** You specified port 30002 (raw AVR). | Keep 30002 and decode on the device; or switch to 30003 (SBS/BaseStation CSV, already decoded); or 30005 (Beast binary). | **Keep 30002** as specified. Comparison in [Data source](#data-source-feed-format). | — (assumed 30002) |
| D3 | **UI framework.** Licensing matters. | Slint (GPLv3, or Slint's royalty-free/commercial licences; check which covers embedded use for you) vs embedded-graphics (MIT/Apache) vs LVGL bindings. | **Slint**, if GPLv3 (or another Slint licence that fits) is fine for this repo. Otherwise embedded-graphics. Details in [UI framework](#ui-framework). | Phase 4 |
| D4 | **Exact board revision, receiver position, vicinity radius.** | Confirm the board is the 800×480 "ESP32-S3-Touch-LCD-7" and not the 1024×600 "-7B" (different expander and timings). Give the receiver lat/lon (build-time config, not committed) and a default radius. | Radius default 150 km. Aircraft with no position listed last. | Phase 1 (board), Phase 2.8 (radius) |
| D5 | **CI host.** The repo has no remote CI yet. | GitHub Actions / Gitea / GitLab / none. | GitHub Actions if the remote is GitHub. | Step 0.4 |
| D6 | **Confirm fr24feed output.** | Port 30002 must be enabled and reachable from the LAN (fr24feed `raw="yes"`; check the bind address). Same check for 30003 if D2 changes. | Run step 0.1 before anything else. | Phase 3 |

---

## Context

The user wants a standalone wall/desk display. It connects over WiFi to an existing fr24feed
receiver at 10.10.10.57 and shows the aircraft near the receiver: callsign/aircraft, route
(source → destination), and direction. Constraints: Rust, `no_std`, Embassy, a suitable embedded
UI framework.

Repo state: only `README.md` and `.devcontainer/`. There's no firmware project yet. The
container has rustup (`stable` and `esp` toolchains via espup, all targets), `espflash`,
`cargo-espflash` and `esp-generate`. USB can't be passed into the container on macOS, so the
container builds and the host flashes.

**README bug to fix:** it shows `esp-generate --headless -o esp32 ...`. The target chip is the
**ESP32-S3**, so the chip must be `esp32s3`. Recent esp-generate takes the chip as `--chip esp32s3`
rather than `-o` [VERIFY with `esp-generate --help` / `esp-generate list-options`]. Step 0.2
handles this.

### Hardware summary (ESP32-S3-Touch-LCD-7, 800×480)

| Block | Part / interface | Notes |
|-------|------------------|-------|
| SoC | ESP32-S3 (N16R8 class: 16 MB flash, **8 MB octal PSRAM**) | Octal PSRAM uses GPIO33–37, so those pins aren't available. [VERIFY module variant] |
| LCD | 7" 800×480 TFT, **16-bit RGB565 parallel (DPI)**, ST7262-class timing controller | No command interface. The panel needs continuous HSYNC/VSYNC/DE/PCLK scanout from the ESP32-S3 `LCD_CAM` peripheral. |
| Touch | **GT911**, I2C, INT line to a GPIO | The I2C address depends on the INT level during reset (0x5D or 0x14), so the reset sequence matters. |
| IO expander | **CH422G** on the same I2C bus | Drives touch reset, backlight enable, LCD reset and SD CS. Odd addressing: each register has its own I2C address. |
| Other | USB-UART / native USB, CAN, RS485, SD | Out of scope. |

**Pin assignments.** These come from Waveshare's published ESP-IDF/Arduino demo for this board as I
remember it. **[VERIFY every line against the Waveshare wiki and the board schematic PDF for the
exact revision before step 1.2.]** Data-line ordering errors show up as wrong colours, so step 1.3
is designed to catch them.

| Signal | GPIO | Signal | GPIO |
|--------|------|--------|------|
| I2C SDA | 8 | I2C SCL | 9 |
| GT911 INT | 4 | PCLK | 7 |
| HSYNC | 46 | VSYNC | 3 |
| DE | 5 | DISP | n/c (backlight via CH422G) |
| B0..B4 | 14, 38, 18, 17, 10 | G0..G5 | 39, 0, 45, 48, 47, 21 |
| R0..R4 | 1, 2, 42, 41, 40 | | |

| CH422G output | Function [VERIFY] |
|---------------|-------------------|
| EXIO1 | GT911 reset (TP_RST) |
| EXIO2 | Backlight enable (DISP), on/off only, no PWM on this revision [VERIFY] |
| EXIO3 | LCD reset |
| EXIO4 | SD card CS |

CH422G I2C "addresses" (one per register): 0x24 = system/mode config, 0x38 = output IO0–7,
0x23 = open-drain OC outputs, 0x26 = input [VERIFY against the CH422G datasheet].

Panel timing from the Waveshare demo [VERIFY]: PCLK ≈ 16 MHz, HSYNC pulse/back porch/front porch
= 4/8/8, VSYNC = 4/8/8, PCLK sampled on the falling edge. Some community reports suggest lowering
PCLK (12–14 MHz) when WiFi is active (see bandwidth risk below).

GPIO0 (G1) and GPIO45/46 (G2/HSYNC) are strapping pins. That works on Waveshare's design, but
nothing else may be attached to them.

### esp-hal support check

From my knowledge of esp-hal up to mid-2026. Confirm all of it at implementation time.

- **LCD_CAM RGB/DPI:** esp-hal has `esp_hal::lcd_cam::lcd::dpi::Dpi` (behind the `unstable`
  feature), with a config for timings, polarities and pin assignment of 16 data lines. It sends a
  DMA buffer and can **loop the transfer continuously** (`send(true, buf)` / "next frame" mode) for
  scanout [VERIFY exact API and whether a looped transfer from a PSRAM `DmaTxBuf` is supported].
  esp-hal has an example (`lcd_dpi`) for this peripheral [VERIFY whether it uses PSRAM].
- **PSRAM:** esp-hal supports octal PSRAM on the S3 (`psram` config in `esp_hal::Config`,
  `esp_hal::psram`). `esp-alloc` can add the PSRAM region as a heap region
  (`esp_alloc::psram_allocator!` or equivalent) [VERIFY macro name]. GDMA on the S3 can read
  external memory. esp-hal DMA buffers need an external-memory burst/alignment config for PSRAM
  [VERIFY the `ExternalBurstConfig` name and the alignment requirement, likely 16/32/64 bytes].
- **Cache coherence:** the CPU writes to PSRAM through the data cache, but DMA reads PSRAM
  directly. After rendering, dirty cache lines must be written back, or the panel shows stale
  pixels. Find out whether esp-hal does this inside `DmaTxBuf` for a *looped* transfer (probably
  not, because the buffer is handed over once). If not, we need an explicit cache writeback of
  the dirtied region after each render. [VERIFY: esp-hal API or ROM function
  `Cache_WriteBack_Addr`.] Step 1.4 tests this directly.
- **DMA descriptors:** each descriptor covers at most 4095 bytes, so a 768,000-byte framebuffer
  needs about 188 descriptors (≈2.3 KB in internal RAM). Descriptors must sit in internal SRAM.

### Bandwidth and tearing

- Frame: (800+4+8+8) × (480+4+8+8) = 820 × 500 = 410,000 PCLKs → at 16 MHz ≈ **39 Hz**.
- Scanout bandwidth: 16 MHz × 2 B = **32 MB/s, continuous, read from PSRAM**.
- Octal PSRAM at 80 MHz DDR has about 160 MB/s raw and much less in practice. CPU rendering
  writes, WiFi buffers that may live in PSRAM, and cache misses compete for that bandwidth.
- **Failure mode:** if the LCD DMA FIFO underruns, the image shifts or drifts horizontally and
  stays shifted until the next VSYNC resync. This is a known ESP32-S3 RGB-panel problem. ESP-IDF
  fixes it with SRAM "bounce buffers" refilled by the CPU. **esp-hal has no bounce-buffer support
  as far as I know** [VERIFY].
- Mitigations, cheapest first:
  1. Lower PCLK (12–14 MHz, ≈29–34 Hz refresh; still fine for a mostly static UI).
  2. Keep WiFi/net buffers and hot data in internal SRAM. PSRAM holds only the framebuffer and
     the Slint heap.
  3. Partial rendering: redraw only dirty regions, rendered into an SRAM line buffer and copied
     line by line into the framebuffer (sequential writes, cache-friendly).
  4. Run the PSRAM clock at the highest stable setting [VERIFY whether esp-hal exposes 120 MHz].
  5. Last resort, deferred: hand-rolled bounce buffers (SRAM ring + EOF interrupt).
- **Tearing:** one buffer scanned out continuously means a frame can show half-updated content.
  For a list UI that updates about once per second, a one-frame tear is barely visible, so **I
  accept it in v1**. Double buffering (2 × 768 KB fits in 8 MB) would need retargeting the DMA
  descriptor chain at VSYNC. That's deferred until it's confirmed that esp-hal allows it cheaply.

---

## Data source (feed format)

| Port | Format | Device-side work | Trade-off |
|------|--------|------------------|-----------|
| **30002** (specified) | AVR raw text: `*8D4840D6202CC371C32CE0576098;\n`, possibly also `@<12 hex timestamp><frame>;` | Full Mode S / ADS-B decode: CRC, DF17/18 type codes, CPR, velocity, altitude | Most work, but well specified, self-contained and fully host-testable. No dependence on fr24feed's decoder quirks. |
| 30003 | SBS/BaseStation CSV (`MSG,3,...,callsign,alt,gs,track,lat,lon,...`) | Parse CSV | Least work, and positions/velocity come pre-decoded. Depends on fr24feed's SBS output being enabled and correct. Fields are sparse per message type. |
| 30005 | Beast binary (0x1A escaping, MLAT timestamp, signal level) | Binary framing + full decode, same as 30002 | Adds framing complexity, gains nothing for this use case. |

**Recommendation: keep 30002 as specified.** The decoder is the most testable part of the system
(published test vectors exist), and the aircraft table runs on our own decoded state either way. I
keep the line parser behind a small `FeedParser` boundary so SBS input could be added later
without touching the table or UI. Switching to 30003 would remove phases 2.2–2.6 and make the
project smaller, but it's **only a recommendation for you to approve** (D2). It won't be done
silently.

Decoding scope (v1):

- **CRC-24** parity check on every 112-bit DF17/18 frame. Drop it if non-zero. No error correction.
- **DF17**, and **DF18 with CF=0** (ADS-B from non-transponder devices, same ME format). Other DF18
  CF values (TIS-B/ADS-R) are deferred.
- Type codes:
  - TC 1–4: identification (callsign, 8 chars of a 6-bit charset; emitter category).
  - TC 9–18: airborne position with barometric altitude (Q-bit 25 ft encoding; Gillham Q=0 →
    `None` in v1).
  - TC 20–22: airborne position with GNSS altitude.
  - TC 19: airborne velocity. Subtypes 1/2 (ground speed → track = atan2) and 3/4 (airspeed +
    magnetic heading when available), plus vertical rate.
  - TC 5–8: surface position, deferred (needs reference-based decode, low value for a vicinity
    display).
- **CPR:**
  - Global decode from an even/odd pair at most 10 s apart and in the same NL zone. Otherwise reject.
  - Local decode against a reference (the aircraft's last known position if fresh, else the
    receiver position). Valid within ±180 NM of the reference for airborne positions.
  - Plausibility gate: drop positions more than 450 km from the receiver, or an implied speed
    jump above about 1,000 kt from the previous fix.
  - Use `f64` for CPR maths (the S3 FPU is single-precision only, so f64 is software, but the
    message rate is a few hundred per second, which is fine).
- DF4/5/20/21 (Mode S altitude/squawk without position) are deferred. Aircraft seen only through
  those wouldn't have a position anyway.

---

## Route and aircraft metadata

ADS-B carries the **ICAO24 address** and the **callsign**. It doesn't carry origin, destination,
type or registration. Those need a lookup:

- callsign → route (e.g. `KLM1023` → AMS–…)
- ICAO24 → registration/type (e.g. `4840D6` → PH-xxx, B738)

| Option | How | Pros | Cons |
|--------|-----|------|------|
| (a) Defer | Show callsign, ICAO24 and a placeholder "—" for route/type | No new risk, ships fastest | Doesn't meet the "source, destination, airplane" goal |
| (b) On-device HTTPS | `reqwless` HTTP client + TLS to e.g. `https://api.adsbdb.com/v0/callsign/{cs}` and `/v0/aircraft/{icao}` [VERIFY endpoints and response schema]. Parse JSON with `serde-json-core`. LRU cache with negative caching. | Self-contained device, no extra infra | TLS on `no_std`: **embedded-tls** (pure Rust, TLS 1.3 only, weak/limited certificate verification [VERIFY current state]) or **esp-mbedtls** (C, hardware-accelerated, adds a C build to the Xtensa toolchain [VERIFY crate name/status; the project may have been renamed]). About 35–40 KB of TLS buffers (fine in PSRAM). Third-party API rate limits, uptime and terms of use. Route data from callsign DBs is sometimes stale. |
| (c) LAN helper | Small service on the LAN (could run next to fr24feed) exposing `GET /route/{cs}` and `/aircraft/{icao}` over plain HTTP. It caches upstream lookups or uses a local DB. | Simplest firmware, no TLS on the device, upstream can change without reflashing | Another service to deploy and keep running. Its design and ops are outside this doc. |
| (d) Offline DB | ICAO24 → type/reg table in flash | Works offline | Global aircraft DB is hundreds of thousands of rows, too big unless filtered to a region. Routes change daily, so impractical. |

**Recommendation:** Phase 1–4 with (a). The metadata comes in through a `MetadataProvider` boundary
(firmware side) plus a pure cache and JSON-parsing layer (core side), so (b) or (c) slots in
during Phase 5. My default for Phase 5 is (b) with embedded-tls, falling back to esp-mbedtls if
the API endpoint doesn't negotiate TLS 1.3 or verification is inadequate. **The user decides (D1).**

---

## UI framework

| Option | Trade-off |
|--------|-----------|
| **Slint** (`slint` with `default-features = false`, software renderer, `unsafe-single-threaded`, `libm`; `slint-build` compiling `.slint` at build time) [VERIFY feature names for current 1.x] | Declarative UI, built-in `ListView`/scrolling/touch handling, partial (dirty-region) rendering, line-by-line rendering into an SRAM buffer. It has official MCU support with ESP32-S3 examples, and the same `.slint` file renders on the host for headless snapshot tests. It needs `alloc` (PSRAM heap is fine). Licence: GPLv3 or Slint's own licences (decision D3). |
| embedded-graphics (+ own `DrawTarget` over the PSRAM framebuffer, `embedded-text`, `u8g2-fonts`/`profont`) | MIT/Apache, no alloc, tiny and predictable. All widgets, scrolling, hit-testing and dirty tracking are hand-written: more code for the list and detail views. |
| LVGL via `lvgl-rs` | Mature C UI, but the Rust bindings are thinly maintained, the bindgen + Xtensa C cross-build is fragile, it's not idiomatic Rust, and it brings a C dependency the constraints try to avoid. |
| Others (Kolibri immediate-mode on embedded-graphics, `mousefood` = ratatui on embedded-graphics) | Lightweight. Kolibri is immature. Mousefood gives a text-mode look, which is acceptable but doesn't use the panel well. |

**Decision: Slint** (subject to D3). Fallback is embedded-graphics. The UI is a thin layer over
`adsb-core::Snapshot`, so switching costs only Phase 4.

---

## Decision (ADR)

**ADR-001: Architecture of the ADS-B display firmware**

- **Context:** as above. Hardware risk (RGB scanout from PSRAM under WiFi load) is the biggest
  unknown. Decoding is well specified and testable. Route metadata needs an external source.
- **Decision:**
  1. **Two separate Cargo projects in the repo, not one workspace:**
     - `adsb-core/`: `#![no_std]` library (with `std` only under `cfg(test)`), built and tested with
       the **stable** host toolchain. Contents: line parser, Mode S/ADS-B decoder, CPR, geo
       maths, aircraft table with ageing, snapshot/sort/filter, metadata cache and JSON parsing.
       Dependencies: `heapless`, `libm`, optionally `serde` + `serde-json-core` (Phase 5). No
       Embassy, no esp-hal. Time is passed in as `now_ms: u64`.
     - `firmware/`: generated by `esp-generate` for `esp32s3`, uses the `esp` toolchain via
       `rust-toolchain.toml`, target `xtensa-esp32s3-none-elf`, and depends on
       `adsb-core = { path = "../adsb-core" }`.
     - `ui-sim/` (Phase 4): host binary/test that renders the firmware's `.slint` files with
       fixture data through Slint's software renderer into PNGs (headless, CI-able).
     - Separate projects stop the firmware's `.cargo/config.toml` (target, `build-std`) and the
       `esp` toolchain from affecting host tests.
  2. **Feed:** 30002 raw AVR, decoded on the device (D2).
  3. **Stack** [VERIFY all versions at step 0.2; `esp-generate` output is authoritative]:
     `esp-hal` 1.x (`unstable` feature for LCD_CAM/PSRAM), `esp-rtos` (the Embassy integration
     that replaced `esp-hal-embassy`) [VERIFY], `esp-radio` (renamed from `esp-wifi`) [VERIFY],
     `esp-alloc`, `esp-backtrace`, `esp-println` + `log`, `embassy-executor`, `embassy-time`,
     `embassy-net` (TCP, DHCPv4, DNS for Phase 5), `embassy-sync`, `static_cell`, `slint`.
  4. **UI:** Slint software renderer, line-by-line into an SRAM line buffer, copied into a single
     PSRAM framebuffer scanned out continuously by `Dpi` DMA. Tearing is accepted in v1.
  5. **Metadata:** deferred behind a boundary; Phase 5 implements the option chosen in D1.
  6. **Configuration:** build-time environment variables read by `firmware/build.rs` (below).
     Never committed.
- **Consequences:**
  - All decoding logic is CI-tested on the host with no device.
  - Hardware integration is de-risked first (Phase 1) with a test-pattern spike before any UI
    investment.
  - Two toolchains (stable for core, esp for firmware). CI needs both.
  - Credentials are baked into the firmware binary, so the binary must not be shared.
  - Changing the WiFi network means rebuilding and reflashing. A runtime provisioning UI is
    deferred.

### Configuration

`firmware/build.rs` reads these and fails the build with a clear message if a required one is
missing or malformed (lat/lon parsed and range-checked at build time):

| Var | Required | Example (not committed) |
|-----|----------|-------------------------|
| `ADSB_WIFI_SSID` | yes | |
| `ADSB_WIFI_PASSWORD` | yes | |
| `ADSB_FEED_HOST` | yes | `10.10.10.57` |
| `ADSB_FEED_PORT` | no (default 30002) | |
| `ADSB_RX_LAT`, `ADSB_RX_LON` | yes | decimal degrees |
| `ADSB_RADIUS_KM` | no (default 150) | |

`build.rs` emits `cargo:rustc-env=...` and `cargo:rerun-if-env-changed=...`. The code reads the
values with `env!`. A committed `firmware/.env.example` documents the vars. `.env` is in
`.gitignore`, and the developer loads it with `set -a; . ./.env; set +a` or direnv. Don't use
`[env]` in `.cargo/config.toml`, because that file is committed.

### "Vicinity"

- The receiver position (`ADSB_RX_LAT/LON`) is the reference for local CPR decoding and for
  distance and bearing (haversine, f64) from the receiver to each aircraft.
- The list shows aircraft with a position within `ADSB_RADIUS_KM`, sorted by distance ascending.
  Aircraft heard but without a position yet (or with a stale one) follow, sorted by last-seen.
  They're shown because they're within radio range and so plausibly nearby.
- **Direction** shows two things: the aircraft's **track** (from velocity, or heading for airspeed
  subtypes) as a rotated arrow plus degrees, and the **bearing/distance from the receiver** (e.g.
  "NE 23 km").

### Embassy task structure

Single core, single executor in v1. Moving UI rendering to core 1 is a later optimisation if
measurements call for it.

```
main
 ├─ init: clocks 240 MHz, PSRAM, heaps (internal: WiFi/net; PSRAM: framebuffer + Slint),
 │        I2C → CH422G (LCD reset, backlight, touch reset seq), Dpi scanout start
 ├─ wifi_task        esp-radio controller: connect, wait for disconnect, back off, reconnect
 ├─ net_task         embassy-net runner
 ├─ feed_task        loop { wait for link+IP; TCP connect HOST:PORT (timeout 10 s);
 │                          read → LineParser → decode → TABLE.lock(update);
 │                          idle timeout 60 s → reconnect; exponential backoff 1 s → 30 s }
 ├─ touch_task       owns I2C after init; GT911 poll at about 60 Hz (or INT-triggered);
 │                   → Channel<TouchEvent> to ui_task
 ├─ ui_task          owns the Slint window; every 1 s: TABLE.lock(age + snapshot) → model;
 │                   drains touch channel → window.dispatch_event; slint timers/animations;
 │                   render dirty region → line buffer → framebuffer (+ cache writeback)
 └─ meta_task (P5)   pulls lookup requests from a Channel; HTTP(S); fills metadata cache
```

- Shared state: `TABLE: blocking_mutex::Mutex<CriticalSectionRawMutex, RefCell<AircraftTable>>`.
  Critical sections are short (one update or one snapshot copy).
- A `Status` struct (WiFi state, feed connected, msgs/s, CRC failures) sits in the same mutex,
  for the status bar.
- Capacity: `AircraftTable` holds at most 128 entries (`heapless`). When full, the oldest
  last-seen entry is evicted. Entries are removed after 60 s with no messages, and a position is
  marked stale after 30 s.

### UI layout (800×480 landscape)

- **Status bar** (top, about 32 px): WiFi/feed status dots, aircraft count, msgs/s, clock (uptime
  in v1; SNTP deferred).
- **List view:** rows about 56 px (8 visible). Columns: callsign (or ICAO24), type* | route*
  `ORIG→DEST`, altitude (FL or ft), GS, track arrow, distance + bearing. Items marked * are "—"
  until Phase 5. It scrolls with drag.
- **Detail view** (tap a row): ICAO24, callsign, category, registration/type*, route*, altitude,
  vertical rate, GS, track, position, distance/bearing, last seen, position age. Back button.

---

## Plan

Each step finishes with its verification green before the next starts. Steps inside a phase are
ordered. Phase 2 (pure host work) can run in parallel with Phase 1 (hardware).

### Phase 0 — scaffolding

0.1 **[hw/devops, manual]** Confirm the feed: from the container, `nc 10.10.10.57 30002 | head -50`.
   *Verify:* lines look like `*[0-9A-F]{14|28};`. Note whether `@`-timestamped lines appear.
   Save 10 minutes to `adsb-core/tests/data/capture-30002.txt` for replay tests. It contains no
   secrets, only public broadcast data, but confirm with the user that committing it is OK.
   Resolves D6.

0.2 **[devops]** Generate `firmware/` with esp-generate for `esp32s3` with options embassy,
   unstable-hal, alloc, wifi/radio, PSRAM (if offered), log via esp-println
   [VERIFY flags with `esp-generate list-options`]. Fix the README invocation. Add a root
   `.gitignore` (`target/`, `.env`). Record the resolved crate versions in this doc's revision log.
   *Verify:* `cargo build --release` in `firmware/` succeeds in the container.

0.3 **[hw]** Flash the generated hello-world from the host (`espflash flash --monitor
   firmware/target/xtensa-esp32s3-none-elf/release/<app>`).
   *Verify:* boot log and periodic log line appear on the serial monitor. Note which USB port
   (UART vs native USB) works and record it in the README.

0.4 **[code]** Create `adsb-core/` (lib, `#![cfg_attr(not(test), no_std)]`, edition 2024, deps
   `heapless`, `libm`), with one placeholder test.
   *Verify:* `cargo test` (host, stable) passes. `cargo build --target riscv32imc-unknown-none-elf`
   (or `thumbv7em-none-eabihf`, whichever is installed) succeeds, which proves no_std.
   Then `firmware/` depends on it via path and still builds.

0.5 **[devops]** CI (depends on D5): job 1 runs `cargo fmt --check`, `clippy -D warnings`,
   `cargo test` and the no_std build for `adsb-core`. Job 2 builds `firmware/` with the esp
   toolchain (e.g. the `esp-rs/xtensa-toolchain` action [VERIFY]) using dummy `ADSB_*` env vars.
   *Verify:* a pipeline run is green, and a deliberately failing test turns it red.

### Phase 1 — hardware bring-up spike (de-risk first)

1.1 **[code+hw]** PSRAM init and PSRAM heap region. Allocate a 768,000-byte buffer and run a
   write/read pattern test.
   *Verify (serial):* PSRAM size logged as 8 MB, pattern test passes, internal heap free logged.

1.2 **[code+hw]** I2C bus (GPIO8/9) and a minimal CH422G driver (set push-pull output mode,
   write outputs). Log an I2C address scan. Toggle the backlight every second.
   *Verify:* the scan lists the expected addresses (CH422G register addresses, GT911 at 0x5D or
   0x14). The backlight visibly blinks. If the addresses differ from this doc, stop and correct
   the pin/address tables.

1.3 **[code+hw]** `Dpi` scanout from the PSRAM framebuffer: release LCD reset via CH422G, start a
   looped DMA transfer of a static test pattern. The pattern has 8 vertical colour bars (R, G, B,
   C, M, Y, W, K), a 1-px white border, and one row of each single bit set (R0..R4, G0..G5,
   B0..B4) as separate stripes.
   *Verify (visual):* bar colours in order (catches swapped R/B or bit-order errors). Each
   single-bit stripe has a distinct, monotonic intensity (catches miswired data pins). The border
   is visible on all four edges with no wrap (catches porch/timing errors). The image stays stable
   for 10 min. Record PCLK and timings.
   **If this step can't be made to work with esp-hal, stop and report**: the architecture
   depends on it (see Risks R1).

1.4 **[code+hw]** Update under load: move a 100×100 rectangle across the framebuffer at about
   10 Hz from the CPU, with cache writeback after each update.
   *Verify:* no stale-pixel trails (cache coherence works), no horizontal drift over 10 min.
   Log CPU time per update. Repeat with writeback removed to confirm it's actually needed, and
   document the result.

1.5 **[code+hw]** GT911: reset sequence (INT low, TP_RST via CH422G) and a minimal async driver
   (read status, read points, clear status). Use an existing `gt911` crate only if it supports
   embedded-hal-async 1.0 [VERIFY]. Otherwise write about 100 lines of our own. Log touch points.
   *Verify:* touching the four corners logs about (0,0), (799,0), (0,479), (799,479), with no axis
   swap or inversion. Multi-touch isn't needed.

### Phase 2 — core decoding (host, TDD; may run in parallel with Phase 1)

Test vectors come from *The 1090 Megahertz Riddle* (J. Sun). **[VERIFY expected values against
the book when writing the tests; the ones below are from memory.]**

| Frame | Expect |
|-------|--------|
| `8D4840D6202CC371C32CE0576098` | DF17, ICAO 4840D6, TC4, callsign `KLM1023` |
| `8D40621D58C382D690C8AC2863A7` (even) + `8D40621D58C386435CC412692AD6` (odd) | global CPR ≈ 52.2572°N, 3.9194°E; alt 38,000 ft |
| `8D40621D58C382D690C8AC2863A7` with ref 52.258, 3.918 | local CPR ≈ 52.2572, 3.9194 |
| `8D485020994409940838175B284F` | velocity ST1: GS ≈ 159 kt, track ≈ 182.9°, VR −832 ft/min |
| `8DA05F219B06B6AF189400CBC33F` | velocity ST3: heading ≈ 244.0°, TAS 375 kt, VR −2304 ft/min |

2.1 **[code]** `LineParser`: streaming and fixed-capacity. It accepts arbitrary byte chunks (lines
   split across TCP reads), and handles `*…;` and `@<12-hex ts>…;` with CR/LF. It rejects
   non-hex, wrong lengths (only 14/28 hex chars) and overlong garbage (resyncs at the next `*`/`@`)
   without panicking. *Verify:* unit tests for each case, including byte-at-a-time feeding.

2.2 **[code]** `Frame`: hex → `[u8; 14]`/`[u8; 7]`, DF, ICAO24 (DF17/18), CRC-24 (poly
   0xFFF409). *Verify:* the table vectors pass CRC. A single flipped bit fails. DF/ICAO extracted.

2.3 **[code]** Identification (TC 1–4): callsign charset, trailing-space trim, category.
   *Verify:* `KLM1023` vector.

2.4 **[code]** Altitude (Q=1, 25 ft). Q=0 returns `None`. *Verify:* 38,000 ft vector, plus a Q=0
   case returning `None`.

2.5 **[code]** CPR: NL function (table or formula), global pair decode (zone-mismatch reject),
   local decode. *Verify:* the vectors above; zone-mismatch pair rejected; latitude edge cases
   near ±87°; longitude wrap at ±180°.

2.6 **[code]** Velocity TC19 ST1–4: GS/track, airspeed/heading (heading-available bit), vertical
   rate and sign, "no data" encodings. *Verify:* ST1 and ST3 vectors, plus zero/no-data cases.

2.7 **[code]** Geo: haversine distance, initial bearing, 8-point compass label.
   *Verify:* known city pairs (e.g. AMS–LHR ≈ 370 km) within 0.5 %. Bearings at N/E/S/W.

2.8 **[code]** `AircraftTable`: `update(frame_decoded, now_ms)`; per-aircraft even/odd CPR state
   (global when the pair is ≤ 10 s apart, local vs last position or receiver otherwise);
   plausibility gate; ageing (`age(now_ms)`); capacity eviction; `snapshot(radius_km) ->
   heapless::Vec<Row, N>` sorted as specified in Vicinity.
   *Verify:* unit tests with synthetic time cover position from a pair, from a single message
   with a reference, stale and expiry transitions, eviction when full, sort order (positioned by
   distance, then unpositioned), and the radius filter.

2.9 **[code]** Replay test: feed `tests/data/capture-30002.txt` (from 0.1) through LineParser →
   decode → table.
   *Verify:* no panics. CRC pass rate above 95 %. Each resolved position lies within 450 km of
   the receiver (test-only coordinates via env/fixture, not committed if the user considers the
   location sensitive). At least N aircraft with callsign and position. Optionally add a
   `proptest`/random-bytes test for the parser (no panics).

### Phase 3 — networking

3.1 **[code]** `firmware/build.rs` config (see Configuration) and `.env.example`.
   *Verify:* building without vars fails with a message naming the missing var. Bad lat/lon
   fails. A valid `.env` builds. `git status` shows `.env` ignored.

3.2 **[code+hw]** WiFi (esp-radio) + embassy-net DHCP. Log the IP, and reconnect after the AP
   drops.
   *Verify:* IP logged within 10 s of boot. Power-cycle the AP (or block the MAC) and the device
   reconnects and logs the new IP without a reboot.

3.3 **[code+hw]** `feed_task`: TCP connect, read loop into LineParser, log frames/s and CRC
   failures every 10 s, with backoff and idle timeout.
   *Verify:* rates logged roughly match the receiver's traffic. Stopping fr24feed (or firewalling
   the port) leads to logged reconnect attempts with growing backoff. Restarting it resumes
   without a reboot.

3.4 **[code+hw]** Wire the decoder and table. Log the 5 nearest aircraft every 5 s.
   *Verify:* compare against fr24feed's local status page or flightradar24 for the same moment.
   Callsigns, altitudes (±100 ft) and positions (±1 km) agree for at least 3 aircraft.

3.5 **[hw]** Re-run the 1.3 and 1.4 display checks with WiFi plus the feed active.
   *Verify:* no drift over 30 min. If drift appears, apply the bandwidth mitigations in order and
   record the final PCLK and PSRAM settings here.

### Phase 4 — UI (after D3)

4.1 **[code+hw]** Slint platform: implement `slint::platform::Platform` (time from
   `embassy-time`), `MinimalSoftwareWindow` with partial repaint, `render_by_line` into an SRAM
   line buffer → framebuffer copy → cache writeback. Show a static "hello" `.slint`.
   *Verify:* renders correctly with correct colours. Log full-screen and partial render times.
   Target: full redraw under 150 ms, typical 1 Hz update under 30 ms [targets are estimates;
   adjust after measuring].

4.2 **[code/devops]** `ui-sim/`: host test that loads the same `.slint` files, fills a fixture
   model, renders through the software renderer into a PNG under `target/ui-snapshots/`, and
   (optionally) compares against committed reference PNGs.
   *Verify:* `cargo test` in `ui-sim/` produces PNGs in CI (artefact). Manual review of the first
   render.

4.3 **[code]** List view + status bar bound to `snapshot()` at 1 Hz (model diffing so unchanged
   rows don't repaint).
   *Verify:* ui-sim snapshot with fixture data (0, 1, 8 and 30 aircraft; long callsigns; no
   position). On device, the list matches the 3.4 log output.

4.4 **[code+hw]** Touch → `WindowEvent::PointerPressed/Moved/Released` (plus scroll), then tap a
   row to open details and use a back button.
   *Verify:* drag-scrolls a 30-entry list smoothly enough (no missed taps). A tap opens the row
   that was tapped (check by callsign). Back returns to the list at the same scroll position.

4.5 **[code]** Detail view with track arrow (rotated path) and receiver bearing/distance.
   *Verify:* ui-sim snapshots at tracks 0/90/180/270/45 show the arrow pointing correctly. On
   device, compare with the 3.4 log.

4.6 **[hw]** Soak: 12 h run, logging heap free (internal and PSRAM), reconnect count and
   aircraft count every minute.
   *Verify:* no reboot or panic, heap free flat (no leak trend), display stable.

### Phase 5 — route and aircraft metadata (after D1; skipped if (a))

5.1 **[code]** `adsb-core::meta`: fixed-size LRU cache keyed by callsign (route) and ICAO24
   (aircraft), with TTLs (route 6 h, aircraft 7 d, negative 1 h). JSON parsing of the chosen
   provider's responses with `serde-json-core`.
   *Verify:* host tests on recorded response fixtures (found, not-found, malformed, oversized),
   cache hit/miss/expiry/eviction.

5.2 **[code+hw]** `meta_task`: request queue (deduplicated, at most 1 request/s, retry with
   backoff), DNS, HTTP client (`reqwless`). Option (b) adds TLS (embedded-tls or esp-mbedtls).
   Option (c) uses plain HTTP to the helper.
   *Verify:* serial log shows a lookup per new callsign, cached hits not re-requested, and
   graceful handling of no network or HTTP 404/429. For (b), a TLS handshake succeeds, and
   behaviour with an invalid certificate is documented.

5.3 **[code]** UI: type/registration and `ORIG→DEST` in list and detail, "—" while unknown.
   *Verify:* ui-sim snapshots with and without metadata. On device, routes for at least 3
   aircraft match flightradar24.

5.x **[devops]** If D1 = (c): separate design doc for the LAN helper service (outside this
   repo's firmware scope).

---

## Risks

| ID | Risk | Likelihood / impact | Mitigation / what to verify |
|----|------|---------------------|-----------------------------|
| R1 | esp-hal `Dpi` can't do a continuous looped scanout from a PSRAM DMA buffer, or lacks needed timing knobs | Medium / **critical** | Step 1.3 runs first. If blocked: check esp-hal issues/PRs and upstream a fix. Last resort is register-level LCD_CAM setup in the firmware crate. Don't fall back silently: report to the user. |
| R2 | PSRAM bandwidth contention → horizontal drift, worse with WiFi | Medium–high / high | Mitigations listed under Bandwidth (PCLK down, buffers in SRAM, partial rendering, PSRAM clock). Bounce buffers deferred. Verified in 3.5 and 4.6. |
| R3 | Cache coherence: CPU writes not visible to DMA | Medium / medium | Explicit writeback after rendering. Tested in 1.4. API name [VERIFY]. |
| R4 | Pin map, CH422G addressing or panel timings wrong for the actual board revision (e.g. -7 vs -7B, or a later PCB rev) | Medium / medium | All marked [VERIFY]. Steps 1.2/1.3/1.5 are designed to expose errors. Decision D4 confirms the board. |
| R5 | Crate churn: esp-wifi→esp-radio, esp-hal-embassy→esp-rtos, embassy version skew with Slint/reqwless/embedded-tls | High / medium | Pin versions from esp-generate output (0.2). Record them in the revision log. Upgrade deliberately, not incidentally. |
| R6 | Internal SRAM pressure: WiFi + embassy-net + Slint line buffers + DMA descriptors (+ TLS in P5) | Medium / medium | Log heap stats from 1.1 on. TLS buffers and Slint heap go in PSRAM. Reduce TCP buffer sizes if needed. |
| R7 | Slint licence not acceptable (D3) | Low / low | embedded-graphics fallback. Only Phase 4 is affected. |
| R8 | Software render too slow at 800×480 from PSRAM | Low–medium / medium | Partial rendering, 1 Hz model updates, move UI to core 1 later. Measured in 4.1. |
| R9 | fr24feed 30002 quirks: not enabled, binds to localhost only, Mode-AC lines, timestamp format, drops idle clients | Medium / low | Step 0.1 checks. Parser tolerant to unknown lines. Idle timeout and reconnect. |
| R10 | CPR mis-decode producing wrong positions (zone boundaries, stale pairs) | Medium / medium | Global+local strategy, plausibility gate, replay test against real data (2.9), cross-check with FR24 (3.4). |
| R11 | Metadata API (P5, option b): rate limits, terms of use, downtime, TLS 1.3-only stack incompatibility, weak cert validation in embedded-tls | Medium / low–medium | Caching and rate limiting. Option (c) as an alternative. The decision is the user's (D1). |
| R12 | Secrets in binary / accidental commit of `.env` | Low / medium | `.gitignore`, build-time vars only, `.env.example` without values. Reviewers check the diff in 3.1. |
| R13 | Receiver location sensitivity in committed test data | Low / low | Test-only coordinates via env/fixture. Ask before committing capture data (0.1). |

Deliberately deferred: surface positions, DF4/5/20/21, TIS-B/ADS-R, Gillham altitude, double
buffering, bounce buffers, SNTP/wall clock, on-device WiFi provisioning, radar/map view, OTA
updates, backlight dimming (hardware appears on/off only), dual-core split.

Unverified (summary): every [VERIFY] item above, especially the esp-hal `Dpi`+PSRAM looped
scanout (R1), the pin map, CH422G addresses and panel timings (R4), current crate names and
versions (R5), test-vector values, and adsbdb endpoints/schema.

---

## Revision log

- 2026-10-21: Initial draft.

# ADS-B display on Waveshare ESP32-S3-Touch-LCD-7

Status: **revision 3, approved by the user 2026-10-21.** Revision 2 was approved by the user on 2026-10-21
(D1–D8 resolved) and implementation is in progress on this branch. Revision 3 adds the
approved-scope browser simulator (D9), records the step 0.1 results, and re-sequences the plan so
UI work can start without the board. Step numbers from revision 2 are kept as stable IDs; new
simulator steps are numbered S.1–S.8.
Date: 2026-10-21. Branch: `feature/adsb-display`.

Conventions used in this doc:

- **[VERIFY]** marks a crate version, API name, pin or register value that I could not confirm
  from inside the container (no network access during design). Confirm it at implementation time
  against crates.io / docs.rs / the Waveshare wiki and schematic. Don't treat it as fact. Items
  verified online on 2026-10-21 (Waveshare demo repo and docs, esp-hal 1.2.2 source, crates.io,
  adsbdb.com) no longer carry the tag. The revision log lists them.
- Step tags: **[code]** = firmware or library code with automated tests where possible;
  **[devops]** = repo, CI, tooling or infra; **[hw]** = needs manual verification on the board
  (no device in CI; flashing happens from the host per README).

---

## Decisions needed from user

D1–D9 are **resolved**. D7 and D8 were added in revision 2, D9 in revision 3.

| # | Decision | Options | Recommendation / resolution | Blocks |
|---|----------|---------|-----------------------------|--------|
| D1 | **Where do "source / destination" (route) and "airplane" (type, registration) come from?** ADS-B doesn't carry them. | (a) Defer: show callsign + ICAO24 only. (b) Device calls a public API (e.g. adsbdb.com) over **HTTPS**. (c) A small **LAN helper service** that the device calls over plain HTTP; it proxies/caches a public API or a local DB. (d) Offline DB in flash. | **Resolved: (b).** The device calls public APIs directly. `http://api.adsbdb.com/...` answers 301 → https, so the device must speak TLS. Phase 5 is in scope. TLS stack default: **embedded-tls**, with mbedtls-rs as the gated fallback (see [Route and aircraft metadata](#route-and-aircraft-metadata)). | Phase 5 |
| D2 | **Feed format.** You specified port 30002 (raw AVR). | Keep 30002 and decode on the device; or switch to 30003 (SBS/BaseStation CSV, already decoded); or 30005 (Beast binary). | **Resolved: keep 30002**, decode on the device, so the device doesn't depend on fr24feed's decoder or other extra software. Comparison in [Data source](#data-source-feed-format). | — |
| D3 | **UI framework.** Licensing matters. | Slint (GPLv3, or Slint's royalty-free/commercial licences; check which covers embedded use for you) vs embedded-graphics (MIT/Apache) vs LVGL bindings. | **Resolved: Slint under GPLv3.** The project will never be commercial. Licence consequences are in D7. Details in [UI framework](#ui-framework). | Phase 4 |
| D4 | **Exact board revision, receiver position, vicinity radius.** | Confirm the board is the 800×480 "ESP32-S3-Touch-LCD-7" and not the 1024×600 "-7B" (different expander and timings). Give the receiver lat/lon (build-time config, not committed) and a default radius. | **Resolved:** board is the ESP32-S3-Touch-LCD-7 (800×480), module **ESP32-S3N16R8**. Radius of interest is **1–5 km** ("when I hear an airplane flying over"): default **5 km**, configurable. The receiver location is sensitive, so it's build-time only and never committed. This changes the UX: an "overhead" card is now the main screen (see [UI layout](#ui-layout-800480-landscape)). | Phase 1, step 2.10 |
| D5 | **CI host.** | GitHub Actions / Gitea / GitLab / none. | **Resolved: GitHub Actions.** The remote is on GitHub. | Step 0.5 |
| D6 | **Confirm fr24feed output.** | Port 30002 must be enabled and reachable from the LAN (fr24feed `raw="yes"`; check the bind address). Same check for 30003 if D2 changes. | **Resolved:** 30002 is enabled and reachable. Additional requirement: **design a general ADS-B/Mode S client** that might later be connected directly to an SDR. Result: a transport-agnostic frame-source boundary (see [Frame-source boundary](#frame-source-boundary-general-mode-s-client)). Also: **don't commit local data** (captures, receiver location). | Phase 2 |
| D7 | **Licence files** (follows from D3). | Whole repo GPL-3.0-or-later; or `firmware/` + `ui-sim/` GPL-3.0-or-later and the libraries (`modes`, `adsb-core`, which don't use Slint) MIT/Apache-2.0. | **Resolved:** user doesn't mind (personal use, maybe published later) → **GPL-3.0-or-later for the whole repo** (default; one licence, simplest). Pick the split instead if you want the Mode S library to be reusable under permissive terms. | Step 0.2 (LICENSE file) |
| D8 | **"Overhead" UX choices.** | Empty state when nothing is within the radius: blank "nothing overhead" vs. nearest aircraft beyond the radius, greyed out. Distance metric: ground distance vs slant range. | **Resolved: defaults accepted by user** — show the nearest aircraft beyond the radius, greyed out, with an "approaching" hint. Filter and rank by **ground distance**, using dead-reckoned position; show altitude and elevation angle separately. Reasoning in ["Vicinity"](#vicinity). | Steps 2.10, 4.3 |
| D9 | **Simulator** (revision 3): the user wants to iterate on the UI from the devcontainer on macOS (podman) without the board. A previous Slint + XQuartz setup was bad: there's no X11 forwarding. | Native app over X11/XQuartz; native app on the mac host; VNC/noVNC of a native app in the container; WebAssembly in the browser. | **Resolved: WebAssembly in the browser**, served from the container on a forwarded port. Mouse stands in for touch. Inputs: live (through a localhost WebSocket→TCP bridge), replay of the local capture, and synthetic scenarios. Design in ADR-002 under [Decision (ADR)](#decision-adr). A native macOS host build is a nice-to-have ("should compile"), with no steps planned. | Phase S |

---

## Context

The user wants a standalone wall/desk display. It connects over WiFi to an existing fr24feed
receiver at 10.10.10.57 and shows the aircraft near the receiver: callsign/aircraft, route
(source → destination), and direction. Constraints: Rust, `no_std`, Embassy, a suitable embedded
UI framework.

Primary use case (from D4): **"which plane is that I can hear overhead right now?"** The radius
of interest is 1–5 km. At that scale a jet crosses the radius in about 30–60 s, so the main screen
has to show the right aircraft quickly and keep it steady. A list of everything in radio range is
secondary.

Secondary goal (from D6): the decoding side should be a **general Mode S / ADS-B client
library** that isn't tied to fr24feed's TCP output. It might later be fed by an SDR directly.

Third goal (from D9, revision 3): **iterate on the UI without the board.** A browser simulator,
built from the same UI and logic code as the firmware, runs from the devcontainer and is viewed in
the mac's browser. It has to be usable before any hardware step is done, and it must not leak the
receiver location.

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
| SoC | ESP32-S3N16R8 module (16 MB flash, **8 MB octal PSRAM**) | Octal PSRAM uses GPIO33–37, so those pins aren't available. |
| LCD | 7" 800×480 TFT, **16-bit RGB565 parallel (DPI)**, ST7262 timing controller | No command interface. The panel needs continuous HSYNC/VSYNC/DE/PCLK scanout from the ESP32-S3 `LCD_CAM` peripheral. |
| Touch | **GT911**, I2C at 400 kHz, INT on GPIO4 (active low) | The I2C address depends on the INT level during reset (0x5D or 0x14). With the reset sequence below it's **0x5D**. No axis swap or mirroring. |
| IO expander | **CH422G** on the same I2C bus | Drives touch reset, backlight enable, LCD reset, SD CS and the USB/CAN select. Odd addressing: each register has its own I2C address. |
| Other | USB on GPIO19/20; SD over SPI (MOSI 11, SCK 12, MISO 13); RS485 on 15/16; CAN (CH422G EXIO5 selects USB or CAN) | Out of scope, except that EXIO5 must keep USB selected. |

**Pin assignments.** Verified against Waveshare's demo repo `github.com/waveshareteam/ESP32-S3-Touch-LCD-7`
(`examples/Arduino/.../esp_panel_board_custom_conf.h` and `08_DrawColorBar/waveshare_lcd_port.h`)
and the docs.waveshare.com page. Schematic:
<https://files.waveshare.com/wiki/ESP32-S3-Touch-LCD-7/ESP32-S3-Touch-LCD-7-Sch.pdf>. CH422G,
GT911 and ST7262 datasheets are under `files.waveshare.com/wiki/common/`. Data-line ordering errors
show up as wrong colours, and step 1.3 is designed to catch them, which also confirms the PCB
revision on hardware.

Data pins in LCD_CAM order are D0..D15 = 14, 38, 18, 17, 10, 39, 0, 45, 48, 47, 21, 1, 2, 42,
41, 40. So D0–4 = B0–4, D5–10 = G0–5 and D11–15 = R0–4. Waveshare's docs label the same wires
with panel-side 8-bit names (B3–B7, G2–G7, R3–R7). The wiring is identical.

| Signal | GPIO | Signal | GPIO |
|--------|------|--------|------|
| I2C SDA | 8 | I2C SCL | 9 |
| GT911 INT | 4 | PCLK | 7 |
| HSYNC | 46 | VSYNC | 3 |
| DE | 5 | DISP | n/c (backlight via CH422G) |
| B0..B4 | 14, 38, 18, 17, 10 | G0..G5 | 39, 0, 45, 48, 47, 21 |
| R0..R4 | 1, 2, 42, 41, 40 | | |

| CH422G output | Function |
|---------------|----------|
| EXIO1 | GT911 reset (TP_RST) |
| EXIO2 | Backlight enable (DISP): 1 = on. A switch only, no PWM. |
| EXIO3 | LCD reset (LCD_RST) |
| EXIO4 | SD card CS (active low) |
| EXIO5 | USB/CAN select: **low = USB**. Must be driven low at init or USB stops working. |

CH422G I2C "addresses" (one per register), as used by Waveshare's driver: 0x24 = WR_SET
(system/mode config), 0x23 = WR_OC (open-drain OC outputs), 0x38 = WR_IO (output IO0–7),
0x26 = RD_IO (input). Init: set WR_SET bit 0 (IO_OE) so all IO pins are outputs, then write the
initial output state with EXIO5 low and the others as the reset sequences require.

**Reset sequences** (from the Waveshare demo):

- LCD: LCD_RST low, wait 10 ms, LCD_RST high, wait 100 ms.
- Touch: GPIO4 (INT) as output, driven low; wait 10 ms. TP_RST low for 100 ms, then high, wait
  200 ms. Then release INT to an input. The GT911 comes up at address **0x5D**.

Panel timing (from the Waveshare demo): PCLK 16 MHz, HSYNC pulse/back porch/front porch =
4/8/8, VSYNC = 4/8/8, `pclk_active_neg = 1` (data sampled on the falling edge). Waveshare's
ESP-IDF code also configures a bounce buffer of 800×10 px with the comment "Set bounce buffer to
avoid screen drift". That's direct evidence for the drift risk (see Bandwidth below).

GPIO0 (G1) and GPIO45/46 (G2/HSYNC) are strapping pins. That works on Waveshare's design, but
nothing else may be attached to them.

### esp-hal support check

Checked against the **esp-hal 1.2.2** crate source (2026-10-21). Items still tagged weren't
covered by that check.

- **LCD_CAM RGB/DPI (verified):** `esp_hal::lcd_cam::lcd::dpi::Dpi` exists (behind the
  `unstable` feature). It has `with_data0..with_data15`, `with_hsync/vsync/de/pclk`, and
  `Config`/`Format`/`FrameTiming`. `send(next_frame_en: bool, buf: impl DmaTxBuffer) ->
  DpiTransfer` with `next_frame_en = true` sets `lcd_next_frame_en`, which gives continuous
  scanout. So the looped-scanout API exists. **Still untested: whether it runs stably from a
  PSRAM `DmaTxBuf`.** Step 1.3 tests that. The module doc example uses `dma_loop_buffer!`, a small
  repeating SRAM buffer, not a full PSRAM framebuffer [VERIFY whether any esp-hal example scans
  out from PSRAM].
- **PSRAM:** esp-hal supports octal PSRAM on the S3 (`psram` config in `esp_hal::Config`,
  `esp_hal::psram`). `esp-alloc` can add the PSRAM region as a heap region
  (`esp_alloc::psram_allocator!` or equivalent) [VERIFY PSRAM init/config API and macro names].
  GDMA on the S3 can read external memory. **Verified:** `ExternalBurstConfig` exists with
  `Size16/Size32/Size64`. The buffer is expected to need alignment to the chosen burst size
  [VERIFY the alignment rule].
- **Cache coherence:** the CPU writes to PSRAM through the data cache, but DMA reads PSRAM
  directly. After rendering, dirty cache lines must be written back, or the panel shows stale
  pixels. A looped transfer hands the buffer over once, so esp-hal won't write back per frame.
  **Verified:** `esp_hal::soc::cache_writeback_addr(addr, size)` exists for esp32s3. It wraps the
  ROM `Cache_WriteBack_Addr`, but it's `#[doc(hidden)] pub unsafe`, so it's **usable but not a
  stable API**. Wrap it in one firmware function so an API change touches one place. esp-hal also
  has an internal `ManualWritebackBuffer`, which isn't public API. Step 1.4 tests coherence
  directly. With bounce buffers (below) the DMA reads internal SRAM and the CPU copy reads PSRAM
  through the cache, so **no writeback is needed on that path**.
- **DMA descriptors:** each descriptor covers at most 4095 bytes, so a 768,000-byte framebuffer
  needs about 188 descriptors (≈2.3 KB in internal RAM). Descriptors must sit in internal SRAM.

### Bandwidth and tearing

- Frame: (800+4+8+8) × (480+4+8+8) = 820 × 500 = 410,000 PCLKs → at 16 MHz ≈ **39 Hz**.
- Scanout bandwidth: 16 MHz × 2 B = **32 MB/s, continuous, read from PSRAM**.
- Octal PSRAM at 80 MHz DDR has about 160 MB/s raw and much less in practice. CPU rendering
  writes, WiFi buffers that may live in PSRAM, and cache misses compete for that bandwidth.
- **Failure mode:** if the LCD DMA FIFO underruns, the image shifts or drifts horizontally and
  stays shifted until the next VSYNC resync. This is a known ESP32-S3 RGB-panel problem. ESP-IDF
  fixes it with SRAM "bounce buffers" refilled by the CPU, and **Waveshare's own ESP-IDF code for
  this board enables a bounce buffer of 800×10 px "to avoid screen drift"**. So drift is the
  *expected* outcome of direct PSRAM scanout under load, not an edge case. **esp-hal has no
  bounce-buffer support as far as I know** [VERIFY].
- **Mitigation plan.** Concrete, with a decision gate in step 1.4:
  - **M0, baseline (step 1.3):** direct looped scanout from the PSRAM framebuffer at 16 MHz.
  - **M1, cheap tuning (always applied):** keep WiFi/net buffers, DMA descriptors and hot data in
    internal SRAM. PSRAM holds only the framebuffer, the Slint heap and TLS buffers. Use partial
    rendering: only dirty regions, rendered into an SRAM line buffer and copied line by line into
    the framebuffer (sequential writes, cache-friendly). Use a 64-byte external burst if it's
    stable. Run the PSRAM clock at the highest stable setting [VERIFY whether esp-hal exposes
    120 MHz octal].
  - **Gate (step 1.4):** run M0+M1 under synthetic PSRAM contention (a core-1 task doing
    continuous PSRAM memcpy) for 10 min. If there's any drift, M2 becomes mandatory. Given
    Waveshare's experience, **plan for M2**.
  - **M2, bounce buffers (step 1.5, firmware-owned):** two internal-SRAM buffers of 800×10 px
    (16,000 B each, matching Waveshare) in a circular DMA descriptor chain passed to `Dpi::send`
    as a custom `DmaTxBuffer` implementation [VERIFY that the trait can be implemented outside
    esp-hal and that a circular chain is accepted]. A GDMA out-EOF interrupt fires per buffer
    [VERIFY the esp-hal interrupt API on the channel]. The handler copies the next 10 lines from
    the PSRAM framebuffer into the buffer just consumed. It keeps a fill counter modulo 48 (480/10)
    to know which lines come next, and resyncs to line 0 on the LCD VSYNC interrupt if available
    [VERIFY]. Run the handler on **core 1** (esp-hal second-core start and interrupt affinity
    [VERIFY APIs]), with the app and WiFi on core 0. Cost: about 30 MB/s of PSRAM→SRAM memcpy,
    which may take a large share of core 1 and pollutes the data cache. Measure it. Consider
    upstreaming this to esp-hal once it works.
  - **M3, if M2's CPU cost is too high:** lower PCLK to 12–14 MHz (≈29–34 Hz refresh, still fine
    for this UI). That cuts both bandwidth and memcpy load by up to 25 %.
  - **M4, deferred last resort:** a 4-bit palette framebuffer (192 KB) in *internal* SRAM,
    expanded through a lookup table in the bounce handler. Scanout then uses no PSRAM at all, but
    it limits the UI palette and competes with WiFi for SRAM.
- **Tearing:** one buffer scanned out continuously means a frame can show half-updated content.
  For a UI that updates once or twice per second, a one-frame tear is barely visible, so **I
  accept it in v1**. Double buffering (2 × 768 KB fits in 8 MB) would need retargeting the DMA
  descriptor chain at VSYNC. That's deferred until it's confirmed that esp-hal allows it cheaply.

---

## Data source (feed format)

| Port | Format | Device-side work | Trade-off |
|------|--------|------------------|-----------|
| **30002** (specified) | AVR raw text. **This fr24feed emits only the timestamped form** `@<12 hex timestamp><14 or 28 hex>;\n` (step 0.1). The plain `*8D4840D6202CC371C32CE0576098;\n` form is what other feeders emit; it's still supported. | Full Mode S / ADS-B decode: CRC, DF17/18 type codes, CPR, velocity, altitude | Most work, but well specified, self-contained and fully host-testable. No dependence on fr24feed's decoder quirks. |
| 30003 | SBS/BaseStation CSV (`MSG,3,...,callsign,alt,gs,track,lat,lon,...`) | Parse CSV | Least work, and positions/velocity come pre-decoded. Depends on fr24feed's SBS output being enabled and correct. Fields are sparse per message type. |
| 30005 | Beast binary (0x1A escaping, MLAT timestamp, signal level) | Binary framing + full decode, same as 30002 | Adds framing complexity, gains nothing for this use case. Its adapter fits the frame-source boundary below and is deferred. |

**Decision (D2, resolved by the user): keep 30002** and decode on the device. The decoder is the
most testable part of the system (published test vectors exist), and the aircraft table runs on
our own decoded state either way. The raw format also fits the general-client goal (D6) because
the boundary carries raw frames. SBS (30003) is pre-decoded, so it does **not** fit the
frame-source boundary. If it's ever wanted, it would feed the tracker through a separate update
path.

### Frame-source boundary (general Mode S client)

D6 asks for a general ADS-B/Mode S client that could later take frames straight from an SDR.
The boundary sits at **raw Mode S frames**, not at text lines:

- **`RawFrame`**: `{ data: Short([u8; 7]) | Long([u8; 14]), timestamp: Option<Timestamp>,
  signal: Option<SignalLevel> }`. `Timestamp` carries ticks and a clock kind (e.g. Beast/dump1090
  12 MHz counter, SDR sample index at rate R, or none), so MLAT-grade timestamps can pass through
  unchanged. `SignalLevel` is an RSSI-like value whose scale depends on the source.
- **`FrameSource`** (adapter trait, **sans-IO**): `feed(&mut self, input: &[Self::Input], emit:
  &mut impl FnMut(RawFrame))` plus counters (accepted, malformed, Mode A/C skipped). The adapter
  never does IO. The caller (a firmware task, a host tool, a test) owns the transport and passes
  in whatever it read. That keeps the library runtime-agnostic: no Embassy or tokio dependency,
  and it's testable byte by byte. [The exact trait shape is settled in step 2.1; the
  `Input`-associated-type form lets an SDR adapter take I/Q samples instead of bytes.]
- **Adapters:**
  - `AvrSource` (port 30002 text, `Input = u8`): **built now**. The `@`-timestamped form is the
    primary path, since it's all this receiver sends (step 0.1); the `*` form is secondary.
  - `BeastSource` (port 30005 binary, `Input = u8`): named, **deferred**.
  - `Demodulator` (direct 1090 MHz I/Q samples from an SDR, `Input = IQ sample`, on another host
    or, in principle, on the device): named, **deferred**. Not built in this project.
- **Decoder:** `modes::decode(&RawFrame) -> Result<Message, DecodeError>`. It's stateless and
  sees only raw frames, so it doesn't know or care which adapter produced them.
- **Tracker** (stateful policy: aircraft table, CPR pairing, vicinity) consumes `Message`s and
  lives in a separate crate from the decoder (see ADR).

Decoding scope (v1):

- **CRC-24** parity check on every 112-bit DF17/18 frame. Drop it if non-zero. No error correction.
- **DF17**, and **DF18 with CF=0** (ADS-B from non-transponder devices, same ME format). Other DF18
  CF values (TIS-B/ADS-R) are deferred.
- Type codes:
  - TC 1–4: identification (callsign, 8 chars of a 6-bit charset; emitter category).
  - TC 9–18: airborne position with barometric altitude (Q=1 25 ft encoding; Q=0 Gillham
    100 ft encoding, now in scope, see short frames below).
  - TC 20–22: airborne position with GNSS altitude.
  - TC 19: airborne velocity. Subtypes 1/2 (ground speed → track = atan2) and 3/4 (airspeed +
    magnetic heading when available), plus vertical rate.
  - TC 5–8: surface position, deferred (needs reference-based decode, low value for a vicinity
    display).
- **CPR:**
  - Global decode from an even/odd pair at most 10 s apart and in the same NL zone. Otherwise reject.
  - Local decode against a reference (the aircraft's last known position if fresh, else the
    receiver position). Valid within ±180 NM of the reference for airborne positions. A first fix
    from receiver-relative local decode is marked **provisional** until a global pair or a
    consistent second fix confirms it. If a global decode contradicts it, the global decode wins.
  - Plausibility gate: drop positions more than 450 km from the receiver, or an implied speed
    jump above about 1,000 kt from the previous fix. The gate is about **decoding sanity** and is
    independent of the 1–5 km display radius.
  - Use `f64` for CPR maths (the S3 FPU is single-precision only, so f64 is software, but the
    message rate is a few hundred per second, which is fine).
- **Short Mode S frames (changed in revision 2: now in scope)**, because a general client is
  wanted (D6). They're also useful here: they give altitude and squawk for nearby aircraft that
  have a Mode S transponder but no ADS-B out (common in general aviation). **Step 0.1 confirms
  they dominate this feed:** 339 of 372 sampled lines (91 %) were short frames. So the
  known-ICAO filter below is on the hot path.
  - **DF11** all-call reply: AA (ICAO address) + capability. Accept it when the CRC remainder is 0
    (acquisition squitter, II/SI = 0). A non-zero remainder that fits in 7 bits is an interrogator
    ID: expose the IID, and accept the address only if it's already known.
  - **DF4/DF20** (altitude reply) and **DF5/DF21** (identity reply): 13-bit altitude code (M and Q
    bits; Q=1 25 ft, Q=0 Gillham) and 13-bit squawk. These frames use **address/parity
    overlay**: the parity field is CRC XOR ICAO address. The decoder exposes the *recovered*
    address and can't validate it. The **tracker** accepts the frame only if that address
    is already known from a DF11/DF17/DF18 seen in the last 60 s (the "known-ICAO filter",
    as in dump1090). Otherwise it drops the frame, because a corrupted frame would recover a
    random address and create a ghost aircraft.
  - DF20/21 Comm-B payload (MB, 56 bits): exposed raw. BDS register inference is **deferred**
    (ambiguous, low value here).
  - DF0/DF16 (ACAS): decode altitude through the same AP path. The ACAS fields are deferred.
- Mode A/C replies (`*XXXX;`, 4 hex digits in AVR) are skipped and counted, not decoded. None
  appeared in the step 0.1 sample.
- **`@` timestamps:** 12 hex digits = 48-bit counter. The tick rate is assumed to be 12 MHz (the
  dump1090/Beast convention) [VERIFY for fr24feed; calibrate from the 10-minute capture, see
  0.1]. The tracker doesn't need the timestamp, but simulator replay uses it for pacing (S.6).

---

## Route and aircraft metadata

ADS-B carries the **ICAO24 address** and the **callsign**. It doesn't carry origin, destination,
type or registration. Those need a lookup:

- callsign → route (e.g. `KLM1023` → AMS–…)
- ICAO24 → registration/type (e.g. `4840D6` → PH-xxx, B738)

| Option | How | Pros | Cons |
|--------|-----|------|------|
| (a) Defer | Show callsign, ICAO24 and a placeholder "—" for route/type | No new risk, ships fastest | Doesn't meet the "source, destination, airplane" goal |
| (b) On-device HTTPS (**chosen**) | `reqwless` HTTP client + TLS to `https://api.adsbdb.com/v0/callsign/{cs}` (verified) and `/v0/aircraft/{icao}`. Parse JSON with `serde-json-core`. LRU cache with negative caching. | Self-contained device, no extra infra | TLS on `no_std` is required (`http://` answers 301 → https, verified). Stacks: **embedded-tls 0.19.0** (2026-06; pure Rust, TLS 1.3 only, certificate verification limited [VERIFY current algorithm support]) or **mbedtls-rs 0.3.0** (2026-09; Rust wrapper over mbedTLS. It appears to succeed esp-mbedtls, which is no longer on crates.io as a stable crate. ESP32-S3 hardware acceleration [VERIFY]; whether it needs a C cross-build or ships prebuilt libs [VERIFY]). Roughly 20–40 KB of TLS buffers (fine in PSRAM). Third-party API rate limits, uptime and terms of use. Route data from callsign DBs is sometimes stale. |
| (c) LAN helper | Small service on the LAN (could run next to fr24feed) exposing `GET /route/{cs}` and `/aircraft/{icao}` over plain HTTP. It caches upstream lookups or uses a local DB. | Simplest firmware, no TLS on the device, upstream can change without reflashing | Another service to deploy and keep running. Its design and ops are outside this doc. |
| (d) Offline DB | ICAO24 → type/reg table in flash | Works offline | Global aircraft DB is hundreds of thousands of rows, too big unless filtered to a region. Routes change daily, so impractical. |

**Decision (D1 resolved = (b)).** Phases 1–4 still show "—" for route and type, so metadata can't
block hardware or UI work. Phase 5 then implements (b) behind the same `MetadataProvider`
boundary (firmware side) plus a pure cache and JSON-parsing layer (core side).

**API facts (verified 2026-10-21):**

- `GET https://api.adsbdb.com/v0/callsign/KLM1023` works. Shape:
  `{"response":{"flightroute":{"callsign","callsign_icao","callsign_iata","airline":{name,icao,iata,country,country_iso,callsign},"origin":{country_iso_name,country_name,elevation,iata_code,icao_code,latitude,longitude,municipality,name},"destination":{…same as origin}}}}`.
  We parse only what the UI shows: airline name, origin/destination `iata_code`, `icao_code`,
  `municipality` and `name` (airport name for the detail view). Everything else is ignored, which keeps the serde structs small.
- `GET /v0/aircraft/{icao}` for an unknown address returns `{"response":"unknown aircraft"}`. So
  `response` is a **string** on not-found and an object on found. [VERIFY the HTTP status codes
  for not-found (200 vs 404) and the **found-aircraft schema** (expected fields: type, ICAO
  type code, manufacturer, registration, owner).] [VERIFY the not-found shape for unknown
  callsigns, presumably the same string form.] [VERIFY whether adsbdb has a combined
  `/v0/aircraft/{icao}?callsign={cs}` query that returns both in one request; if so, use it to
  halve the request count.] [VERIFY rate limits and terms of use.]
- The string-or-object `response` probably can't go through an untagged enum in
  `serde-json-core` (it doesn't support `deserialize_any`) [VERIFY]. Plan: check for the
  `"response":"` string form first, then parse into the struct.

**TLS stack decision (re-decided with the 2026 crate landscape): default embedded-tls 0.19,
gated fallback mbedtls-rs 0.3.** Reasons:

- It's pure Rust, so it fits the Rust constraint and the user's preference against extra software
  stacks. No C toolchain enters the Xtensa build.
- `reqwless` has an integrated embedded-tls connector [VERIFY that reqwless 0.14 is compatible with
  embedded-tls 0.19].
- mbedtls-rs is a month old as a release line, and its S3 acceleration is unverified.

embedded-tls's weak points are TLS 1.3 only (one cipher suite family) and limited certificate
verification. Step 5.2 is a **spike with explicit gate criteria**:

1. The handshake with `api.adsbdb.com` succeeds (TLS 1.3 negotiated).
2. The server chain can be verified against a **pinned root CA** compiled into the firmware. The CA
   certificate is public, so committing it is fine. [VERIFY which CA issues api.adsbdb.com's
   certificate and whether embedded-tls can verify that algorithm, e.g. RSA vs ECDSA roots.]
3. Peak memory stays acceptable (16 KB+ record buffer in PSRAM).

If any criterion fails, switch to mbedtls-rs. If **neither** stack can verify the certificate,
stop and report. Shipping unverified TLS is a security trade-off the user must accept
explicitly. The impact is low (the data is public and the device sends only callsigns and ICAO
addresses), but it isn't the architect's call.

Certificate validity checks need wall-clock time, so Phase 5 adds **SNTP**. It used to be deferred;
it's now in scope and also gives the status bar a real clock.

**Lookup policy (new, follows from the 1–5 km radius):** don't look up every aircraft heard (up to
128). Look up only the aircraft that can appear on the main screen: those inside the radius,
"approaching" ones (closest approach predicted inside the radius), and the nearest 5 overall. That
cuts the request rate to a few per minute and keeps well inside any reasonable rate limit.

---

## UI framework

| Option | Trade-off |
|--------|-----------|
| **Slint** 1.18.1 (`slint` with `default-features = false`, software renderer, `unsafe-single-threaded`, `libm`; `slint-build` compiling `.slint` at build time) [VERIFY feature names for 1.18] | Declarative UI, built-in `ListView`/scrolling/touch handling, partial (dirty-region) rendering, line-by-line rendering into an SRAM buffer. It has official MCU support with ESP32-S3 examples, and the same `.slint` file renders on the host for headless snapshot tests. It needs `alloc` (PSRAM heap is fine). Licence: GPLv3 or Slint's own licences (decision D3). |
| embedded-graphics (+ own `DrawTarget` over the PSRAM framebuffer, `embedded-text`, `u8g2-fonts`/`profont`) | MIT/Apache, no alloc, tiny and predictable. All widgets, scrolling, hit-testing and dirty tracking are hand-written: more code for the list and detail views. |
| LVGL via `lvgl-rs` | Mature C UI, but the Rust bindings are thinly maintained, the bindgen + Xtensa C cross-build is fragile, it's not idiomatic Rust, and it brings a C dependency the constraints try to avoid. |
| Others (Kolibri immediate-mode on embedded-graphics, `mousefood` = ratatui on embedded-graphics) | Lightweight. Kolibri is immature. Mousefood gives a text-mode look, which is acceptable but doesn't use the panel well. |

**Decision: Slint under GPLv3** (D3 resolved). embedded-graphics stays the technical fallback,
for example if render performance fails in 4.1. The UI is a thin layer over `adsb-core::Snapshot`,
so switching costs only Phase 4. Revision 3 also counts in Slint's favour: the same `.slint` files
and the same software renderer run unchanged in the browser simulator (ADR-002).

---

## Decision (ADR)

**ADR-001: Architecture of the ADS-B display firmware**

- **Context:** as above. Hardware risk (RGB scanout from PSRAM under WiFi load) is the biggest
  unknown. Decoding is well specified and testable. Route metadata needs an external source.
- **Decision:**
  1. **Separate Cargo projects in the repo, not one workspace** (revision 2 splits the former
     single core crate into two, for D6):
     - `modes/`: **general Mode S / ADS-B library**. `#![no_std]` (with `std` only under
       `cfg(test)`), built and tested with the **stable** host toolchain. Contents: `RawFrame`,
       the `FrameSource` trait, the `AvrSource` adapter, CRC/parity, the stateless `decode()` for
       DF0/4/5/11/16/17/18/20/21, the CPR primitives (global pair and local decode as pure
       functions), and altitude/squawk/Gillham decoding. Dependencies: `heapless`, `libm`. No
       knowledge of fr24feed, the display, or any runtime. This is the piece a future SDR host
       tool or on-device demodulator plugs into.
     - `adsb-core/`: `#![no_std]` application logic on top of `modes` (`path = "../modes"`).
       Contents: tracker / aircraft table with ageing, CPR pairing state, the known-ICAO filter,
       geo maths, dead reckoning, closest point of approach, vicinity and overhead selection,
       `Snapshot`, and the metadata cache and JSON parsing (Phase 5). Dependencies: `modes`,
       `heapless`, `libm`, `serde` + `serde-json-core` (Phase 5). Time is passed in as
       `now_ms: u64`.
     - `firmware/`: generated by `esp-generate` for `esp32s3`, uses the `esp` toolchain via
       `rust-toolchain.toml`, target `xtensa-esp32s3-none-elf`, and depends on `modes` and
       `adsb-core` via path.
     - `adsb-app/`, `sim/` and `sim-bridge/` (revision 3, replacing the former `ui-sim/`): the
       shared UI/app layer, the browser simulator plus headless PNG renderer, and the localhost
       bridge. See ADR-002. The `.slint` files now live in `adsb-app/ui/`, not in `firmware/`.
     - Separate projects stop the firmware's `.cargo/config.toml` (target, `build-std`) and the
       `esp` toolchain from affecting host and wasm builds.
  2. **Feed:** 30002 raw AVR through `AvrSource`, decoded on the device (D2). Other transports
     plug in at the `FrameSource` boundary (D6).
  3. **Stack.** Versions are from crates.io on 2026-10-21. The compatible set that `esp-generate`
     1.4.0 produces at step 0.2 is authoritative, and if it differs, record why in the revision
     log.

     | Crate | Version | Notes |
     |-------|---------|-------|
     | `esp-hal` | 1.2.2 | `unstable` feature for LCD_CAM/PSRAM |
     | `esp-rtos` | 0.4.0 | Embassy integration that replaced `esp-hal-embassy` (last release 0.9.1, 2025-10) |
     | `esp-radio` | 1.0.0-beta.1 (esp-generate 1.4.0; crates.io max stable 0.18.0) | WiFi; renamed from `esp-wifi` (last release 0.15.1, 2025-10) |
     | `esp-alloc` | 0.11.0 | |
     | `embassy-executor` | 0.10.0 | |
     | `embassy-net` | 0.9.1 | TCP, DHCPv4, UDP (SNTP), DNS (Phase 5) |
     | `slint` | 1.18.1 | |
     | `reqwless` | 0.14.0 | Phase 5 |
     | `embedded-tls` / `mbedtls-rs` | 0.19.0 / 0.3.0 | Phase 5, default and fallback |
     | `gt911` | 0.3.0 (2025-02) | Only if it supports embedded-hal-async 1.0 [VERIFY] |

     Also: `esp-backtrace`, `esp-println` + `log`, `embassy-time`, `embassy-sync`, `static_cell`
     (versions from esp-generate).
  4. **UI:** shared in `adsb-app` (ADR-002). On the device: Slint software renderer, line-by-line
     into an SRAM line buffer, copied into a single PSRAM framebuffer. Scanout is either direct looped `Dpi` DMA or, more likely, through
     firmware-owned SRAM bounce buffers (M2, step 1.5). Tearing is accepted in v1. Main screen is
     an "overhead" card, with a list secondary (D4).
  5. **Metadata:** adsbdb.com over HTTPS from the device (D1 = b). embedded-tls by default,
     mbedtls-rs if the step 5.2 gate fails. SNTP for certificate validity.
  6. **Configuration:** build-time environment variables read by `firmware/build.rs` (below).
     Never committed.
  7. **Privacy (new):** no local data is committed. No captures, no receiver location, no
     responses recorded from local traffic. The receiver location isn't logged at `info` level.
     See [Privacy](#privacy-no-local-data-in-the-repo).
  8. **CI:** GitHub Actions (D5), see steps 0.5 and S.7.
- **Consequences:**
  - All decoding logic is CI-tested on the host with no device.
  - `modes` is reusable on its own (host tools, a future SDR path) and stays free of
    display/vicinity policy.
  - Hardware integration is de-risked first (Phase 1) with a test-pattern spike before any UI
    investment.
  - Two toolchains (stable for the libraries, esp for firmware). CI needs both.
  - Credentials **and the receiver location** are baked into the firmware binary, so the binary
    must not be shared.
  - Changing the WiFi network means rebuilding and reflashing. A runtime provisioning UI is
    deferred.
  - Slint's GPLv3 applies to the firmware, `adsb-app` and `sim`. The whole repo is
    GPL-3.0-or-later (D7).
  - The pinned root CA can rotate. That needs a firmware update (R16).

**ADR-002: Browser simulator and shared app layer** (revision 3, D9)

- **Context:** the user wants to work on the UI without the board, from a podman devcontainer
  on macOS. X11/XQuartz forwarding was a bad experience and is ruled out. The user chose
  WebAssembly in the browser. The simulator has to exercise the *same* UI and app code as the
  firmware, or it validates nothing. It also has to keep the receiver location out of anything
  committed or shareable.
- **Options considered.** Rendering in the browser:
  - (A) Slint's stock web backend (winit + femtovg/WebGL canvas, wasm-bindgen) [VERIFY the
    current Slint 1.18 wasm setup and features]. Least glue code. But it's a different renderer
    from the device (GPU vector rendering vs the device's software renderer), it loads fonts
    differently, and our platform layer isn't exercised. Pixels won't match the device.
  - (B) **A custom minimal Slint platform:** `MinimalSoftwareWindow` + the software renderer
    with the same embedded fonts as the device, rendering into an 800×480 RGB565 buffer that's
    converted to RGBA and blitted to a 2D `<canvas>` via `web-sys`. Driven by
    `requestAnimationFrame`, with pointer events mapped exactly like touch. About 200–300 lines
    of glue. In exchange: **the same renderer, fonts, RGB565 quantisation and partial-repaint
    path as the device**, and the same code renders PNGs headlessly.
  - (C) VNC/noVNC of a native Linux build in the container: no XQuartz, but a heavy stack, laggy,
    and still a different backend.
  - (D) A native build on the mac host: the default Slint backend works, but it runs outside the
    container (a second toolchain on the mac). It's kept as a nice-to-have only.
- **Options considered.** Live data into the browser (browsers can't open raw TCP):
  - `websockify`: off the shelf, Python, fixed target.
  - **A small Rust bridge** (`sim-bridge`).
  - The Rust bridge is chosen. It's the same toolchain with no Python dependency, and one
    process can also serve the *local-only* data the simulator needs (receiver config, the
    replay capture, Phase 5 metadata proxy) with Origin/Host checks. websockify would need a
    second server for those anyway.
- **Options considered.** Build tool: `trunk` vs `wasm-pack`.
  - **trunk** is chosen. It builds, runs wasm-bindgen and serves with watch/rebuild, and has a
    WebSocket-capable dev proxy, so the browser sees a single origin. No node/npm needed.
  - wasm-pack targets npm packaging and would need a separate server. [VERIFY trunk's current
    version, its `[[proxy]]` `ws = true` support, and whether it fetches wasm-bindgen-cli itself
    or needs it pre-installed in an offline container.]
- **Decision:** (B) + `sim-bridge` + trunk. Option (A) is the fallback if the custom platform hits
  a wall. If it's used, note that pixel parity is lost.
- **Code split** (avoid duplicating the Slint model glue):

  | Crate | Kind | Contents | Used by |
  |-------|------|----------|---------|
  | `modes` | no_std lib | decoder (unchanged), plus an `encode` feature: frame and CPR *encoder* for tests and scenarios | all |
  | `adsb-core` | no_std lib | tracker, vicinity, meta cache (unchanged). **New:** `pipeline` (`FeedPipeline`: `AvrSource` → decode → tracker, with counters; the one ingest path for firmware and sim) and a `scenario` feature (deterministic, seeded synthetic traffic around a fictitious receiver, emitting `@`-timestamped AVR lines plus feed-outage events) | firmware, sim, tests |
  | **`adsb-app`** (new) | no_std + alloc lib | all `.slint` files and `slint-build` (software-renderer font embedding, committed OFL font [VERIFY licence fit]); the `App` view-model (screen/navigation state, chip override, selected aircraft); `Snapshot`/`Status`/meta → Slint model binding with diffing; `PointerEvent { x, y, phase }` → `slint::WindowEvent` mapping (touch and mouse share it); a generic line-render helper used by both platform layers, so partial repaint is identical | firmware, sim |
  | `firmware` | esp binary | platform layer only: esp `Platform` impl, `Dpi`/bounce scanout, GT911 → `PointerEvent`, Embassy tasks, TCP, TLS | — |
  | **`sim`** (new) | lib + wasm entry + native tests | platform layer only: buffer platform (`MinimalSoftwareWindow` → RGB565 buffer); web glue (canvas blit, rAF loop, pointer events, WebSocket/fetch/File inputs, controls); headless PNG renderer (native `cargo test`, replaces the old `ui-sim`); optional `native` feature for the mac host (default Slint backend, "should compile", no steps) | — |
  | **`sim-bridge`** (new) | std binary (tokio) | localhost-only: `/ws/live` (WebSocket ↔ TCP to the configured feed), `/local/config.json`, `/local/capture`, and from Phase 5 `/local/meta/*` (proxy to adsbdb) | sim (dev only) |

  Rule: anything that decides *what the UI shows or how it reacts* lives in `adsb-core` or
  `adsb-app`. `firmware` and `sim` only move bytes, pixels, time and pointer events.
- **Consequences:**
  - UI work can start as soon as the tracker (2.10) exists, without the board.
  - Sim PNGs and browser frames are bit-identical to each other and nearly so to the device. The
    remaining differences are panel colour and gamma and the scanout path.
  - The sim validates **none** of: drift/PSRAM bandwidth, render timing on Xtensa, memory limits,
    touch calibration, WiFi. Every [hw] step stays.
  - Slint features unify per build: `adsb-app` depends on `slint` with
    `default-features = false`, and each binary (firmware, sim) adds its own features.
    [VERIFY that the `slint-build` output for the software renderer with embedded fonts works
    for both xtensa and wasm32 from one build script.]
  - New tooling in the devcontainer (0.6) and a new CI job (S.7).

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
| `ADSB_RX_ELEV_M` | no (default 0) | receiver ground elevation, metres above MSL; used for height above receiver and elevation angle |
| `ADSB_RADIUS_KM` | no (default **5**, range-checked 0.5–50) | radius of the "overhead" zone (D4) |

`build.rs` emits `cargo:rustc-env=...` and `cargo:rerun-if-env-changed=...`. The code reads the
values with `env!`. A committed `firmware/.env.example` documents the vars with **fictitious**
values (never the real location). `.env` is in `.gitignore`, and the developer loads it with
`set -a; . ./.env; set +a` or direnv. Don't use `[env]` in `.cargo/config.toml`, because that
file is committed.

Test-only (host) variable: `ADSB_REPLAY_CAPTURE` is the path to a local, gitignored 30002
capture for the replay test (step 2.11). The replay test also reads `ADSB_RX_LAT/LON` at runtime
from the same local `.env`.

Simulator (revision 3). **Nothing location-related is compiled into the wasm bundle.**
`sim-bridge` reads the same local `.env` at runtime:

| Var | Used by | Default |
|-----|---------|---------|
| `ADSB_FEED_HOST`, `ADSB_FEED_PORT` | bridge `/ws/live` upstream (fixed; the client can't choose it) | — / 30002 |
| `ADSB_RX_LAT`, `ADSB_RX_LON`, `ADSB_RX_ELEV_M`, `ADSB_RADIUS_KM` | bridge `/local/config.json` | — |
| `ADSB_REPLAY_CAPTURE` | bridge `/local/capture` | — |
| `ADSB_SIM_BRIDGE_ADDR` | bridge listen address; **must be a loopback address**, anything else is refused at startup | `127.0.0.1:8081` |
| `ADSB_SIM_ALLOWED_ORIGINS` | accepted `Origin`/`Host` values | `http://localhost:8080,http://127.0.0.1:8080` |

trunk serves the app on port 8080 and proxies `/ws/*` and `/local/*` to the bridge. The browser
only ever talks to one forwarded port.

### Privacy: no local data in the repo

The receiver location is sensitive (D4/D6: doxxing). Aircraft positions near the receiver reveal
it just as well, especially with a 5 km radius. Rules:

- **Never committed:** receiver lat/lon/elevation, raw captures, logs from the device, and API
  responses recorded from local traffic (the set of callsigns seen locally narrows down the
  location too). `.gitignore` covers `.env`, `captures/` and `*.capture`.
- **Committed fixtures** are limited to the book's test vectors (*The 1090 Megahertz Riddle*,
  whose example positions are the book's, not ours), synthetic frames built by a test helper, and
  **fictitious receiver coordinates** (e.g. a mid-ocean point). The same goes for `ui-sim`
  fixtures and their PNGs, the `.env.example`, and the dummy CI env vars. adsbdb response fixtures
  are hand-written from the verified schema, using the book's `KLM1023` example.
- **Logging:** receiver coordinates are never logged. Aircraft lat/lon is logged only at `debug`
  or `trace`. At `info`, logs show callsign, ICAO, distance, bearing and altitude only. Release
  builds default to `info`. Combined with public flight-tracking history (callsign plus time),
  distance and bearing still reveal the location, so device logs shouldn't be pasted into public
  issues without review.
- The firmware binary contains the location, so it isn't shared (see Consequences).
- **Simulator (revision 3).** The goal is that a built sim (`sim/dist/`) is always safe to share
  and that local data only exists at runtime, served from the container's loopback.
  - **Receiver location:** the wasm bundle has no location in it. At startup the sim fetches
    `/local/config.json` from the bridge (read from the local `.env`). If that's unavailable
    (no bridge, CI, a shared `dist/`), the sim runs scenarios around a **fictitious** receiver and
    shows a "SIM: fictitious location" banner. Live mode stays disabled, because vicinity would be
    meaningless. **Query parameters and localStorage are deliberately not used** for the location:
    URLs end up in browser history, sync, screenshots and shared links, and localStorage persists
    in the browser profile beyond the session.
  - **Replay capture:** streamed by the bridge from `ADSB_REPLAY_CAPTURE` on request, or loaded
    with a browser file picker (the File API reads locally; nothing is uploaded). It's never
    copied into `sim/` or `dist/`. trunk must not reference `captures/` as an asset.
  - **Bundle check:** `dist/` is gitignored. The CI build (S.7) runs without any `.env`, and a
    check fails the build if `dist/` contains `config.json`, `*.capture` or the `captures/` path.
  - **Bridge exposure:**
    - It listens on loopback only.
    - Its upstream is fixed from env, so it isn't an open proxy.
    - It sends no CORS headers, so other sites can't read `/local/*`.
    - It **checks `Origin` and `Host`** on every request, including the WebSocket upgrade, which
      CORS doesn't protect. Without that, any web page open in the mac's browser could connect to
      the forwarded `localhost` port and read the location or the live feed. The same check also
      blocks DNS-rebinding attacks.
    - It never logs coordinates.
  - **Port forwarding** publishes to the mac's **loopback only** (`127.0.0.1`), never on the LAN
    (0.6).
  - Screenshots of the sim in live or replay mode carry the same risk as photos of the device.
    The detail view shows no raw lat/lon in either.

### "Vicinity"

Revised for D4 (radius 1–5 km, "the plane I hear overhead").

- The receiver position (`ADSB_RX_LAT/LON`) is the reference for local CPR decoding and for
  distance and bearing (haversine, f64) from the receiver to each aircraft.
- **Metric: ground distance, not slant range (D8 default).** At this radius altitude dominates. An
  airliner cruising directly overhead at 11 km has ground distance 0 but slant range 11 km.
  Filtering by slant range ≤ 5 km would hide exactly the "plane flying over" case. So:
  - **filter and rank by ground distance** (horizontal distance to the receiver);
  - show **altitude** and **height above the receiver** (altitude − `ADSB_RX_ELEV_M`) separately;
  - show the **elevation angle** (`atan2(height, ground distance)`), which together with bearing
    says *where to look* ("NE, 40° up").
  - Slant range is computed but only used internally (sound-lag estimate below).
  - Barometric altitude is pressure altitude (1013.25 hPa reference), not true height, so it can
    be a few hundred feet off. That's fine for display. The GNSS−baro difference field from TC19
    could correct it later (deferred).
- **Dead reckoning.** Positions arrive about twice a second with gaps. At 250 m/s a 4 s gap is
  1 km, which matters at this radius. The tracker computes a **predicted position** = last fix +
  ground velocity × age, for fixes up to **15 s** old with a velocity no older than 15 s. Beyond
  that the aircraft shows its last known distance marked "?". Ranking, radius tests, distance
  display and closest approach all use the predicted position. Displayed values carry a "~"
  when the fix is more than 3 s old. This is a pure function in `adsb-core` and unit-tested.
- **Closest point of approach (CPA).** From the predicted position and the ground-velocity vector
  (flat-earth local tangent plane; fine at < 50 km): `t_cpa = −(r·v)/|v|²`, `d_cpa = |r + v·t_cpa|`.
  An aircraft is **approaching** if `t_cpa > 0`, `d_cpa ≤ radius` and `t_cpa ≤ 5 min`. Then the
  card can already show the plane before the user hears it.
- **Sound lag.** Sound needs slant range / 343 m/s to reach the listener. That's about 30 s from
  10 km altitude, enough time for a jet to move about 7 km. A cruising jet you "hear overhead" has
  often already left a 5 km radius. Hence the **"just passed"** state below. For approach traffic
  at 1 km it's about 3 s, which is negligible.
- **Overhead selection** (computed in `adsb-core`, testable). The snapshot provides:
  - `overhead`: positioned (non-stale) aircraft with ground distance ≤ radius, nearest first. The
    **primary** is the nearest, with **hysteresis**: it only switches to a different aircraft if
    that one is closer by more than 20 % *and* 300 m, or if the current primary leaves the radius
    or goes stale. This stops the card flickering between two aircraft.
  - `just_passed`: the most recent primary that left the radius within the last 60 s.
  - `approaching`: aircraft with CPA inside the radius, soonest first.
  - `nearest_beyond`: nearest positioned aircraft outside the radius.
  - `list`: all positioned aircraft sorted by ground distance (in-radius ones flagged), then
    unpositioned or stale ones sorted by last-seen. These could include no-position Mode S
    aircraft heard via short frames.
- **Direction** shows two things: the aircraft's **track** (from velocity, or heading for airspeed
  subtypes) as a rotated arrow plus degrees, and **where it is from the receiver**: bearing,
  ground distance and elevation angle (e.g. "NE 2.3 km, 70° up").

### Embassy task structure

All Embassy tasks run on a single executor on core 0. **Core 1 is reserved for the
bounce-buffer refill handler** if M2 is needed (step 1.5). Otherwise core 1 is free, and UI
rendering could move there later if measurements call for it.

```
main (core 0)
 ├─ init: clocks 240 MHz, PSRAM, heaps (internal: WiFi/net; PSRAM: framebuffer + Slint + TLS),
 │        I2C 400 kHz → CH422G (IO_OE, EXIO5 low = USB, LCD reset seq, backlight on,
 │        touch reset seq → GT911 0x5D), Dpi scanout start (direct or via bounce buffers)
 ├─ wifi_task        esp-radio controller: connect, wait for disconnect, back off, reconnect
 ├─ net_task         embassy-net runner
 ├─ feed_task        loop { wait for link+IP; TCP connect HOST:PORT (timeout 10 s);
 │                          read → FeedPipeline.feed (AvrSource → decode → TABLE.lock(update));
 │                          idle timeout 60 s → reconnect; exponential backoff 1 s → 30 s }
 ├─ touch_task       owns I2C after init; GT911 poll at about 60 Hz (or INT-triggered);
 │                   → Channel<TouchEvent> to ui_task
 ├─ ui_task          owns the Slint window + adsb-app::App; every 1 s (card: 2 Hz if allowed):
 │                   TABLE.lock(age + snapshot(now)) → models; queue metadata lookups per
 │                   lookup policy; drains touch channel → window.dispatch_event;
 │                   render dirty region → line buffer → framebuffer (+ cache writeback if
 │                   direct scanout)
 ├─ sntp_task (P5)   SNTP over UDP at boot and every 6 h → wall clock for TLS validity and
 │                   the status bar
 └─ meta_task (P5)   pulls lookup requests from a Channel; DNS; HTTPS (reqwless + TLS);
                     fills metadata cache in the same mutex as TABLE
bounce ISR (core 1, if M2): GDMA out-EOF → copy next 10 lines PSRAM → SRAM bounce buffer
```

- Shared state: `TABLE: blocking_mutex::Mutex<CriticalSectionRawMutex, RefCell<AircraftTable>>`.
  Critical sections are short (one update or one snapshot copy).
- A `Status` struct (WiFi state, feed connected, msgs/s, CRC failures) sits in the same mutex,
  for the status bar.
- Capacity: `AircraftTable` holds at most 128 entries (`heapless`). When full, the oldest
  last-seen entry is evicted. Entries are removed after 60 s with no messages, and a position is
  marked stale after 30 s. Dead reckoning applies only up to 15 s (see Vicinity).
- `Status` also counts frames dropped by the known-ICAO filter and skipped Mode A/C lines.

### Simulator structure (revision 3)

The browser is single-threaded, so there's no executor: a `requestAnimationFrame` callback does
the work.

```
browser tab (wasm, sim crate)
 ├─ startup: fetch /local/config.json → receiver (else fictitious + banner); App::new(window)
 ├─ input (one active):
 │   live      WebSocket /ws/live (binary msgs = raw feed bytes) → queue; reconnect w/ backoff
 │   replay    fetch-stream /local/capture or File picker → paced by @ timestamps × speed
 │   scenario  adsb-core::scenario (seeded) → AVR lines + outage events, × speed
 ├─ rAF tick: now = sim clock (performance.now() for live; scaled virtual clock otherwise)
 │   drain queue → FeedPipeline.feed(bytes, now)            ← same code as firmware feed_task
 │   at 1 Hz (2 Hz card): tracker.snapshot(now) → App::tick  ← same code as firmware ui_task
 │   slint::platform::update_timers_and_animations; render dirty region via the adsb-app
 │   line helper → RGB565 buffer → RGBA → canvas putImageData (CSS-scaled, pixelated)
 └─ pointer events on canvas (mouse = touch) → PointerEvent → App/WindowEvent (shared mapping)
sim-bridge (container, 127.0.0.1:8081) ⇄ trunk serve (8080, proxy /ws, /local) ⇄ forwarded port
```

Controls (an HTML strip outside the 800×480 canvas, so the device UI itself stays unchanged):
input selector, scenario picker, speed (×1/×5/×20/×60), pause/step, "dump frame" (downloads the
RGB565 buffer's hash and PNG, for parity checks against the headless renderer).

### UI layout (800×480 landscape)

Revised for D4: the main screen answers "what is that plane overhead?" at a glance, readable from
a couple of metres away. Items marked * are "—" until Phase 5.

- **Status bar** (top, about 32 px): WiFi/feed status dots, aircraft count (total / in radius),
  msgs/s, clock (uptime until SNTP lands in Phase 5).
- **Main screen: overhead card** (the rest of the screen). States, in priority order:
  1. **Overhead**: the primary aircraft. Big callsign. Airline name*. Route* as large
     `ORIG → DEST` IATA codes with municipality underneath. Type* and registration*. Altitude
     (ft and FL) with a climb/descent arrow. Ground speed. **Track arrow** (rotated, large).
     **Where to look**: bearing, ground distance and elevation angle. Position age shown with "~"
     when dead-reckoned. If more aircraft are in the radius, a strip at the bottom shows
     "+N more" chips (callsign + distance). Tapping a chip makes that aircraft primary for 60 s.
  2. **Just passed**: same card, dimmed, with "left the 5 km zone 20 s ago, now 7.3 km NE".
     This covers sound lag.
  3. **Quiet (D8 default)**: nothing in the radius and nothing just passed. Show the **nearest
     aircraft beyond the radius, greyed out**, as a compact card: "Nothing within 5 km. Nearest:
     KLM1023 23 km NE". If any aircraft is approaching, show "approaching: closest 1.2 km in
     ~45 s" instead, so the card is ready before the plane is audible. *Why not a blank "nothing
     overhead":* blank looks the same as a broken feed. The greyed nearest aircraft proves the
     system is alive and often turns out to be the next overhead aircraft.
  4. **No data**: feed disconnected or no frames for 30 s. Show a clear error ("feed
     10.10.10.57:30002 unreachable, retrying in 8 s") instead of a stale card.
- **List view** (swipe or tab button from the card): rows about 56 px (8 visible). Columns:
  callsign (or ICAO24), type* | route* `ORIG→DEST`, altitude, GS, track arrow, ground distance +
  bearing. Rows in the radius are highlighted. It scrolls with drag.
- **Detail view** (tap the card or a row): ICAO24, callsign, squawk (from short frames),
  category, registration/type*, airline*, route* with airport names, altitude, height above
  receiver, vertical rate, GS, track, distance/bearing/elevation, CPA if approaching, last seen,
  position age. It doesn't show raw lat/lon by default, so photos of the screen don't reveal the
  location. Back button.

---

## Plan

Each step finishes with its verification green before the next starts. Step IDs are stable from
revision 2; revision 3 adds 0.6 and Phase S (S.1–S.8), moves 4.2 into S.2, and changes the
**execution order** to two parallel tracks so UI work doesn't wait for the board:

```
Track B — host / simulator (no board needed)
  0.2 → 0.4 → 0.5 → 0.6 → 2.1 … 2.11 → S.1 → S.2 → S.3 → S.4 → S.5 → S.6 → S.7
      → 4.3 → 4.4 → 4.5   (implemented and verified in the sim; device verification later)
      → 5.1 → S.8
Track A — board (starts after 0.2)
  0.3 → 1.1 … 1.6 → 3.1 … 3.5 (3.4 needs S.1's FeedPipeline)
      → 4.1 (needs S.2's adsb-app) → device verification of 4.3–4.5 → 4.6
Join — device metadata
  5.2 → 5.3 → 5.4   (needs Track A through 3.5, plus 5.1)
```

0.1 is done for formats; its 10-minute capture follows 0.2 (which creates `.gitignore`). Within a
track steps are sequential. The two tracks only synchronise where noted. One developer can
interleave them freely; the order inside each track is what matters.

### Phase 0 — scaffolding

0.1 **[hw/devops, manual]** Feed format check. Reachability is already confirmed (D6). From the
   container, run `nc 10.10.10.57 30002 | head -50`.
   *Verify:* lines look like `*[0-9A-F]{14|28};`. Note whether `@`-timestamped lines and 4-digit
   Mode A/C lines appear, and record only *which formats* appear, not the content, in the
   revision log. Save a 10-minute capture to a **gitignored** location (`captures/` in the repo,
   or outside it) for the local replay test (2.11). **The capture is never committed**: it
   reveals the receiver location. Run `git status` and `git check-ignore -v captures/` to
   confirm it's ignored.
   **Status (revision 3): format check DONE.** Sample of 12 s, 372 lines, kept outside the repo:
   - **every** line is `@` + 12 hex timestamp + 14 or 28 hex + `;`;
   - 339 short (14 hex) and 33 long (28 hex);
   - **zero** plain `*` lines, no Mode A/C lines;
   - LF line endings only (no CR).

   **Remaining:** the 10-minute capture, taken after 0.2 creates `.gitignore`. When taking it, also
   note the wall-clock start and end times in a gitignored sidecar file next to the capture. From
   that and the first/last `@` timestamps, compute the timestamp tick rate (expected 12 MHz
   [VERIFY]) for S.6 replay pacing. Record only the derived tick rate in the revision log.

0.2 **[devops]** Generate `firmware/` with esp-generate 1.4.0 for `esp32s3` with options
   embassy, unstable-hal, alloc, wifi/radio, PSRAM (if offered), log via esp-println
   [VERIFY flags with `esp-generate list-options`]. Fix the README invocation. Add a root
   `.gitignore` (`target/`, `.env`, `captures/`, `*.capture`) and the LICENSE file(s) per D7.
   Record the resolved crate versions in this doc's revision log and note any deviation from
   the table in the ADR.
   *Verify:* `cargo build --release` in `firmware/` succeeds in the container.

0.3 **[hw]** Flash the generated hello-world from the host (`espflash flash --monitor
   firmware/target/xtensa-esp32s3-none-elf/release/<app>`).
   *Verify:* boot log and periodic log line appear on the serial monitor. Note which USB port
   (UART vs native USB) works and record it in the README.

0.4 **[code]** Create `modes/` and `adsb-core/` (libs, `#![cfg_attr(not(test), no_std)]`, edition
   2024, deps `heapless`, `libm`; `adsb-core` depends on `modes` via path), each with one
   placeholder test.
   *Verify:* `cargo test` (host, stable) passes in both. `cargo build --target
   riscv32imc-unknown-none-elf` (or `thumbv7em-none-eabihf`, whichever is installed) succeeds for
   both, which proves no_std. Then `firmware/` depends on both via path and still builds.

0.5 **[devops]** CI on **GitHub Actions** (D5): `.github/workflows/ci.yml`, triggered on push and
   pull_request.
   - **Job `libs`** (`ubuntu-latest`, stable toolchain via `dtolnay/rust-toolchain@stable`
     [VERIFY action ref], plus the no_std target): runs `cargo fmt --check`,
     `cargo clippy --all-targets -- -D warnings`, `cargo test` and the no_std build, for `modes/`
     and `adsb-core/` (matrix over the two directories). `adsb-app/`, `sim/` and `sim-bridge/`
     join in S.7.
   - **Job `firmware`**: esp toolchain via `esp-rs/xtensa-toolchain@v1.x` with
     `default: true, buildtargets: esp32s3, ldproxy: false` [VERIFY current version and inputs].
     Runs `cargo build --release` and `cargo clippy` in `firmware/` with **dummy** env vars (fake
     SSID/password, `ADSB_FEED_HOST=192.0.2.1` (TEST-NET), fictitious lat/lon), set inline in the
     workflow. No repository secrets are needed, since CI never connects to anything. The ELF can
     be uploaded as an artefact, but the dummy-config build must never be confused with a real
     one.
   - **Caching:** `Swatinem/rust-cache@v2` [VERIFY version] with `workspaces:` listing `modes`,
     `adsb-core` and `firmware` (and `ui-sim` later) so each project's `target/` is cached
     separately. The firmware job's cache key includes the esp toolchain version.
   - *Verify:* locally, `actionlint` passes on the workflow (install it with `go install` or
     download the release binary into the scratchpad if it isn't present; if it's unavailable,
     note that and rely on the pipeline run). The remote is on GitHub, so the real check is to
     push the feature branch, **but only with the user's OK**. Check that the pipeline goes green,
     then push a commit with a deliberately failing test, check that it turns red, and revert it.

0.6 **[devops]** Devcontainer tooling for the simulator (revision 3).
   - **Add a new local feature** `./.devcontainer/features/wasm-sim/` (`devcontainer-feature.json` +
     `install.sh`, modelled on `./features/esp-rs`, running as the remote user):
     - `rustup target add wasm32-unknown-unknown` for the **stable** toolchain;
     - `cargo binstall -y --locked trunk wasm-bindgen-cli` [VERIFY: whether trunk needs a
       pre-installed wasm-bindgen-cli, which must match the `wasm-bindgen` crate version
       exactly, or downloads it; pre-installing avoids network fetches at build time];
     - optionally `wasm-opt` (binaryen) [VERIFY whether trunk fetches it].

     No websockify (D9: Rust bridge).
   - **Ports:**
     - add `"forwardPorts": [8080]` to `devcontainer.json`, which covers the VS Code path. Only
       the trunk port; the bridge stays internal.
     - For the devcontainer CLI path (no VS Code), document `runArgs` `-p 127.0.0.1:8080:8080`.
       Podman then publishes the port on the mac's loopback only, and inside the container trunk
       must bind `0.0.0.0` for the published port to reach it. Never publish without the
       `127.0.0.1:` prefix. [VERIFY that podman machine (libkrun/applehv) forwards published
       ports to the mac's loopback.]
   - **The user's `.devcontainer/` has uncommitted local modifications.** The devops engineer
     must:
     - run `git status`/`git diff -- .devcontainer` first and **coordinate with the user**
       before touching anything;
     - make only additive edits: a new feature directory, plus one `features` entry and
       `forwardPorts` in `devcontainer.json`, and `runArgs` only if the user wants the CLI path;
     - never run `git checkout`, `git restore`, `git stash` or overwrite whole files in
       `.devcontainer/`;
     - never commit the user's unrelated local changes along with this step.
   - The container rebuild is done by the user.
   - *Verify:* after rebuild, `rustup target list --installed` includes `wasm32-unknown-unknown`,
     and `trunk --version` and `wasm-bindgen --version` work. A throwaway hello page served by
     `trunk serve --address 127.0.0.1 --port 8080` opens at `http://localhost:8080` in the mac
     browser (VS Code path), or with `--address 0.0.0.0` behind the loopback publish (CLI path).
     `lsof -iTCP:8080 -sTCP:LISTEN` on the mac shows `127.0.0.1` only. `git diff -- .devcontainer`
     shows only the intended additions on top of the user's local changes.

### Phase 1 — hardware bring-up spike (de-risk first)

1.1 **[code+hw]** PSRAM init and PSRAM heap region. Allocate a 768,000-byte buffer and run a
   write/read pattern test.
   *Verify (serial):* PSRAM size logged as 8 MB, pattern test passes, internal heap free logged.

1.2 **[code+hw]** I2C bus (SDA GPIO8, SCL GPIO9, 400 kHz) and a minimal CH422G driver: write
   WR_SET (0x24) with IO_OE so all IO pins are outputs, then write WR_IO (0x38) outputs with
   **EXIO5 low (USB selected)**. Log an I2C address scan. Toggle the backlight (EXIO2) every
   second, leaving the other bits unchanged.
   *Verify:* the scan lists 0x23, 0x24, 0x26 and 0x38 (CH422G) and the GT911 (0x5D or 0x14, not yet
   reset). The backlight visibly blinks. **USB serial stays connected** throughout: the monitor
   keeps receiving logs after the CH422G write, which shows EXIO5 is correct. If the addresses
   differ from this doc, stop and correct the tables.

1.3 **[code+hw]** `Dpi` scanout from the PSRAM framebuffer: LCD reset sequence via CH422G (EXIO3
   low, 10 ms, high, 100 ms), `Dpi` with the verified pins and timings (16 MHz, 4/8/8, 4/8/8,
   falling edge), then `send(true, buf)` for a looped DMA transfer of a static test pattern. The
   pattern has 8 vertical colour bars (R, G, B, C, M, Y, W, K), a 1-px white border, and one row
   of each single bit set (R0..R4, G0..G5, B0..B4) as separate stripes.
   *Verify (visual):* bar colours in order (catches swapped R/B or bit-order errors). Each
   single-bit stripe has a distinct, monotonic intensity (catches miswired data pins). The border
   is visible on all four edges with no wrap (catches porch/timing errors). The image stays stable
   for 10 min. Record PCLK and timings.
   **If this step can't be made to work with esp-hal, stop and report**: the architecture
   depends on it (see Risks R1).

1.4 **[code+hw]** Update under load, plus the **drift gate**. Move a 100×100 rectangle across
   the framebuffer at about 10 Hz from the CPU, with cache writeback (the wrapped
   `cache_writeback_addr`) after each update. At the same time, run a synthetic PSRAM-contention
   load on core 1 (continuous memcpy between two PSRAM buffers) to stand in for later WiFi and
   render traffic.
   *Verify:* no stale-pixel trails (cache coherence works). Log CPU time per update. Repeat with
   writeback removed to confirm it's actually needed, and document the result. **Gate:** any
   horizontal drift within 10 min under the synthetic load means M2 (step 1.5) is mandatory.
   Record the outcome. If there's no drift, 1.5 is postponed until 3.5 re-checks under real
   WiFi load.

1.5 **[code+hw]** Bounce-buffer scanout (M2; mandatory if the 1.4 gate fails, which Waveshare's
   experience predicts). Two 16,000-byte internal-SRAM buffers (800×10 px) in a circular
   descriptor chain behind a custom `DmaTxBuffer`. A GDMA EOF handler on core 1 refills from the
   PSRAM framebuffer (fill counter mod 48, VSYNC resync if available), with no cache writeback
   on this path. Details and [VERIFY] items are under Bandwidth M2.
   *Verify:* the 1.3 pattern is pixel-identical (photo comparison of the single-bit stripes and
   border). The moving rectangle has no trails *without* writeback. No drift over 30 min under
   the 1.4 synthetic load. Log core-1 load from the handler (target < 50 %; if it's higher,
   apply M3 and lower PCLK) and the internal SRAM cost. **If the custom `DmaTxBuffer` or the
   interrupt hook isn't possible with esp-hal 1.2.x, stop and report** (R1/R2).

1.6 **[code+hw]** GT911: reset sequence as verified (INT/GPIO4 output low, 10 ms; TP_RST/EXIO1
   low 100 ms, then high, 200 ms; INT back to input) gives address 0x5D. Minimal async driver
   (read status, read points, clear status). Use the `gt911` 0.3.0 crate only if it supports
   embedded-hal-async 1.0 [VERIFY]. Otherwise write about 100 lines of our own. Log touch points.
   *Verify:* the I2C scan after reset shows 0x5D (not 0x14). Touching the four corners logs about
   (0,0), (799,0), (0,479), (799,479), with no axis swap or inversion (as documented for this
   board). Multi-touch isn't needed.

### Phase 2 — core decoding (host, TDD; may run in parallel with Phase 1)

Steps 2.1–2.7 live in `modes/` and 2.8–2.11 in `adsb-core/`. (Renumbered in revision 2: the
short-frame, kinematics and overhead steps were inserted. Old 2.7 → 2.8, old 2.8 → 2.10, old
2.9 → 2.11.)

Test vectors come from *The 1090 Megahertz Riddle* (J. Sun). **[VERIFY expected values against
the book when writing the tests; the ones below are from memory.]** Where the book has no vector
(short frames, Gillham edge cases), tests use **synthetic frames** from a test helper that
assembles fields and computes CRC/AP. Committed tests never use locally received data (see
Privacy).

| Frame | Expect |
|-------|--------|
| `8D4840D6202CC371C32CE0576098` | DF17, ICAO 4840D6, TC4, callsign `KLM1023` |
| `8D40621D58C382D690C8AC2863A7` (even) + `8D40621D58C386435CC412692AD6` (odd) | global CPR ≈ 52.2572°N, 3.9194°E; alt 38,000 ft |
| `8D40621D58C382D690C8AC2863A7` with ref 52.258, 3.918 | local CPR ≈ 52.2572, 3.9194 |
| `8D485020994409940838175B284F` | velocity ST1: GS ≈ 159 kt, track ≈ 182.9°, VR −832 ft/min |
| `8DA05F219B06B6AF189400CBC33F` | velocity ST3: heading ≈ 244.0°, TAS 375 kt, VR −2304 ft/min |

2.1 **[code]** `RawFrame`, the `FrameSource` trait (sans-IO, see [Frame-source
   boundary](#frame-source-boundary-general-mode-s-client)) and the `AvrSource` adapter (was
   `LineParser`): streaming and fixed-capacity. It accepts arbitrary byte chunks (lines split
   across TCP reads), and handles `@<12-hex ts>…;` (**primary**: it's the only form this feed
   sends, per 0.1) and `*…;`, with LF and tolerated CR/LF. The timestamp goes into
   `RawFrame.timestamp`. It emits 14/28-hex frames as Short/Long, skips and counts 4-hex Mode A/C,
   and rejects non-hex, other lengths and overlong garbage (resyncs at the next `*`/`@`) without
   panicking. Also write a short doc comment sketching how `BeastSource` and an SDR
   `Demodulator` would implement the trait. They aren't built. Review that the trait shape fits
   them (I/Q input type, timestamp clock kind, signal level).
   *Verify:* unit tests for each case, including byte-at-a-time feeding and timestamp parsing.
   Synthetic `@` lines mirror the observed shape (12 + 14 and 12 + 28 hex, LF only). A
   random-bytes test (`proptest` or a simple PRNG loop) shows no panics.

2.2 **[code]** Frame basics: DF; ICAO24 for DF11/17/18; CRC-24 (poly 0xFFF409); parity
   remainder; **address recovery** for AP-overlay formats (DF0/4/5/16/20/21); DF11 IID
   extraction. Add a test helper that builds synthetic frames with a given DF/address/payload and
   correct parity.
   *Verify:* the table vectors pass CRC. A single flipped bit fails. DF/ICAO extracted. A
   synthetic DF4 built for address `ABCDEF` recovers `ABCDEF`. A synthetic DF11 with IID 5 yields
   remainder 5.

2.3 **[code]** Identification (TC 1–4): callsign charset, trailing-space trim, category.
   *Verify:* `KLM1023` vector.

2.4 **[code]** Altitude: 12-bit ADS-B altitude field (Q=1 25 ft; Q=0 Gillham 100 ft) and the
   13-bit Mode S AC code (M bit metric flag → `None` in v1; Q bit; Gillham). Gillham was deferred
   in the draft and is now in scope.
   *Verify:* 38,000 ft vector. Gillham round-trip on synthetic codes across −1,000…+126,700 ft at
   100 ft steps, plus known-invalid codes returning `None`. M=1 returns `None`.

2.5 **[code]** CPR: NL function (table or formula), global pair decode (zone-mismatch reject),
   local decode, all as pure functions. *Verify:* the vectors above; zone-mismatch pair rejected;
   latitude edge cases near ±87°; longitude wrap at ±180°.

2.6 **[code]** Velocity TC19 ST1–4: GS/track, airspeed/heading (heading-available bit), vertical
   rate and sign, "no data" encodings. *Verify:* ST1 and ST3 vectors, plus zero/no-data cases.

2.7 **[code]** Short Mode S frames (new): DF11 (AA, CA, IID), DF4/20 altitude via 2.4, DF5/21
   squawk (13-bit identity → 4 octal digits), DF20/21 MB exposed raw, DF0/16 altitude only.
   `decode()` returns these with the *recovered* address and a flag saying the address is
   unverified (AP overlay).
   *Verify:* book vectors where available [VERIFY which DF4/5/20/21 examples the book or the
   pyModeS test suite provides]. Otherwise synthetic frames: squawk 7700/1200/7000 round-trip, and
   DF11 with remainder 0 accepted as verified.

2.8 **[code]** Geo (`adsb-core`): haversine distance, initial bearing, 8-point compass label,
   elevation angle from height and ground distance.
   *Verify:* known city pairs (e.g. AMS–LHR ≈ 370 km) within 0.5 %. Bearings at N/E/S/W.
   Elevation 45° when height = ground distance, and 90° directly overhead.

2.9 **[code]** Kinematics (new): dead-reckoned position (`predict(fix, velocity, now_ms)` with the
   15 s horizon) and closest point of approach (`t_cpa`, `d_cpa`) on a local tangent plane.
   *Verify:* synthetic straight-line tracks. An aircraft heading straight for the receiver at
   250 m/s from 10 km gives `t_cpa` ≈ 40 s and `d_cpa` ≈ 0. One passing 2 km abeam gives
   `d_cpa` ≈ 2 km. An aircraft moving away gives `t_cpa < 0` (not approaching). Prediction is
   refused when the fix or velocity is older than 15 s. Error against an exact great-circle
   track stays below 10 m at 15 s.

2.10 **[code]** `AircraftTable`/tracker: `update(message, now_ms)`; per-aircraft even/odd CPR state
   (global when the pair is ≤ 10 s apart, local vs last position or receiver otherwise;
   provisional first fix); plausibility gate (450 km / 1,000 kt, independent of the display
   radius); **known-ICAO filter** for AP-overlay frames; squawk/altitude from short frames;
   ageing (`age(now_ms)`); capacity eviction; `snapshot(now_ms, radius_km) -> Snapshot`
   containing `overhead` (with primary + hysteresis), `just_passed`, `approaching`,
   `nearest_beyond` and `list`, as specified in Vicinity. Receiver coordinates in tests are
   fictitious.
   *Verify:* unit tests with synthetic time cover:
   - position from a pair, and from a single message with a reference;
   - a provisional fix replaced by a contradicting global decode;
   - stale and expiry transitions;
   - eviction when full;
   - a DF4 for an unknown address is dropped, and for a known address it updates altitude;
   - list sort order (positioned by ground distance, then unpositioned);
   - overhead membership uses ground distance (an aircraft at 0 km ground and 11 km altitude is
     overhead);
   - hysteresis: no switch for a 10 % closer rival, a switch for 30 % and > 300 m;
   - `just_passed` set when the primary leaves the radius and cleared after 60 s;
   - `approaching` populated from CPA.

2.11 **[code]** Replay test, **local only**: an `#[ignore]` integration test reads the capture at
   `ADSB_REPLAY_CAPTURE` and the receiver position from `ADSB_RX_LAT/LON` (both from the local
   `.env`). It returns early with a message if they're unset, so `cargo test -- --ignored`
   without a capture still passes. It feeds `AvrSource → decode → tracker`. Run it with
   `cargo test -- --ignored replay`. It isn't part of CI, and its output prints counts and
   distances only, never coordinates.
   *Verify:* run locally against the capture from 0.1. No panics. CRC pass rate above 95 % for
   DF17/18. Each resolved position lies within 450 km of the receiver. At least 5 aircraft with
   callsign and position. Known-ICAO filter drop rate is logged. Separately, confirm with
   `git status` that no capture or output file is tracked.

### Phase 3 — networking

3.1 **[code]** `firmware/build.rs` config (see Configuration) and `.env.example` (fictitious
   values).
   *Verify:* building without vars fails with a message naming the missing var. Bad lat/lon,
   elevation or radius (out of range) fails. A valid `.env` builds. `git status` shows `.env`
   ignored. `grep` of the committed tree for the real coordinates finds nothing (run locally,
   don't paste the coordinates anywhere).

3.2 **[code+hw]** WiFi (esp-radio) + embassy-net DHCP. Log the IP, and reconnect after the AP
   drops.
   *Verify:* IP logged within 10 s of boot. Power-cycle the AP (or block the MAC) and the device
   reconnects and logs the new IP without a reboot.

3.3 **[code+hw]** `feed_task`: TCP connect, read loop into `AvrSource`, log frames/s, CRC
   failures and Mode A/C skips every 10 s, with backoff and idle timeout.
   *Verify:* rates logged roughly match the receiver's traffic. Stopping fr24feed (or firewalling
   the port) leads to logged reconnect attempts with growing backoff. Restarting it resumes
   without a reboot.

3.4 **[code+hw]** Wire the decoder and tracker through `adsb-core::pipeline::FeedPipeline` (from
   S.1; the same ingest code the simulator uses). Every 5 s, log the snapshot summary at `info`:
   primary/overhead/approaching callsigns with ground distance, bearing and altitude. Log the 5
   nearest with lat/lon at `debug` only (Privacy).
   *Verify:* compare against fr24feed's local status page or flightradar24 for the same moment.
   Callsigns and altitudes (±100 ft) agree for at least 3 aircraft, and positions agree within
   ±1 km (checked at debug level, locally). For an aircraft actually passing overhead, it shows
   up as primary while it's audible or visible (or as just passed shortly after). Don't paste
   the logs into shared places.

3.5 **[hw]** Re-run the 1.3 and 1.4 display checks (with 1.5 bounce buffers if they're in use)
   with WiFi plus the feed active.
   *Verify:* no drift over 30 min. If drift appears, apply M2 (if not yet done), then M3, and
   record the final scanout mode, PCLK and PSRAM settings here.

### Phase S — shared app layer and browser simulator (revision 3, Track B)

Starts after 2.10 (tracker). It doesn't need the board.

S.1 **[code]** Encoder, scenarios and ingest pipeline (no UI yet).
   - **`modes` `encode` feature:** builds DF17 identification, airborne position (CPR even/odd
     *encoding*), velocity ST1, DF11 and DF4/5 with AP. It supersedes the 2.2 test helper.
   - **`adsb-core::pipeline::FeedPipeline`:** `AvrSource` → decode → tracker update, plus
     counters. The single ingest path for firmware (3.4) and sim.
   - **`adsb-core` `scenario` feature:** deterministic, seeded scripts around a **fictitious**
     receiver, emitting `@`-timestamped AVR lines (so `AvrSource` is exercised too) and
     feed-outage events. Scenarios:
     - `overflight`, in order: quiet; a jet approaching; overhead at FL350; just passed; a GA
       aircraft at 1,500 ft passing 2 km abeam with short frames only after its first DF17;
       quiet; a 40 s feed outage (no data);
     - `busy`: 30 aircraft;
     - `edge`: long callsigns, no position, stale, squawk 7700.
   - *Verify:* encode→decode round-trip property tests for every encoded type, including CPR
     even/odd at random positions (error < 5 m). `overflight` run through `FeedPipeline` and
     `snapshot()` produces the state sequence Quiet → Approaching → Overhead → Just passed →
     Quiet → No data at the scripted times ±2 s. Same seed gives identical output.

S.2 **[code]** `adsb-app/` shared crate and the headless PNG renderer (replaces 4.2).
   - **`adsb-app`** (no_std + alloc):
     - the `.slint` files (status bar and card skeleton for now);
     - a `build.rs` with software-renderer font embedding and a committed OFL font;
     - the `App` view-model, model binding with diffing, `PointerEvent` mapping, and the generic
       line-render helper.
   - **`sim/` native part:** a buffer platform (`MinimalSoftwareWindow` → 800×480 RGB565 buffer)
     and a `cargo test` that renders scenario keyframes to `target/ui-snapshots/*.png`.
     Reference PNGs can optionally be committed later; they only ever contain fictitious data.
   - *Verify:*
     - `adsb-app` builds for the host, `wasm32-unknown-unknown`, and (via `cargo check` in
       `firmware/`) `xtensa-esp32s3-none-elf`. The latter proves the no_std/feature split.
     - The PNG test produces images for each `overflight` state.
     - A unit test drives `App` with `PointerEvent`s and asserts navigation state.

S.3 **[code]** Browser simulator with scenario input.
   - The `sim` wasm entry (wasm-bindgen, web-sys): the canvas blit, rAF loop and pointer events
     as in [Simulator structure](#simulator-structure-revision-3).
   - Controls: scenario picker, speed and pause.
   - The fictitious-location banner. `trunk serve` on 8080. `dist/` is added to `.gitignore`.
   - *Verify (manual, mac browser via the forwarded port):*
     - `overflight` walks through all card states.
     - Clicks behave like taps.
     - "Dump frame" at a scripted keyframe gives the **same hash** as the S.2 PNG renderer for
       that keyframe (pixel parity of the shared render path).
     - The devtools console shows no errors.
     - The wasm size is logged (target < 5 MB uncompressed, an estimate).
     - Optional: `cargo build --features native` for `sim` on the mac host compiles. That's the
       only native-host check; no further steps.

S.4 **[code]** `sim-bridge/` (tokio + axum or similar [VERIFY current versions and WebSocket
   support]).
   - Endpoints: `/ws/live`, `/local/config.json` and `/local/capture`, as in
     [Configuration](#configuration).
   - It refuses to start on a non-loopback address.
   - Origin/Host allow-list on every request, including the WebSocket upgrade (403 otherwise).
     No CORS headers. Coordinates are never logged.
   - trunk `[[proxy]]` entries for `/ws` (with `ws = true`) and `/local` [VERIFY syntax].
   - *Verify:*
     - Unit tests cover config parsing, the loopback-only check, and Origin/Host rejection
       (foreign origin, missing origin, rebinding-style Host).
     - Integration test: a local fake TCP server streams `overflight` lines, and a WebSocket
       client through the bridge receives them byte-exact.
     - Manual: `ss -ltnp` shows only `127.0.0.1:8081`. `curl -H 'Origin: http://evil.example'`
       is rejected. `/local/config.json` without a `.env` returns 404 and the sim shows the
       fictitious banner.

S.5 **[code + manual]** Live mode in the sim: WebSocket client with backoff → `FeedPipeline`,
   using real time. The receiver comes from `/local/config.json`; without it, live mode is
   disabled.
   *Verify (local, manual):* live aircraft appear. Cross-check 3 aircraft against flightradar24
   as in 3.4; this is now possible **before the board exists**. Stopping the bridge gives the No
   data state within 30 s, and restarting it recovers without a page reload. Don't share
   screenshots (Privacy).

S.6 **[code]** Replay mode: input from `/local/capture` or the file picker; pacing from `@`
   timestamps using the tick rate calibrated in 0.1; speed ×1/×5/×20/×60; restart. A virtual
   clock drives `now_ms`, so tracker timing (CPR pair window, dead reckoning) stays consistent
   at any speed.
   *Verify:* the 10-minute capture at ×20 completes in about 30 s (±10 %). The final
   `FeedPipeline` counters (frames, CRC failures, known-ICAO drops, aircraft seen) **equal** the
   2.11 host replay test's counters for the same capture, which shows determinism across
   targets. The file picker path gives the same counters.

S.7 **[devops]** CI for the new crates (extends 0.5).
   - `adsb-app` and `sim-bridge` join the `libs` matrix (fmt, clippy, test).
   - New job `sim`: add the wasm32 target; install trunk (binstall, or a trunk action [VERIFY]);
     run `trunk build --release` in `sim/`; run the native PNG test; upload the PNGs and `dist/`
     as artefacts; run the bundle check (fail if `dist/` contains `config.json`, `*.capture` or
     `captures/`).
   - `firmware` job: `cargo check` also covers `adsb-app` for xtensa (happens via the dependency).
   - Cache: add the new projects to `rust-cache` `workspaces`.
   - *Verify:* `actionlint` passes. With the user's OK, push: green; then a deliberately broken
     `.slint` turns both `sim` and `firmware` jobs red.

S.8 **[code]** (after 5.1) Metadata in the sim: bridge `/local/meta/callsign/{cs}` and
   `/local/meta/aircraft/{icao}` proxy to adsbdb over HTTPS (std HTTP client, same rate limit and
   lookup policy). The sim reuses `adsb-core::meta` cache + parser unchanged. Scenario mode uses
   the hand-written fixtures from 5.1.
   *Verify:* in live mode the card shows routes for at least 3 aircraft matching flightradar24.
   Scenario mode shows fixture routes with no network. Bridge unit tests cover the rate limit and
   the 404/429 pass-through.

### Phase 4 — UI (D3 resolved: Slint)

Revision 3: 4.3–4.5 are implemented and first verified **in the simulator** (Track B, after
S.7). Their device verification runs after 4.1 (Track A). UI code goes in `adsb-app`; firmware
only adds the platform glue.

4.1 **[code+hw]** Slint platform on the device: implement `slint::platform::Platform` (time from
   `embassy-time`), `MinimalSoftwareWindow` with partial repaint, and the `adsb-app` line-render
   helper into an SRAM line buffer → framebuffer copy → cache writeback (skipped when bounce
   buffers are in use). Show the `adsb-app` UI. For bring-up before networking, a build flag
   `ADSB_DEMO=overflight` feeds the S.1 scenario into `FeedPipeline` on the device.
   *Verify:* renders correctly with correct colours, and a photo of the demo keyframe matches
   the S.2 PNG. Log full-screen and partial render times.
   Target: full redraw under 150 ms, typical 1 Hz update under 30 ms [targets are estimates;
   adjust after measuring]. Decide here whether the card can update at 2 Hz.

4.2 **Moved to S.2 in revision 3.** The headless PNG renderer is now part of `sim/`.

4.3 **[code]** Main screen (changed in revision 2: previously the list view): overhead card +
   status bar bound to `snapshot()` (1 Hz, or 2 Hz per 4.1), with all four states (Overhead,
   Just passed, Quiet with nearest beyond / approaching, No data) and the "+N more" strip. Uses
   model diffing so unchanged parts don't repaint.
   *Verify (sim):* PNG snapshots for each state from the `overflight`/`edge` scenarios, plus:
   0 / 1 / 3 in radius; long callsign; no route; dead-reckoned "~" marker; elevation 10° vs 85°.
   In the browser, the live/replay modes look right.
   *Verify (device, after 4.1):* readability from 2 m (callsign and route legible). The card
   matches the 3.4 log output for the same moment.

4.4 **[code+hw]** Touch and navigation: `PointerEvent` → `WindowEvent::PointerPressed/Moved/Released`
   (plus scroll), with the mapping shared in `adsb-app`. Card ↔ list (swipe or tab button). Tap a
   "+N more" chip to make it primary for 60 s. Tap the card or a list row to open details. Back
   button. The list view (rows as in UI layout, in-radius rows highlighted) is built in this step.
   *Verify (sim, mouse):* drag-scrolls a 30-entry list (`busy`) smoothly enough (no missed
   clicks). A click opens the row that was clicked (check by callsign). Back returns to the list
   at the same scroll position. The chip override expires after 60 s (virtual clock).
   *Verify (device, after 4.1 and 1.6):* the same checks with the finger on the GT911, plus tap
   accuracy at the screen edges (calibration isn't covered by the sim).

4.5 **[code]** Detail view with track arrow (rotated path), receiver bearing/ground
   distance/elevation, squawk and CPA.
   *Verify (sim):* PNG snapshots at tracks 0/90/180/270/45 show the arrow pointing correctly.
   *Verify (device):* compare with the 3.4 log.

4.6 **[hw]** Soak: 12 h run, logging heap free (internal and PSRAM), reconnect count, aircraft
   count and (if M2 is in use) bounce-handler core-1 load every minute.
   *Verify:* no reboot or panic, heap free flat (no leak trend), display stable.

### Phase 5 — route and aircraft metadata (D1 = (b), in scope)

5.1 **[code]** `adsb-core::meta`: fixed-size LRU cache keyed by callsign (route) and ICAO24
   (aircraft), with TTLs (route 6 h, aircraft 7 d, negative 1 h). Parsing of the adsbdb
   responses with `serde-json-core`: the `flightroute` subset (airline name; origin/destination
   `iata_code`, `icao_code`, `municipality`, `name`), the string-form not-found
   (`{"response":"unknown aircraft"}`), and the aircraft schema once verified. Also the lookup
   policy as a pure function: which aircraft in a `Snapshot` need a lookup (in radius,
   approaching, nearest 5; not cached; not in flight).
   *Verify:* host tests on **hand-written** fixtures following the verified schema (the KLM1023
   route; not-found string; malformed; oversized; missing optional fields like `callsign_iata`).
   No responses recorded from local traffic (Privacy). Cache hit/miss/expiry/eviction. The lookup
   policy picks only the expected ICAOs from a synthetic snapshot of 50 aircraft.

5.2 **[code+hw]** TLS spike with a gate (see the TLS stack decision): SNTP (UDP; `sntpc` or a
   minimal own client [VERIFY crate and embassy-net compatibility]), DNS for `api.adsbdb.com`,
   then **embedded-tls 0.19** through `reqwless` 0.14 with the pinned root CA, fetching
   `/v0/callsign/KLM1023`.
   *Verify:* SNTP time is logged and plausible. The handshake succeeds with TLS 1.3. Verification
   is **on**: a deliberately wrong pinned CA makes the handshake fail (proves verification is
   real). The body parses with 5.1's parser. Peak heap use is logged. **Gate:** if handshake,
   verification or memory fails, repeat with **mbedtls-rs 0.3** (also check S3 hardware
   acceleration and build impact [VERIFY]). If neither verifies, stop and report to the user (no
   silent unverified TLS). Record the chosen stack and the reason in the revision log.

5.3 **[code+hw]** `meta_task`: request queue fed by the lookup policy (deduplicated, at most 1
   request/s, retry with backoff, back off fully on HTTP 429), keep-alive connection if
   `reqwless` supports it, results into the cache.
   *Verify:* the serial log shows a lookup per new in-scope callsign/ICAO, cached hits not
   re-requested, aircraft beyond the policy never looked up, and graceful handling of no
   network, DNS failure, not-found (both 200-with-string and 404, whichever the API uses), 429 and
   a mid-transfer disconnect.

5.4 **[code]** UI: airline, type/registration and `ORIG → DEST` on the card, list and detail; "—"
   while unknown or not found.
   *Verify:* sim PNG snapshots (S.2 renderer) with and without metadata. On device, routes for
   at least 3 aircraft match flightradar24.

---

## Risks

| ID | Risk | Likelihood / impact | Mitigation / what to verify |
|----|------|---------------------|-----------------------------|
| R1 | esp-hal `Dpi` looped scanout (`send(true, …)`, verified to exist in 1.2.2) doesn't run stably from a PSRAM DMA buffer, or a custom circular `DmaTxBuffer` for bounce buffers isn't possible | Low–medium (was medium: the API is now confirmed) / **critical** | Step 1.3 runs first, and 1.5 tests the custom-buffer path. If blocked: check esp-hal issues/PRs and upstream a fix. Last resort is register-level LCD_CAM/GDMA setup in the firmware crate. Don't fall back silently: report to the user. |
| R2 | PSRAM bandwidth contention → horizontal drift, worse with WiFi | **High** (was medium–high: Waveshare's own code needs a bounce buffer "to avoid screen drift") / high | Concrete plan M0–M4 under Bandwidth: a drift gate in 1.4 under synthetic load, **bounce buffers planned as step 1.5** (no longer deferred), PCLK reduction as M3, palette framebuffer in SRAM as M4. Re-verified in 3.5 and 4.6. |
| R3 | Cache coherence: CPU writes not visible to DMA | Medium / medium | Explicit writeback after rendering via `esp_hal::soc::cache_writeback_addr` (verified to exist; `#[doc(hidden)] unsafe`, so unstable, and wrapped in one function). Tested in 1.4. Not needed on the bounce-buffer path. |
| R4 | Pin map, CH422G addressing or panel timings wrong for the actual board | Low (was medium: verified against Waveshare's demo code and docs; the board is confirmed as -7 800×480, N16R8) / medium | Remaining risk is a PCB revision differing from the demo. Steps 1.2/1.3/1.6 are designed to expose errors. 1.2 also confirms EXIO5 keeps USB alive. |
| R5 | Crate churn: renames confirmed (esp-wifi → esp-radio 1.0.0-beta.1 (per esp-generate), esp-hal-embassy → esp-rtos 0.4.0); embassy version skew with Slint/reqwless/embedded-tls/mbedtls-rs | High / medium | Pin versions from esp-generate 1.4.0 output (0.2). Record them in the revision log. Upgrade deliberately, not incidentally. |
| R6 | Internal SRAM pressure: WiFi + embassy-net + Slint line buffers + DMA descriptors + **2×16 KB bounce buffers** (+ TLS in P5) | Medium / medium | Log heap stats from 1.1 on. TLS buffers and Slint heap go in PSRAM. Reduce TCP buffer sizes if needed. |
| R7 | Slint licence | **Resolved (D3):** GPLv3 accepted, never commercial. Residual: GPL obligations if binaries are ever distributed (they aren't; they contain secrets) | Licence files per D7. embedded-graphics remains the technical fallback only. |
| R8 | Software render too slow at 800×480 from PSRAM, made worse by bounce-copy cache pollution | Low–medium / medium | Partial rendering, 1 Hz model updates (2 Hz card only if measured OK), render on core 0 with bounce on core 1. Measured in 4.1. |
| R9 | fr24feed 30002 quirks. Step 0.1 found **only `@`-timestamped lines** (12-hex timestamp + 14/28 hex, LF only, no `*` lines, no Mode A/C). Remaining unknowns: the timestamp tick rate (assumed 12 MHz) and dropping of idle clients. (Enabled and reachable is confirmed, D6.) | Low / low | `AvrSource` treats `@` as the primary form, keeps `*` and CR tolerance, and skips unknown lines. Tick rate calibrated from the 10-minute capture (0.1) before S.6 relies on it. Idle timeout and reconnect. |
| R10 | CPR mis-decode producing wrong positions (zone boundaries, stale pairs, far aircraft aliased by receiver-relative local decode) | Medium / medium | Global+local strategy, provisional first fix, plausibility gate, local replay test (2.11), cross-check with FR24 (3.4). |
| R11 | Metadata API (adsbdb): rate limits, terms of use, downtime, schema change, unknown HTTP status semantics | Medium / low–medium | Caching, lookup policy (only card-relevant aircraft), rate limiting, back-off on 429, "—" on failure. Schema details [VERIFY] in 5.1. |
| R12 | Secrets in binary / accidental commit of `.env` | Low / medium | `.gitignore`, build-time vars only, `.env.example` with fictitious values. Reviewers check the diff in 3.1. |
| R13 | **Doxxing: the receiver location leaks** through committed captures, fixtures, `.env.example`, CI vars, sim PNGs, the sim bundle (`dist/`), the sim bridge, logs, screenshots of the detail view or sim, or the firmware binary | Medium (rewritten: was low/low "ask before committing capture") / **high** | Nothing local is ever committed (Privacy section). The replay test reads a gitignored capture through `ADSB_REPLAY_CAPTURE` and is `#[ignore]`d. Fixtures use book vectors, synthetic frames and fictitious coordinates. Coordinates are never logged at `info`. The detail view shows no raw lat/lon. Binary not shared. Sim: location only at runtime from the loopback bridge, never in the bundle, query string or localStorage; `dist/` bundle check in CI; bridge Origin/Host checks (R23). Checked in 0.1, 2.11, 3.1, S.3, S.4 and S.7. |
| R14 | TLS (new): embedded-tls can't negotiate with or verify api.adsbdb.com; mbedtls-rs too new or S3 support incomplete | Medium / medium | Gated spike 5.2 with a wrong-CA negative test. Fallback order embedded-tls → mbedtls-rs → stop and ask the user. |
| R15 | Ghost aircraft from AP-overlay short frames (new): a corrupted DF4/5/20/21 recovers a random address | High (without the filter) / medium | Known-ICAO filter (address seen in DF11/17/18 within 60 s). Drops counted in `Status`. Tested in 2.10. |
| R16 | Pinned root CA rotates, or there's no wall clock (new) | Low–medium / medium | Pin the root (not the leaf). Pin a second plausible root if the stack allows it. SNTP before the first lookup, with lookups paused until time is valid. Metadata failure degrades to "—", never breaks the main display. |
| R17 | Bounce-buffer handler too expensive (new): about 30 MB/s of PSRAM memcpy on core 1, plus cache thrash | Medium / medium | Measured in 1.5. M3 (lower PCLK) reduces load. M4 palette framebuffer as a last resort. |
| R18 | "Overhead" UX misleads (new): dead-reckoning error, sound lag, baro vs true altitude, hysteresis choosing the wrong aircraft | Medium / low | Ground-distance metric, a 15 s DR horizon with "~"/"?" markers, the "just passed" state, CPA "approaching". Field check in 3.4 (and earlier in S.5). Thresholds are constants tuned after real-world use. |
| R19 | Slint on wasm with a custom platform (rev. 3): rAF/timer integration, canvas blit cost, devicePixelRatio scaling, `slint-build` config shared between xtensa and wasm32, wasm size | Medium / medium | Render at a fixed 800×480 and CSS-scale with `image-rendering: pixelated`. Blit only the dirty region. One `adsb-app` build script verified for both targets in S.2. Fallback is ADR-002 option (A), the stock web backend, losing pixel parity. [VERIFY Slint 1.18 wasm specifics.] |
| R20 | Fonts (rev. 3): wasm has no system fonts; the embedded font's licence and size; glyph sets for callsigns/airport names (accents in municipality names) | Medium / low | One committed OFL font, embedded through the software renderer for both targets (no runtime font loading). Pre-render only the sizes in use. A test renders a string with diacritics. Flash cost measured in 4.1. |
| R21 | Numeric differences between targets (rev. 3): f64/`libm` on xtensa vs wasm vs host | Low / low | All maths goes through `libm` (no `std` float intrinsics in core). S.6 checks that the replay counters match the host test exactly. Distance display rounding hides last-bit differences. |
| R22 | Port forwarding with podman on macOS (rev. 3): VS Code `forwardPorts` vs devcontainer CLI `-p`, gvproxy behaviour, bind address inside the container | Medium / low | Two documented paths in 0.6, each with a mac-side `lsof` check. Only one port (8080) is needed thanks to the trunk proxy. [VERIFY on the user's podman machine.] |
| R23 | Bridge exposure (rev. 3): a localhost service reachable by any web page in the mac browser (cross-site WebSocket hijacking, DNS rebinding), or accidentally exposed on the LAN | Medium (without mitigations) / **high** (location and live feed) | Loopback-only bind enforced at startup. Publish to the mac's loopback only. Origin/Host allow-list including the WS upgrade. No CORS. Fixed upstream (not an open proxy). Tests in S.4. |
| R24 | Sim ≠ device (rev. 3): the sim doesn't validate drift/PSRAM bandwidth, Xtensa render timing, memory limits, touch calibration, WiFi or TLS | High (by design) / medium | Every [hw] step stays. 4.3–4.5 keep their device verification. Render-time budgets are measured only on the device (4.1). |
| R25 | Unknown `@` timestamp tick rate or counter wrap (rev. 3) | Medium / low | Calibrate in 0.1. Replay handles 48-bit wrap and falls back to fixed-rate pacing if timestamps go backwards. |
| R26 | Devcontainer changes clobber the user's uncommitted local `.devcontainer` modifications (rev. 3) | Medium / medium | 0.6 rules: diff first, coordinate with the user, additive edits only, no checkout/restore/stash, don't commit unrelated changes. |

Deliberately deferred: surface positions, TIS-B/ADS-R, Comm-B BDS register decoding, ACAS field
decoding, single-bit CRC error correction, the **Beast adapter and SDR demodulator** (boundary
named, not built), SBS/30003 input, the GNSS−baro altitude correction, double buffering, the
palette framebuffer (M4), on-device WiFi provisioning, radar/map view, OTA updates, backlight
dimming (hardware is on/off only), moving UI rendering to core 1, pinning an aircraft
indefinitely. Revision 3 adds:

- any native-host sim work beyond "should compile";
- hosting the sim publicly (e.g. GitHub Pages; it would be safe in scenario-only mode, but it's
  not planned);
- committed reference PNGs as a pixel-diff gate;
- driving the sim from an SDR.

Unverified (summary): every [VERIFY] item above, especially:

- stable PSRAM-sourced looped scanout and a custom circular `DmaTxBuffer` plus EOF interrupt hook
  in esp-hal 1.2.x (R1/R2);
- PSRAM init API names and burst alignment;
- the esp-hal second-core/interrupt-affinity APIs;
- Slint 1.18 feature names, `gt911` 0.3.0 async support, and the `xtensa-toolchain`,
  `rust-cache` and `rust-toolchain` action versions;
- embedded-tls certificate-verification capabilities, mbedtls-rs S3 acceleration and build
  model, and reqwless/embedded-tls version compatibility;
- the adsbdb found-aircraft schema, HTTP status codes, rate limits and combined query, and the
  issuing CA of api.adsbdb.com;
- the `esp-generate` chip flag syntax;
- the test-vector values from the book;
- revision 3:
  - the Slint 1.18 wasm specifics for a custom platform and a shared `slint-build` config;
  - trunk's version, proxy (`ws = true`) syntax and wasm-bindgen-cli handling;
  - the tokio/axum (or alternative) versions for `sim-bridge`;
  - podman-on-macOS loopback port publishing;
  - the `@` timestamp tick rate;
  - OFL font licence fit;
  - the trunk CI action.

---

## Revision log

- 2026-10-21: Initial draft.
- 2026-10-21, **revision 2**: incorporates the user decisions D1–D6 and facts verified online
  (Waveshare demo repo/docs, esp-hal 1.2.2 source, crates.io, adsbdb.com). Every change of
  substance against the draft:
  - **Header/conventions:** status changed from "draft, awaiting user review" to revision 2. The
    [VERIFY] convention now says which sources were checked.
  - **Decisions table:** the intro no longer says all decisions are open. D1 resolved = (b),
    replacing the old recommendation "Phase 1 ships with (a), then (b) or (c)". D2 resolved =
    keep 30002. D3 resolved = Slint GPLv3 (the conditional wording was removed). D4 resolved:
    board -7 800×480, module N16R8, radius 1–5 km with **default 5 km, replacing the old default
    of 150 km**, and the old "aircraft with no position listed last" recommendation moved into
    Vicinity/list. D4 "Blocks" changed from "Phase 2.8 (radius)" to "step 2.10". D5 resolved =
    GitHub Actions; its "Blocks" changed from the wrong "Step 0.4" to "Step 0.5". D6 resolved
    (reachable), with the new general-client and no-local-data requirements; "Blocks" changed
    from Phase 3 to Phase 2. **Added D7** (licence files) and **D8** (overhead UX defaults), both
    open with defaults.
  - **Context:** added the primary use case (identify the plane overhead) and the general-client
    goal.
  - **Hardware summary:** removed "[VERIFY module variant]" (N16R8 confirmed). "ST7262-class"
    became "ST7262". Touch row now has 400 kHz, INT GPIO4 active low, address 0x5D, no
    swap/mirror. The IO expander row adds the USB/CAN select. The "Other" row now lists USB
    19/20, SD SPI 11/12/13 and RS485 15/16.
  - **Pin assignments:** **removed** the "as I remember it" note and the "[VERIFY every line…]"
    instruction (verified). Added the sources, schematic link, datasheet location, the D0..D15
    order and Waveshare's panel-side naming note. The pin table itself is unchanged (it matched).
  - **CH422G:** removed the [VERIFY] tags on the function table, the backlight/PWM note and the
    register addresses. Register names (WR_SET/WR_OC/WR_IO/RD_IO) added. EXIO4 marked active
    low. **Added EXIO5** (USB/CAN select, must be low) and the init procedure.
  - **Reset sequences:** added (LCD and touch, with timings).
  - **Panel timing:** removed [VERIFY]. "PCLK ≈ 16 MHz" became exactly 16 MHz with
    `pclk_active_neg = 1`. **Removed** the "community reports suggest lowering PCLK (12–14 MHz)
    when WiFi is active" sentence, replaced by the verified evidence (Waveshare's 800×10 px
    bounce buffer). PCLK reduction survives as mitigation M3.
  - **esp-hal support check:** the "from my knowledge… confirm all" intro was replaced by the
    1.2.2 source check. `Dpi` API verified (tags removed; signature added). The `lcd_dpi`
    example [VERIFY] was replaced by a note that the module example uses `dma_loop_buffer!`, with
    [VERIFY] kept on whether any example uses PSRAM. `ExternalBurstConfig` verified (tag removed);
    the alignment rule keeps [VERIFY]. PSRAM init/macro names keep [VERIFY]. Cache writeback:
    the [VERIFY] on the API name was removed (`soc::cache_writeback_addr` exists, hidden and
    unsafe, so unstable), `ManualWritebackBuffer` noted, and the bounce path needs no writeback.
  - **Bandwidth:** the failure-mode bullet now cites Waveshare's bounce buffer. The old
    five-item "mitigations, cheapest first" list was **replaced** by the M0–M4 plan:
    - old 1 (lower PCLK) → M3;
    - old 2–4 (SRAM buffers, partial rendering, PSRAM clock) → M1;
    - old 5 ("last resort, deferred: bounce buffers") → **M2, now planned** (step 1.5);
    - new M4 (palette framebuffer in SRAM, deferred);
    - new drift gate.
    Tearing wording changed from "list UI ~1 Hz" to "UI 1–2 Hz".
  - **Data source:** the "Recommendation… only a recommendation for you to approve" paragraph
    was **replaced** by "Decision (D2, resolved)". The **`FeedParser` boundary for adding SBS
    later was removed**: the boundary is now at raw frames, and SBS would need a separate
    tracker path. The sentence "switching to 30003 would remove phases 2.2–2.6" was removed (moot
    after D2). The 30005 row notes the deferred adapter.
  - **Added "Frame-source boundary"** section: `RawFrame`, sans-IO `FrameSource`, adapters
    `AvrSource` (built) and `BeastSource`/`Demodulator` (named, deferred), stateless decoder,
    separate tracker.
  - **Decoding scope:**
    - Gillham (Q=0) changed from "`None` in v1" to in scope.
    - CPR local decode gained the provisional first fix.
    - The plausibility gate is clarified as separate from the display radius.
    - **Removed "DF4/5/20/21 are deferred…"**; replaced by short frames in scope: DF11,
      DF4/5/20/21 with AP overlay and the known-ICAO filter, DF0/16 altitude only, Comm-B BDS
      and ACAS fields deferred.
    - Mode A/C lines are skipped and counted.
  - **Route and aircraft metadata:**
    - Option (b) marked chosen.
    - Endpoint [VERIFY] removed for `/v0/callsign` (verified) and the 301→https fact added.
    - TLS options updated: **esp-mbedtls removed** as the named alternative and replaced by
      mbedtls-rs 0.3.0 (successor; S3 acceleration and build model [VERIFY]). embedded-tls is
      pinned to 0.19.0.
    - TLS buffer estimate changed from "35–40 KB" to "20–40 KB" (rough, measured in 5.2).
    - The old recommendation paragraph ("Phase 1–4 with (a)… The user decides (D1)", "fall back
      to esp-mbedtls") was **replaced** by the decision, the verified API facts (route schema,
      not-found string), [VERIFY] items, the **re-decided TLS default** (embedded-tls, gated
      fallback mbedtls-rs, stop if neither verifies), **SNTP moved from deferred into Phase 5**,
      and the new lookup policy.
    - Options (a), (c) and (d) are kept as considered alternatives.
  - **UI framework:** Slint version 1.18.1 added. The decision is no longer "subject to D3";
    embedded-graphics is only the technical fallback.
  - **ADR:**
    - Item 1 changed from "two separate Cargo projects" to separate projects with **the core
      split into `modes/` (general Mode S library) and `adsb-core/` (tracker/vicinity/meta)**;
      firmware depends on both, and `ui-sim` fixtures are fictitious.
    - Item 2 mentions `AvrSource`/`FrameSource`.
    - Item 3: the [VERIFY] on the esp-rtos/esp-radio renames was **removed** (confirmed), and
      "esp-hal 1.x" was replaced by a version table (esp-generate output still authoritative).
    - Item 4 covers bounce buffers and the overhead card.
    - Item 5 changed from "deferred behind a boundary" to adsbdb over HTTPS.
    - Added items 7 (privacy) and 8 (CI).
    - Consequences: added `modes` reuse, the location baked into the binary, the GPLv3
      consequence and CA rotation.
  - **Configuration:** `ADSB_RADIUS_KM` default **changed from 150 to 5** with a range check.
    Added `ADSB_RX_ELEV_M` and the test-only `ADSB_REPLAY_CAPTURE`. `.env.example` must use
    fictitious values.
  - **Added "Privacy: no local data in the repo"** section.
  - **Vicinity: rewritten.**
    - **Removed:** "the list shows aircraft within `ADSB_RADIUS_KM`" (the radius now defines the
      overhead set, and the list shows all positioned aircraft); "unpositioned aircraft shown
      because plausibly nearby" (kept in substance in `list`); the example "NE 23 km".
    - **Added:** ground distance over slant range (with reasoning), elevation angle and height
      above the receiver, dead reckoning (15 s horizon), CPA/approaching, the sound-lag analysis,
      and overhead selection with hysteresis plus the snapshot fields.
  - **Task structure:**
    - "Single core, single executor in v1… move UI to core 1 later" changed: core 1 is reserved
      for the bounce handler.
    - Init now spells out IO_OE, EXIO5, the reset sequences and 0x5D.
    - feed_task uses `AvrSource` (was `LineParser`).
    - ui_task may run the card at 2 Hz and queues lookups.
    - Added `sntp_task` and the bounce ISR. meta_task is now HTTPS-only (the "HTTP(S)" option is
      gone).
    - Added the DR horizon note and the `Status` filter counters.
  - **UI layout: rewritten.**
    - **Removed:** the list view as the main screen, the "SNTP deferred" note in the status bar,
      and **raw lat/lon ("position") from the detail view** (privacy).
    - **Added:** the overhead card with four states (Overhead, Just passed, Quiet = nearest
      beyond greyed + approaching, which is the D8 default with reasoning, and No data), "+N more"
      chips, list as secondary with in-radius highlighting, and squawk/airline/height/CPA in the
      detail view.
  - **Step 0.1:** **removed** "save to `adsb-core/tests/data/capture-30002.txt`… confirm
    committing is OK" and "Resolves D6". It's now a format check only, with the capture saved to
    a gitignored location and never committed.
  - **Step 0.2:** esp-generate 1.4.0, `.gitignore` adds `captures/`/`*.capture`, LICENSE per D7,
    record deviations.
  - **Step 0.4:** creates both `modes/` and `adsb-core/`.
  - **Step 0.5:** made concrete for GitHub Actions (jobs, action refs [VERIFY], dummy vars,
    caching, actionlint, a push only with the user's OK). The old "depends on D5" was removed.
  - **Step 1.2:** 400 kHz, IO_OE/WR_IO writes, **EXIO5 low**, expected scan addresses, and a
    USB-stays-alive check.
  - **Step 1.3:** explicit LCD reset sequence and verified timings.
  - **Step 1.4:** added the synthetic core-1 PSRAM load and the drift gate.
  - **New step 1.5** (bounce buffers). **The old 1.5 (GT911) is renumbered 1.6** and now has the
    exact reset sequence and a 0x5D check.
  - **Phase 2 renumbered and extended:**
    - 2.1 `LineParser` → `RawFrame`/`FrameSource`/`AvrSource`, accepting Mode A/C, with the
      random-bytes test moved here from old 2.9 (and no longer optional).
    - 2.2 adds AP address recovery, DF11 IID and the synthetic frame helper.
    - 2.4 changed from "Q=0 returns `None`" to Gillham decoding plus the 13-bit AC code.
    - **New 2.7** short frames.
    - Old 2.7 (geo) → **2.8**, adding elevation angle.
    - **New 2.9** kinematics (DR, CPA).
    - Old 2.8 (table) → **2.10**: `snapshot(radius_km) -> Vec<Row>` was **replaced** by
      `snapshot(now_ms, radius_km) -> Snapshot`, plus the known-ICAO filter, provisional fix,
      hysteresis/just-passed/approaching tests and the ground-distance test. The old "radius
      filter" test became the overhead membership test.
    - Old 2.9 (replay) → **2.11**: **removed the committed `tests/data/capture-30002.txt`**. It
      now reads a gitignored capture via `ADSB_REPLAY_CAPTURE`, is `#[ignore]`d, and is
      excluded from CI. "At least N aircraft" became "at least 5".
    - The Phase 2 intro adds synthetic vectors and no local data.
  - **Step 3.1:** fictitious `.env.example`, range checks for elevation/radius, and a local grep
    check.
  - **Step 3.3:** `AvrSource`, Mode A/C counter.
  - **Step 3.4:** changed from "log the 5 nearest aircraft" to an info-level summary without
    coordinates plus debug-level positions, and an overhead field check.
  - **Step 3.5:** refers to M2/M3 and records the scanout mode.
  - **Phase 4:**
    - The heading is no longer "after D3".
    - 4.1: writeback skipped with bounce; 2 Hz decision.
    - 4.2: fictitious fixtures, ui-sim added to CI.
    - **4.3 changed from "list view + status bar" to the overhead card main screen.**
    - 4.4 now also builds the list view and card navigation and chips.
    - 4.5 adds elevation/squawk/CPA.
    - 4.6 logs bounce load.
  - **Phase 5:**
    - The heading is no longer "after D1; skipped if (a)".
    - 5.1 changed from "recorded response fixtures" to hand-written fixtures from the verified
      schema, plus the lookup-policy function.
    - **Old 5.2 split** into 5.2 (TLS spike with gate, SNTP, wrong-CA negative test) and 5.3
      (meta_task). **The option (c) plain-HTTP variant was removed** (D1 = b).
    - Old 5.3 → **5.4**, adding airline and card.
    - **Removed 5.x** (LAN helper design doc), moot after D1 = (b).
  - **Risks:**
    - R1 likelihood lowered from medium to low–medium (API verified) and now covers the custom
      buffer.
    - R2 likelihood **raised** from medium–high to high, with "bounce buffers deferred"
      **removed** (now planned).
    - R3: API [VERIFY] removed.
    - R4 likelihood lowered from medium to low, and "All marked [VERIFY]" removed.
    - R5 renames confirmed.
    - R6 adds bounce buffers.
    - R7 marked resolved.
    - R8 adds cache pollution.
    - R9 "not enabled, binds to localhost only" removed (confirmed reachable), likelihood
      lowered to low.
    - R10 references 2.11 and the provisional fix.
    - R11 "Option (c) as an alternative" and "the decision is the user's" removed; it now covers
      lookup policy and schema.
    - R12 fictitious example values.
    - **R13 rewritten** (doxxing, raised to medium/high; "ask before committing capture data"
      removed).
    - **Added R14–R18.**
  - **Deferred list:**
    - **Removed** DF4/5/20/21 (now in scope, except BDS/ACAS fields), Gillham (in scope), bounce
      buffers (step 1.5), SNTP (Phase 5) and "dual-core split" (replaced by "moving UI rendering
      to core 1").
    - "Backlight dimming (hardware appears on/off only)" became "is on/off only" (verified).
    - **Added** Comm-B BDS, ACAS fields, single-bit error correction, the Beast adapter, the SDR
      demodulator, SBS input, the GNSS−baro correction, the M4 palette framebuffer, and
      indefinite pinning.
  - **Unverified summary:** **removed** the pin map, CH422G addresses and panel timings, current
    crate names/versions, and the adsbdb callsign endpoint (all verified). Added the remaining
    open items (custom DmaTxBuffer/EOF hook, second-core API, action versions, TLS capabilities,
    adsbdb aircraft schema/status/CA, gt911 async).
- 2026-10-21, **approval**: user approved the revision-2 plan. D7 resolved (user indifferent →
  default GPL-3.0-or-later, whole repo). D8 resolved (defaults accepted). Status line updated.
  No other changes.
- 2026-10-21, **revision 3** (approved-scope addition: browser simulator; step 0.1 results). Base:
  the approved doc at 0515239. Every change:
  - **Status line:** replaced "revision 2, approved by the user 2026-10-21 (D1–D8 resolved).
    Implementation in progress on this branch." with the revision 3 status, which keeps the
    approval and in-progress facts and adds the stable-ID note.
  - **Decisions intro:** **removed** "D1–D6 are resolved (answers below). D7 and D8 are new in
    revision 2. Each has a recommendation that the plan follows unless the user overrides it."
    It was stale, since all decisions are resolved. Replaced with "D1–D9 are resolved…". **Added
    D9** (simulator = WebAssembly in the browser, resolved).
  - **Context:** added the third goal (UI iteration without the board, D9).
  - **Data source table, 30002 row:** **replaced** "possibly also `@<12 hex timestamp><frame>;`"
    with the observed fact that this feed sends only the `@` form; the `*` form stays supported.
  - **Frame-source boundary:** the `AvrSource` bullet now names `@` as the primary path.
  - **Decoding scope:** added the 91 % short-frame observation; Mode A/C bullet notes none were
    observed; **added** an `@` timestamp bullet (48-bit, assumed 12 MHz [VERIFY], used for replay
    pacing).
  - **UI framework decision:** added one sentence (the same `.slint` and renderer run in the sim).
  - **ADR-001:**
    - Item 1: **removed** the `ui-sim/` bullet ("host binary/test that renders the firmware's
      `.slint` files with fixture data … into PNGs"), replaced by the `adsb-app/`, `sim/` and
      `sim-bridge/` bullet. **The `.slint` files move from `firmware/` to `adsb-app/ui/`.** The
      separate-projects rationale changed from "host tests" to "host and wasm builds".
    - Item 4 starts with "shared in `adsb-app`".
    - Item 8 also references S.7.
    - Consequences: "Slint's GPLv3 applies to the firmware (and `ui-sim`). For the licence files,
      see D7." **replaced** by "…firmware, `adsb-app` and `sim`. The whole repo is
      GPL-3.0-or-later (D7)."
  - **Added ADR-002** (browser simulator and shared app layer), covering:
    - the rendering options A–D: chosen (B), a custom software-renderer platform blitting to a
      canvas, with (A), the stock web backend, as fallback;
    - websockify vs a Rust bridge: the Rust bridge is chosen;
    - trunk vs wasm-pack: trunk is chosen;
    - the code-split table (`modes` `encode` feature; `adsb-core` `pipeline` + `scenario`; new
      `adsb-app`; firmware and `sim` as platform layers only; new `sim-bridge`);
    - the consequences.
  - **Configuration:** added the simulator/bridge variables table and the single-origin note.
  - **Privacy:** added the simulator bullet:
    - location only at runtime from the loopback bridge, fictitious fallback with a banner, and
      query params/localStorage deliberately not used;
    - capture via the bridge or the file picker, never bundled;
    - the `dist/` bundle check;
    - bridge exposure rules including Origin/Host checks;
    - loopback-only publishing;
    - screenshots.
  - **Embassy task structure:**
    - The `feed_task` line changed from `read → AvrSource.feed → modes::decode → TABLE.lock(update)`
      to the shared `FeedPipeline`.
    - The `ui_task` line now names `adsb-app::App`, and its wording was shortened from "if
      render cost allows" to "if allowed". Same meaning.
  - **Added "Simulator structure"** section (browser loop, inputs, controls, port layout).
  - **Plan intro:** **removed** "Phase 2 (pure host work) can run in parallel with Phase 1
    (hardware)." Replaced by the explicit two-track execution order (Track A board, Track B
    host/sim, join for device metadata), with the stable-ID note.
  - **Step 0.1:** added the "Status: format check DONE" block (12 s, 372 lines, all `@`, 339
    short / 33 long, no `*`, no Mode A/C, LF only, sample outside the repo). The remaining
    10-minute capture comes after 0.2, with a gitignored wall-clock sidecar for tick-rate
    calibration. The original step text is unchanged.
  - **Step 0.5:** "`ui-sim/` joins later (4.2)" **replaced** by "`adsb-app/`, `sim/` and
    `sim-bridge/` join in S.7".
  - **Added step 0.6** [devops]:
    - new local feature `features/wasm-sim` (wasm32 target, trunk, wasm-bindgen-cli, optional
      wasm-opt; no websockify);
    - `forwardPorts: [8080]`, or a `-p 127.0.0.1:8080:8080` CLI path;
    - explicit rules not to clobber the user's uncommitted `.devcontainer` changes;
    - mac-side checks.
  - **Step 2.1:** `@` marked primary and CR changed to tolerated (was "with CR/LF"). Verify adds
    synthetic `@` lines that mirror the observed shape.
  - **Step 3.4:** now wires through `FeedPipeline` (S.1), which adds a dependency of 3.4 on S.1.
  - **Added Phase S** (S.1–S.8): encoder/scenarios/pipeline; `adsb-app` + PNG renderer; browser
    sim; `sim-bridge`; live mode; replay mode; CI; sim metadata.
  - **Phase 4:**
    - Added an intro: 4.3–4.5 are first done in the sim, with device verification after 4.1.
    - 4.1: **removed** "Show a static 'hello' `.slint`", replaced by "Show the `adsb-app` UI"
      plus the `ADSB_DEMO=overflight` flag; it uses the shared line-render helper; verify adds a
      photo-vs-PNG check.
    - **4.2's content removed and moved to S.2/S.7.** The old text was: "`ui-sim/`: host test
      that loads the same `.slint` files, fills a fixture model…, renders… into a PNG under
      `target/ui-snapshots/`, and (optionally) compares against committed reference PNGs. Add
      `ui-sim` to the CI `libs` job…; Verify: `cargo test` in `ui-sim/` produces PNGs in CI…
      Manual review of the first render." The ID 4.2 remains as a pointer.
    - 4.3: verify split into sim and device. "ui-sim snapshots" became "PNG snapshots from the
      `overflight`/`edge` scenarios", and a browser check was added.
    - 4.4: "touch →" became "`PointerEvent` →" (mapping shared in `adsb-app`). Verify split into
      sim (mouse, `busy` scenario, virtual clock) and device (GT911 plus edge tap accuracy, new).
    - 4.5: verify split, with "ui-sim snapshots" becoming "PNG snapshots".
  - **Step 5.4:** "ui-sim snapshots" became "sim PNG snapshots (S.2 renderer)".
  - **Risks:**
    - **R9 rewritten** with the observed formats. The old text was "fr24feed 30002 quirks:
      Mode-AC lines, timestamp format, drops idle clients… Step 0.1 checks the formats. Parser
      tolerant to unknown lines." Mitigation is now `@` primary plus tick-rate calibration.
    - R13: the leak vectors **replace "ui-sim PNGs" with "sim PNGs"** and add the sim bundle,
      the bridge and sim screenshots. Mitigation adds the sim rules, and the checked-in steps add
      S.3/S.4/S.7.
    - R18 mitigation mentions S.5.
    - **Added R19–R26** (Slint wasm/custom platform, fonts, numeric parity, podman port
      forwarding, bridge exposure, sim ≠ device, timestamp tick rate/wrap, devcontainer clobber).
  - **Deferred list:** added native-host sim work beyond compiling, public sim hosting,
    committed reference PNGs as a gate, and SDR into the sim.
  - **Unverified summary:** added the revision 3 items.
  - **Not changed:** hardware, bandwidth, metadata/TLS, vicinity, UI layout, steps 1.x, 2.2–2.11,
    3.1–3.3, 3.5, 4.6 and 5.1–5.3. Step IDs are preserved.
- 2026-10-21, **approval of revision 3**: user approved. Status line updated; no other changes.
- 2026-10-21, **step 0.2 done** (firmware generated). Command (run in the repo root):
  `esp-generate --headless -s -o esp32s3 -o esp32s3-wroom-1-octal-psram -o embassy -o unstable-hal -o alloc -o wifi -o log -o esp-backtrace firmware`.
  - **Options:** esp-generate 1.4.0 has no standalone "PSRAM" option. PSRAM is expressed through the
    module option `esp32s3-wroom-1-octal-psram` (ESP32-S3-WROOM-1, 8 MB octal PSRAM, matches the
    N16R8), which only reserves GPIO27–37 in the template. **The generated project does not enable
    PSRAM**: no `psram` feature on `esp-hal` and no PSRAM heap region. Step 1.1/1.3 must add it.
    `wifi` here means `esp-radio` (it requires `alloc`, `unstable-hal`, `embassy`). `log` +
    `esp-backtrace` are required together (`log` gives esp-println logging). The README's `-o esp32`
    was wrong; the chip is selected with `-o esp32s3` (no `--chip` flag exists).
  - **Resolved versions (Cargo.lock):** `esp-hal` 1.2.2, `esp-rtos` 0.4.0, `esp-radio`
    **1.0.0-beta.1**, `esp-alloc` 0.11.0, `esp-bootloader-esp-idf` 0.6.0, `esp-println` 0.18.0,
    `esp-backtrace` 0.20.0, `embassy-executor` 0.10.0, `embassy-net` 0.9.1, `embassy-time` 0.5.1,
    `smoltcp` 0.14.0, `static_cell` 2.1.1, `log` 0.4.34, `embedded-io` 0.7.1,
    `embedded-io-async` 0.7.0, `critical-section` 1.2.0. Toolchain: `esp` channel, rustc 1.97.0,
    `rust-version = "1.95"`.
  - **Deviation from the ADR table:** only `esp-radio`: **1.0.0-beta.1 instead of 0.18.0**. The
    generator's set is authoritative (ADR-001 item 3), so the table is superseded for that row.
    All other rows match. The table gained `esp-bootloader-esp-idf`, which the generator requires.
    `embassy-sync` is not a direct dependency yet (add it with Phase 1+ code).
  - **Other:** generated code contains no WiFi credentials or network config (only controller
    init). `license = "GPL-3.0-or-later"` added to `firmware/Cargo.toml`; root `LICENSE` is the
    GPLv3 text from gnu.org. `.gitignore` adds `target/`, `.env`, `captures/`, `*.capture`.
    `cargo build --release` and `cargo clippy --release` pass (one linker warning about an RWX
    LOAD segment, generated code, ignored).
- 2026-10-21, **step 0.2 QA follow-up**: ADR version table row and R5 updated to esp-radio 1.0.0-beta.1 to match the recorded deviation. No other changes.

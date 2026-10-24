# Radar UI on the device: live feed, radar view and closest-aircraft panel

Status: **revision 1, pre-approved by the user for unattended execution** (overnight), on the
condition that the work stays in the scope of this doc. Date: 2026-10-23. Branch:
`feature/radar-ui` (base: `feature/adsb-display`). **Revision 3 (2026-10-24)** adds RD.14 (radar
pixel aspect) and RD.15 (touch selection), ADR-004 and risks K15–K24. Revision 3 is **not**
pre-approved: the user reviews it before RD.14/RD.15 start.

Parent design: [adsb-display.md](adsb-display.md), revision 3 ("the parent"). Bench:
[board-bench.md](board-bench.md) and `.claude/skills/board-verify/SKILL.md`.

Conventions (same as the parent):

- **[VERIFY]** marks a fact that I could not confirm from the container during design (crate API,
  API shape, licence term). Confirm it at implementation time. Do not treat it as fact.
- Step tags: **[code]** = host-testable code, verified by host tests and sim PNGs;
  **[code+hw]** = code plus an on-board verify through the bench (flash, serial log, webcam
  photo); **[hw]** = bench run only, no new code.
- Parent step IDs (for example 3.2, 4.1, 5.2) are used as references. The steps of this doc have
  the IDs **RD.0–RD.13**, plus **RD.14.1–RD.14.4** and **RD.15.1–RD.15.7** (revision 3). Risks of
  this doc have the IDs **K1–K24** (the parent uses R1–R26).
- **[user]** (revision 3) marks a check that needs the user's hands or eyes at the board (the
  bench cannot touch the screen or hold a ruler).

---

## Decisions (resolved by the user)

All items are resolved. They are recorded here so that the plan can run without questions.

| # | Decision | Resolution |
|---|----------|------------|
| RD-D1 | **Configuration source** | `firmware/.env` (gitignored, a copy of `sim-bridge/.env`, already present) holds `ADSB_FEED_HOST`, `ADSB_RX_LAT`, `ADSB_RX_LON` (and optionally the other `ADSB_*` vars of the parent) plus WiFi as **`WIFI_SSID`** and **`WIFI_KEY`** (no `ADSB_` prefix). Builds load it with `set -a; . firmware/.env; set +a`. The orchestrator and agents **never print or commit its values**. The parent privacy rules apply. The parent names `ADSB_WIFI_SSID`/`ADSB_WIFI_PASSWORD` are **dropped** (decision of this doc, see Configuration). |
| RD-D2 | **Plan approval** | Pre-approved while the user is away, if the work stays in scope. The plan must run unattended with the bench (flash + serial + webcam photo). Visible progress must land early. |
| RD-D3 | **Metadata and photo source** | The device fetches directly over HTTPS: **adsbdb.com** (route, airline, type, manufacturer, registration) and the **planespotters.net public photo API** (`https://api.planespotters.net/pub/photos/hex/<icao24>`, thumbnail JPEG, photographer credit shown per its terms). JPEG decode on the device to the panel format. TLS per parent 5.2 (embedded-tls, gate to mbedtls-rs). **Fallback** if device TLS fails: a small host-side HTTP proxy on the LAN (plain HTTP, serves JSON and a pre-scaled RGB565 image). It is a gated step, not the main path, and its use must be reported. |
| RD-D4 | **Radar (left side)** | North-up. Receiver at the centre. Auto range in steps of **10 / 25 / 50 / 100 km**, so that about the nearest 10 aircraft fit. Range rings with labels. Each aircraft is a symbol turned to its track, with a label (callsign, or ICAO24 if there is no callsign). The closest aircraft is highlighted. |
| RD-D5 | **Info panel (right side)** | Shows the **closest aircraft by ground distance**: photo, callsign, airline, make/model/type, registration, origin → destination (IATA + city), altitude, ground speed, track, distance/bearing, photo credit. "—" when a value is unknown. |
| RD-D6 | **Status bar** | Keep it: WiFi state, feed state, aircraft count, msgs/s (and the clock). |
| RD-D7 | **Scope** | Reuse parent steps 3.1–3.4, 4.1, 5.1–5.3. **Replace** the parent overhead-card main screen (4.3) with this layout. Touch (1.6, 4.4), list view and detail view (4.5) are **out of scope**. *(Revision 3: changed by RD-D8. Touch selection on the radar is in scope; list view, detail view and the rest of parent 4.4 stay out.)* |
| RD-D8 | **Touch selection** (user request, 2026-10-24) | Default selected aircraft = the closest (focus rule, ADR-003 decision 7). A tap on another aircraft on the radar overrides the selection; the info panel shows it, and the text/photo lookups follow it. The override ends when the selected aircraft leaves the radar scope (then the closest is selected again) or after a timeout set at build time (`ADSB_SELECT_TIMEOUT_S`). Default value and the details (tap on empty radar, tap on the panel, feedback) are architect choices in ADR-004; the user can change them in review. |
| RD-D9 | **Radar aspect** (user feedback, 2026-10-24) | The radar looks elliptic on the panel. Correct it in the radar geometry (rings, blips, labels, hit-testing), not in the scanout. |

---

## Context

The parent project has a working display scanout on the board (steps 1.1, 1.2, 1.3/1.5 PASS),
a tested decoder and tracker (2.x), and a browser simulator that renders the shared Slint UI
(S.1–S.5). The device does not yet run Slint, WiFi, the feed or any metadata lookup. The main
screen of the parent is an "overhead card" for one aircraft.

The user now wants two things:

1. The ADS-B Slint app runs **on the ESP32-S3**, with the live feed and the receiver location
   from the local `.env`.
2. A new main screen: a **radar view on the left** with nearby aircraft (direction and
   callsign), and an **info panel on the right** about the closest aircraft, with a photo, make,
   model, route and more from public APIs.

Why now: the bench (board-bench.md) can flash the board, read the log and take a photo without
a human. Thus the hardware steps can run unattended. The user is away overnight and pre-approved
the plan.

### Facts from the code (2026-10-23)

- `firmware/src/bin/main.rs`: PSRAM init (8 MB, heap region added **after** the internal heap of
  73,744 B), framebuffer 768,000 B in PSRAM, CH422G, `Dpi` with per-frame restart from core 1
  (`firmware/src/restart.rs`), WiFi controller created but not connected. Logged heap: internal
  free about **27 KB after WiFi init** (WiFi takes about 47 KB).
- `firmware/build.rs`: generator code only. No config handling yet (parent 3.1 not done). There
  is no `firmware/.env.example` yet.
- `firmware/Cargo.toml`: `[patch.crates-io] esp-hal = { path = "vendor/esp-hal" }` (reorder
  patch). The board result "fw-D vendored vs stock" is still open.
- `adsb-app`: `App` view-model with field-by-field diffing (`Ui` struct), `Status`,
  `render_by_line` line helper, `MinimalSoftwareWindow` with `RepaintBufferType::ReusedBuffer`,
  Slint 1.18.1 with `renderer-software`, fonts Liberation Sans embedded via
  `EmbedForSoftwareRenderer`, a `glyph-seed` property for non-ASCII glyphs. One `App` per
  process.
- `adsb-core::tracker::Snapshot`: `list` (positioned aircraft by ground distance, then the rest),
  `in_radius`, `primary` (with hysteresis, in radius only), `just_passed`, `approaching`,
  `counters`. `PositionView` has `ground_m`, `bearing_deg`, `elevation_deg`, `marker`,
  `provisional`. `AircraftView` has `callsign`, `altitude_ft`, `vertical_rate_fpm`,
  `ground_speed_kt`, `track_deg`.
- `sim`: `Sim` (scenario → `FeedPipeline` → `App` → RGB565 framebuffer), PNG test
  `sim/tests/snapshots.rs` writes `sim/target/ui-snapshots/*.png`, scenarios `Overflight`,
  `Busy`, `Edge` around a fictitious receiver.
- `sim-bridge`: loopback-only axum server (`/ws/live`, `/local/config.json`, `/local/capture`)
  with Host/Origin guard.

### Context for revision 3 (RD.14, RD.15; 2026-10-24)

RD.2–RD.10 are done on the board, and K4 is fixed at PCLK 12 MHz (9af2c78; QA PASS, user saw no
flicker). The user now sees two things on the real panel:

1. **The radar is elliptic** (wider than tall). The sim PNG is round. Hypothesis: the panel pixels
   are not square.
2. **The user wants to select an aircraft with a finger** (RD-D8). The board has a GT911 touch
   controller; parent step 1.6 (GT911 driver) was never done.

Facts from the code for this revision (head c804300):

- `adsb-core::radar::project` maps ground distance and bearing to radar-box pixels with one scale
  `k = RING_PX / range` for x and y (square-pixel assumption). `RANGE_STEPS_KM`, `MAX_BLIPS = 48`,
  `LABELLED = 12`, `SPRITE_PX = 20`. `Blip` has `x, y, sprite, label, focus, dim, flag` (no ICAO).
- `adsb-app/ui/app.slint` draws the rings as `Rectangle`s with `border-radius` (component `Ring`,
  centre (224, 224) in the 448×448 box), the focus mark as a 30×30 round `Rectangle`, labels as
  `Text` 88 px wide at `x + 13` (or `x − 100` right-aligned when `x > 340`), `y − 8`.
- **Slint software renderer limits (verified in the vendored 1.18.1 source):** `Path` drawing is
  a no-op in line-by-line mode ("Path rendering is not supported in line-by-line mode",
  `vendor/i-slint-renderer-software/lib.rs` around line 2173), and `supports_transformations()`
  returns `false` (line 1266). We render with `render_by_line`. So an ellipse cannot come from a
  Slint `Path` or a scale transform. A `Rectangle` with `width ≠ height` and `border-radius` gives a
  stadium shape, not an ellipse.
- `App` owns the radar `RangeState` and calls `radar::select_range` in `update`. `App::pointer`
  already maps a `PointerEvent` (logical px, top-left origin) to Slint window events; nothing in
  the UI handles them today. The sim maps mouse events to `PointerEvent` (`sim/src/web.rs`, canvas
  CSS-scaled, mapping by the bounding rect).
- `firmware/src/ui.rs` `tick`: `pipeline.snapshot()` → `lookup::publish(policy, snap)` (lookup
  plan from `snap.focus`) → `App::update(snap, meta)`. So the lookups and the panel both follow
  `snap.focus`. `ui_task` ticks at 1 Hz.
- `adsb-core::tracker` keeps the focus hysteresis (`AircraftTable.focus`, switch only if > 20 %
  and > 300 m closer). `FeedPipeline::snapshot` = `age` + `table.snapshot`.
- `adsb-core::meta::Policy`: focus + 2 prefetch, photo only after the focus is stable for
  `PHOTO_DWELL_MS` = 3 s.
- `firmware/src/ch422g.rs`: `Ch422g` **owns** the async `I2c` (400 kHz, 50 ms software timeout).
  `TP_RST` = EXIO1. After boot only `main` uses it (backlight on). `main.rs` does not reset the
  GT911 and does not touch GPIO4; the comment says "touch_task will take over the bus (and the
  expander) in step 1.6". Parent step 1.2 found the GT911 at **0x5D** on the I2C scan.
- `firmware/build_config.rs` has the `number(name, default, min, max)` helper and value-free error
  messages (tests in the same file).
- The bench (board-bench.md) can flash, read the serial log and take photos. It has **no serial
  input** and nothing can touch the screen.

---

## Options

### O1. How to draw the radar (Slint software renderer, 800×480 RGB565)

| # | Option | Trade-off |
|---|--------|-----------|
| O1a | **Pure declarative Slint**: rings as `Rectangle` with `border-radius`, aircraft as `Path` with a rotation transform, labels as `Text`, all from a repeater model. | Least Rust code. But it depends on `Path` and rotation support in the **software** renderer of Slint 1.18 [VERIFY: both were limited or feature-gated in earlier releases]. If one is missing, the step stalls overnight. |
| O1b | **Rust-rasterised radar layer**: `adsb-app` draws rings and symbols into one RGBA pixel buffer, shown as one Slint `Image`; labels are Slint `Text`. | No dependence on Slint vector features. But each update makes the full 448×448 image dirty (about 200 k pixels per second to repaint), and we own a small rasteriser. |
| O1c | **Slint layout + pre-rotated sprites**: rings as `Rectangle` with `border-radius` (supported by the software renderer), aircraft as small `Image`s chosen from a set of **24 sprites (15° steps)** generated once in Rust at start-up, labels as `Text`. | Depends only on features already in use (rectangles, images, text). Partial repaint stays small (only moved blips are dirty). Costs about 40 KB of PSRAM for sprites and a small pure sprite generator (host-testable). 15° steps are coarse but fine at this size. |

### O2. JPEG decoder on the device

| # | Option | Trade-off |
|---|--------|-----------|
| O2a | **`zune-jpeg`** (pure Rust, `no_std` + `alloc`, baseline and progressive) [VERIFY current version and `no_std` feature set] | Same code on host, wasm and xtensa, so it is testable on the host. Decodes the whole image into memory (about 350 KB RGB for 420×280; fine in PSRAM). Decode time on Xtensa is unknown (K9). |
| O2b | A TJpgDec-style decoder (tiny, streaming, built-in 1/2–1/8 scaling, baseline only) | Very small memory use. But the original is C (a C build in the Xtensa build, against the parent constraint), and Rust ports are small, little-used crates [VERIFY]. No progressive JPEG. |
| O2c | `jpeg-decoder` / `image` | Need `std`. Not usable on the device. |
| O2d | No decode on the device: the proxy pre-scales to RGB565 | Only possible on the fallback path (RD-D3). |

### O3. How build-time config reaches the code

| # | Option | Trade-off |
|---|--------|-----------|
| O3a | `cargo:rustc-env` + `env!` (parent 3.1) | Simple. But `cargo build -v`/`-vv` can print the rustc environment, which then contains the WiFi key [VERIFY exact cargo behaviour]. One careless verbose build in an agent log leaks it. |
| O3b | **`build.rs` writes `$OUT_DIR/config.rs`** with typed constants; the firmware uses `include!`. | Values never pass through the rustc command line or environment. Lat/lon are parsed and range-checked in `build.rs`, so the constants are typed. The file sits under `firmware/target/` (gitignored). |

### O4. JSON parsing of API responses

| # | Option | Trade-off |
|---|--------|-----------|
| O4a | `serde-json-core` (parent 5.1) | No alloc. But no `deserialize_any`, so the adsbdb "string or object" `response` needs a pre-check, and arrays (planespotters `photos`) need fixed sizes. |
| O4b | **`serde_json` with `default-features = false, features = ["alloc"]`** | Handles the string-or-object form with an untagged enum, unknown fields and arrays. Needs alloc (PSRAM). Bodies are small (1–3 KB). |

### O5. Step order

| # | Option | Trade-off |
|---|--------|-----------|
| O5a | All UI in the sim first, then all device work | Few board runs, but nothing is visible on the board until late. A device-only problem (heap, render speed) shows up last. |
| O5b | **Interleaved, device-visible early**: Slint on the device with the demo feed → live feed → new layout (sim then device) → metadata text → photo | Each night stage leaves a working, visible result on the board. Device risks (heap, render time, drift) show up in the first hours. |

### O6. Where to correct the pixel aspect (revision 3, RD.14)

| # | Option | Trade-off |
|---|--------|-----------|
| O6a | Resample the whole frame (render 800/a × 480, stretch to 800 at copy time) | Corrects everything, also text and photo. But every rendered row needs a resample pass on core 0, text gets blurred, and partial repaint gets complex. |
| O6b | Non-uniform Slint scale factor | Not possible: Slint has one uniform scale factor. |
| O6c | **Radar geometry only**: one constant `PIXEL_ASPECT` in `adsb-core::radar`; projection, rings, sprites, focus mark and hit-test use it | Small, pure, host-testable. Text and photo stay 7–8 % wide (hardly visible for text; the photo is a separate, optional fix). |
| O6d | Shrink the whole layout horizontally | Glyphs stay stretched; large layout change for little gain. |

### O7. How to draw elliptic rings (software renderer, `render_by_line`)

| # | Option | Trade-off |
|---|--------|-----------|
| O7a | Slint `Path` or a scale transform | **Not possible** (verified: no paths in line-by-line mode, no transformations; see Context for revision 3). |
| O7b | `Rectangle` with `width ≠ height` and `border-radius` | Gives a stadium shape, not an ellipse. Wrong shape. |
| O7c | **One opaque RGB8 background image** (448×448: background colour, cross-hair, rings, radius ring), rasterised in `adsb-core::radar`, rebuilt only when the ring set changes | Simplest Slint tree (one `Image` replaces 2 lines + 3 rings). 602 KB PSRAM. Every repainted radar pixel becomes an image read instead of a solid fill: extra render time, size unknown (K16). |
| O7d | Ring tiles: rasterise each ring once, cut it into small RGBA tiles (row bands, only the non-transparent column runs), show them in a repeater | Repaint cost only near the rings. About 100 more items and more code (cutter, tile model). |

### O8. Where the selection state lives (RD.15)

| # | Option | Trade-off |
|---|--------|-----------|
| O8a | In `App` (`adsb-app`) | `App` knows the shown blips and the range. But `lookup::publish` runs **before** `App::update`, so the lookups would follow a tap one tick late, or `App` needs a two-phase API. |
| O8b | In each platform (firmware `ui.rs`, sim) as glue | Same logic twice; it needs the range that `App` computes privately. |
| O8c | **In `FeedPipeline` (`adsb-core`)**, next to the tracker focus hysteresis: `snapshot()` also computes the range (the `RangeState` moves from `App`) and applies the selection to `snap.focus` | One place for both platforms, pure, virtual-clock tests. Panel, radar focus mark, lookup policy and logs follow `snap.focus` with no change. Cost: two new `Snapshot` fields; test fixtures that build a `Snapshot` by hand need them. |

### O9. GT911 input (RD.15)

| # | Option | Trade-off |
|---|--------|-----------|
| O9a | INT-triggered (GPIO4 falling edge), then read | No idle cost. Depends on the GT911 INT trigger configuration [VERIFY register 0x804D on this board]; GPIO4 also selects the I2C address during reset. |
| O9b | **Polling at 25 Hz** over I2C | Independent of the INT configuration. A tap lasts ≥ 80 ms, so it gets ≥ 2 samples. Cost: about 25 short I2C reads per second (about 150 µs bus time each, async), negligible CPU. |
| — | Driver: the `gt911` crate vs. **our own** (about 100 lines) | Parent 1.6 allowed the crate only with embedded-hal-async 1.0 [VERIFY]. Our own driver keeps the register parse in `adsb-core` (host-testable) and has no trait-version risk. |

### O10. How to test touch without a finger (RD.15)

| # | Option | Trade-off |
|---|--------|-----------|
| O10a | Inject taps through the serial line | The bench has no serial input. Adding one changes the bench host, which this doc forbids. |
| O10b | **Timed touch script in a debug build** (feature `touch-script`): synthetic raw touch samples go into the same tap detector → hit-test → selection path; targets are resolved against the blips on screen. The same script runs against the sim on the host. | Covers everything except the GT911 coordinate read and the finger. The board log can be compared with the sim log line by line (busy demo, deterministic). |
| O10c | Fake the I2C device | Much work, and it tests only our own fake. |

---

## Decision (ADR-003)

**ADR-003: Radar main screen, on-device metadata and photo, unattended execution**

- **Context:** as above. The device must run the shared Slint app with the live feed. The new
  main screen is radar + info panel. The data for the panel comes from adsbdb and planespotters
  over HTTPS. Internal SRAM is tight (about 27 KB free after WiFi). The work runs overnight
  without the user, so every step needs an automatic verify and clear stop rules.
- **Decision:**
  1. **Radar: O1c** (Slint layout + 24 pre-rotated sprites + `Text` labels). O1a stays possible
     later if `Path` + rotation are confirmed in the software renderer; the switch only touches
     `adsb-app`.
  2. **Metadata: direct HTTPS** from the device (RD-D3). TLS stack per parent 5.2: embedded-tls
     first, mbedtls-rs if the gate fails, then the **gated fallback proxy** (RD.11). Unverified
     TLS is never shipped silently (parent rule stays).
  3. **JPEG: O2a `zune-jpeg`**, decode and scale in shared code (`adsb-core`, feature `photo`),
     so that the host, the sim and the device use the same code. Photos are shown at a fixed size
     of **336×224** (the planespotters "large thumbnail" is 420×280 [VERIFY]; scale 0.8, same 3:2
     aspect ratio).
  4. **Config: O3b** (generated `config.rs`). WiFi vars are `WIFI_SSID` and `WIFI_KEY`. The
     parent `ADSB_WIFI_*` names are dropped, not aliased: they were never implemented (3.1 not
     done), and one name per value is simpler to check. `build.rs` fails with a hint if
     `ADSB_WIFI_SSID` is set and `WIFI_SSID` is not.
  5. **JSON: O4b `serde_json` (alloc).** This supersedes `serde-json-core` from parent 5.1.
  6. **Step order: O5b.**
  7. **Focus aircraft** ("closest"): the nearest **positioned** aircraft by ground distance, with
     the parent hysteresis rule (switch only if the new one is closer by more than 20 % **and**
     300 m, or if the current one goes stale or disappears). Inside the radius this is the same
     aircraft as the parent `primary`. It is computed in `adsb-core` (pure, testable).
  8. **The parent card states change**: Overhead / Approaching / Quiet become a small badge in
     the info panel ("OVERHEAD" in the radius, "NEAREST" outside, "APPROACHING" when the CPA
     rule is true). "Just passed" is not shown in this layout (the radar shows where the aircraft
     went). "No data" becomes an overlay on the radar.
  9. **Threads and cores (unchanged from the parent):** all Embassy tasks and all Slint work on
     core 0, one executor. Core 1 runs only the scanout restart ISR. JPEG decode runs on core 0
     in `meta_task` and must not call Slint.
- **Consequences:**
  - The device needs certificate roots for up to three HTTPS hosts (adsbdb API, planespotters
    API, planespotters image CDN). These are public certificates and are committed. A root change
    needs a reflash (parent R16).
  - More PSRAM use: sprites about 40 KB, photo cache 4 × 226 KB, decode buffers about 0.5 MB,
    TLS buffers about 25 KB. 8 MB is enough. Internal SRAM must **not** get these (K1).
  - The parent list view, detail view and touch stay open and are not part of this feature.
  - Photos of the screen (bench snapshots) now show a radar picture of the local traffic around
    the receiver. They are as sensitive as before or more (Privacy).
  - If the fallback proxy is used, a process on the LAN must run. The orchestrator cannot start a
    process on the Mac or publish a container port to the LAN, so the **device verify of the
    fallback needs the user** (RD.11).

**ADR-004 (revision 3): Radar pixel aspect and touch selection**

- **Context:** RD-D8, RD-D9 and "Context for revision 3". The Slint software renderer cannot draw
  ellipses through `Path` or transforms in line-by-line mode. `ui_task` is single-core, 1 Hz, and
  TLS handshakes block the executor for up to about 4 s. The bench cannot touch the screen.
- **Decision:**
  1. **Aspect: O6c.** One constant `adsb_core::radar::PIXEL_ASPECT` = physical pixel width /
     physical pixel height. Start value **1.076** from the usual 7" 800×480 active area of
     154.08 × 85.92 mm [VERIFY against this panel, RD.14.1]. All radar geometry keeps the vertical
     scale (outer ring 208 px high) and divides horizontal offsets by `PIXEL_ASPECT` (outer ring
     about 193 px wide). It is a code constant, not a build variable: it describes fixed hardware;
     a measured value is committed once.
  2. **Rings: O7c** (one opaque RGB8 background image), with a measured gate in RD.14.4. If the
     gate fails, switch to **O7d** (ring tiles) inside RD.14.4 (no design change needed; same
     rasteriser, different cut).
  3. **Sim and aspect:** the sim renders **the same device pixels** (same constant, same code).
     The PNGs stay device framebuffers (the rings are narrower ellipses in them), so `frame_hash`
     parity and hit-test coordinates stay identical to the board. For the human view, the browser
     canvas is shown with the physical aspect (CSS width × `PIXEL_ASPECT`; the pointer mapping
     already uses the bounding rect), and the PNG test writes one extra `*-physical.png` per radar
     keyframe, resampled horizontally by `PIXEL_ASPECT`, in which the rings must be round. A
     sim-only "square pixel" mode is rejected: it would hide exactly the geometry we must check.
  4. **Selection: O8c.** New pure module `adsb_core::select` (state + rules), used by
     `FeedPipeline`. The tracker focus (closest, with hysteresis) is unchanged and keeps running
     under an override, so "back to the closest" needs no extra state.
  5. **Touch input: O9b** (25 Hz polling) with our own GT911 driver. The register parse and the
     tap detector are pure code in `adsb_core::touch`. `touch_task` owns the I2C bus and the
     CH422G after boot (as the parent planned).
  6. **Hit-testing in Rust, not with Slint `TouchArea`**: `radar::hit` tests the blips that were
     last **shown** (`App::ui().blips`), with a tolerance radius and the aspect, so the sim and
     the board resolve a tap the same way and host tests cover it.
  7. **Test without a finger: O10b** (`touch-script` feature). The finger check is a [user] step.
  8. **Defaults:** `ADSB_SELECT_TIMEOUT_S` = **60 s** (range 10–3600). Reasons: a new selection
     needs about 4 s for the text lookup (adsbdb handshake), then the 3 s photo dwell and about
     3.6 s for the photo, so the panel is complete after about 8–11 s; 60 s leaves about 50 s to
     read it; parent 4.4 used 60 s for its manual override; a forgotten selection returns to the
     closest within one minute. The lower limit 10 s keeps the panel from switching back before it
     is complete.
- **Consequences:**
  - `Snapshot` gets `range_km` and `selection`; `App` loses its `RangeState` (uses
    `snap.range_km`). Hand-built `Snapshot`s in tests need the new fields.
  - `radar::Blip` gets `icao`. Label geometry constants (offset 13 px, width 88 px, flip at
    x > 340 px) exist in Rust (hit-test) and in `app.slint`; a test keeps them equal (K20).
  - Text glyphs and the photo stay about 7.6 % too wide on the panel. A photo fix (scale to
    312×224 instead of 336×224) is a small optional change; it is **not** in the plan unless the
    user asks.
  - +602 KB PSRAM for the background image; +1 task (`touch_task`) in internal `.bss`; +25 I2C
    reads per second on core 0.
  - Parent 1.6 is done by RD.15.5/RD.15.7. Parent 4.4 stays open except for its "manual override
    with timeout" idea, which RD.15 replaces for this layout.

---

## Design details

### Configuration

`firmware/build.rs` reads the process environment (loaded with
`set -a; . firmware/.env; set +a`). It writes `$OUT_DIR/config.rs`. The firmware includes it with
`include!(concat!(env!("OUT_DIR"), "/config.rs"))`.

| Var | Required | Notes |
|-----|----------|-------|
| `WIFI_SSID` | yes | 1–32 bytes |
| `WIFI_KEY` | yes | 8–63 bytes (WPA2-PSK). Open networks are out of scope. |
| `ADSB_FEED_HOST` | yes (unless `ADSB_DEMO` is set) | IPv4 literal or host name [host name needs DNS, RD.8; use the IPv4 literal before RD.8] |
| `ADSB_FEED_PORT` | no, default 30002 | |
| `ADSB_RX_LAT`, `ADSB_RX_LON` | yes | decimal degrees, range-checked |
| `ADSB_RX_ELEV_M` | no, default 0 | −500…10,000 |
| `ADSB_RADIUS_KM` | no, default 5 | 0.5…50 (radius for "OVERHEAD") |
| `ADSB_DEMO` | no | `overflight` or `busy`: feed the scenario instead of the network (parent 4.1) |
| `ADSB_META_PROXY` | no | `http://<host>:<port>`: use the fallback proxy (RD.11) instead of direct HTTPS |
| `ADSB_SELECT_TIMEOUT_S` | no, default 60 | Revision 3 (RD.15): integer seconds, 10…3600. Time after which a manual (tapped) selection returns to the closest aircraft. Validated in `build_config.rs` with the `number` helper; written as `SELECT_TIMEOUT_MS: u64`. |

Rules for `build.rs`:

- Error messages name the variable and the rule. They **never** contain the value.
- `cargo:rerun-if-env-changed` for each var. No `cargo:warning` with values.
- `firmware/.env.example` (committed) uses fictitious values only (same mid-ocean receiver as
  `adsb-core::scenario`: 47.0, −30.0; SSID `example-ssid`; host `192.0.2.10`).

Rules for the orchestrator and agents (unattended):

- Do not `cat`, `grep`, `env`, `printenv`, `set -x` or `cargo -v`/`-vv` while `.env` is loaded.
- Do not log the SSID, the key, the feed host or the receiver coordinates on the device. The
  device logs only "wifi: connected ip=…" (a private LAN address) and "feed: connected".
- The privacy check of each config step uses `git grep -c` with the values read from the file
  inside one shell command, and prints only the count.

### Screen layout (800×480)

```
y=0   +---------------------------------------------------------------+
      | status bar 32 px: WiFi dot, Feed dot, "23 ac", "310 msg/s", clock |
y=32  +------------------------------+--------------------------------+
      | RADAR 448 x 448              | INFO PANEL 352 x 448           |
      |   centre (224, 256)          |  photo 336x224 (or placeholder)|
      |   outer ring r = 208 px      |  credit line 14 px             |
      |   N mark, rings + labels     |  CALLSIGN 36 px + badge        |
      |   blips: sprite 20x20 +      |  airline                       |
      |   label 14 px                |  make / model (ICAO type)      |
      |   focus: ring + bold label   |  registration                  |
      |   range label "25 km"        |  ORIG -> DEST (IATA, 28 px)    |
      |                              |  city -> city                  |
      |                              |  ALT  GS  TRK  DIST/BRG grid   |
y=480 +------------------------------+--------------------------------+
```

The exact pixel values are a start point. The rule is: everything fits; long text elides; no
overlap between panel rows (checked in sim PNGs, RD.5 and RD.7).

**Radar rules** (pure functions in `adsb-core::radar`, unit-tested):

- **Projection:** north-up azimuthal from the receiver, from `PositionView.ground_m` and
  `bearing_deg`: `x = cx + r·sin(b)·k`, `y = cy − r·cos(b)·k`, `k = 208 px / range`.
  *(Revision 3: `x = cx + r·sin(b)·k / PIXEL_ASPECT`; see [Radar pixel aspect](#radar-pixel-aspect-rd14).)*
- **Range:** let `d` = ground distance of the n-th nearest positioned aircraft, n = min(10,
  count). Range = the smallest step of {10, 25, 50, 100} km with `step ≥ 1.1·d`; if none, 100 km.
  No aircraft: 25 km. **Hysteresis:** step up at once; step down only after the smaller step was
  valid for 15 s without a break.
- **Rings:** at half and full range, with labels ("5", "10 km"). Also the `ADSB_RADIUS_KM` ring in
  a dim colour when it is smaller than half the range.
- **Blips:** at most 48 (nearest first). Aircraft outside the range are not drawn; the range
  label shows "+k beyond". Labels only for the nearest 12 and the focus aircraft (clutter).
  Label = callsign (trimmed), else ICAO24 as 6 hex digits.
- **Sprite:** chosen by `round(track/15) mod 24`. No track (stationary, or unknown) → a round
  sprite. Colours: normal; dim for `Marker::Unknown` ("?"); amber outline for `provisional`.
- **Focus:** a ring around the blip and a bold label; the same aircraft is in the info panel.
- **No data:** a centred overlay "No data: feed disconnected" over the radar.

**Info panel rules:**

- Values from the tracker: callsign, altitude (FL and ft, climb/descend arrow), GS (kt), track
  (°), distance + compass bearing (+ elevation angle, as in the parent), "~" if dead-reckoned.
- Values from metadata: airline name, manufacturer + type + ICAO type code, registration,
  origin/destination IATA + municipality, photo, photographer credit.
- Unknown or not yet loaded: "—". Photo not available: a grey placeholder with a plane outline
  and the text "no photo".
- Credit line: "Photo © <photographer> / planespotters.net" (exact form per the terms, [VERIFY]).

### View-model changes (`adsb-app`)

- `App::update(&Snapshot, &Status, Option<&FocusMeta>)`. `FocusMeta` (from `adsb-core::meta`)
  holds the text fields and an optional photo (`width`, `height`, RGB8 bytes). The platform
  (firmware or sim) gives it; `App` never fetches.
- Radar blips go to Slint as a `VecModel<Blip>` (`x`, `y`, `sprite`, `label`, `focus`, `dim`,
  `flag`). Update rows only when they change (diffing as today), so partial repaint stays small.
- Sprites: generated once (`adsb-core::radar::sprite(angle_deg, style) -> [u8; 20*20*4]`, simple
  anti-aliased polygon fill), then converted to `slint::Image` once.
- Photo: `slint::Image::from_rgb8(SharedPixelBuffer<Rgb8Pixel>)` [VERIFY the software renderer
  draws runtime RGB8 images without a per-frame conversion]. Set only when the photo changes.
- The `NavButton` and the `Screen` switch are removed from the main screen (no touch in scope).
  The `glyph-seed` grows to cover the airport-city glyphs (parent carry-forward).

### Metadata (`adsb-core::meta`, extends parent 5.1)

- **adsbdb** [VERIFY shapes]: one request `GET https://api.adsbdb.com/v0/aircraft/<ICAO24>?callsign=<CS>`
  is expected to return `response.aircraft` (`type`, `icao_type`, `manufacturer`,
  `registration`, `registered_owner`, `url_photo`, …) and `response.flightroute` (shape verified
  in the parent). Without a callsign: `GET /v0/aircraft/<ICAO24>`. Not found:
  `{"response":"unknown aircraft"}` (verified) or similar strings. If the combined query does not
  exist, use two requests (`/v0/aircraft/…`, `/v0/callsign/…`).
- **planespotters** [VERIFY shapes and terms]: `GET https://api.planespotters.net/pub/photos/hex/<ICAO24>`
  is expected to return `{"photos":[{"id","thumbnail":{"src","size":{"width","height"}},
  "thumbnail_large":{"src","size"},"link","photographer"}]}`. Use the first photo,
  `thumbnail_large.src` (else `thumbnail.src`). Expected terms: show the photographer name and
  planespotters as the source; a User-Agent that names the app may be required. If the terms
  forbid this use, stop the photo part and report (do not work around it).
- **Lookup policy** (replaces the parent policy for this layout): look up the **focus**
  aircraft, plus the next 2 nearest as prefetch (text only). Fetch a **photo only for the focus**.
  Skip callsigns that are empty or contain `?` (parent carry-forward). At most 1 request/s, back
  off on errors, stop for 5 min on HTTP 429. A new focus must stay focus for 3 s before its photo
  fetch starts (limits fetch storms).
- **Cache:** LRU, text 64 entries (TTL: route 6 h, aircraft 7 d, negative 1 h, as parent);
  decoded photos 4 entries in PSRAM. Photos are never written to flash or to the repo.
- **Photo decode:** download into a PSRAM buffer with a cap of **96 KB** (larger → "no photo");
  `zune-jpeg` decode to RGB8; box/bilinear scale to 336×224 (crop if the aspect ratio differs);
  free the decode buffers at once.
- **Fixtures (Privacy, licence):** hand-written JSON (the parent `KLM1023` example and
  fictitious entries for scenario callsigns) and a **synthetic JPEG** generated by a test helper
  (`jpeg-encoder` dev-dependency [VERIFY]). No real API responses from local traffic and **no
  real planespotters photos** are committed (copyright of the photographers).

### Firmware tasks (extends parent "Embassy task structure")

```
main (core 0): init as today → spawn tasks → ui loop
 ├─ wifi_task   connect with WIFI_SSID/WIFI_KEY, reconnect with backoff (parent 3.2)
 ├─ net_task    embassy-net runner (DHCP; DNS from RD.8)
 ├─ feed_task   TCP to the feed, FeedPipeline.feed, reset_clock on reconnect (parent 3.3/3.4)
 │              or the ADSB_DEMO scenario
 ├─ ui_task     1 Hz: snapshot → App.update(meta) → render dirty lines → PSRAM fb
 │              → cache writeback of the dirty rows (direct scanout path)
 ├─ sntp_task   (RD.8) wall clock
 ├─ meta_task   (RD.9/RD.10) lookup queue → HTTPS (or proxy) → cache; JPEG decode
 └─ touch_task  (RD.15.5) owns I2C + CH422G after boot; GT911 reset, 25 Hz poll → tap detector
                → TAP signal → ui_task (extra tick at once: hit-test, select, render)
core 1: scanout restart ISR only (unchanged)
```

Memory placement (K1): Slint, sprites, photos, TLS buffers, JSON bodies and socket buffers go
to **PSRAM**. RD.2 decides how: either the PSRAM region is added **first** so that plain allocs
use it, while esp-radio asks for internal memory by capability [VERIFY esp-radio/esp-alloc
behaviour], or large buffers use `HEAP.alloc_caps(External)` explicitly. Gate: internal free
heap stays **≥ 10 KB** in steady state with WiFi, feed and UI running.

### Fallback proxy (RD.11, gated)

A new binary `meta-proxy` in the `sim-bridge` crate. It shares an upstream module (`meta`) with
the optional sim endpoints (RD.12). It is **not** the sim bridge: it serves no feed, no config,
no capture.

- `GET /v1/meta/<icao24>?callsign=<cs>` → compact JSON (the `FocusMeta` text fields).
- `GET /v1/photo/<icao24>.rgb565` → 8-byte header (width, height as u16 LE, 4 bytes reserved)
  + 336×224 RGB565 LE pixels. Scaled with the same `adsb-core` photo code.
- Input checks: ICAO24 = 6 hex digits, callsign = `[A-Z0-9]{1,8}`. Upstream hosts are fixed.
  Rate limit and cache as on the device.
- It listens on an address from `META_PROXY_ADDR` (it must be on the LAN for the device to reach
  it). Optional client allow-list `META_PROXY_ALLOW` (the device IP). No CORS, read-only, logs
  ICAO/callsign only at debug.
- The device uses it when `ADSB_META_PROXY` is set (plain HTTP, no TLS, no JPEG decode).

### Privacy (adds to the parent rules)

- Bench snapshots and serial logs show the radar picture and live callsigns. They stay in the
  gitignored `.bench/`. Do not copy them into `docs/`, commits or reports. Describe only what the
  verify needs (layout correct, values present).
- Sim PNGs are committed only if they come from the fictitious scenarios and fixtures.
- The info panel shows no raw lat/lon. The radar shows no coordinates, only km rings.
- WiFi key, SSID, feed host and receiver location: see Configuration rules.

### Render performance targets

Measured in RD.2 and RD.6, logged as `ui: render full_ms=… partial_ms=… rows=…`. Targets
(estimates, adjust after measuring): full redraw ≤ 400 ms; typical 1 Hz radar update ≤ 80 ms;
photo change ≤ 250 ms. If the 1 Hz update is over 300 ms, lower the radar update to 0.5 Hz and
record it.

### Radar pixel aspect (RD.14)

**Numbers.** `PIXEL_ASPECT` = (active width / 800) / (active height / 480). With 154.08 × 85.92 mm
[VERIFY]: 0.1926 mm / 0.1790 mm = **1.076**. Square pixels would give 1.000. A circle drawn with
the same pixel radius in x and y is then 7.6 % wider than tall, which matches the user's report.
The 416×416 px ring of the sim PNG is about 80.1 mm wide and 74.5 mm high on the panel.

**What uses the constant** (all in `adsb-core::radar`, pure):

| Item | Today | With the correction |
|------|-------|---------------------|
| `project` | `x = cx + r·sin(b)·k` | `x = cx + r·sin(b)·k / PIXEL_ASPECT` (y unchanged) |
| Rings (outer, half, radius ring) | Slint `Ring` circles r = 208/104/n px | ellipses `rx = r / PIXEL_ASPECT`, `ry = r` (outer: 193 × 208 px) in the background image (O7c) |
| Sprites | shape tested in square px | the rasteriser maps a pixel offset `dx` to `dx·PIXEL_ASPECT` before the shape test, so the plane keeps its true shape and its drawn track angle is right (a 45° track is drawn at 45° on the glass, not 47°) |
| Focus mark | 30×30 round `Rectangle` | `round(30 / PIXEL_ASPECT)` × 30 = 28×30, `border-radius` 14 (a stadium so small looks like an ellipse) |
| Labels | offset in px from the blip | unchanged (they follow the corrected blip `x`) |
| Hit-test (RD.15) | — | distance `sqrt((dx·PIXEL_ASPECT)² + dy²)`, i.e. a round area on the glass |

The radar centre stays at (224, 224) of the 448×448 box. The outer ring now leaves about 31 px
left and right inside the box; the box size and the info panel do not change. Bearings and
distances in the panel text are not affected.

**Background image (O7c).** `radar::background(radius_ring_px: Option<u16>) -> Vec<u8>` returns
448×448 RGB8 (602 KB): the radar background colour `#0a0d12`, the two 1-px cross-hair lines
`#1c2431`, the outer and half rings `#2f3a4d`, and the dim radius ring `#4a3b1c` when present.
Rings are 1 px anti-aliased ellipse outlines (coverage from the distance to the ellipse, only
evaluated near the edge of each ring). `App` builds it again only when `radius_ring_px` changes
(a range change), in PSRAM, and sets it once on a new Slint `radar-bg` image property. Log on the
board: `radar: bg_ms=…`. The N mark, range label, half label, "+k beyond" and the no-data overlay
stay Slint `Text`/`Rectangle` (they do not depend on the aspect; the half label keeps its y
because `ry` is unchanged).

**Fallback O7d (only if the RD.14.4 gate fails):** the same ring raster as RGBA, cut into tiles:
16 row bands per ring, and in each band only the column runs that have alpha > 0. Shown through a
`VecModel<RingTile {x, y, image}>`. The background stays a solid `Rectangle`.

**Sim.** Same constant, same code; PNGs are device pixels. The browser shows the canvas with the
physical aspect (see ADR-004 decision 3). `sim/tests/snapshots.rs` also writes
`<keyframe>-physical.png` for the radar keyframes: each row resampled to
`round(800·PIXEL_ASPECT)` px wide (linear). In those files the outer ring must be round.

**How to confirm the constant on the bench and by the user** (RD.14.1): a firmware feature
`diag-aspect` draws, instead of the UI, a white 1-px border on the 800×480 edge, a 300×300 px
square with its inscribed circle on the left ("square pixels" assumption) and a
`round(300/PIXEL_ASPECT)`×300 px rectangle with its inscribed ellipse on the right ("corrected"),
and the text of the constant in use. Then:

- *Bench (plausibility, no ruler):* in the webcam photo, measure the border rectangle (average of
  top and bottom edge length / average of left and right edge length). Non-square pixels give
  about 1.79 (= 1.667 × 1.076); square pixels give 1.667. The camera is not exactly frontal, so
  accept only a clear result: ≥ 1.75 confirms the hypothesis, ≤ 1.70 rejects it (stop and report),
  in between is INCONCLUSIVE. Also check that the right shape looks round and the left one wide.
- *User (decisive):* measure with a ruler the border width W and height H in mm (the white line
  on the panel edge). `PIXEL_ASPECT = (W/800)/(H/480)`. Expected W ≈ 154 mm, H ≈ 86 mm. If the
  result differs from 1.076 by more than 1 %, commit the measured value (3 decimals).

### Touch selection (RD.15)

**Data flow (firmware):**

```
touch_task (25 Hz): GT911 read ─► touch::Sample{t_ms, Option<(x,y)>} ─► touch::TapDetector
     │                                                      │ Tap{x,y}
     │  (feature touch-script: script samples replace       ▼
     │   the GT911 samples while a script tap is active)  TAP: Signal ─► ui_task (wakes at once)
     ▼                                                                  │
  `touch:` logs                         App::hit(x,y) on the shown blips/layout ─► Target
                                        pipeline.tap(Target, now_ms, SELECT_TIMEOUT_MS) ─► Outcome (`select:` log)
                                        tick(): snapshot (applies selection) → publish → update → render
```

The sim uses the same `TapDetector` (mouse down/up with the browser clock), `App::hit` and
`FeedPipeline::tap`, plus a direct `Sim::tap(x, y)` for tests.

**Selection rules** (`adsb_core::select::Selection`, pure; times on the snapshot clock `now_ms`):

| Situation | Result | Log (platform) |
|-----------|--------|----------------|
| No override | `snap.focus` = tracker focus (closest, hysteresis as ADR-003 decision 7) | — |
| Tap on a blip of aircraft X; X is not the current override and not the tracker focus | override X until `now + timeout` | `select: manual icao=X until=<ms>` |
| Tap on the blip of the current override | renew: until `now + timeout` | `select: renew icao=X until=<ms>` |
| Tap on the blip of the tracker focus (closest), no override | no change (it is already selected; no override starts) | `select: tap icao=X already` |
| Tap on the blip of the tracker focus (closest) while an override runs | override ends, closest again (the user asked for the closest) | `select: auto reason=user icao=<old>` |
| Tap on the info panel while an override runs | override ends, closest again | `select: auto reason=user icao=X` |
| Tap on the info panel, no override | no change | `touch: tap target=panel` |
| Tap on empty radar space, "+k beyond", N mark, range label | **no change** (a near miss on a crowded radar must not drop the selection) | `touch: tap target=radar miss` |
| Tap on the status bar | no change | `touch: tap target=status` |
| Override and X is **out of scope**: not in the table, no position (stale), `ground_m > range`, or rank ≥ `MAX_BLIPS` (= X is not drawn) | override ends at the next snapshot | `select: auto reason=scope icao=X` |
| Override and `now ≥ until` | override ends | `select: auto reason=timeout icao=X` |

- "In scope" is one function, `radar::in_scope(snap, index, range)`, also used by `radar::blips`,
  so "drawn" and "in scope" cannot disagree. A range step-down (after 15 s) can push X out: that
  is "leaves the radar scope" and ends the override.
- After an override ends, the panel shows the tracker focus at once (the tracker kept it up to
  date). If that is the same aircraft, only the badge changes.
- While the feed is down (no-data overlay), no blips are shown, so taps hit nothing. A running
  override still ends by timeout or scope.
- `Snapshot` gets `range_km: RangeKm` and `selection: SelectView` (`Auto` or
  `Manual { icao, until_ms }`, a copy of the `Selection` state for the view). `FeedPipeline::tap(target, now_ms, timeout_ms) -> TapOutcome`
  and `FeedPipeline::take_select_event() -> Option<SelectEvent>` (for the `select: auto` logs,
  which happen inside `snapshot`). `adsb-core` does not log.

**Hit-test** (`radar::hit(blips: &[Blip], x, y) -> Option<u32>`, radar-box coordinates; `App::hit`
first maps screen coordinates to a `Target { Blip(icao), RadarMiss, Panel, Status }` by the layout
rectangles: status bar y < 32; radar x < 448; panel x ≥ 448):

1. Candidates by centre: blips with `sqrt((dx·PIXEL_ASPECT)² + dy²) ≤ TOUCH_RADIUS_PX` = **28 px**
   (about 5 mm on the glass, so the target is about 10 mm wide, a normal finger target; the
   sprite alone is 20 px ≈ 3.8 mm). The nearest centre wins.
2. Else, labels: a blip whose label rectangle (same geometry as `app.slint`: `x+13 … x+101`, or
   `x−100 … x−12` when `x > 340`; `y−8 … y+8`), grown by 4 px, contains the point. If more than
   one, the one drawn last (on top) wins.
3. Else: miss.

`radar::Blip` gets `icao: u32`. The constants `LABEL_DX = 13`, `LABEL_W = 88`, `LABEL_FLIP_X = 340`
live in `radar.rs`; a test in `adsb-app` renders a blip label and checks that its text pixels are
inside the Rust rectangle (K20).

**Tap detector** (`adsb_core::touch::TapDetector`, pure, sample timestamps in ms):

- Down = first sample with a point. Keep the mean position of all touch samples.
- Drag: any sample more than `SLOP_PX` = 24 px (aspect-weighted) from the first point → no tap.
- Up = **2 empty samples in a row** (about 80 ms at 25 Hz; filters a single dropped report).
- Tap if not a drag and the touch lasted ≤ 1000 ms; long presses are ignored (reserved).
- After a tap, ignore a new down for 250 ms (bounce, double reports).
- Output `Tap { x, y }` in screen pixels (after calibration).

**GT911 driver** (`firmware/src/gt911.rs`; parse in `adsb_core::touch::gt911`). Register map
[VERIFY against the GT911 programming guide and Waveshare's `esp_lcd_touch_gt911` driver]:
16-bit big-endian register address; `0x8140` product ID (ASCII "911"), `0x8144` firmware
version (u16 LE), `0x8146`/`0x8148` x/y resolution (u16 LE); `0x814E` status (bit 7 = buffer
ready, bits 3..0 = number of points); first point at `0x814F` (track id, x u16 LE, y u16 LE, size
u16 LE, reserved); write `0x00` to `0x814E` after each read with bit 7 set.

- **Reset and address** (in `touch_task`, after the backlight is on, so boot is not slower):
  GPIO4 output low; 10 ms; `TP_RST` (EXIO1 through `Ch422g::set_pin`, the shadow keeps the
  backlight and USB bits) low 100 ms; high; 200 ms; GPIO4 to input (no pull) — the parent 1.6
  sequence, which gives **0x5D**. Then read the product ID at 0x5D; if it NAKs, try 0x14 and log
  which one answered. If neither answers: `touch: gt911 missing`, the task parks, the UI works
  without touch. [VERIFY the esp-hal 1.2 API to switch GPIO4 from output to input, e.g. `Flex`.]
- **Init log:** `touch: gt911 ok addr=0x5d id=911 fw=<hex> res=<x>x<y>`.
- **Poll** every 40 ms: read status (1 byte). If bit 7 and points ≥ 1: read point 1 (8 bytes),
  clear status. Bit 7 and 0 points = release report: clear status. Bit 7 clear = no new report:
  repeat the last state (a held finger does not always produce new reports) [VERIFY].
- **Mapping and calibration:** if the reported resolution is 800×480, coordinates are used as
  they are (the parent records no swap or mirror). Else scale per axis to 800×480. A calibration
  constant (offset and scale per axis, default identity) sits in `touch.rs`; RD.15.7 decides if it
  is needed. Coordinates are clamped to the screen.
- **Errors:** an I2C error is counted and logged at most once per 10 s (`touch: i2c error n=…`);
  after 10 errors in a row, poll once per second until a read succeeds. Never panic after boot.
- **Stats:** every 60 s `touch: stats polls=… reports=… taps=… err=…`.

**Bus ownership.** `main` keeps using the `Ch422g` for the LCD reset and the backlight, then
**moves** it into `touch_task`. `Ch422g` gets `pub fn bus(&mut self) -> &mut I2c<'d, Async>` so the
GT911 driver borrows the same bus. No mutex is needed because nothing else uses the bus after
boot. A later user of the expander (for example backlight off at night) sends a command to
`touch_task`.

**Visual feedback of a manual selection:**

- Focus mark colour: cyan `#22d3ee` instead of yellow `#ffcc00` (new Slint property
  `manual-select: bool`; only the one focus mark item depends on it).
- Panel badge: `MANUAL 52 s` (cyan, new `Badge::Manual`, new string property `badge-text` for the
  countdown; `Ui.manual_left`), in place of OVERHEAD/APPROACHING/NEAREST while the override runs.
  The countdown changes once per second: a small dirty area (112×24 px), like the clock.
- Nothing else changes: the panel shows the selected aircraft's values and metadata as for the
  closest.

**Response time.** `ui_task` waits on `select(ticker.next(), TAP.wait())`. A tap gives an extra
tick at once (snapshot → lookups → update → render, 50–100 ms on the board), so the ring and the
panel change about 0.1–0.2 s after the finger lifts. The 1 Hz ticks continue as before. During a
TLS handshake (up to about 4 s) or a photo decode (about 0.5 s) the executor does not run: a tap is
handled late, or lost if the whole touch falls inside the stall (K19).

**Interaction with the lookups and the photo flow.**

- `lookup::publish` reads `snap.focus`, so the text lookup of the selected aircraft is planned on
  the same tick, and the 2 prefetches are the 2 nearest others (this includes the closest, so the
  return to it is quick from cache).
- The photo still waits for `PHOTO_DWELL_MS` = 3 s of stable selection. Fast taps on several
  aircraft make text lookups (rate limiter: at most 1/s, cache) but no photo fetches. A photo flow
  that already runs for the old selection completes and goes to the photo cache (4 entries); it
  is not cancelled.
- No new lookup rules are needed.

**Memory and CPU (core 0, single executor).** Selection state: about 24 B. Tap detector: about
40 B. `touch_task` future: in internal `.bss` (task arena); esp-hal's async I2C futures are large
(see the `large_stack_frames` allowance in `main.rs`), so measure the `.bss` growth and keep the K1
gate (internal free ≥ 10 KB, today `internal_min` = 20,588 B). CPU: 25 status reads per second
(async, about 150 µs of bus time each), hit-test over ≤ 48 blips per tap, one extra render per
tap. Flash-resident code runs 25 times per second more often: re-check K4 (K17).

**Touch script (feature `touch-script`, debug only; O10b).** The script is data in
`adsb_core::touch::script` so the sim and the firmware use the same list:
`&[(at_ms, Action)]` with `Action` = `TapIcao(u32)` (busy demo, deterministic), `TapRank(n)`
(n-th shown blip that is not the current focus; for live builds), `TapPanel`, `TapEmpty` (a
radar point at least 60 px from every blip and outside every label rectangle), `TapStatus`. The firmware resolves the target
against a copy of the shown blips that `ui_task` publishes (only in this feature), then generates
raw samples (down for 120 ms at 25 Hz, then release) into the same `TapDetector`. Logs
`touch: script action=… x=… y=…`. The busy-demo script (times on the scenario clock; exact ICAOs
and times chosen in RD.15.3 from the sim and asserted by the sim test):

1. tap a non-focus blip → `manual`;
2. +10 s, tap it again → `renew`;
3. +10 s, tap empty radar → miss, selection unchanged;
4. +5 s, tap the panel → `auto reason=user`;
5. tap a blip that the scenario moves out of the range before the timeout → `auto reason=scope`;
6. tap a blip that stays in range → `auto reason=timeout` (build with `ADSB_SELECT_TIMEOUT_S=20`
   so the run stays short).

---

## Plan

Each step ends with its verify green, then one commit. The orchestrator runs every verify
without the user. Use these command forms:

- **Host:** `cd <crate> && cargo test` (and `cargo clippy --all-targets -- -D warnings`) for
  `modes`, `adsb-core`, `adsb-app`, `sim`, `sim-bridge`.
- **Sim PNG:** `cd sim && cargo test --test snapshots`; then `Read` the PNGs in
  `sim/target/ui-snapshots/` and judge them against the written expectation.
- **Firmware build:** `cd firmware && set -a && . ./.env && set +a && cargo build --release`
  (no `-v`). Also `cargo clippy --release`.
- **Bench:** per `.claude/skills/board-verify/SKILL.md`: write the expectation first;
  `tools/bench/bench health`; `tools/bench/bench flash firmware/target/xtensa-esp32s3-none-elf/release/firmware --until '<marker>' --timeout <s>`;
  `tools/bench/bench log --session --until '<marker>' --timeout <s>` for later markers;
  `tools/bench/bench snap --delay 2`; `Read` the photo.

**Stop rules (unattended):** max 2 fix loops per step (as in the parent pipeline). If a step
still fails, record the evidence in the revision log, skip only the steps that depend on it, and
continue with independent steps (the dependency column below). Stop the whole run if the bench
is unhealthy (`bench health` exit 4) or a photo shows a person or room (skill rules). Never
change `.claude/`, `.devcontainer/` or the bench host configuration.

| Step | Depends on | Visible result |
|------|-----------|----------------|
| RD.0 | — | settles fw-D |
| RD.1 | — | config |
| RD.2 | RD.1 | **Slint UI on the board (demo feed)** |
| RD.3 | RD.2 | **live aircraft on the board** |
| RD.4 | — | radar logic (host) |
| RD.5 | RD.4 | new layout in sim PNGs |
| RD.6 | RD.3, RD.5 | **radar on the board** |
| RD.7 | RD.5 | metadata + photo in sim PNGs |
| RD.8 | RD.3 | TLS gate result |
| RD.9 | RD.7, RD.8 | **route/type on the board** |
| RD.10 | RD.9 | **photo on the board** |
| RD.11 | RD.8 failed | fallback proxy (gated) |
| RD.12 | RD.7 | optional sim live metadata |
| RD.13 | RD.6 (and RD.10 if done) | soak result |
| RD.14.1 | — | aspect measured (bench + [user] ruler) |
| RD.14.2 | — (uses the start value; RD.14.1 may change the constant later) | aspect in radar logic (host) |
| RD.14.3 | RD.14.2 | elliptic rings in sim PNGs, round in `*-physical.png` |
| RD.14.4 | RD.14.1, RD.14.3 | **round radar on the board** (+ [user] look) |
| RD.15.1 | RD.14.2 | selection policy + hit-test (host) |
| RD.15.2 | — | GT911 parse + tap detector (host) |
| RD.15.3 | RD.15.1, RD.15.2, RD.14.3 | manual selection in the sim (mouse, PNGs, script log) |
| RD.15.4 | — | `ADSB_SELECT_TIMEOUT_S` |
| RD.15.5 | RD.15.2, RD.15.3, RD.15.4 | GT911 alive on the board (no finger) |
| RD.15.6 | RD.15.5 | **scripted taps select on the board** |
| RD.15.7 | RD.15.6 | [user] finger check |

Revision 3 steps follow the same stop rules. They start only after the user approved revision 3.

### RD.0 [hw] Bench check and the fw-D question

The open parent result: is the vendored esp-hal reorder patch still needed with the per-frame
restart? It matters here because every later firmware step builds on `firmware/Cargo.toml`.

- Build two ELFs from the current head: as is (`fw-D-vendored`), and with the
  `[patch.crates-io]` lines commented out (`fw-D-stock`). Copy them to `firmware/target/`.
- For each: `bench flash <elf> --until 'alive 5s' --timeout 60`, then 3 snapshots 10 s apart.
- *Verify / decision rule:* both show the 1.3 bar pattern, full white border on all four
  edges, no shift between the 3 photos, and `restart: frames/s=39` or 40 with `isr_max_us` < 50.
  If **stock passes**, keep the patch lines commented out (one commit, note in VENDORED.md; keep
  the vendor directory for now). If stock fails and vendored passes, keep the patch. If both
  fail, stop and report (the display baseline is broken).
- Record the result in this doc's revision log and in the parent's revision log.

### RD.1 [code] Build-time config (parent 3.1, changed by RD-D1 and O3b)

- `firmware/build.rs` reads the vars in [Configuration](#configuration), validates them and
  writes `$OUT_DIR/config.rs` (`pub const WIFI_SSID: &str`, `RX_LAT: f64`, …, `DEMO:
  Option<Demo>`). Keep the generator code in `build.rs`.
- Put the parse/validate logic in `firmware/build_config.rs`, included by `build.rs` with
  `#[path]`, and test it from a host test crate path that does not need the Xtensa toolchain
  (for example a `#[cfg(test)]` module run with `rustc --test` on the host, or a small
  `tools/build-config-test` crate that includes the same file). [Pick the simplest that runs in
  CI without the esp toolchain.]
- `firmware/.env.example` with fictitious values. `firmware/src/config.rs` wraps the include.
- *Verify:*
  - Host tests: missing var → error names the var; bad lat/lon/elev/radius/port → error; error
    text never contains the input value (test with a marker value); `ADSB_WIFI_SSID` without
    `WIFI_SSID` → hint; `ADSB_DEMO=overflight` makes `ADSB_FEED_HOST` optional.
  - Firmware build with `firmware/.env` loaded succeeds; with an empty env it fails naming
    `WIFI_SSID` first.
  - `git check-ignore firmware/.env` succeeds.
  - Leak check, printing only counts: values from `firmware/.env` (SSID, key, lat, lon) found in
    `git grep -F` of the tree → 0, and in the captured build output → 0.

### RD.2 [code+hw] Slint on the device with the demo feed (parent 4.1)

- Firmware platform glue: `App::new` with time from `embassy-time`; `ui_task` at 1 Hz:
  `FeedPipeline.snapshot` → `App::update` → `App::render` with a 800-px SRAM line buffer →
  copy spans into the PSRAM framebuffer → `cache_writeback_addr` over the dirty row range (one
  wrapper function, parent R3).
- Replace `display::draw_pattern` at boot with a black frame, and turn on the backlight after
  the first rendered UI frame. Keep the pattern behind a `diag-pattern` feature.
- `ADSB_DEMO=overflight` feeds the scenario into `FeedPipeline` in a `feed_task` (needs the
  `scenario` feature of `adsb-core` in the firmware only when `ADSB_DEMO` is set; use a Cargo
  feature `demo`).
- Memory placement decision (see Firmware tasks, K1). Log `heap internal_free=… psram_free=…`
  every 10 s.
- The UI is still the **current card UI**. This step proves Slint on the board.
- *Verify:*
  - Host: `cargo check` of `adsb-app` for xtensa (via the firmware build), all host tests green.
  - Bench: expectation "status bar with WiFi/Feed dots and counts, card 'Overhead' for the
    demo aircraft at scenario time ≥ 211 s". `bench flash … --until 'ui: first frame'
    --timeout 90`; then `bench log --session --until 'ui: card=Overhead' --timeout 300`; then
    `bench snap`. The photo shows the card layout like `sim/target/ui-snapshots/3-overhead.png`
    (same texts; colours may differ by panel gamma).
  - Log has `ui: render full_ms=` and `partial_ms=` values; record them here.
  - **Gate K1:** steady-state `internal_free` ≥ 10 KB, and no allocation failure or panic over 5
    minutes (`bench log --session --until 'alive 300s' --timeout 360`).

### RD.3 [code+hw] WiFi, live feed and drift check (parent 3.2, 3.3, 3.4, part of 1.4/3.5)

- `wifi_task` (esp-radio station, `WIFI_SSID`/`WIFI_KEY`, reconnect with backoff 1 s → 30 s),
  `net_task` (DHCP), `feed_task` (TCP connect to `ADSB_FEED_HOST:PORT`, timeout 10 s, idle
  timeout 60 s, backoff, `FeedPipeline::reset_clock` on reconnect). `Status.wifi_ok`/`feed_ok`
  from these tasks. TCP socket buffers in PSRAM (rx 8 KB, so about 0.5 s of stall from a JPEG
  decode later does not drop data).
- Every 10 s at `info`: `feed: frames/s=… crc_fail=… aircraft=…`. Every 30 s at `info`: focus
  callsign, distance and bearing only (no lat/lon; parent privacy).
- Firmware feature `diag-border`: draws a 1-px white border over the UI after each render (for
  the drift check).
- *Verify:*
  - Bench: expectation "both dots green, aircraft count > 0, msgs/s > 0, card shows a real
    callsign or 'Nothing within 5 km'". `--until 'wifi: connected'` (60 s), then
    `--until 'feed: connected'`, then `--until 'feed: frames/s=[1-9]'`. Snapshot.
  - Plausibility without the user: aircraft count in the log is 3–128; the focus distance is
    under 450 km. (The parent cross-check against flightradar24 needs a human; it is deferred
    to the user's review.)
  - Drift (with `diag-border`): 6 snapshots at 0, 2, 4, 6, 8, 10 min. The border is complete on
    all four edges in every photo, and no content shift is visible. Log shows
    `restart: frames/s` 39–40 all the time. If drift shows, stop RD.6/RD.13 and report (parent
    M3/M4 decision is the user's).
  - Not verifiable unattended (deferred to the user): AP power cycle, fr24feed stop/start.

### RD.4 [code] Radar and focus logic (`adsb-core`)

- `adsb-core::radar`: `select_range(&Snapshot, now_ms, &mut RangeState) -> RangeKm` (rule +
  hysteresis), `project(&PositionView, range) -> (x, y)`, `blips(&Snapshot, range, focus) ->
  Vec<Blip, 48>` (label rules, styles), `sprite(angle, style)` rasteriser.
- `tracker`: new snapshot field `focus: Option<usize>` (nearest positioned with the hysteresis
  of decision 7). Keep `primary` for the badge.
- *Verify (host):* unit tests: range picks 10/25/50/100 for synthetic sets (10 aircraft at 8 km
  → 10; one at 30 km among 10 → 50 with factor 1.1 → check the exact boundaries); step up at
  once, step down only after 15 s; empty → 25 km. Projection: bearings 0/90/180/270 land on the
  N/E/S/W axes ±1 px. Focus: two aircraft swapping order by < 20 % do not switch; > 20 % and
  > 300 m switch; stale focus switches. Sprite: the 0° sprite's nose pixel is at the top, the
  90° sprite's at the right; 24 sprites are distinct. Labels: callsign trimmed, ICAO fallback,
  only 12 + focus labelled.

### RD.5 [code] New layout in `adsb-app` and the sim (no metadata)

- `ui/app.slint`: status bar (kept), radar area (rings, N mark, range label, blip repeater,
  focus ring, no-data overlay), info panel with "—" for metadata fields and the photo
  placeholder. Remove the card/list navigation from the main screen.
- `App::update` gains the `FocusMeta` parameter (pass `None` for now). Model diffing for blips.
- Update `sim` and its tests to the new layout. New keyframes: overflight (quiet, approaching,
  overhead, after), busy (30 aircraft), edge (no position, long callsign, no track, stale),
  plus synthetic snapshots for each range step and for tracks 0/45/90/180/270.
- *Verify:*
  - Host tests green in `adsb-app` and `sim`; `trunk build --release` in `sim/` still builds.
  - PNG checks by test (counts of distinct colours, the focus blip pixel colour at the projected
    position, the range label text in the `Ui` struct) and by reading the PNGs: radar on the
    left with rings and labels, blips turned to their track, focus highlighted and the same
    callsign in the panel, "—" in all metadata fields, no overlapping text, status bar intact.
  - `frame_hash` parity test (`sim/tests/hash.rs`) updated and green.

### RD.6 [code+hw] New layout on the board

- Firmware: pass the new snapshot fields; nothing else should be needed (shared code).
- *Verify:*
  - Bench with `ADSB_DEMO=busy` first: expectation "radar with about 30 blips, range ring
    labels, focus ring, panel with the focus callsign and '—' fields". Photo compared with the
    sim `busy` PNG at the same scenario time (log the scenario time).
  - Then the live build: expectation "radar with live blips, range 10–100 km, status bar green".
    Log `radar: range=… blips=… focus=<callsign>`; the panel callsign in the photo equals the
    logged focus.
  - Render times logged and recorded; check against the targets. Gate K1 again (≥ 10 KB
    internal free over 10 min).

### RD.7 [code] Metadata model, parsing, photo decode, panel with fixtures (parent 5.1 extended)

- `adsb-core::meta`: `FocusMeta`, adsbdb parser (combined, aircraft-only, callsign-only,
  not-found string), planespotters parser, LRU cache with TTLs, lookup policy (focus + 2
  prefetch, photo for focus only, 3 s dwell, `?` callsign skip), request rate limiter (pure,
  time passed in).
- `adsb-core` feature `photo`: `zune-jpeg` decode + scale/crop to 336×224 RGB8; size cap.
- `adsb-app`: show `FocusMeta` (text and photo, credit line). Widen `glyph-seed`; add a
  diacritics render test (parent carry-forward).
- `sim`: fixtures for scenario callsigns (fictitious airline/route/registration) and a synthetic
  photo; `Sim` passes them to `App`.
- *Verify:*
  - Host tests: parsers on hand-written fixtures (found, not found, missing optional fields,
    malformed, oversized, empty `photos` array); cache hit/miss/expiry/eviction; policy picks the
    expected ICAOs from a synthetic 50-aircraft snapshot; JPEG round trip (synthetic image
    encoded in the test, decoded, scaled; mean colour error per channel < 8; baseline and
    progressive if the encoder can make both); oversized input rejected.
  - `cargo check` of `adsb-core --features photo` for `xtensa-esp32s3-none-elf` and for
    `wasm32-unknown-unknown` (proves the `no_std` build of `zune-jpeg` [VERIFY]).
  - PNG keyframes "overhead with metadata + photo", "metadata without photo", "not found" (all
    "—"), "long airline name" (elided), "city with diacritics". Read the PNGs: photo at the top
    of the panel, credit line under it, all fields readable, nothing overlaps.

### RD.8 [code+hw] TLS spike with gate (parent 5.2, three hosts)

- SNTP (parent 5.2) and DNS (embassy-net `dns` feature).
- embedded-tls 0.19 through reqwless 0.14 with pinned roots (committed PEM/DER under
  `firmware/certs/`, one per host as needed). Fetch: `api.adsbdb.com/v0/callsign/KLM1023`,
  `api.planespotters.net/pub/photos/hex/<an ICAO24 from the KLM1023 aircraft or a fixed public
  example>`, and one image URL from that answer (first 1 KB only).
  [VERIFY which CA issues each host's certificate and whether embedded-tls can verify it.]
- TLS record buffers in PSRAM. Log peak heap use (internal and PSRAM).
- *Verify (bench log only, no photo needed):* `sntp: time=` plausible; per host
  `tls: ok host=<name> stack=embedded-tls verify=on`; the body parses with RD.7 parsers
  (`meta: parse ok`); negative test build (feature `tls-wrong-ca`) → every handshake fails with
  a certificate error. Internal heap ≥ 10 KB during the handshake.
- **Gate:** if handshake, verification or memory fails for any host, repeat with mbedtls-rs 0.3
  [VERIFY build model and S3 support]. If one stack works for all hosts, record it and go on. If
  **neither** verifies all hosts: do **not** ship unverified TLS; go to RD.11 (fallback) and
  report. A partial result (for example adsbdb OK, image CDN not) is recorded; then RD.9 runs
  and RD.10 waits for RD.11 or the user.

### RD.9 [code+hw] `meta_task`: text metadata on the board (parent 5.3)

- Queue fed by the lookup policy from `ui_task` (channel, deduplicated). One HTTPS connection at
  a time. Results into the cache under the same mutex as the table (short critical sections).
  `ui_task` builds `FocusMeta` from the cache.
- Logs at `info`: `meta: req icao=… kind=aircraft|route`, `meta: ok icao=… airline=… route=XXX-YYY`,
  `meta: not found`, `meta: error <kind>`, `meta: backoff`. No coordinates.
- *Verify:* bench log shows at least 3 `meta: ok` within 10 min, no repeated request for a
  cached ICAO within its TTL, no request for aircraft outside the policy, no more than 1
  request/s. Photo: the panel shows airline, type and `ORIG → DEST` for the focus aircraft (or
  "—" where the log says not found). Internal heap gate holds. No panic over 15 min.

### RD.10 [code+hw] Photo on the board

- `meta_task` fetches the planespotters JSON, then the image (cap 96 KB, PSRAM), decodes and
  scales it (RD.7 code), stores it in the photo cache, and signals `ui_task`. While it decodes,
  the UI keeps the old content. Log `photo: ok icao=… bytes=… decode_ms=… scale_ms=…` or
  `photo: none|too big|error`.
- Credit line with the photographer's name.
- *Verify:* bench log shows `photo: ok` for the focus aircraft; `decode_ms` recorded; feed
  frames/s does not drop to 0 during a decode (check the next `feed:` line) and the TCP feed does
  not reconnect because of it. Snapshot: a photo is visible at the top of the panel with the
  credit line under it, colours plausible (not swapped R/B), no stripes or shift. Gate K1 holds.
  If decode takes > 1.5 s, record it (K9) and reduce to the small thumbnail (200×133, shown at
  that size).

### RD.11 [code + user hw] Fallback metadata proxy (gated: only if RD.8 fails)

- `sim-bridge/src/meta.rs` (upstream client, validation, rate limit, cache) and the binary
  `meta-proxy` as in [Fallback proxy](#fallback-proxy-rd11-gated). Firmware: plain-HTTP client
  path when `ADSB_META_PROXY` is set; reads the RGB565 photo straight into the photo cache.
- *Verify (unattended):* host unit tests (validation rejects bad ICAO/callsign, fixed upstream,
  rate limit, 404/429 pass-through, RGB565 header and size), integration test with a fake
  upstream (a local HTTP server serving the fixtures and a synthetic JPEG). Firmware builds with
  `ADSB_META_PROXY` set.
- *Verify (needs the user, report it):* the user starts `meta-proxy` on a LAN-reachable host;
  then the RD.9/RD.10 bench verifies run against it. The orchestrator cannot start it (no
  process start on the Mac, no LAN port publish from the container).

### RD.12 [code, optional] Live metadata in the sim (parent S.8, extended)

Only if time is left after RD.10. Bridge endpoints `/local/meta/<icao>?callsign=` and
`/local/photo/<icao>` (JPEG pass-through; the wasm decodes it with the RD.7 code), using the
RD.11 `meta` module, behind the existing Host/Origin guard.
- *Verify:* bridge unit tests (guard on the new routes, validation, rate limit); `trunk build`;
  manual browser check deferred to the user.

### RD.13 [hw] Overnight soak (part of parent 4.6)

- Run the final live build. Every 30 min for as long as the night allows (target 6 h): one
  `bench log` excerpt (heap, `feed:`, `meta:`, `restart:` lines) and one snapshot.
- *Verify:* no reboot or panic (`--fail-on` default and no new `ESP-ROM:` boot banner),
  internal and PSRAM free heap without a falling trend (max − min of the hourly minimum < 2 KB
  internal), the display steady in every photo, metadata lookups continue. Record a short table
  (time, heap, frames/s, lookups) in the revision log; no callsigns or photos in the doc.

### RD.14.1 [code+hw+user] Measure the pixel aspect

- Firmware feature `diag-aspect` (like `diag-pattern`: drawn at boot instead of the UI, UI tasks
  not started): border, left 300×300 square + circle, right `round(300/PIXEL_ASPECT)`×300
  rectangle + ellipse, the constant as text (or as a row of 1-px ticks if text is not available
  outside Slint; the left/right shapes are enough). Pure drawing function in `firmware/src/display.rs`
  next to `draw_pattern`; log `diag-aspect: pixel_aspect=1.076`.
- *Verify:*
  - Bench: expectation "white border; left shape wider than tall; right shape round". `bench
    flash … --until 'diag-aspect:' --timeout 60`, `bench snap`. Measure the border rectangle ratio
    in the photo (method and limits in [Radar pixel aspect](#radar-pixel-aspect-rd14)). Record
    the ratio. ≤ 1.70: stop RD.14 and report (hypothesis wrong).
  - [user]: ruler W and H of the border line in mm; compute the constant; commit it if it differs
    by more than 1 % from 1.076. Record W, H and the result in the revision log. Until the user
    answers, RD.14.2–RD.14.4 run with 1.076.

### RD.14.2 [code] Aspect in the radar logic (`adsb-core::radar`)

- `PIXEL_ASPECT` constant; internal functions take the aspect as a parameter so tests can run
  with 1.0. `project` divides the x offset; `in_scope(snap, index, range)` (shared with `blips`);
  `sprite` maps `dx` to `dx·aspect`; `background(radius_ring_px) -> Vec<u8>` (448×448 RGB8,
  colours and AA as in the design); `FOCUS_W_PX = 28`.
- *Verify (host):*
  - Projection: 25 km at 0/90/180/270° → (224, 16), (417, 224), (224, 432), (31, 224) ±1 px; with
    aspect 1.0 the old values (existing test).
  - All existing radar tests pass unchanged with aspect 1.0 (old sprite and blip outputs are
    bit-identical).
  - Background: the ring colour is at (224 ± 193, 224) and (224, 224 ± 208) ±1 px; the pixel at
    (224 + 208, 224) (the old circle) is background colour; the image is mirror-symmetric left/right
    and top/bottom; with and without the radius ring; size is 448·448·3.
  - Sprite: the 90° sprite's covered width is about `19/aspect` px (±1); the 0° sprite is still
    nose-up and symmetric.
  - Host time of `background` recorded (the board time comes in RD.14.4).

### RD.14.3 [code] Elliptic rings in `adsb-app` and the sim

- `app.slint`: remove the two cross-hair `Rectangle`s and the three `Ring`s; add `in property
  <image> radar-bg` as the first child of the radar box; focus mark 28×30, `border-radius` 14.
  `App`: build the background on the first update and when `radius_ring_px` changes; keep the
  `Rc` so an unchanged image is not set again.
- Sim: canvas CSS with the physical aspect (`index.html`/`web.rs`); `snapshots.rs` writes
  `*-physical.png` for the radar keyframes.
- *Verify:*
  - Host tests green in `adsb-core`, `adsb-app`, `sim`; `trunk build --release` in `sim/`.
  - PNG check by test: in `7-busy.png` (device pixels), scan the centre row and the centre column
    for the outer-ring colour: half width / half height = 1/`PIXEL_ASPECT` ± 0.01. In
    `7-busy-physical.png` the same ratio is 1.00 ± 0.02.
  - Read the PNGs: rings smooth (no steps or gaps), blips on the rings where expected, labels not
    cut, the N mark and range labels in place, the focus mark around the focus blip.
  - `frame_hash` references updated (all radar keyframes change) and the parity test green.
  - Render time on the host recorded (full and the busy 1 Hz tick).

### RD.14.4 [code+hw+user] Round radar on the board

- Firmware: log `radar: bg_ms=… bytes=…` when the background is built. Nothing else should be
  needed (shared code).
- *Verify:*
  - Bench, busy demo: expectation "radar rings look round in the photo (allow the camera angle),
    blips match the sim busy keyframe at the same scenario time". Compare with
    `7-busy-physical.png` rather than `7-busy.png`.
  - **Render gate (K16):** busy demo `ui: render partial_ms` ≤ 100 ms typical (was 51–76 ms),
    full ≤ 400 ms; live typical ≤ 80 ms; `radar: bg_ms` ≤ 100 ms. If the busy partial is over
    100 ms or the full is over 400 ms, switch to O7d (ring tiles) in this step, then measure again.
  - **K4 re-check (K17):** the K4 burst metric (busy demo, 8 s burst at scenario ~100 s, same
    counter as the K4 fix entry): ≤ 1 glitched per 100.
  - Gate K1 (internal free ≥ 10 KB over 10 min), PSRAM drop about 0.6 MB once, no trend.
  - [user]: "the radar now looks round" (record the answer).

### RD.15.1 [code] Selection policy and hit-test (`adsb-core`)

- New `select` module: `Selection`, `SelectView`, `Target`, `TapOutcome`, `SelectEvent`, rules as
  in [Touch selection](#touch-selection-rd15). `FeedPipeline` owns `RangeState` and `Selection`:
  `snapshot()` = age → table snapshot → `select_range` → `Selection::resolve` (may set
  `snap.focus`, sets `snap.range_km` and `snap.selection`); `tap()`, `take_select_event()`.
  `App` uses `snap.range_km` and drops its `RangeState`. `radar::Blip.icao`, `radar::hit`,
  label constants. Update the hand-built `Snapshot`s in tests.
- *Verify (host, virtual clock):*
  - Default: focus = tracker focus (existing tracker tests unchanged).
  - Tap on X (not focus) at t → `snap.focus` = X until `t + timeout − 1`; at `t + timeout` back to
    the tracker focus and event `Auto{Timeout}`.
  - Renew moves `until`; tap on the tracker focus → `AlreadySelected` without an override, and
    `Auto{User}` with one; panel tap ends an override (`Auto{User}`) and is a no-op without one;
    miss and status taps change nothing.
  - Scope: X beyond the range, X stale (no position), X removed, X at rank 48 → `Auto{Scope}` at
    the next snapshot; a range step-down that excludes X → `Scope`; `in_scope` and `blips` agree
    for every index of a 60-aircraft snapshot.
  - Lookup policy follows: `Policy::plan` on a snapshot with an override plans X first.
  - Hit: point at a blip centre → that ICAO; 27 px above → hit, 29 px above → miss; for an
    unlabelled blip, 27 px sideways → miss because of the aspect weighting
    (`27·1.076 = 29.1 > 28`), 26 px sideways → hit; two blips within the radius → the nearer;
    point on a label rectangle → that blip; flipped label (x > 340) → hit on the left side;
    empty → miss.
  - Pipeline integration: busy scenario, tap the 5th blip at 60 s, run to the timeout: the
    expected event sequence.

### RD.15.2 [code] GT911 parse and tap detector (`adsb-core::touch`)

- `gt911::parse_status`, `parse_point`, `parse_info` (product ID, firmware, resolution) on byte
  slices; `map(raw, res) -> (x, y)` with calibration and clamp; `TapDetector`.
- *Verify (host):* byte fixtures written from the register map (not from a device dump): status
  with/without bit 7, 0/1/5 points, point coordinates LE, resolution 800×480 → identity,
  1024×600 → scaled, out-of-range clamped. Detector: clean tap (3 samples) → 1 tap at the mean;
  1-sample tap → tap; one dropped sample inside a touch → still 1 tap; drag 30 px → none; 1.2 s
  press → none; two taps 100 ms apart → the second is ignored, 300 ms apart → 2 taps; noise sample
  pattern down-up-down within 80 ms → 1 tap.

### RD.15.3 [code] Manual selection in `adsb-app` and the sim

- `App::hit(x, y) -> Target`; `Ui` fields `manual`, `manual_left`; `Badge::Manual`; Slint
  `manual-select` and `badge-text`; cyan focus mark; `MANUAL 52 s` badge.
- Sim: `Sim::tap(x, y)` (direct) and mouse down/up through the `TapDetector`; `Sim::pointer` keeps
  the Slint dispatch. Default timeout in the sim: `adsb_core::select::DEFAULT_TIMEOUT_MS` (60 s).
- `adsb_core::touch::script` with the busy-demo script; new test `sim/tests/touch.rs` runs the
  script through the same detector and writes `sim/target/ui-snapshots/touch-script.log` (one
  `select:`/`touch:` line per event, with scenario time) — the expected board log of RD.15.6.
- *Verify:*
  - Host tests: script run gives exactly the 6 expected outcomes in order (manual, renew, miss,
    user, scope, timeout) with a 20 s timeout; label geometry test (K20).
  - PNG keyframes: `busy-manual` (cyan mark on the tapped blip, panel shows its callsign and
    `MANUAL 60 s`), `busy-manual-50s` (countdown text), `busy-after-panel-tap` (yellow mark on the
    closest, NEAREST/OVERHEAD badge). Read the PNGs: the panel callsign equals the marked blip's
    label; the badge text fits.
  - Browser (`trunk serve`, deferred to the user if no browser is available): a click on a blip
    selects it.

### RD.15.4 [code] Build variable `ADSB_SELECT_TIMEOUT_S`

- `build_config.rs`: optional, `number("ADSB_SELECT_TIMEOUT_S", "60", 10, 3600)`, integer only;
  writes `SELECT_TIMEOUT_MS: u64`; listed in the rerun vars. `.env.example` line
  `# ADSB_SELECT_TIMEOUT_S=60` (if the deny rule still blocks edits to that file, report the line
  as in RD.8).
- *Verify (host tests in `build_config.rs`):* default 60 → 60000; 10 and 3600 accepted; 9, 3601,
  `1.5`, `abc`, empty → error naming the variable and the rule, without the value (marker test,
  as the other vars). Firmware builds with and without the variable.

### RD.15.5 [code+hw] GT911 driver and `touch_task` (no finger)

- `firmware/src/gt911.rs` (reset, probe 0x5D/0x14, info, poll, clear); `touch_task` (owns
  `Ch422g`, GPIO4); `Ch422g::bus()`; `TAP` signal; `ui_task` waits on ticker or `TAP`, and on a
  tap calls `App::hit` → `pipeline.tap` → logs → `tick`. Logs as in the design.
- *Verify:*
  - Bench (live build and busy demo): markers `touch: gt911 ok addr=0x5d id=911` (60 s), then
    `touch: stats polls=` with polls 1450–1550 per 60 s (25 Hz ± 3 %), `err=0`, `taps=0`.
    Record `fw=` and `res=`. If `res` is not 800×480, record it (mapping scales).
  - No regression: `ui: render` times as in RD.14.4, `restart: frames/s` 29–30, no panic over
    10 min, gate K1 (record `.bss` growth from the ELF and `internal_min`).
  - K4 burst metric again with touch polling on: ≤ 1 per 100 (K17).
  - Negative path: a build with a wrong GT911 address constant (temporary, not committed) logs
    `touch: gt911 missing` and the UI keeps running.

### RD.15.6 [code+hw] Scripted taps on the board (`touch-script`)

- Feature `touch-script` (needs `demo` for the deterministic script; a live variant uses only
  `TapRank`/`TapPanel`). `ui_task` publishes the shown blips for the script.
- *Verify:*
  - Busy demo build with `ADSB_SELECT_TIMEOUT_S=20`: the `select:`/`touch: script` lines equal
    `touch-script.log` from RD.15.3 (same ICAOs, same outcomes, same order; times ± 1 s).
    Snapshots right after step 1 and step 4: cyan mark + `MANUAL` badge + the selected callsign
    in the panel; then yellow mark on the closest.
  - Live build with the live script (`TapRank(2)` at 60 s, `TapPanel` at 120 s, default timeout):
    after the tap, `meta: req icao=<selected>` within 5 s (unless cached; then the panel shows the
    metadata at once) and, after the 3 s dwell, `photo: ok|none icao=<selected>`; after the panel
    tap the panel shows the closest again (`ui: focus` / `radar: focus=` lines). No more than 1
    request/s.
  - Response time: log the time from `touch: tap` to the end of the following `ui: render`;
    record it (target ≤ 200 ms outside TLS handshakes).

### RD.15.7 [user] Finger check

Needs the user at the board; the orchestrator reads the log.

- Live build (no script). The user:
  1. taps near the four corners of the screen (about 10 px from each edge) and the centre; the log
     `touch: tap x=… y=…` values must be within ±15 px of the aimed points, with no axis swap or
     mirror. If not: set the calibration constant and repeat.
  2. taps a blip that is not the closest → it gets the cyan mark, the panel shows it with
     `MANUAL 60 s` counting down;
  3. taps the info panel → back to the closest;
  4. taps a blip and waits 60 s → back to the closest by itself;
  5. tries a few taps on small blips near the ring edges and in a crowded area, and says if the
     target size feels right (`TOUCH_RADIUS_PX`).
- Record the user's answers and the log excerpt (counts only, no positions of aircraft) in the
  revision log.

---

## Risks

| ID | Risk | Likelihood / impact | Mitigation / what to verify |
|----|------|---------------------|-----------------------------|
| K1 | **Internal SRAM runs out**: Slint, sockets, TLS or JSON allocate from the internal heap (it is the first region), and WiFi fails later at runtime | High / high | Memory placement decision in RD.2 [VERIFY esp-alloc region order and esp-radio caps use]. Gate "internal free ≥ 10 KB" in RD.2, RD.3, RD.6, RD.8–RD.10, trend in RD.13. Large buffers by explicit `alloc_caps(External)`. |
| K2 | **Device TLS fails** for one or more of the three hosts (cipher, certificate algorithm, chain, SNI, memory) | Medium / medium | Gate in RD.8: embedded-tls → mbedtls-rs → fallback proxy RD.11. Never unverified TLS. Partial results still allow text metadata. |
| K3 | **Fallback cannot be verified unattended** (no LAN listener can be started by the orchestrator) | High (if K2 happens) / medium | RD.11 code and host tests run overnight; device check is listed for the user. The morning report says it clearly. |
| K4 | **Drift or flicker** under WiFi + render PSRAM load (parent R2) on the direct PSRAM path | Medium / high | `diag-border` drift check in RD.3, photos in every hw step, soak RD.13. If it fails: stop display-load steps and report (parent M3/M4 is a user decision). |
| K5 | **Render too slow** on Xtensa (radar repaint, RGB8 photo from PSRAM, cache pollution) | Medium / medium | Partial repaint, diffed models, photo pre-scaled to the exact size, measurement in RD.2/RD.6/RD.10, fallback 0.5 Hz radar update. |
| K6 | **Slint software-renderer limits** (rotation, Path, runtime RGB8 images) [VERIFY] | Medium / medium | O1c needs only rectangles, images and text. If runtime RGB8 images are a problem, convert the photo to RGBA8 or to an embedded-format image. |
| K7 | **API shapes, terms and limits** (adsbdb combined query, planespotters JSON, attribution rule, User-Agent, rate limits, image CDN host) [VERIFY] | Medium / medium | Parsers tolerant to missing fields; "—" on failure; focus-only lookups with 1 req/s and 429 backoff; credit line; stop the photo part if the terms forbid this use. |
| K8 | **Secrets leak** (WiFi key via `cargo -v`, logs, agent transcripts, commits) | Low–medium / high | O3b generated config; no `-v`; no value echo in `build.rs` errors (tested); no SSID/key/host/coordinates in device logs; count-only leak checks in RD.1. |
| K9 | **JPEG decode time/memory on Xtensa** blocks core 0 (feed stalls, TCP drop) or fails on progressive images | Medium / medium | 96 KB cap; 8 KB feed rx buffer; measure in RD.10; fall back to the small thumbnail; core-1 decode is deferred (would need a second executor). |
| K10 | **Location doxxing** through bench photos and logs of the radar (parent R13) | Medium / high | Bench output stays in `.bench/`; nothing from it goes into docs or commits; only fictitious scenario PNGs may be committed. |
| K11 | **Focus flicker** causes a lookup storm and a jumping panel | Medium / low | Hysteresis (20 % and 300 m), 3 s dwell before a photo fetch, cache, rate limit; tests in RD.4/RD.7. |
| K12 | **App image too large** for the app partition (Slint + fonts + TLS + zune-jpeg + serde_json) [VERIFY the current partition table] | Medium / medium | Check the ELF section sizes after RD.2, RD.8, RD.10; if needed add a custom partition table for the 16 MB flash (one factory app of up to about 6 MB). |
| K13 | **Unattended run stalls** (bench down, camera moved, moiré makes photos hard to judge, quota) | Medium / medium | Stop rules in Plan; independent steps continue; visual INCONCLUSIVE is reported, not guessed; serial-log markers carry most verifies. |
| K14 | **Fallback proxy exposure on the LAN** | Low / medium | Fixed upstreams, input validation, optional client allow-list, read-only, no location data passes through it, runs only when the user starts it. |
| K15 | **`PIXEL_ASPECT` wrong**: the panel's active area is not 154.08 × 85.92 mm, or the user's impression came partly from viewing angle [VERIFY] | Medium / low | RD.14.1: bench plausibility (border ratio 1.79 vs 1.67) and the user's ruler (decisive). The constant is one line; the measured value is committed. Stop RD.14 if the bench says square pixels. |
| K16 | **Background image too slow** (O7c): every repainted radar pixel reads RGB8 from PSRAM instead of a solid fill; busy partial was already 51–76 ms | Medium / medium | Render gate in RD.14.4 (busy partial ≤ 100 ms, full ≤ 400 ms); fallback O7d ring tiles inside the step; last resort the 0.5 Hz radar rule. |
| K17 | **K4 comes back**: 12 MHz is a threshold (13 MHz still glitched 21/100). More PSRAM reads (background image) and more flash-resident code per second (touch polling, extra renders) can push it over | Medium / high | K4 burst metric in RD.14.4 and RD.15.5 (≤ 1 per 100, busy demo). If it fails: try 11 MHz (parent M3; 27 Hz refresh) and report; touch polling can drop to 10 Hz or use INT (O9a). |
| K18 | **GT911 details wrong** (register map, address after reset, reset timing through the CH422G, held-finger report behaviour, resolution not 800×480) [VERIFY] | Medium / medium | Probe 0x5D then 0x14; log id, firmware and resolution; scale by the reported resolution; tap detector tolerates missing reports; no panic, UI works without touch; RD.15.5 log check and RD.15.7 corners. |
| K19 | **Taps lost or late during executor stalls** (TLS handshake up to about 4 s, photo decode about 0.5 s): the user taps and nothing happens | High (at times) / low | Extra render at once on a tap when the executor runs; the stall windows are short and rare (keep-alive, cache). Documented for the user. A real fix is the interrupt-executor idea from RD.9 (not planned). |
| K20 | **Hit-test and drawing disagree**: label geometry is in Rust and in `app.slint`; blip positions differ between what was shown and the current snapshot | Low / medium | Hit-test only on the **shown** blips; label constants in one Rust place with a render test that checks the Slint label lies inside the Rust rectangle. |
| K21 | **Internal SRAM**: the `touch_task` future (large esp-hal async I2C futures) goes to internal `.bss` | Medium / medium | Measure `.bss` growth and `internal_min` in RD.15.5; K1 gate ≥ 10 KB. If tight, make the I2C calls through one small non-inlined function, or lower the future size by reading into a static buffer. |
| K22 | **Selection UX**: near misses on a crowded radar, the wrong of two close blips, the timeout too short or long | Medium / low | 28 px aspect-weighted radius (about 10 mm target), nearest wins, label hit area, misses keep the selection, panel tap to go back, timeout is a build variable; the user judges in RD.15.7. |
| K23 | **The finger path cannot be verified unattended** (bench cannot touch) | High / medium | `touch-script` covers detector → hit → selection → panel → lookups on the board and matches the sim log; GT911 init/poll is checked by log; only the finger coordinates and feel are a [user] step (RD.15.7). |
| K24 | **Snapshot/App API change** (range moves into `FeedPipeline`, new `Snapshot` fields) breaks tests or changes the sim's reset behaviour | Low / low | `Sim::reset` and `start_live` already make a new pipeline (so range and selection reset with it); all hand-built `Snapshot`s updated in RD.15.1; `frame_hash` parity test. |

**Deliberately deferred:** touch (1.6, 4.4), list and detail views (4.5), the "just passed" text
state, label collision avoidance on the radar, a map background, decoding on core 1, photo
sources other than planespotters (for example adsbdb `url_photo`), OTA, the flightradar24
cross-check and the AP/feed outage tests (need a human), the open sim-live distance issue of the
parent (unrelated to this feature). *(Revision 3: parent 1.6 and the "manual override" part of
4.4 are now RD.15. Still deferred from revision 3: aspect correction of the photo and of text,
GT911 INT mode (O9a), gestures (drag, long press), multi-touch, backlight control by touch, a
selection timeout set at runtime, the interrupt executor against K19.)*

**Unverified (summary):** Slint 1.18 software renderer: `Path`, rotation, runtime RGB8 images;
`zune-jpeg` `no_std` build and Xtensa speed; `jpeg-encoder` as dev-dependency; adsbdb combined
query and aircraft schema; planespotters JSON shape, image CDN host, terms and User-Agent rule;
the CAs of all three hosts and embedded-tls support for them; mbedtls-rs build model; esp-alloc
region order and esp-radio internal-caps use; cargo verbose output of `rustc-env`; the current
app partition size. *Revision 3:* the panel active area (154.08 × 85.92 mm assumed) and so
`PIXEL_ASPECT`; the GT911 register map, held-finger reporting, INT configuration (0x804D) and
configured resolution; the esp-hal 1.2 API to switch GPIO4 from output to input; the board render
cost of a 448×448 RGB8 image; `.bss` size of the `touch_task` future. (Verified in revision 3:
the vendored Slint 1.18.1 software renderer draws no `Path` in line-by-line mode and supports no
transformations.)

---

## Revision log

- 2026-10-23, **revision 1**: initial design. Records RD-D1–RD-D7 (user decisions, resolved),
  ADR-003 (radar via pre-rotated sprites, direct HTTPS with gated fallback proxy, `zune-jpeg`,
  generated config file, `serde_json`, interleaved step order, focus rule), plan RD.0–RD.13 and
  risks K1–K14. Changes against the parent (to be mirrored in the parent's revision log when RD.1
  lands): parent 4.3 overhead card **replaced** by this layout; parent config names
  `ADSB_WIFI_SSID`/`ADSB_WIFI_PASSWORD` **replaced** by `WIFI_SSID`/`WIFI_KEY`; parent
  `cargo:rustc-env` approach **replaced** by a generated `config.rs`; parent 5.1
  `serde-json-core` **replaced** by `serde_json` (alloc); parent lookup policy (in radius,
  approaching, nearest 5) **replaced** by focus + 2 prefetch for this layout.

- 2026-10-23, **RD.0 result** (bench, head 75b721f): `fw-D-vendored` and `fw-D-stock` both PASS:
  bar pattern, border on all four edges, no shift between photos 10–30 s apart,
  `restart: frames/s=39`–40, `isr_max_us` 13 (vendored, first second) / 6 (stock). Decision rule:
  stock passes, so `[patch.crates-io]` is commented out in `firmware/Cargo.toml`; the vendor
  directory stays. Also found: build-script binaries under `*/target/` from 2026-10-21 had lost
  their execute bit (host file-mode reset); fixed with `chmod u+x`, no clean needed.

- 2026-10-23, **RD.2 result: code and numbers done, display gate FAILED (flicker)** (bench, base
  ffacf82 + the RD.2 commit; demo build `ADSB_DEMO=overflight --features demo`).
  - Memory placement (K1) decided: **PSRAM is the first heap region, the internal region is added
    second** (plain allocs, Slint, pipeline buffers go to PSRAM; esp-radio uses its own
    `InternalMemory` allocator by capability [verified in esp-radio `compat/malloc.rs`]; the
    framebuffer uses `alloc_caps(External)` as before). Measured: after `WifiController::new`
    `internal_free=29892 psram_free=7617616`; steady state with UI + demo feed for 300 s
    `internal_free=29892` (constant), `psram_free=7607408`. Gate K1 (>= 10 KB internal): pass for
    now; WiFi is only initialised, not connected, until RD.3, so re-check there.
  - Render times (`ui: render full_ms=50`): full redraw 50 ms (first frame, 800x480); 1 Hz partial
    update 5-21 ms (clock only 5 ms / 18 rows; overhead card text 11-21 ms / 150-190 rows); worst
    partial in the 460 s scenario 50 ms (card change). All far under the targets (400/80/250 ms);
    no need for 0.5 Hz. ELF sections: `.text` 650 KB, `.rodata` 726 KB (K12: fits, image loads).
  - Markers in the 300 s run: `ui: first frame`, `ui: card=` NoData, Quiet, Approaching, Overhead,
    JustPassed; no panic, no allocation failure; `restart: frames/s=39` throughout, isr_max_us <= 22.
    Photos show the status bar and card with changing values (callsign, FL, speed, distance).
  - **Flicker (K4/R2), root cause investigation.** With the UI running, a few percent of photos
    show a corrupted frame (one scanline garbled, all later rows shifted sideways; severity grows
    with the amount rendered per tick, worst on the overhead card). Method: 30 to 40 photos per
    variant, automatic count of photos whose static lower panel area differs from the median
    (build variants through a temporary `UI_EXP` flag, not committed). Results (glitched photos):
    UI as designed 4/50 (clock only), 5/20 (demo); UI task idle (no render) 0/30; Slint render but
    **no write and no writeback to the framebuffer** 3/30 and 4/29 (so not the framebuffer writes,
    not the cache writeback, not a visible intermediate state); busy-loop CPU only 0/30; reading
    192 KB of **PSRAM** per second 0/30; read-modify-write plus writeback of 160 rows (256 KB) of
    the live framebuffer per second 0/30 (identical pixels, heavy PSRAM traffic); reading 192 KB
    of **flash rodata** per second (no render) 2/30. Conclusion: PSRAM contention from CPU data
    traffic is **not** the cause; the stall comes from **flash cache refills** (code and font data
    of Slint run from flash at DIO 40 MHz, boot log `Boot SPI Speed: 40MHz, SPI Mode: DIO`) that
    block the scanout DMA's PSRAM reads on the shared external-memory path for several us.
    Tried and not enough: only copying rows inside the blank period (rows part is not the main
    flash user, 6/30 worse), restart ISR made flash-free (`now_us` was a flash function called 6
    times inside the ISR; inlined, no measurable change 0/40 then 7/40).
    Options (user decision, per K4 and parent M3/M4): (1) flash at 80 MHz (and QIO) through the
    host's espflash settings, about 4x shorter refills; the bench service cannot pass flash flags
    and its host config is out of scope here; (2) run hot code and font data from RAM or from PSRAM
    (XIP from PSRAM style), large linker work; (3) lower the pixel clock (M3) to give the DMA more
    slack; (4) the SRAM bounce-buffer path (M2) or a palette framebuffer in SRAM (M4).

- 2026-10-23, **RD.2 display gate (orchestrator, after 96e37ea)**: K4 glitches confirmed with a 30 fps
  burst of the bench stream (frames shifted sideways by up to about half a line width, while the card
  updates). Tried: flash 80 MHz **QIO** via repo-root `espflash.toml` → boot loop (`TG0WDT_SYS_RST`
  in the 2nd-stage bootloader), so QIO is not usable on this board as is; flash **80 MHz DIO** → boots
  (`Boot SPI Speed: 80MHz`, `SPI Mode: DIO`), UI runs, but still 16/111 distinct frames flagged in a
  6 s burst, worst one clearly shifted → not fixed. `--features bounce` with the UI: no refills, no
  first UI frame (path not integrated with the UI; only built in RD.2). Kept: `espflash.toml` at
  80 MHz DIO (repo root and `firmware/`). **Gate parked for the user** (options: bounce path
  integration (M2), lower PCLK (M3), palette framebuffer (M4), code/rodata from PSRAM). RD.4, RD.5,
  RD.7 continue meanwhile (no dependency on RD.2).

- 2026-10-23, **RD.4 QA PASS** (2ab2847), done ahead of RD.2/RD.3 per the dependency table. Not yet
  recorded in beads (`fs claim` refuses: `add-step` chained the steps linearly, RD.2 waits on the
  user); record it with `fs` once RD.2 is unblocked. QA notes for RD.5: `Snapshot.focus` and
  `primary` are tracked separately (don't rely on equality); the focus aircraft may have no blip
  (outside range or past 48), so the radar must cope with "no focus blip".

- 2026-10-23, **revision 2 (design change, decided by the orchestrator because the user is away):
  vendored Slint renderer fix** (radar-ui RD.5). The new radar layout hits an off-by-one in
  `i-slint-renderer-software` 1.18.1 (`scene.rs`, path that skips lines: `<` should be `<=`).
  When the dirty region has gaps, an expired item stays active: debug builds panic on an assertion,
  release builds draw wrong frames (10 of 880 per-second frames in busy/edge/overflight differed
  from the fixed renderer). 1.18.1 is the latest release. Decision: option (a), vendor the crate
  with that one-line fix in `vendor/i-slint-renderer-software/` (see its `VENDORED.md`) and use it
  through `[patch.crates-io]` in `sim/`, `adsb-app/` and `firmware/`; drop it when upstream fixes
  it. Option (b), full repaint on every update (`RepaintBufferType::NewBuffer`), rejected: it adds
  board load while K4 is open. The per-second sweep in `sim/tests/snapshots.rs` fails in a debug
  build without the patch. RD.5 host numbers: full draw 0.95 ms, busy 1 Hz tick 0.37 ms average
  (host only, not the board).

- 2026-10-23, **RD.5 QA PASS** (2c55d18), ahead of RD.2/RD.3 (beads entry pending, as RD.4). Radar
  label overlap in crowded scenes is the deferred "label collision avoidance" (Risks/deferred);
  suggestion for later: draw the focus label last.

- 2026-10-23, **RD.7 done** (host only, no bench). Code: `adsb-core::meta` (FocusMeta, adsbdb and
  planespotters parsers, `Lru`, `Policy`, `RateLimiter`), `adsb-core::photo` (feature `photo`),
  panel rows, photo and credit in `adsb-app`, fixtures in `sim/src/fixtures.rs`.
  - **API facts verified live (2026-10-23, no raw responses committed):**
    - adsbdb `GET /v0/aircraft/<ICAO24>?callsign=<CS>` exists and returns
      `response.aircraft` (`type`, `icao_type`, `manufacturer`, `mode_s`, `registration`,
      `registered_owner*`, `url_photo`, `url_photo_thumbnail`) and `response.flightroute`
      (`callsign*`, `airline{name,icao,iata,...}`, `origin`/`destination{iata_code, icao_code,
      municipality, name, latitude, longitude, ...}`). Without the callsign only `aircraft`;
      `/v0/callsign/<CS>` only `flightroute`. Not found: `{"response":"unknown aircraft"}` or
      `"unknown callsign"`. **Catch:** with the combined query an unknown callsign turns the
      whole reply into `"unknown callsign"` although the aircraft is known (checked with a made-up
      callsign), so RD.9 must retry without the callsign. Same for an unknown aircraft with a
      known callsign (`"unknown aircraft"`, no route).
    - planespotters `GET /pub/photos/hex/<ICAO24>`: `{"photos":[{"id","thumbnail":{"src",
      "size"},"thumbnail_large":{"src","size"},"link","photographer"}]}`; `{"photos":[]}` when
      there is none. **A User-Agent with a contact URL or e-mail is required**; without it the API
      answers `{"error":"Server User-Agent strings must include a contact URL or email ..."}`.
      RD.9 must set one (the user decides which contact; not committed yet). Image host is
      `t.plnspttrs.net` (the parser accepts only `https://t.plnspttrs.net/`). The large thumbnail
      is **280 px high and 422-498 px wide** (not always 420x280), 35 KB, **progressive JPEG**
      (SOF2): the decoder must handle progressive (zune-jpeg does; a real photo decodes in
      about 2 ms on the host, release). Panel crop is centre crop to 3:2, then scale to 336x224.
  - Still `[VERIFY]`: the planespotters terms page (behind a Cloudflare challenge, not readable
    here) for the exact credit form; the panel shows "Photo (c) <photographer> / planespotters.net"
    (name cut at 22 chars so the source stays visible). Xtensa decode time and memory (K9, RD.10).
    HTTP 429 behaviour of both services (only the client-side pause is implemented).
  - `zune-jpeg`: pinned `=0.5.15` (newest stable; `0.5.16-rc*` are pre-releases),
    `default-features = false`. `cargo check -p adsb-core --features photo` passes for
    `xtensa-esp32s3-none-elf` (esp toolchain, `-Zbuild-std=core,alloc`) and `wasm32-unknown-unknown`:
    `no_std` + `alloc` confirmed. `serde` + `serde_json` (alloc) are now plain dependencies of
    `adsb-core` (the firmware links them anyway). `jpeg-encoder` 0.7 (no default features) is a
    dev-dependency and makes baseline and progressive test images.
  - Sim: scenario callsigns `ZQJ*` (jet, full metadata and synthetic photo), `ZQB*` (aircraft
    only, no photo), `ZQLONG99` (very long names) have fixtures; everything else shows dashes.
    `frame_hash` reference values changed for keyframes 1-5 (the panel has metadata now).

- 2026-10-23, **RD.7 QA PASS** (b92a66e, 110eaab, fix 525f343 after QA FAIL F1: 1-px-wide JPEG
  panicked in the scaler; fix loop 1/2). Beads entry pending (as RD.4). Open for the user:
  planespotters needs a User-Agent with a contact URL/e-mail (not committed; RD.9/RD.10 need it);
  credit wording still [VERIFY]; firmware .text 697 KB / .rodata 886 KB (K12, partition headroom
  unchecked).
- 2026-10-23, **orchestrator decision:** RD.3 (WiFi, feed) starts while RD.2's display gate waits on
  the user. The networking code does not depend on the K4 fix; RD.3's drift check is recorded as
  pending K4.

- 2026-10-23, **RD.3 done, drift check: no persistent drift; K4 glitches seen** (base 1173e4e + the RD.3
  commit, live build against the real feed, `diag-border` build for the drift run). Code:
  `firmware/src/net.rs` (`wifi_task`, `net_task`, `link_task`, `feed_task`), focus log and
  `diag-border` in `ui.rs`. WiFi is WPA2, backoff 1 s to 30 s; DHCP; feed socket rx 8 KB (PSRAM), connect
  timeout 10 s, idle timeout 60 s, backoff 1 s to 30 s, `reset_clock` on connect. In demo builds
  with `ADSB_DEMO` set the network tasks are not started (demo feed instead). A feed host that is not
  an IPv4 literal logs one warning and parks the task (DNS is RD.8).
  - Markers (`bench flash --until`): `wifi: connected ip=...`, `feed: connected`, `feed: frames/s=18`
    (first line, 10 s window) all matched in boot 2. Device log has no SSID, key, feed host or lat/lon
    (count-only grep of the 590 s session log: 0 for each).
  - Numbers, 590 s run: `feed: frames/s` 0-53 (this receiver sees little traffic, 3-4 aircraft at best),
    `crc_fail=0`, `reconnects=0`, no WARN lines, no panic. Aircraft count 1-4 (13x 2, 31x 3, 13x 4, 1x 1):
    3-128 is met in 44 of 58 lines, below 3 when the sky was quiet (traffic, not a fault; focus
    distance 26-54 km, under 450 km).
  - **Gate K1 pass:** `internal_free` 27404-29752 B (min 27404) with WiFi connected, feed and UI running,
    10 min. WiFi association did not move it (29892 B before, 29108 B after).
  - Drift check (6 photos at 0, 2, 4, 6, 8, 10 min): the white border is complete on all four edges and
    at the same camera position in every unglitched photo; 4 of 6 photos show a K4 glitch (rows shifted
    sideways, border cut inside the frame), no persistent shift or border loss. `restart: frames/s`:
    538x 39, 50x 40, 1x 41, 1x 48 (first second after boot). Verdict: no drift; K4 is
    more visible with live data: the radar redraws 450-480 rows (partial_ms 44-48) almost every second
    because blips move, versus 5-21 ms in the demo card UI. More K4 evidence for the user's decision
    (M2/M3/M4); not touched here.
  - Not verified unattended: AP power cycle and fr24feed stop/start (reconnect paths are untested on
    the board), flightradar24 cross-check.

- 2026-10-23, **RD.3 QA PASS** (d3dfea6), ahead of the RD.2 display gate (beads entry pending).
  QA observations for later: feed stats line only logs when data arrives; a WiFi drop reaches the
  feed task only via the 60 s idle timeout; wifi backoff resets on each association (a flapping
  AP retries every 1–2 s). Reconnect paths reviewed by reading only (AP power cycle and fr24feed
  stop/start need the user). Live radar redraws ~450 rows/s → K4 glitches much more frequent.

- 2026-10-23, **RD.8 result: embedded-tls verifies all three hosts, gate PASSED** (base 744184a + the RD.8
  commit, live build on the board; negative build `--features tls-wrong-ca`). mbedtls-rs not needed.
  - **Stack choice: embedded-tls 0.19 used directly, not through reqwless 0.14.** Mismatch with the
    design text: reqwless 0.14 depends on embedded-tls **0.18**, builds its verifier with `NoClock` (no
    certificate validity check) and 0.18 matches the host name against the CN only (the leaf CNs are
    `www.adsbdb.com` and `planespotters.net`, the names are in the SANs). So `firmware/src/tls.rs` has
    its own `get()` over `TlsConnection` with the 0.19 `pki::CertVerifier` (features `rustpki`, `p384`,
    `rsa`; webpki is out because it needs ring), a wall clock from SNTP, SAN host matching, TLS 1.3
    `TLS_AES_128_GCM_SHA256`. HTTP/1.1 is a few lines plus `adsb_core::wire` (`parse_head`, `dechunk`,
    host-tested); reqwless is not a dependency. RD.9 reuses `tls::get`.
  - **CA facts (live, 2026-10-23, all TLS 1.3, AES-256-GCM chosen by openssl):** `api.adsbdb.com`: leaf
    EC P-256 (SAN api.adsbdb.com), Let's Encrypt YE1, Root YE, last sent entry ISRG Root X2 cross-signed by
    **ISRG Root X1**. `api.planespotters.net`: leaf EC P-256, Google Trust Services WE1, GTS Root R4, last
    entry R4 cross-signed by **GlobalSign Root CA** (RSA 2048). `t.plnspttrs.net`: leaf RSA 2048 (server
    signs with rsa_pss_rsae_sha256), Let's Encrypt YR2, Root YR, last entry **ISRG Root X1** itself. Only
    two roots are pinned (`firmware/certs/`, README has source, fingerprints, expiry: X1 2035-06-04,
    GlobalSign 2028-01-28). The embedded-tls verifier checks that the **last sent** certificate is signed by the
    single pinned CA and each sent certificate is signed by the next one, plus validity time and host
    name; so the pin is chosen per host, and a changed server chain fails closed (reflash).
    The embedded-tls verifier alone does not check basicConstraints, keyUsage or issuer names (orchestrator
    finding: a holder of any ordinary leaf could sign a forged leaf; fixed in the RD.8 fix, see below).
    The whole chain must fit `CHAIN_BYTES` = 8 KB (4 KB failed for the RSA chain with
    `OutOfMemory`).
  - **Numbers (board, 3 runs):** SNTP `pool.ntp.org` via DHCP DNS, rtt 1.0 s, time plausible (logged as UTC).
    Handshake `hs_ms`: adsbdb 3.6-3.7 s (ECDSA P-384 chain checks in software, 3 of them), planespotters
    1.6 s, image host 0.9 s; request totals 3.8 s, 1.7-2.1 s, 1.0-1.1 s. Bodies: adsbdb 673 B (status 200,
    `meta: parse ok route=AMS-LHR` with the RD.7 parser), planespotters 410 B (200, `photo=true`), image
    first 1 KB through `Range` (206, JPEG magic present). Each handshake blocks the executor for
    about its `hs_ms` (the crypto does not yield): the feed socket buffer (8 KB) and the UI tick stall for
    that time, so RD.9 must run lookups rarely (policy: 1 req/s max, focus only) and RD.10 should check the
    feed after a fetch; the `restart` ISR on core 1 kept `frames/s=39` after the spike.
  - **Memory (K1):** TLS record buffers 16.6 KB + 2 KB, TCP 8 KB + 1 KB, chain copy 8 KB: all plain allocs,
    confirmed in PSRAM (`bufs_psram=true`). Internal free before 29,108 B; minimum during the requests
    **20,588 B** (adsbdb, first request; WiFi/DNS/TCP allocate internally), 22.3-25.7 KB for the others;
    back to 29,108 B and PSRAM back to the old figure afterwards (no leak). Total heap peak
    (`esp-alloc` `internal-heap-stats` `max_usage`) 0.90-0.91 MB against about 0.85 MB at idle, so TLS peaks at
    about 55 KB (PSRAM). Gate >= 10 KB internal: pass.
  - **K12 (ELF sections, release):** `.text` 1,184,437 B (was 697 KB), `.rodata` 942,872 B (was 886 KB),
    `.data` 19 KB, `.bss` 69 KB, `.rwtext` 16 KB. The TLS crypto (RSA, P-384, bignum) added about 490 KB of
    code. The image is about 2.3 MB and flashed fine into the default app partition (about 4 MB).
  - **Negative test:** `--features tls-wrong-ca` pins the other root for every host: all three handshakes fail
    with `Tls(InvalidCertificate)` after about 200 ms (chain signature check), no HTTP request is sent.
    Unsynced clock: the verifier gets time 0, so a certificate is never accepted before SNTP.
  - **API facts (K7) from this step:** planespotters answered **200 with the photo list for a User-Agent without a
    contact** (`esp32-adsb/0.1.0`) on the board, and with curl's default, an empty and a browser UA from this
    network; the RD.7 error message ("must include a contact URL or email") did not reproduce. The contact stays an
    open point for the user: new optional build variable `ADSB_HTTP_CONTACT` (validated, 1-120 printable ASCII, no
    white space; tests in `build_config.rs`) makes the User-Agent `esp32-adsb/<version> (+<contact>)`, else
    `esp32-adsb/<version>`. No contact is committed or logged (`contact=true|false` only). Hex 3C6444 is the
    fixed example aircraft; the image URL comes from the answer, no photo URL is in the repo.
  - **Other changes:** `embassy-net` `dns` feature, SNTP task (`sntp_task`, at boot and every 6 h; wall clock
    in `tls.rs`), `StackResources` 3 -> 5 sockets, `tls_spike_task` (one shot at boot in every networked build;
    RD.9 replaces it by `meta_task`). `adsb_core::wire` (SNTP packet parse, UTC date, HTTP head, chunked body) with
    unit tests. A feed host given as a name still needs DNS in `feed_task`: not done (untestable without
    changing the private `.env`).
  - **Not verified:** the `.env.example` entry for `ADSB_HTTP_CONTACT` (a deny rule blocks edits to that file in
    this environment; the line to add is `# ADSB_HTTP_CONTACT=https://example.invalid/contact` with a comment),
    expired-certificate and host-name-mismatch negative tests on the board (library code, not exercised),
    behaviour when a server changes its chain, mbedtls-rs (not needed).

- 2026-10-23, **RD.8 fix (security, orchestrator finding): structural chain checks.** `firmware/src/tls.rs`
  wraps the embedded-tls `CertVerifier` in `Checked`, which first runs `adsb_core::x509::check_chain` (pure,
  host-tested, own minimal DER reader, every parse problem rejects): leaf is not a CA, its keyUsage (if present)
  has digitalSignature and its extendedKeyUsage (if present) has serverAuth or anyEKU; every other certificate has
  basicConstraints cA=TRUE, keyUsage (if present) has keyCertSign, pathLenConstraint holds; each issuer name equals
  the next subject (byte compare) and the last issuer equals the pinned root's subject; chains over 8 entries
  and raw public keys are rejected. Host tests use fictitious throw-away fixtures in `adsb-core/tests/x509/`
  (`gen.sh`): good chains pass; forged leaf under an ordinary leaf, intermediate without basicConstraints, CA leaf,
  bad leaf keyUsage/EKU, bad CA keyUsage, pathLen 0 above a CA, wrong root, garbage all fail with the matching
  reason. Board: the real chains of all three hosts still pass (`tls: ok ... verify=on`), handshakes 4.3 s / 1.8 s /
  0.9 s (before: 3.6-3.7 / 1.6 / 0.9; the checks cost milliseconds, the spread is network and server speed).
  `tls-wrong-ca` still fails all three with `InvalidCertificate` (~200-235 ms); note it is now rejected by the
  issuer-name check first, so it no longer exercises the signature check (that is unchanged library code).

- 2026-10-24, **RD.8 fix 2 (QA F1 critical, F3, F4)**: server authentication is now enforced, not assumed.
  - **F1 (upstream embedded-tls 0.19 security issue):** `process_server_verify` (`src/connection.rs`) calls
    `verify_certificate` only if a Certificate message arrives and `verify_signature` only if a CertificateVerify
    arrives; nothing requires either before the server Finished. A man in the middle that sends ServerHello,
    EncryptedExtensions and Finished (no certificate), or replays the real public chain and omits
    CertificateVerify, gets `open()` -> `Ok` with the pinned root, SAN and chain checks never run. Reproduced by QA
    with a hostile server; reproduced again by the new host test with the guard removed (2 of 6 tests fail).
    Fix: `adsb_core::tlsauth::open_verified` (feature `tls`) is the only open the firmware uses. Its verifier
    wrapper records `cert` (chain structure + embedded-tls verification passed) and `signature` (verification passed,
    only accepted after `cert`; a new Certificate resets both). After `open()` it returns `NoServerAuth` unless both
    are set; the firmware writes no request byte before that and drops the connection (log `tls: failed host=..
    err=NoServerAuth`). The caller cannot reach the TLS context without the check.
    Regression test: `tools/tls-hostile/` (own workspace, host only; needs openssl and Python `cryptography`,
    see its README): no Certificate -> `NoServerAuth`; real chain without CertificateVerify -> `NoServerAuth`;
    forged chain (with and without proof) and a leaf signed by an ordinary leaf -> `InvalidCertificate`; good flow
    -> Ok; unsynced clock -> refused.
    *Draft text for an upstream report (not filed; the user decides):* "embedded-tls 0.19.0, TLS 1.3 client with a
    `TlsVerifier`: the handshake state machine does not require a Certificate and CertificateVerify message before
    accepting the server Finished. `process_server_verify` verifies a certificate only when the message is received,
    so a peer that skips both completes `open()` successfully although a verifier is configured. Expected: when a
    verifier is configured (not `NoVerify`/PSK), fail with an alert (`unexpected_message`/`certificate_required`) if
    Finished arrives without a verified Certificate and CertificateVerify, and require CertificateVerify to follow
    Certificate. Reproducer: a server that sends ServerHello, EncryptedExtensions and Finished only."
  - **F3:** SNTP is unauthenticated, so `build.rs` stamps the build time (`ADSB_BUILD_UNIX`, set whenever build.rs
    runs) and `tls::set_unix_time` refuses any time before it (log `sntp: time before the firmware build, ignored`;
    the clock stays unsynced, so no certificate is accepted). A spoofed time after the build date is still possible
    (it can accept an expired certificate); full protection needs authenticated time (NTS) and is not planned.
  - **F4 (info, no fix):** embedded-tls 0.19 contains `unwrap!`/`unwrap()` calls that a malicious server can reach and
    that panic the device (denial of service): `verify_signature` unwraps the stored transcript and certificate when a
    CertificateVerify arrives without a preceding Certificate (now blocked by our wrapper, which refuses it first),
    and the `rsa` path unwraps `RsaPublicKey::from_pkcs1_der` for a leaf with a malformed RSA key (a MITM would
    need a leaf that passes the chain check first, so it needs a CA-signed bad key; not reachable before verification
    succeeds). QA's own reproduction details were not available to me beyond the finding text. A panic reboots the
    board; the attacker already needs to be on the path.
  - **Board (after the fix):** the three hosts verify; the wrong-CA build still fails all three with
    `InvalidCertificate` (200-310 ms). planespotters now answers **403 "User-Agent must include a contact URL or
    email"** for every User-Agent without a contact (curl too), including the image host path `/`; earlier today the
    same requests got 200, so the contact requirement is enforced intermittently or since a rate/abuse change. The
    TLS result is unaffected; RD.9/RD.10 need `ADSB_HTTP_CONTACT` from the user. Handshake times: adsb 4.3 s,
    planespotters 1.8-4.9 s, image 0.9 s (the 4.9 s planespotters run followed a server-side chain change or a
    slower path; not investigated).

- 2026-10-24, **RD.8 fix 3 (QA F6)**: Certificate, CertificateVerify, CertificateVerify panicked inside embedded-tls
  (`pki.rs:137`, the transcript is gone after the first CertificateVerify). `tlsauth` now forwards a CertificateVerify
  only in the exact expected state (certificate verified, no signature yet), else `InvalidCertificate`. Test
  `repeated_certificate_verify_is_an_error_not_a_panic` (mode `dupcv` in `tools/tls-hostile/srv.py`); it fails with the
  panic when the guard is removed.

- 2026-10-24, **RD.8 QA PASS** (0a24ded, eb67aff, 33b5635, a516943, c197b49; beads entry pending).
  Gate: embedded-tls 0.19 directly (not reqwless, see engineer entry) verifies all three hosts.
  Security history: orchestrator found missing basicConstraints/CA checks (fixed eb67aff); QA
  FAIL F1 critical: embedded-tls accepts a handshake without Certificate/CertificateVerify (fixed
  a516943 via `tlsauth::open_verified`, regression tests in `tools/tls-hostile/`; fix loop 1/2);
  QA F6 DoS (repeated CertificateVerify panics the library) fixed c197b49 (verified by the
  orchestrator: tls-hostile 7/7). Open, low: F2 CN fallback, F3 residual (SNTP spoofing after the
  build time), F4 library unwraps. Upstream report for F1 drafted above, NOT filed (user decides).
  planespotters now answers 403 without a contact: RD.10 waits for `ADSB_HTTP_CONTACT`.

- 2026-10-24, **RD.9 done: `meta_task` (text metadata from adsbdb) on the board** (base 15905b8 + the RD.9 commit,
  live build flashed). Photos stay out (RD.10); `plan.photo` of the policy is not used yet.
  - **Code.** `adsb-core::meta`: `Kind` (combined, aircraft, route; `path()` builds the URL and accepts only a
    `[A-Z0-9]{1,8}` callsign), `Step` + `adsbdb_step()` (what one reply taught us, incl. the RD.7 catch: "unknown
    callsign" -> route negative, aircraft is asked for alone next; "unknown aircraft" -> aircraft negative, route
    asked for next through `/v0/callsign/`), `TextCache` (aircraft by ICAO 7 d, route by callsign 6 h, negative
    1 h; `needs()` names the missing half, so no extra retry state), `Lru::new` is `const`. `Policy` now plans only
    valid callsigns. `adsb-core::wire::Head` has `content_length` and `close`. Firmware: `lookup.rs` (`meta_task`,
    shared cache + latest plan under one async mutex, `publish()` called by `ui_task` each tick, which returns the
    focus `FocusMeta`); `tls::Conn` replaces `tls::get` (see below); `tls_spike_task` and its probe are gone.
    `tls-wrong-ca` stays meaningful: `meta_task` then logs `tls: failed host=api.adsbdb.com err=Tls(InvalidCertificate)`
    plus `meta: error connect` / `meta: backoff` and backs off (not re-run on the board in this step, host and code
    paths unchanged since RD.8). Demo builds pass `None` as meta (no network).
  - **Executor stall, what I chose.** Measured on the board (handshake adsbdb 3.4-3.7 s): each handshake gives
    `ui: tick gap_ms` 3.5-4.1 s and `feed: ... max_gap_ms` 3.3-3.9 s (new log field: longest pause between two
    feed reads in the 10 s window). No reconnect, `crc_fail=0`, the window after it shows catch-up
    (frames/s 11 vs 3) = no bytes lost at night rates (1-4 aircraft, 3-14 frames/s); the TCP feed is not
    dropped. **Not measured at day traffic** (about 1.6 KB/s at 39 frames/s): the net runner does not run during the
    handshake, so the WiFi driver queue (not our 8 KB socket buffer) holds the packets; TCP retransmission covers
    drops but the delay is the same 4 s. Mitigation chosen: **HTTP keep-alive to adsbdb** (the server keeps a
    connection at least 60 s idle and closes it between 60 and 120 s, checked with a Python client): one handshake
    serves all queued lookups (aircraft, route, prefetch), later requests take about 30 ms. The connection is
    closed after 60 s idle; a reused connection that fails before the first reply byte gives `Error::Stale` and
    is reopened without back-off. Result: in two runs of about 19 and 18 min, 4 and 3 handshakes (new aircraft
    arriving more than 60 s after the last lookup need a new one). Yield points inside the handshake are not
    possible (library crypto). **Not done, proposed if day traffic shows loss:** run the net runner, feed and UI in an
    `esp-rtos` interrupt executor (higher priority than the thread executor that runs `meta_task`), so the
    handshake is preempted; it touches the task layout and the render code and needs a decision.
  - **Verify (board, night traffic 1-4 aircraft, no hostile lookups).** Run 1 (872 s uptime at the last look):
    9 `meta: ok`, 1 `not found`, 10 requests, 0 errors, no panic, 3 `ok` within the first 6 min. Run 2 (about
    18 min): 3 `ok`, 3 `not found`, 7 requests, 0 errors, no panic. No `(icao, kind)` requested twice within its
    TTL (log checked). Request rate: new log field `t_ms`; consecutive requests 1.08 s, 5.0 s apart (limiter 1 s
    min; the log has no other timestamps, so only these pairs were checked). Requests are only for the planned
    aircraft by construction (`next_job` reads the plan); the log was not cross-checked against the aircraft list.
    Heap: `internal_min=20588` during the handshake (same as RD.8), `internal_free=29108` after (27404 once,
    back to 29108 later), gate >= 10 KB holds; TLS buffers in PSRAM (`bufs_psram=true`, allocated once and reused).
    Panel (burst frame, K4 glitches ignored): focus callsign, airline line and `CGN -> AYT` with city line present
    (resolution of the bench camera makes the small text unreadable; the log's `meta: ok` gives airline and route
    of the same aircraft). Aircraft the service does not know show "-" as designed.
  - **Observations.** The callsign usually arrives some seconds after the position, so the first lookup of an
    aircraft is often `kind=aircraft` and a second `kind=route` follows (an extra request, still <= 1/s);
    waiting for the callsign before asking would save it (not done). adsbdb answers 404 with the "unknown ..."
    body, which `meta_task` treats as an answer. The feed task still closes its connection after 60 s of silence
    (`feed: lost (idle timeout)`, then reconnects): seen several times at night, before any lookup, unrelated.

- 2026-10-24, **RD.9 hardening (QA F1-F5).**
  - F1: `Conn::get` refuses a reply head longer than 4 KB without CRLFCRLF (`MAX_HEAD`).
  - F2/F3: `adsb_core::wire::body(head, rest)` cuts the body by Content-Length or chunking and says whether
    anything followed (`exact`); extra bytes are cut off and the connection is not reused. `Head.bad_framing`
    flags conflicting or unparsable Content-Length and Content-Length together with chunked; `Conn::get` returns
    an error for it. Host tests `framing_conflicts_are_flagged`, `body_framing_is_exact`.
  - F4: `tls::Reply` has `complete`; `meta_task` uses a 200/404 body only when complete and parsable. A truncated
    or garbage body is an error (back-off, no cache write); only a genuine "unknown aircraft/callsign" answer is
    cached as negative.
  - F5: `feed_task` resets its back-off only when data kept arriving for at least 30 s (`STABLE`); `wifi_task`
    resets it only when the association lasted 30 s, otherwise it waits and doubles (up to 30 s). Reasoned, not
    reproduced on the board (the host cannot flap the feed).
  - Checks: adsb-core tests (78, plain and `photo,tls`) and clippy, adsb-app, sim, tls-hostile 7/7, firmware clippy
    (live and demo), demo build. Board: live build flashed, 7 min, one request path (`combined`, `ok`), no panic,
    `internal_min=20588`, `internal_free=29108`.

- 2026-10-24, **RD.9 QA PASS with gaps** (57b364f, hardening 7aab1e1 for QA F1–F5, reviewed by
  the orchestrator: feed/wifi back-off reset only after 30 s stable; adsb-core 78 tests). Beads
  entry pending. Gaps (night traffic, 1 aircraft): "≥ 3 meta: ok in 10 min", no-repeat within TTL,
  ≥ 1 s spacing, 429/Stale/5xx paths and the panel text on the board are verified by code and host
  tests only → re-check at day traffic (also the ~3.5–4 s executor stall per handshake, K5).
  Bench transport timed out every ~10 min during long `--follow` waits (host side; noted for the
  bench follow-up).

- 2026-10-24, **RD.6 result** (bench, head 237b3c6 + RD.6 change). Firmware: demo `feed_task` and
  `new_pipeline` now honour `ADSB_DEMO` (`busy` = `Kind::Busy`, seed 1, as the sim keyframe);
  logs `radar: range=<km> blips=<n> focus=<callsign|ICAO hex|none>` (every 30 s and on focus change)
  and, in demo builds, `demo: scenario_ms=<ms>` every second.
  - Busy demo (expectation: ~30 blips, ring labels, focus ring, focus callsign, `—` fields): the
    photo at scenario_ms ~100-103 s (sim keyframe 7-busy is 100 s) matches the sim PNG: 50 km range,
    same blip pattern, focus ring and `ZQ6717` in the panel, `no photo`, `—` fields, `+10 beyond`.
    PASS (the photo is dim and soft; the sim shows the same layout).
  - Live build: photo (clean frame, glitched=0) shows range 100 km, DLH417 in the panel with
    metadata (airline, route); logged `radar: range=100 blips=2 focus=DLH417` equal. PASS.
  - Render times: full 77-78 ms (target <= 400); partial, busy demo (30 blips) 51-76 ms; live 10 min:
    650 renders, max partial 93 ms, 1 Hz row-limited updates (<100 rows) avg 7 ms, max 10 ms (target
    <= 80 typical). No 0.5 Hz fallback needed.
  - K1 gate: 10 min live, `internal_min=20588`, heartbeat `internal_free` 20588-29108 (>= 10 KB), no
    panic, 4 `ui: tick gap` (TLS handshakes, known).
  - Not verified: photo (JPEG) panel on the board (RD.10); night traffic = 1-2 aircraft.

- 2026-10-24, **RD.6 PASS** (fee0864; verified by the orchestrator from the engineer's frames:
  busy demo on the board matches sim `7-busy` (blip pattern, focus, ring labels, "+10 beyond");
  live panel callsign equals the logged `radar:` focus, with airline/type/route from adsbdb).
  Render: full 77–78 ms, busy partial 51–76 ms, live typical 7 ms (max 93 ms once); K1 internal
  min 20588 B. Beads entry pending.

- 2026-10-24, **RD.13 soak (interim, live build after fee0864, started 02:48 UTC)**. Every 30 min:
  | uptime | internal_free | psram_free | restart fps | aircraft | meta errors |
  |---|---|---|---|---|---|
  | 0 h 14 | 29108 | 7524308 | 39 | 2 | 0 |
  | 0 h 44 | 29108 | 7512536 | 39 | 3 | 0 |
  | 1 h 14 | 29108 | 7495064 | 39 | 3 | 0 |
  | 1 h 43 | 29108 | 7464092 | 39 | 3 | 0 |
  | 2 h 13 | 29108 | 7445564 | 39 | 8 | 0 |
  | 2 h 42 | 29108 | 7437796 | 39 | 7 | 0 |
  | 3 h 12 | 29108 | 7419916 | 40 | 9 | 0 |
  No reboot, no panic, internal heap flat, lookups continue (115+ ok, 0 errors). PSRAM free falls
  ~105 KB in 3 h (per round −12/−17/−31/−18/−8/−18 KB, with small recoveries): consistent with the
  metadata cache and tracker filling as traffic grows, but a slow leak is not ruled out → watch a
  longer run with day traffic. **K4 escalates with traffic:** at 7–9 aircraft most camera frames
  show shifted/ghosted content (scanout itself stays 39–40 fps). `bench log --session` keeps only
  the last 20000 lines, so meta counts are per window.

- 2026-10-24, **user decisions (K4 and review)**: fix the 3 MAJOR findings plus MINOR `ui.rs:74`
  (bounce build), `net.rs:35` (sockets), `app.slint:87` (no_data bool), `ci.yml:39` (CI features);
  the other MINORs are deferred. K4: try in order (1) lower PCLK to 12–13 MHz together with
  (2) larger instruction/data cache if esp-hal 1.2 allows it, then (3) the QIO boot-loop cause.
  Success = ~0 glitched frames per 100 in a 30 fps burst at day traffic. **Stop rule:** if these
  fail, or the work becomes highly experimental (bounce-buffer GDMA debugging, XIP from PSRAM,
  patching esp-hal), stop the K4 work and run an architect review of the platform: is
  esp-hal + embassy mature enough for a glitch-free 800×480 RGB UI, or is a more mature base
  better (for example Rust `std` on ESP-IDF via esp-idf-svc/esp-idf-hal with IDF's `esp_lcd` RGB
  driver, bounce buffers and XIP-from-PSRAM). The user prefers 100 % Rust but does not accept a
  glitchy UI.

- 2026-10-24, **user decisions for the unattended run**: if the K4 stop rule triggers, the
  architect writes the platform review **and** a throwaway spike is built on a separate branch
  (Rust `std` on ESP-IDF via esp-idf-svc: `esp_lcd` RGB panel with bounce buffers and a static
  Slint frame) to prove glitch-free scanout on the board. No migration of the main firmware
  without the user. Quota: stop cleanly at **90 % of the weekly limit** (notify first).

- 2026-10-24, **RD.10 done: photo on the board** (base c67a182 + the RD.10 commit; live build flashed, `ADSB_HTTP_CONTACT`
  set by the user, log shows `contact=true` only).
  - **Code.** `meta_task` (firmware `lookup.rs`) runs the photo flow when no text lookup is missing and the focus has
    been stable for 3 s (`Plan.photo`), not cached and not recently failed: it closes the adsbdb connection, opens
    `api.planespotters.net` (JSON), closes it, opens `t.plnspttrs.net` (image), closes it, decodes
    (`adsb_core::photo::decode`, RD.7) and hands the photo to `ui_task` through `Shared.ready`. One TLS socket at a
    time, so `SOCKETS = 5` in `net.rs` is enough (no change; a concurrent fetch would use the spare). `ui_task` owns the
    4-entry `Rc<Photo>` cache (`FbUi.photos`) and keeps its old content until the next tick after the swap.
    `tls::Conn::get` got a `max_body` argument (JSON 32 KB, image `MAX_JPEG` = 96 KB). New `PhotoRef::path()` (host-tested):
    only graphic ASCII goes into the request line. A started fetch is marked in `photo_skip` (5 min) so the next tick
    does not queue it twice; "none" and "too big" skip for 1 h, errors back off through the limiter.
    The adsbdb keep-alive connection is given up when a photo is pending (costs a new 3.5 s handshake for the next text
    lookup).
  - **Numbers (board, 11 min uptime, 7 photos).** planespotters answers 200 with the contact (no 403). Handshakes
    planespotters 1.56-1.66 s, image host 0.93-0.99 s. `photo: ok ... decode_ms` 477-554 (bytes 14-40 KB; decode and
    scale together, `scale_ms` is not logged separately). Whole flow about 3.6 s (two handshakes, ~0.55 s decode), one
    `ui: tick gap` of 3.5-4 s per photo. Decode 0.5 s < 1.5 s: the 336x224 size stays, no thumbnail fallback (K9 closed).
  - **Verify.** `photo: ok` for the focus aircraft: yes. Feed: `frames/s` never 0 (lowest 1 to 2 in the window with the
    stall, `max_gap_ms` up to 4.9 s), `reconnects=0` in all 67 feed lines, `crc_fail` 0-1, scanout `frames/s=39` throughout.
    No panic. Gate K1: `internal_min=20588`, heartbeat `internal_free` 29108 (min seen 20588 during a handshake), PSRAM free
    falls with the photo cache (4 x 226 KB) and tracker, 6.37 MB at 11 min. Panel (burst frame): photo at the top of the
    panel, colours right (blue sky, A380 in Emirates livery; not swapped), credit line and metadata below. The camera
    cannot read the credit text. The screen is glitched in nearly every frame at 5-9 aircraft (K4), so the cleanest frame
    still shows the panel doubled on its left half.
  - **K4 and the photo path (record, not a fix).** At 7-9 aircraft almost all frames jump against the previous one in every
    window (before the fetch 71 %, during the fetch 90 %, in the second after the draw 67 %, steady state 95 %), so no
    photo-specific burst could be separated from the continuous glitching; with 3-7 aircraft and a static photo
    33 of 148 distinct frames were glitched (median-based counter). Not measured with an empty sky.
  - **Not done / not verified:** credit text legibility, photo-less aircraft path on the board (`photo: none` did not occur
    in this run), `too big`/403/429 paths (code and host tests only), a separate `scale_ms`.

- 2026-10-24, **RD.10 QA PASS** (0bc9097, tested at 21c4e6a): 8 `photo: ok` in 565 s, decode+scale
  507–565 ms (K9 < 1.5 s, 336×224 kept), JPEG 23–47 KB, sequential TLS (planespotters 1.56–1.63 s,
  image host 0.92–1.01 s handshakes), feed `reconnects=0` in all 59 lines, `internal_min=20588`,
  colours correct (least-glitched frame shows the photo and panel intact; credit line too small for
  the webcam). User saw the photos on the panel. Leak check by the orchestrator (counts only): WiFi
  key, coordinates, contact 0 log lines; the contact value is the user's own public URL already in
  README.md since the initial commit; feed host only in the known adsb-display.md lines. Not verified
  on the board: `photo: none` / `too big` paths (code + host tests only); cause of one 17 s feed gap
  with frames/s=0 outside any photo flow (likely a traffic lull, not proven). Beads entry pending
  (chain blocked by RD.2).

- 2026-10-24, **review fixes QA PASS** (261cd8e fmt only, 8d89584 render → `Option<Range>` + Slint
  `no_data` bool, f40816a TLS time limits (connect 10 s, handshake 15 s, I/O 10 s each, socket
  timeout 10 s), 88cbef6 bounce build, 8966778 `SOCKETS = 6` (5 concurrent users: DHCP, DNS, feed
  TCP, SNTP UDP, one HTTPS TCP, plus 1 spare; supersedes the RD.10 note "5 is enough"), b6051b4 CI
  `--features photo,tls`). QA reproduced the old panic in a scratch copy (Quiet→JustPassed gives
  drawn with y0=MAX, y1=0). Board 336 s, no panic. Not verified: a real TLS timeout or `Stale`
  retry (code reading only; no host harness for the Xtensa-only TLS path). Residual (low): the read
  limit is per read, not per reply. Deferred MINORs stay open: build_config.rs:85, ui.rs:30,
  build.rs:5, lookup.rs:153, tracker.rs:285, main.rs:10.

- 2026-10-24, **K4 fix: PCLK 16 -> 12 MHz** (display.rs `PCLK_MHZ`). Metric: `ADSB_DEMO=busy` build, burst of
  8 s from the bench MJPEG stream (about 27-40 camera fps) at scenario time ~100 s, first 10 frames skipped; a frame
  counts as glitched if more than 0.4 % of the panel pixels differ by more than 40 grey levels from the pixelwise
  median of its 6 nearest frames (drift-proof; a real content change shows for 1-3 frames).
  | variant | refresh | glitched / 100 (busy demo) |
  |---|---|---|
  | baseline 16 MHz | 39 Hz | 30.6 (71 of 232) |
  | 13 MHz | 32 Hz | 20.7 (48 of 232) |
  | 12 MHz | 29 Hz | 0 (0 of 232), repeated: 0 of 232 |
  | 16 MHz + data-cache line 64 B | 39 Hz | 41.8 (97 of 232) |
  | 13 MHz + data-cache line 64 B | 32 Hz | 22.1 (67 of 303) |
  Cache size: esp-hal 1.2.2 (`esp_config.yml`) already runs the maximum on the S3 (instruction cache 32 KB, 8 ways,
  32 B line; data cache 64 KB, 8 ways); only the data line (32 -> 64 B, `ESP_HAL_CONFIG_DATA_CACHE_LINE_SIZE`)
  can change, and it did not help, so it is not adopted. Live build at 12 MHz (3 aircraft, board uptime 2-3 min):
  0 of 315 frames in 8 s; 1 of 100 in a 20 s burst, those 4 frames are a real focus change (ELI30A to 3C1BAB), not a
  glitch. Restart ISR: 29-30 frames/s, isr_max 15-31 us. Step 3 (QIO boot loop) not needed. Open: day traffic
  (7-9 aircraft) not re-checked; the camera beats against the 29 Hz refresh (brightness pulses, not content).

- 2026-10-24, **K4 user visual check**: the user looked at the panel running the 12 MHz live build
  (9af2c78) and confirmed "no flickering". Independent QA (16 vs 12 MHz reproduction, TLS/photo
  windows, metric validity) is running.

- 2026-10-24, **user feedback and feature request (queued as RD.14 and RD.15)**:
  - RD.14: on the panel the radar looks elliptic, not circular (the sim PNG is round, 416×416 px).
    Hypothesis: non-square panel pixels (a 7" 800×480 active area is about 154.2×85.9 mm, i.e.
    about 7–8 % wider than tall) [VERIFY against the Waveshare/panel datasheet; the camera angle
    also distorts]. Fix in the radar projection (rings, blips, labels), not in the scanout.
  - RD.15 (user request): the board has a touchscreen (GT911, parent step 1.6 never done). Default
    selected aircraft = closest. A tap on another aircraft overrides the selection, and the info
    panel shows it. The override ends when the selected aircraft leaves the radar scope (then the
    next closest is selected) or after a configurable timeout. Needs an architect revision first
    (GT911 driver, hit-testing on the radar, selection policy in adsb-core, config variable and its
    default, sim support with the mouse).

- 2026-10-24, **K4 QA PASS** (9af2c78, PCLK 12 MHz, 29 Hz). Busy demo: 16 MHz 31.9 / 29.7 glitched
  per 100 (reproduced), 12 MHz 0 / 0 / 0 / 0 (232 frames each). Metric checked by eye and by a raw
  consecutive-frame difference count (16 MHz 80/237, 12 MHz 1–4/237). Live (5–8 aircraft in the
  feed, 2–3 blips in range): 0 K4-type glitches by eye in ~3,900 frames, including windows with TLS
  handshakes and photo fetch+decode; the median counter flags focus changes (normal single-buffer
  tearing for 1–2 frames), so the live success figure is counted by eye. Restart 29–30 fps,
  `isr_max_us` ≤ 34 (blank ~0.8 ms), no panic in 368 s. User visual check: no flickering.
  Comment fix in restart.rs (blank time at 12 MHz). Open: true day traffic of 7–9 blips in range
  only covered by the busy demo; 12 MHz is a threshold (13 MHz still glitches 21/100), so a
  heavier load could need 11 MHz. **RD.2 closed.**

- 2026-10-24, **revision 3 (architect): design and plan for RD.14 (radar pixel aspect) and RD.15
  (touch selection)**. Not pre-approved; needs the user's review before implementation.
  - **Added:** status note; conventions (step IDs RD.14.1–RD.14.4 and RD.15.1–RD.15.7, risks up to
    K24, the [user] tag); decisions RD-D8 (touch selection, user semantics) and RD-D9 (aspect fix);
    "Context for revision 3" (facts from the code at c804300, including the verified Slint
    software-renderer limits: no `Path` in line-by-line mode, no transformations); options O6–O10;
    ADR-004 (aspect constant `PIXEL_ASPECT` = 1.076 in `adsb-core::radar` [VERIFY]; elliptic rings
    as one RGB8 background image with a render gate and a ring-tile fallback; sim keeps device
    pixels plus `*-physical.png` and a physical-aspect canvas; selection in `FeedPipeline` via a
    new `adsb_core::select`; GT911 polled at 25 Hz with our own driver; hit-test in Rust on the
    shown blips, 28 px aspect-weighted radius plus label rectangles; `touch-script` debug feature
    for finger-free board tests; `ADSB_SELECT_TIMEOUT_S` default 60 s, range 10–3600); config
    row `ADSB_SELECT_TIMEOUT_S`; `touch_task` in the firmware task diagram; design sections "Radar
    pixel aspect (RD.14)" and "Touch selection (RD.15)"; plan rows and steps RD.14.1–RD.14.4,
    RD.15.1–RD.15.7; risks K15–K24; deferred and unverified items for revision 3.
  - **Changed:** RD-D7 (touch selection on the radar is now in scope; list/detail views stay out);
    the radar projection rule (x offset divided by `PIXEL_ASPECT`, note added in place).
  - **Removed:** nothing.
  - Choices the user may want to change in review: the 60 s default timeout; "tap on empty radar
    keeps the selection"; "tap on the closest aircraft does not start an override (and ends a running one)"; "tap on the
    panel returns to the closest"; the cyan mark and the `MANUAL 52 s` badge in place of the
    OVERHEAD/APPROACHING/NEAREST badge; photo and text aspect stay uncorrected.

- 2026-10-24, **revision 3 approved by the user as is**. All proposed RD.15 defaults kept (timeout
  60 s; tap on empty radar keeps the selection; tap on the closest ends a manual selection; tap on
  the info panel returns to the closest; the MANUAL badge replaces the status badge; photo and text
  not aspect-corrected). RD.14.1: **no ruler measurement**; use the datasheet value 1.076
  (154.08 × 85.92 mm) if the bench plausibility check passes (border ratio ≥ 1.70), else stop RD.14.

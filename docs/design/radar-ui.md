# Radar UI on the device: live feed, radar view and closest-aircraft panel

Status: **revision 1, pre-approved by the user for unattended execution** (overnight), on the
condition that the work stays in the scope of this doc. Date: 2026-10-23. Branch:
`feature/radar-ui` (base: `feature/adsb-display`).

Parent design: [adsb-display.md](adsb-display.md), revision 3 ("the parent"). Bench:
[board-bench.md](board-bench.md) and `.claude/skills/board-verify/SKILL.md`.

Conventions (same as the parent):

- **[VERIFY]** marks a fact that I could not confirm from the container during design (crate API,
  API shape, licence term). Confirm it at implementation time. Do not treat it as fact.
- Step tags: **[code]** = host-testable code, verified by host tests and sim PNGs;
  **[code+hw]** = code plus an on-board verify through the bench (flash, serial log, webcam
  photo); **[hw]** = bench run only, no new code.
- Parent step IDs (for example 3.2, 4.1, 5.2) are used as references. The steps of this doc have
  the IDs **RD.0–RD.13**. Risks of this doc have the IDs **K1–K14** (the parent uses R1–R26).

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
| RD-D7 | **Scope** | Reuse parent steps 3.1–3.4, 4.1, 5.1–5.3. **Replace** the parent overhead-card main screen (4.3) with this layout. Touch (1.6, 4.4), list view and detail view (4.5) are **out of scope**. |

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
 └─ meta_task   (RD.9/RD.10) lookup queue → HTTPS (or proxy) → cache; JPEG decode
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

**Deliberately deferred:** touch (1.6, 4.4), list and detail views (4.5), the "just passed" text
state, label collision avoidance on the radar, a map background, decoding on core 1, photo
sources other than planespotters (for example adsbdb `url_photo`), OTA, the flightradar24
cross-check and the AP/feed outage tests (need a human), the open sim-live distance issue of the
parent (unrelated to this feature).

**Unverified (summary):** Slint 1.18 software renderer: `Path`, rotation, runtime RGB8 images;
`zune-jpeg` `no_std` build and Xtensa speed; `jpeg-encoder` as dev-dependency; adsbdb combined
query and aircraft schema; planespotters JSON shape, image CDN host, terms and User-Agent rule;
the CAs of all three hosts and embedded-tls support for them; mbedtls-rs build model; esp-alloc
region order and esp-radio internal-caps use; cargo verbose output of `rustc-env`; the current
app partition size.

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

# radar-ui: follow-up tickets

Open work after the `feature/radar-ui` run (2026-10-23/24). Each ticket names its evidence in
[radar-ui.md](radar-ui.md) (revision log) or the beads finding. Priority: **P1** = affects what
the user sees or the board's stability, **P2** = correctness or robustness, **P3** = cleanup.

## Features

### FU-1 (P1) RD.15 touch selection

Designed and approved (revision 3, RD.15.1–RD.15.7), not started. Default selection = closest; a
tap on another aircraft selects it; back to the closest when it leaves the radar scope or after
`ADSB_SELECT_TIMEOUT_S` (default 60 s). Includes the GT911 driver (parent step 1.6, never brought
up), scripted taps for the bench, and a finger check by the user (RD.15.7). Risk K17: touch polling
adds load at the 12 MHz K4 threshold, so repeat the K4 burst check.

## Display

### FU-0 (P1) ~~Power-cycle the panel, then finish the RD.14 check~~ DONE 2026-10-24

Since about 17:00 on 2026-10-24 the panel shows its own solid-colour cycle and ignores our signal,
while the firmware runs normally (likely left there by the 11 MHz experiment, below the panel
minimum). Unplug and replug the board's USB. Then: K4 burst on the busy demo with the fixed
`k4.sh` (gate ~0 per 100; the render time is back at the pre-RD.14 level that measured 0), and a
look at the panel: rings round, blips on the rings, no flicker. If K4 is back, revert ec83f24's
parent changes to e24c962's ring code (last known working state).

### FU-2 (P1) K4 margin at peak traffic

PCLK 12 MHz is a threshold, not a proven margin: 13 MHz still glitches about 21 per 100 frames,
and 11 MHz is below the panel minimum. Any new render or flash load can bring the glitches back
(as the RD.14 ring tiles did). Re-check at 7–9 aircraft in range. If load grows, options: reduce
render work, the bounce-buffer path (M2, failed on 2026-10-21 for an unknown reason), or the
platform review (esp-hal vs Rust `std` on ESP-IDF with `esp_lcd`) from the stop rule.

### FU-3 (P2) Tearing at focus changes

At 29 Hz single-buffer scanout, a focus change shows the new callsign over the old one for 1–2
frames. Cheap fix ideas: render the panel change in one tick, or a double buffer swapped at the
per-frame restart (PSRAM has room; check the K4 load).

## Robustness

### FU-4 (P2) Executor stalls during TLS (K5)

Each TLS handshake blocks the single executor for about 3.5–4 s (`ui: tick gap`). Every photo
also drops the adsbdb keep-alive connection, so the next text lookup pays a new handshake. Ideas:
yield points in the handshake, a second executor (interrupt executor) for the UI, keep two
connections.

### FU-5 (P2) Feed gaps and reconnects

The soak showed 6 feed reconnects in 6.5 h, and RD.10 QA saw one 17 s gap with 0 frames/s outside
any photo flow. Find the cause (AP, feeder, or a stall on our side) from the logs.

### FU-6 (P2) TLS timeouts not exercised

The timeouts (f40816a) are checked by code reading only. Build a host test with a stalling server,
or a bench test that drops the AP, and check `meta: timeout <stage>` and the retry. Also consider a
whole-reply deadline (today the 10 s limit is per read).

### FU-7 (P2) Day-traffic re-check of RD.9

Night traffic did not test the rate limit (≥ 1 s spacing), "no repeat within TTL", ≥ 3 `meta: ok`
in 10 min, and the 429/5xx paths on the board.

## Cleanup (deferred MINOR review findings, open in beads)

- FU-8 (P3) `firmware/build_config.rs:85`: `ADSB_META_PROXY` is validated but unused (RD.11 not
  built). Remove it or reject a set value.
- FU-9 (P3) `firmware/src/ui.rs:30`: `ADSB_DEMO` without `--features demo` builds a live build
  with a misleading warning. Fail the build instead.
- FU-10 (P3) `firmware/build.rs:5`: the SNTP time floor `ADSB_BUILD_UNIX` refreshes only when the
  build script reruns.
- FU-11 (P3) `firmware/src/lookup.rs:153`: a truncated 200/404 body logs `meta: error status=200`.
- FU-12 (P3) `adsb-core/src/tracker.rs:285`: wrong doc comment on `Snapshot.focus`.
- FU-13 (P3) `firmware/src/bin/main.rs:10`: stale comments; the design doc header revision.

## Decisions for the user

- FU-14 Upstream reports, drafted in the revision log, not filed: embedded-tls 0.19 accepts a
  handshake without a server certificate; Slint 1.18.1 software renderer off-by-one on partial
  repaint (we vendor a 1-line patch).
- FU-15 The feed host's private IP is committed in `docs/design/adsb-display.md` (since PR #2).
- FU-16 planespotters terms: the exact credit form and whether this use is allowed (the page sits
  behind a Cloudflare challenge; read it in a browser).

## Tooling and process

- FU-17 (P2) Bench: an authenticated `bench clip` endpoint (burst/video) to replace the stream grab
  with a loopback Host header; `bench log --session` keeps only 20,000 lines; client calls time
  out during long waits.
- FU-18 (P2) Session watchdog: the transcript-mtime liveness test fired 3 times on a live session
  (a pending question; long bench waits) and its headless copies worked in parallel. Off until it
  has a reliable liveness signal.
- FU-19 (P3) Beads: `add-step` chains steps linearly, so independent steps could not be claimed
  while RD.2 waited on the user.

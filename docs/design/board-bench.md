# Board bench: automated flash, serial log and webcam check from the container

Status: **approved; step 1 (spike) done, revised with its results.** Date: 2026-10-23.
Branch: `feature/adsb-display`.

Conventions (same as [adsb-display.md](adsb-display.md) and
[devcontainer-gh-cli.md](devcontainer-gh-cli.md)):

- **[VERIFY]** marks a fact I could not confirm from inside the container (no Mac, board or camera
  reachable during design). Confirm it at implementation time. Don't treat it as fact.
- Step tags: **[devops]** = repo tooling, testable in the container without hardware;
  **[manual]** = **user-run acceptance step** that needs the real Mac, board and/or camera.

---

## Decisions needed from user

| # | Decision | Options | Recommendation | Blocks |
|---|----------|---------|----------------|--------|
| B1 | **Which camera?** | (a) UVC USB webcam on a fixed mount. (b) Mac built-in camera. | **(a)**. A built-in camera means the laptop lid is the camera mount and blocks video calls while the bench runs. The design handles both: the avfoundation device is a host-side setting (`--camera`, name or index). | Step 1 (device name), step 6 |
| B2 | **How is the host service started?** | (a) Manually in a macOS Terminal tab (`python3 tools/bench/bench_host.py ...`). (b) launchd user agent (`~/Library/LaunchAgents/*.plist`). | **(a) for now.** Camera permission (TCC) is granted to the app that launches ffmpeg (Terminal/iTerm). Under launchd there is no such app and the grant is unreliable [VERIFY]. (b) is documented as a later option. | Step 7 |
| B3 | **Firmware boot test pattern** (corner markers, colour bars behind a feature flag) in this feature? | (a) Yes. (b) Later, as a firmware step in adsb-display. | **(b).** This feature is host tooling. The existing log lines (`ch422g: backlight on`, `alive 0s`) are good enough boot markers. Revisit once step 6 shows whether photos of the real UI are good enough to judge. | — |
| B4 | **Claude Code allow-list entry for `tools/bench/bench`?** | (a) Add `Bash(tools/bench/bench:*)` to `.claude/settings.local.json`. (b) Leave as is, so each call is prompted. | Your call. This design doesn't change permission settings. (a) is what makes the loop actually unattended. | Step 7 |
| B5 | Service port | any free loopback port | `7777` | Step 1 |

---

## Context

Every firmware change in adsb-display Phase 1 has ended in "waiting on user (board)". The agent
builds an ELF, the user flashes it from the Mac, watches the serial log and the panel, and reports
back in prose. That round trip is the slowest part of the loop, and the reports are lossy (no full
log, no picture).

Goal: the Claude harness **inside the container** can flash the board, read the serial log, and
take a photo of the panel without a human, and then judge the result.

Established facts:

- The podman machine on macOS (applehv/libkrun) **cannot pass USB devices** into containers
  (README "Flashing"). So serial and camera access must happen on the host.
- `espflash` has no `rfc2217://` port support as far as known. RFC2217 + esptool was considered
  and rejected as clunky (two serial stacks, no backtrace decoding, a separate RFC2217 server).
- The workspace is a **bind mount shared between host and container**. ELFs built in the container
  (`firmware/target/xtensa-esp32s3-none-elf/release/firmware`, named copies like
  `firmware/target/fw-D-stock.elf`) appear on the host at the same repo-relative path. So the
  host never needs an upload, only a path.
- The board enumerates two ports on macOS: native USB-Serial-JTAG `/dev/cu.usbmodem101` (VID
  303A, the one to use) and a CH343 UART bridge. *Spike 1b:* on the bench Mac the native port was
  `/dev/cu.usbmodem11401`. The `usbmodem…` suffix depends on the physical USB port/hub, so the
  device name must not be hard-coded (see host config `--port`). `esp-println` is in `auto` mode: while a host is
  attached to the native port, logs go there. Anything printed while no host has the port open is
  dropped, so **who holds the port, and when, matters for capturing the first boot lines.**
- `firmware/.cargo/config.toml` already uses `espflash flash --monitor --chip esp32s3` as the runner.
  The user's manual flow (README) is `espflash flash --monitor --port /dev/cu.usbmodem101 <elf>`.
- Container: Debian trixie with python 3.13 (`devcontainers/features/python`). No pyserial or
  other Python packages are installed, and none are needed by this design.
- Existing log lines that make good markers: `Embassy initialized!` (early), `ch422g: backlight
  on` (panel visible), `alive 0s` (main loop running). Panics come from `esp-backtrace`.
- *Spike 1a/1b (2026-10-23):* `host.containers.internal` resolves to `192.168.127.254` in the
  container and reaches a server bound to `127.0.0.1` on the Mac; **the host sees the client as
  `127.0.0.1`**. Host `espflash` is **4.6.0**. `espflash flash --monitor --non-interactive` with
  stdout piped captures the complete boot log (ROM banner, 2nd-stage bootloader, app), and the
  chip boots **twice** after flashing (the first boot is cut off by a second
  `rst:0x15 (USB_UART_CHIP_RESET)`). A separate `espflash monitor` loses the first ~6 s of the boot.
  Details in the revision log.

## Options

### Transport: how the container reaches the host

| # | Option | Trade-off |
|---|--------|-----------|
| T1 | **Host HTTP service on 127.0.0.1 + bearer token**, reached at `host.containers.internal:7777` (gvproxy forwards the host gateway address to host loopback [VERIFY]). | Natural request/response, long-polling for logs, a browser stream for the human is free. Depends on an unverified gvproxy behaviour. Opens a loopback port that every local process (and every container on the podman machine) can reach, so a token is required. **Spike 1a: confirmed** (requests arrive from `127.0.0.1`). |
| T2 | **File spool over the bind mount**: the container writes request JSON to `.bench/queue/`, a host daemon polls it and writes the results, log file and JPEGs to `.bench/out/`. | No network or token at all; the trust boundary is "can write to the repo", which the container already has. But it's slower (polling), and tailing a growing file across virtiofs may see stale sizes or metadata [VERIFY]. It also needs its own little protocol (ids, atomic rename, cleanup). |
| T3 | **SSH from the container to the Mac** (Remote Login on), and run `espflash`/`ffmpeg` remotely. | No custom service. But it gives the container a **full shell on the Mac**: a much bigger capability than "flash this ELF". It also needs a new key and macOS Remote Login enabled. Rejected. |
| T4 | RFC2217 server on the host + esptool in the container. | Already rejected (see Context). It also doesn't cover the camera. |

### Serial ownership on the host

| # | Option | Trade-off |
|---|--------|-----------|
| S1 | **Run `espflash` for everything**: `espflash flash` (no app reset) then a long-running `espflash monitor --non-interactive --elf <elf>` subprocess whose stdout the service buffers. | One serial stack, and the same tool the user already uses. espflash decodes backtraces against the ELF, and the monitor resets the chip when it attaches, so the first boot lines are captured [VERIFY reset-on-attach and that `--non-interactive` works with stdout piped]. The text output may contain ANSI codes. **Spike 1b: rejected.** `flash --after no-reset` works (exit 0, "Staying in bootloader"), but the attaching monitor's first app line was `alive 6s`: ~6 s of boot log lost. |
| S2 | **Stdlib raw serial reader** (`os.open` + `termios`/`tty.setraw` on `/dev/cu.usbmodem*`) after `espflash flash` exits. | No dependence on espflash monitor behaviour, but the native port re-enumerates on reset, so it needs reopen/retry logic, and boot lines printed before the reopen are lost. No backtrace decoding. |
| S3 | `espflash flash --monitor` as a single process, splitting "flash done" from "monitor running" by matching espflash's progress text. | Closest to the manual flow, but the flash exit code is only known on failure, and success detection depends on espflash's wording. **Spike 1b: confirmed.** Full log from the ROM banner on, success line `Flashing has completed!`, chip boots twice (first boot cut off). |

### Camera capture

| # | Option | Trade-off |
|---|--------|-----------|
| C1 | **One-shot ffmpeg per snapshot** (`-f avfoundation -i "<camera>:none"`), discarding the first N frames so auto-exposure settles, then writing a single JPEG. | Simple and stateless; the camera is only on while a snapshot is taken. Each snapshot costs about 1–2 s for camera start and warm-up [VERIFY]. **Spike 1c: confirmed.** Sharp, well-exposed, judgeable JPEG; ~3 s wall for 30 warm-up frames at the device's ~10 fps. |
| C2 | **Persistent ffmpeg** emitting MJPEG, with the service keeping the latest frame and serving both snapshots and a stream. | Instant snapshots, but the camera stays on (LED, blocks other apps with a built-in camera), and needs frame parsing, staleness handling and idle shutdown. More code for a gain we don't need. |

## Decision (ADR)

- **Context:** as above. The agent needs flash + log + photo from inside the container. USB can't
  enter the container, and the host has to stay safe even though the service can flash hardware
  and run processes.
- **Options considered:** transport T1–T4, serial S1–S3, camera C1–C2 (tables above).
- **Decision:**
  1. **T1, host HTTP service**, Python **stdlib only** (`http.server.ThreadingHTTPServer`,
     `subprocess`, `threading`, `json`, `hmac`, `secrets`). It binds to `127.0.0.1:7777` only.
     The container reaches it at `http://host.containers.internal:7777`. ~~**T2 is the fallback** if
     step 1 shows that gvproxy doesn't forward to host loopback.~~ *Spike 1a confirmed T1; the T2
     fallback is not needed.* The host code is still split into a transport-independent `Bench`
     core (flash, monitor, log buffer, snapshot) and a thin HTTP front end, because that split is
     also what the tests drive. Because gvproxy delivers container requests as `127.0.0.1`, the
     **token is the only thing that distinguishes callers** (see Security).
  2. **S3, one espflash process per flash** (changed from S1 by spike 1b). `POST /flash` stops any
     running espflash process and starts
     `espflash flash --chip esp32s3 --port <port> --monitor --non-interactive <copy-of-elf>` with
     stdout/stderr piped into the log buffer as a new session. The flash/monitor split is
     **derived from output**: the line `Flashing has completed!` marks flash success and the start
     of the monitor phase. If the process exits before that line, the flash failed (exit code +
     output tail). If it exits after, the monitor died. `POST /reset` **re-runs the same command
     with the last flashed ELF copy**: espflash skips unchanged regions (a re-flash reached app
     output ~1 s after `Connecting...`), and it is the only tested way to get a complete boot log.
     `espflash monitor` is not used at all, since it loses ~6 s of boot. Backtrace decoding:
     `flash --monitor` takes the ELF positionally and is expected to use it for symbolisation
     [VERIFY on the first real panic]. The chip boots twice after a flash; this is benign and the
     CLI handles it (see Container CLI). S2 stays the unused last resort.
  3. **C1, one-shot ffmpeg** with configurable warm-up frames. *Spike 1c:* the webcam ignores
     `-framerate 30` (ffmpeg logs "Configuration of video device failed, falling back to default"
     and captures at the device default ~10 fps), so the default argv **does not pass
     `-framerate`**; `-video_size 1280x720` is passed (it worked). Default warm-up is **20 frames**
     (~2 s at ~10 fps). The single frame goes to stdout via `-f image2pipe -c:v mjpeg pipe:1` (no
     `-update 1` temp file needed). ffmpeg's stderr is always logged on the host, because
     unsupported settings fall back silently. A `GET /stream.mjpg` for a human in
     a browser on the Mac is an **optional last step** (step 8), with one ffmpeg per connected
     client. The human can also just open Photo Booth or `ffplay`.
  4. **Container CLI `tools/bench/bench`** (Python stdlib, `urllib`). Regex matching for
     `--until`/`--fail-on` runs **in the container, on the CLI side**. The host only serves lines by
     sequence number with long-polling. That keeps regex evaluation (and any ReDoS) out of the host
     process and keeps the host API dumb.
  5. **Shared secret via the bind mount:** at startup the host writes a random token to
     `.bench/token` (mode 0600, `.bench/` gitignored). The CLI reads it from the same repo path. No
     copy/paste, and only the host user (and so the container user, via `keep-id`) can read it.
  6. **Project skill `.claude/skills/board-verify/SKILL.md`** encodes the agent procedure:
     health → flash → wait for marker / fail on panic → snapshot → judge → report with evidence.
  7. The boot test pattern (B3) and launchd (B2) are **deferred**.
- **Why:**
  - T1 has the best ergonomics (synchronous flash result, long-polled logs, browser stream for
    free), and its one unknown is a 2-minute check (done: spike 1a passed). T2 has two unknowns of
    its own (virtiofs freshness, spool protocol) and is slower, so it was only the fallback. T3's
    capability is far too broad.
  - ~~S1 reuses the tool the user already trusts, gets backtrace decoding for free, and the
    flash/monitor split gives a clean exit code for "did flashing work" and a full boot log for
    "did it boot".~~ S1's premise (monitor-on-attach captures the full boot) failed in spike 1b.
    S3 is the user's own manual flow, is the only variant that produced a complete boot log, and
    still distinguishes "flash failed" from "monitor died" via the `Flashing has completed!` line.
    The price is a dependency on espflash's wording, pinned by the host version check (R9).
  - C1 is the least code and the least intrusive (camera off between snapshots).
  - Client-side regex keeps the host's attack surface (and code) minimal.
- **Consequences:**
  - New host prerequisites: `python3` **>= 3.11** (bench Mac: 3.15.0; was "3.9+ if using the Xcode
    CLT python", relaxed in the step-1 revision, see R7), `espflash` (already required; 4.6.0 on the bench Mac), `ffmpeg`
    (`brew install ffmpeg`; 9.0.2 on the bench Mac), and macOS Camera permission for the
    terminal app that starts the service (already granted on the bench Mac).
  - `POST /reset` is a re-flash of the identical image, not a bare reset. It costs ~1 s extra and
    writes nothing when the image is unchanged, but it holds the flash lock like a flash.
  - The service only works with espflash output that contains `Flashing has completed!`. An
    espflash upgrade that rewords it breaks flash-success detection loudly (timeout/502), not
    silently.
  - **A process in the container can now flash the board and take photos of the room** in front
    of the camera without a human in the loop. Mitigations: the security section and B4.
  - Webcam images and serial logs may show live traffic (adsb-display privacy note) and the room.
    They go to the gitignored `.bench/` and must not be committed or shared.
  - The manual README flow keeps working. The service is optional and must be stopped (or
    `POST /monitor/stop`) before the user runs `espflash` by hand, because only one process can
    hold the port. After a flash, the service's espflash process keeps holding the port (monitor
    phase) until the next flash/reset, `POST /monitor/stop`, or shutdown.

### Host service API (all JSON unless noted)

All requests need `Authorization: Bearer <token>`. The token is compared with
`hmac.compare_digest`. Only `GET /stream.mjpg` also accepts `?token=`, for the browser.

| Endpoint | Behaviour |
|----------|-----------|
| `GET /health` | `{ok, service_version, python_version, serial:{mode, chosen, present, candidates}, espflash_version, ffmpeg_present, camera, monitor:{running, phase, exit_code}, busy}`. `serial.mode` = `fixed` (`--port` given) or `auto`; `chosen` = the port the next flash would use (`null` if auto-detect is ambiguous); `candidates` = current `/dev/cu.usbmodem*` matches; `present` = `chosen` exists. (Replaces the earlier flat `port, port_present` fields.) |
| `POST /flash` `{"elf": "<repo-relative path>"}` | Validate (see Security) → copy to a host temp file → resolve the serial port (auto mode: exactly one candidate, else **503** with the candidate list) → stop any running espflash process → start `espflash flash --monitor --non-interactive` (S3) as a new `session`. The request waits until `Flashing has completed!` appears (→ **200**), the process exits first (→ **502**), or 300 s pass (→ process killed, **504**). Returns `{ok, exit_code, output_tail, session, log_seq, app_seq}`. `log_seq` = first sequence number of this session (espflash's own output included); `app_seq` = seq of the `Flashing has completed!` line (absent on failure); `exit_code` is `null` on success (process still running as monitor). **409** if another flash or reset is running (one global lock), **400** on bad path. |
| `POST /reset` | Re-run the S3 command with the **last flashed ELF copy** (re-flash of an identical image, ~1 s; full boot log). Same response and status codes as `/flash`. 409 if busy, 409 if nothing has been flashed since startup. |
| `POST /monitor/stop` | Kill the espflash process and release the serial port (for manual use by the user). **409** while the session is still in the `flashing` phase (killing mid-write leaves a half-written image). |
| `GET /log?since=N&wait=S&max=M` | Lines with `seq > N`, long-polled up to `S` seconds (cap 30). Returns `{session, app_seq, lines:[{seq, t, text}], next, dropped, monitor:{running, phase, exit_code}}`. `phase` = `flashing` (before `Flashing has completed!`), `running` (after it, process alive), `exited`. `dropped=true` if `N` is older than the oldest buffered line. Ring buffer: 20 000 lines. Lines are UTF-8 decoded with `errors="replace"`, with ANSI escapes and trailing `\r` stripped. The host does no other parsing; boot counting and marker matching happen in the CLI. |
| `GET /snapshot.jpg?warmup=N` | One-shot ffmpeg (timeout 20 s), returns `image/jpeg`. The output is checked for JPEG magic (`FF D8`). **503** if ffmpeg fails (the body has the stderr tail, which is where TCC denials show up). ffmpeg's stderr is also logged on the host for **every** snapshot, because unsupported framerate/size settings fall back to the device default with only a log line (spike 1c). The camera lock is separate from the flash lock, so a snapshot can run during a flash. |
| `GET /stream.mjpg` | Step 8, optional. |

Host config (CLI flags of `bench_host.py`, with env var equivalents): `--port` (serial device;
default: **auto-detect** = the single `/dev/cu.usbmodem*` match; if there are zero or several, it
errors and lists them. Auto mode is checked at startup (fail fast with the list) **and re-resolved
on every flash/reset**, because the `usbmodem…` name depends on the USB port/hub — spike 1b saw
`usbmodem11401`, not the README's `usbmodem101`. A given `--port` is used as is.),
`--camera` (avfoundation name or index, default `1` per B1), `--video-size` (default `1280x720`),
`--framerate` (default: **not passed**; the webcam ignores 30 and uses ~10 fps anyway, spike 1c),
`--warmup` (default warm-up frames, `20`), `--listen` (service address, default `127.0.0.1:7777`;
this is the "configurable port" of B5, distinct from the serial `--port`), `--elf-prefix` (default
`firmware/target/`), `--espflash`/`--ffmpeg` (binary paths, which is also how the tests inject
fakes). Startup fails fast if a binary is missing, and prints the python, espflash and ffmpeg
versions. **It refuses any `--listen` address that isn't loopback.**

### Container CLI

`tools/bench/bench` (executable Python script). It finds the repo root from its own location,
reads `.bench/token`, and uses `BENCH_URL` (default `http://host.containers.internal:7777`).

| Command | Behaviour / exit code |
|---------|-----------------------|
| `bench health` | Prints the health JSON. Exit 0 only if ok, a serial port is chosen and present, and the tools are present. Otherwise exit 4, printing the serial candidates if auto-detect is ambiguous. |
| `bench flash <elf> [--until RE] [--fail-on RE] [--max-boots N] [--timeout S]` | `<elf>` is taken relative to the repo root (an absolute path inside the repo is converted). It prints the espflash output tail. Then it behaves like `bench log --until` starting **after `app_seq`** (only device output after `Flashing has completed!` is matched). **Default `--until 'alive 0s'`**; `--until ''` returns right after flash success with exit 0. |
| `bench log [--since SEQ\|--session] [--until RE] [--fail-on RE] [--max-boots N] [--timeout S] [--follow]` | Prints lines. **Exit codes: 0** = `--until` matched, **1** = `--fail-on` matched or boot loop (more than `--max-boots` boots, default 3), **2** = timeout, **3** = monitor died (process exited after flash success), **4** = transport/auth/request error (400/401/403/409/503, unreachable), **5** = flash failed (502: espflash exited before `Flashing has completed!`, or 504 timeout; the output tail is printed). Default `--fail-on`: `panic|Guru Meditation|abort\(\)|Exception` (overridable, `--fail-on ''` disables it). Matching uses `re.search` per line (lines carry espflash's `INFO - ` prefix). |
| `bench reset [--until ...]` | Same as flash with the last flashed ELF (host re-runs `flash --monitor`, see `POST /reset`). Same defaults and exit codes. |
| `bench snap [OUT] [--warmup N] [--delay S]` | Writes the JPEG (default `.bench/snaps/<UTC timestamp>.jpg`) and prints the path, so the agent can `Read` it. |

**Boots and the double boot (spike 1b).** After `Flashing has completed!` the chip boots twice:
the first boot is cut off (e.g. after `INFO - psram: fb ptr=...`) by a second
`rst:0x15 (USB_UART_CHIP_RESET)` banner, then the second boot runs fully. The CLI counts boots: a
line starting with `ESP-ROM:` starts a new boot, and the CLI prints a `--- boot N ---` separator.
Rules:

- `--until` matches **any** line after `app_seq`, in any boot. This is simple and correct for the
  default `alive 0s`: a cut-off first boot that never reached the marker just doesn't match, and
  a first boot that did reach it really did boot the image.
- A boot ending because a new `ESP-ROM:` banner appears is **not** a failure. Only `--fail-on`
  text or exceeding `--max-boots` (boot loop) gives exit 1. The default `--fail-on` doesn't match
  the ROM reset lines (`rst:0x15 (USB_UART_CHIP_RESET)`), and a cut-off line is never treated as
  a panic. The default `--max-boots 3` leaves room for the observed 2 boots plus one.
- The summary line on exit names the boot in which `--until`/`--fail-on` matched.
- Default marker history: `alive 0s` was originally chosen (R2) because it would appear even if
  early lines were lost. With S3 nothing is lost, but it stays the default because it is the
  latest single line that proves "main loop running". `Embassy initialized!` and
  `ch422g: backlight on` remain usable via `--until`.

### Security

- **No `shell=True` anywhere.** Commands are argv lists built by pure functions (unit-tested). The
  only caller-controlled argv element is the ELF path, and that is the validated host temp copy,
  never the raw string.
- **ELF path validation** (host side, which is the only side that matters): reject absolute paths
  and NUL bytes; `realpath(repo_root / elf)` must lie under `realpath(repo_root / elf_prefix)`
  (`os.path.commonpath`, which also resolves symlinks); it must be a regular file under 64 MiB
  whose first 4 bytes are `\x7fELF`. Then **copy it to a host temp dir and flash the copy**, so the
  file can't be swapped between check and use (TOCTOU).
- Bind address hard-limited to loopback. Bearer token of 32 random bytes (`secrets.token_urlsafe`),
  regenerated on every service start, file mode 0600.
- **The token is the only caller discriminator** (spike 1a). gvproxy delivers container requests
  with source address `127.0.0.1`, so the service cannot tell this container from any other
  container in the podman machine, or from a Mac-local process, by address. Consequences: no
  address-based allowlist is added (it would be false assurance); the peer address is not logged as
  if it identified the caller; anything that can reach the port but cannot read `.bench/token`
  gets 401, and the token's file permissions and per-start regeneration carry the whole access
  control. Containers in the same podman machine that bind-mount this repo (or the user's home)
  can read the token, so they count as trusted.
- Requests that carry an `Origin` header are rejected, except `GET /stream.mjpg`. That blocks
  browser-based CSRF/DNS-rebinding attempts against the loopback port even before the token check.
- Request bodies are capped at 4 KiB, and unknown JSON keys are rejected. Every subprocess has a
  timeout. Child processes are killed on shutdown (SIGINT/SIGTERM handler, `atexit`).
- The serial port is either the user-given `--port` or the single auto-detected
  `/dev/cu.usbmodem*` glob match; it is never taken from a request.
- Residual risk: anything that can read `.bench/token`, meaning anything running as the user on
  the Mac or in the container (or another container with the repo mounted), can flash arbitrary ELFs from `firmware/target/` and take photos.
  That's the intended capability. Stop the service when you're not using it.

### Layout

```
tools/bench/
  bench_host.py        # host service: Bench core + HTTP front end (single file, stdlib)
  bench                # container CLI (stdlib)
  tests/
    fake_espflash.py   # S3 imitation: flash progress, "Flashing has completed!", double boot,
                       # periodic alive lines; failure modes; driven by env vars (see step 3)
    fake_ffmpeg.py     # writes a tiny valid JPEG, or fails like a TCC denial
    test_host_core.py  # path validation, argv builders, ring buffer, ANSI strip
    test_host_http.py  # server on an ephemeral 127.0.0.1 port with the fakes
    test_cli.py        # CLI against the in-container server (end to end, no hardware)
.claude/skills/board-verify/SKILL.md
.bench/                # gitignored: token, snaps/
```

Tests: `python3 -m unittest discover -s tools/bench/tests -t tools/bench` inside the container.
Python target for both host service and CLI: **>= 3.11** syntax/stdlib (container 3.13 runs the
tests, bench Mac has 3.15.0; see R7).

## Plan

Each step is one commit, and its verification runs green before the next starts.

1. **[manual] Spike: verify the three host assumptions.** No repo code. The user runs these and
   pastes the output. Results go into the revision log and pick the fallbacks.
   - a. Transport: on the Mac `python3 -m http.server --bind 127.0.0.1 7777`. In the container
     `curl -sS -o /dev/null -w '%{http_code}\n' http://host.containers.internal:7777/` → `200`.
     If not: try `getent hosts host.containers.internal` and `curl http://192.168.127.254:7777/`
     [VERIFY gvproxy host address]. If neither works → **switch to T2** (revise this doc first).
   - b. Serial: `espflash flash --chip esp32s3 --port /dev/cu.usbmodem101 --after no-reset
     firmware/target/xtensa-esp32s3-none-elf/release/firmware; echo exit=$?`, then
     `espflash monitor --chip esp32s3 --port /dev/cu.usbmodem101 --non-interactive --elf
     <same elf> | cat`. Check: (i) the flags exist in the host espflash version
     (`espflash --version`, `espflash monitor --help`); (ii) the monitor runs with stdout piped;
     (iii) the board boots when the monitor attaches, and the first lines include
     `Embassy initialized!`; (iv) output with ANSI codes, if any. Outcome → S1 confirmed, or
     S3/S2 (revise doc).
   - c. Camera: `ffmpeg -f avfoundation -list_devices true -i ""` (device name/index, B1). Then
     `ffmpeg -f avfoundation -framerate 30 -video_size 1280x720 -i "<cam>:none" -vf
     "select=gte(n\,30)" -frames:v 1 -y /tmp/bench.jpg` run from the terminal that will host the
     service. Grant the Camera permission prompt. Check: a non-black, sharp photo of the panel.
     Note the supported framerate/size (avfoundation rejects unsupported combinations [VERIFY])
     and how many warm-up frames give stable exposure.
   - *Verify:* all three outcomes recorded in the revision log. Fallback decisions, if any, made
     with the user before step 2.
   - **Outcome (2026-10-23): step 1 DONE.** (The commands above are kept as run; on the bench Mac
     the port was `/dev/cu.usbmodem11401`, and the camera was device `1`.)
     - 1a **T1 confirmed**: `host.containers.internal` → `192.168.127.254`, curl 200; host logged
       the client as `127.0.0.1`.
     - 1b **S1 rejected, S3 confirmed**: espflash 4.6.0; separate monitor lost ~6 s of boot; single
       `flash --monitor --non-interactive` gives the full log with a benign double boot.
     - 1c **C1 confirmed**: ffmpeg 9.0.2, device `1` = Logitech HD USB webcam, Camera permission
       already granted. `-list_options true` is not supported by this avfoundation build (use
       `-list_devices true -i ""`). `-framerate 30` was not honoured (fallback to the device
       default ~9.92 fps); 1280x720 honoured; pixel format auto-overridden to `uyvy422`; the
       single-file image2 output warned about needing `-update 1`. 30 warm-up frames ≈ 2.9 s, JPEG
       33 KiB, sharp and well exposed, colour-bar test image clearly judgeable; mild glare and
       room reflections on the panel (R5).
     - Host python3 3.15.0.
     - Full details and design consequences: revision log, 2026-10-23 step-1 entry.

2. **[devops] Host service skeleton.** `bench_host.py`: arg parsing, loopback-only bind check,
   token generation to `.bench/token` (0600), auth middleware (bearer, `compare_digest`, Origin
   rejection, body cap), `GET /health` (binary presence, port presence), ELF path validation +
   temp copy as a pure function, argv builders as pure functions. Add `.bench/` to `.gitignore`.
   - Post-spike changes to this step: the espflash argv builder is the single S3 command
     `espflash flash --chip esp32s3 --port <port> --monitor --non-interactive <elf>` (no
     `--after no-reset` builder, no `espflash monitor` builder); the ffmpeg argv builder omits
     `-framerate` unless configured and pipes via `-f image2pipe -c:v mjpeg pipe:1`; serial-port
     resolution is a pure function (`resolve_port(configured, candidates)` → chosen port or an
     error listing candidates) plus a thin glob wrapper; `/health` reports the `serial` object and
     `python_version`; target python >= 3.11. **If step 2 already landed with the S1 builders**
     (it ran in parallel with the spike), these changes are made at the start of step 3 instead,
     and step 2's tests are updated there.
   - *Verify (container):* unit tests for path validation (accepts a fake ELF under
     `firmware/target/`; rejects `../`, absolute, symlink escaping the prefix, non-ELF, oversize,
     directory, NUL), argv builders (exact lists, no shell; S3 list contains `--monitor` and
     `--non-interactive` and the ELF last; ffmpeg list has no `-framerate` by default),
     `resolve_port` (fixed port returned as is; auto with one candidate → it; zero or two
     candidates → error naming them), non-loopback `--listen` refused. HTTP
     test: 401 without/with wrong token, 403 with `Origin`, 200 `/health` with token and the
     `serial` fields present.

3. **[devops] Flash, monitor, log buffer.** `Bench` core: global flash lock, **one S3 espflash
   subprocess per session** with a reader thread → ring buffer + `threading.Condition` for
   long-poll, sessions with a `phase` (`flashing` → `running` on the `Flashing has completed!`
   line → `exited`) and `app_seq`, ANSI strip, `POST /flash` (waits for completion line / early
   exit / 300 s), `POST /reset` (re-run with the kept last ELF copy; the copy is deleted on the
   next flash and on shutdown), `POST /monitor/stop` (409 in `flashing`), `GET /log`, child
   cleanup on shutdown, timeouts, ffmpeg-independent.
   - `fake_espflash.py` behaviour (selected by env var, e.g. `FAKE_ESPFLASH_MODE`; it also exits
     2 if its argv lacks `flash`, `--monitor` or `--non-interactive`, so the tests pin the argv):
     - `ok` (default): print `Connecting...`, a few progress lines, `Flashing has completed!`,
       then a **double boot**: `ESP-ROM:esp32s3-20210327`, `rst:0x15 (USB_UART_CHIP_RESET)...`,
       `I (27) boot: ...`, a few `INFO - ...` lines ending at `INFO - psram: fb ptr=0x...`, then a
       second `ESP-ROM:` + `rst:0x15` banner, bootloader lines, `INFO - Embassy initialized!`,
       `INFO - ch422g: backlight on`, `INFO - alive 0s`, then `INFO - alive Ns` periodically
       (fast interval, configurable) until killed.
     - `flash_fail`: progress lines, then an error line and exit nonzero **before** the
       completion line.
     - `flash_hang`: never prints the completion line (for the 504/timeout path, with a short
       test timeout).
     - `panic`: `ok` up to `alive 0s`, then esp-backtrace style panic lines.
     - `die`: `ok` up to `alive 0s`, then exit 1 (monitor died).
     - `silent`: completion line and boots, but never `alive 0s`.
     - `bootloop`: completion line, then repeated `ESP-ROM:` banners with partial boots.
     - optional ANSI colour codes and `\r\n` line endings to exercise stripping.
   - *Verify (container), with `--espflash tests/fake_espflash.py`:* flash success → 200 with
     `app_seq` = the completion line's seq, `exit_code` null, and device lines from both boots
     appear in `/log` with increasing `seq` and `phase=running`; flash failure → 502 with the
     output tail and exit code, `phase=exited`; flash hang → 504 and the fake is killed;
     concurrent flash → 409; `/monitor/stop` during `flashing` → 409, during `running` → the fake
     is gone; `since` older than the buffer → `dropped`; long-poll returns early when a line
     arrives and returns empty after `wait`; monitor exit after completion → `running=false`,
     `phase=exited`, with its exit code; reset before any flash → 409; reset after a flash starts
     a new session and re-invokes the fake with the same temp-copy ELF path; ambiguous auto port
     (test injects the candidate list) → 503 listing candidates; ANSI/`\r` stripped; no orphaned
     fake processes after server shutdown.

4. **[devops] Snapshot.** `GET /snapshot.jpg` with C1 argv. Originally
   `-f avfoundation -framerate F -video_size WxH -i "<cam>:none" -vf select=gte(n\,W) -frames:v 1
   -f image2 -c:v mjpeg pipe:1`; **after spike 1c:** `-f avfoundation [-framerate F only if
   configured] -video_size 1280x720 -i "1:none" -vf select=gte(n\,W) -frames:v 1 -f image2pipe
   -c:v mjpeg pipe:1`, default `W = 20` (~2 s at the observed ~10 fps). The pipe form avoids the
   image2 `-update 1` warning seen with a single output file [VERIFY on the Mac in step 6;
   fallback: temp file with `-update 1`]. Camera lock, timeout, JPEG magic check, ffmpeg stderr
   logged on the host for every call.
   - *Verify (container), with `--ffmpeg tests/fake_ffmpeg.py`:* 200 + `image/jpeg` + bytes start
     with `FF D8`; fake failure → 503 with the stderr tail; timeout → 503 and the fake is killed;
     the argv builder test checks the camera/warm-up values and that `-framerate` is absent by
     default and present when configured; a fake that writes a "falling back to default" stderr
     line still returns 200, and the line appears in the host log.

5. **[devops] Container CLI.** `tools/bench/bench` with `health`, `flash`, `log`, `reset`, `snap`,
   the exit-code contract, and repo-root/token discovery.
   - *Verify (container):* end-to-end tests start `bench_host.py` on an ephemeral port with both
     fakes and `BENCH_URL` pointed at it, then check: `bench flash <fake elf> --until 'alive 0s'`
     → exit 0; `bench flash <fake elf>` with **no** `--until` → exit 0 on `alive 0s` (default);
     the fake's double boot → exit 0, output shows `--- boot 1 ---` and `--- boot 2 ---`, and the
     cut-off first boot does not trip `--fail-on`; `--until ''` → exit 0 right after flash
     success; a fake panic line → exit 1; `bootloop` → exit 1 (max boots); no marker → exit 2
     within the timeout; monitor dies → 3; wrong token → 4; ambiguous port (503) → 4 with the
     candidates printed; `flash_fail` → 5 with the espflash tail printed; `flash_hang` → 5;
     `bench reset` after a flash → exit 0 on the default marker; `bench snap` writes a JPEG and
     prints its path; an absolute in-repo path is converted to repo-relative; a marker that
     appears only in espflash's own output before `app_seq` does not match.

6. **[manual] Acceptance on the real bench.** The user starts
   `python3 tools/bench/bench_host.py --camera "<B1 device>"` in a Mac Terminal (B1: device `1`
   is the default; no `--port` needed if exactly one `/dev/cu.usbmodem*` is present), then the
   agent (or user) runs in the container:
   - `tools/bench/bench health` → exit 0, and `serial.chosen` is the board's port
     (`/dev/cu.usbmodem11401` on the spike's USB port);
   - `tools/bench/bench flash firmware/target/xtensa-esp32s3-none-elf/release/firmware --until
     'alive 0s' --timeout 60` → exit 0, and the log shows the boot from the first line
     (ROM banner, bootloader, `Embassy initialized!`), including the double boot without a
     false failure;
   - `tools/bench/bench reset --until 'ch422g: backlight on'` → exit 0 (a re-flash of the same
     image; record its duration);
   - `tools/bench/bench snap` → Claude `Read`s the JPEG and describes the panel correctly; confirm
     the `image2pipe` form works (step 4 VERIFY) and check the host log shows ffmpeg's stderr;
   - deliberately flash a bad path (`../README.md`) → rejected; stop the service → `bench health`
     exit 4.
   - Tune the camera mount, angle (avoid reflections and moiré), warm-up and size, and record the
     settings in the revision log. Spike 1c already showed window glare top-left and room
     reflections on the right of the panel: angle the camera/panel or shade the window before
     judging photos.
   - If a panic is observed at any point (no deliberate panic ELF is built for this step), record
     whether the S3 output shows a symbolised backtrace (Decision 2 VERIFY).
   - *Verify:* all of the above pass, and the times for flash→marker, reset→marker and snapshot
     are recorded.

7. **[devops] Skill and docs.**
   - `.claude/skills/board-verify/SKILL.md` covers:
     - when to use it (after any firmware change that would otherwise wait on the user);
     - preconditions (`bench health`; if it fails, fall back to asking the user, and say so);
     - procedure: build → `bench flash ... --until <marker> --timeout N` → on exit 1/2/3, report
       the log excerpt and stop → `bench snap --delay 1` → `Read` the image → judge it against an
       explicit expectation stated *before* looking (and, when available, a sim reference
       screenshot provided by the user, since there's no headless browser in the container);
     - report format: PASS/FAIL, ELF path + git rev, log excerpt, snapshot path;
     - rules: never claim a visual PASS without having read an image; don't commit `.bench/`;
       don't share images (privacy).
   - README "Flashing" gets a "Bench service (automated)" subsection: prerequisites
     (`brew install ffmpeg`), how to start it, the Camera permission note, the one-port-holder rule
     (`bench`/`POST /monitor/stop` before manual `espflash`), the security note, and the launchd
     option as "not recommended yet (TCC)" (B2). If B4 = (a), add the allow-list entry with the
     user's explicit OK. Post-spike additions: host prerequisite `python3 >= 3.11`; the
     `--port` auto-detect note (the `usbmodem…` name depends on the USB port, e.g. `usbmodem11401`);
     the existing manual flow's `--port /dev/cu.usbmodem101` updated to "your `usbmodem` port";
     manual commands avoid shell pipes, or use `command grep`/`command cat`, because the bench
     Mac aliases `grep`→`rg` and `cat`→`bat` (the service itself runs no shell and is
     unaffected); camera device listing via `ffmpeg -f avfoundation -list_devices true -i ""`
     (`-list_options` is not supported).
   - *Verify:* a fresh agent session that is only given the skill completes step 6's flash→snap
     sequence unaided; the README commands match what was run in step 6.

8. **[devops] [optional] `GET /stream.mjpg`** for a human in a Mac browser: one ffmpeg per client
   (`-f mpjpeg`), `multipart/x-mixed-replace`, killed on client disconnect, `?token=` accepted.
   - *Verify (container):* with the fake ffmpeg emitting two frames, the response has two
     multipart parts and the fake is gone after the client closes. **[manual]** The stream renders
     in Safari/Chrome at `http://127.0.0.1:7777/stream.mjpg?token=...`.

## Risks

| # | Risk | Likelihood / impact | Mitigation |
|---|------|---------------------|------------|
| R1 | `host.containers.internal` doesn't reach host loopback under applehv/libkrun | ~~Medium / high~~ **Resolved (spike 1a)** | Step 1a checked it: it works (via `192.168.127.254`). T2 not needed. Side effect recorded under Security: requests arrive as `127.0.0.1`, so the token is the only discriminator. Not opening a non-loopback port stays a hard rule. |
| R2 | `espflash monitor` won't run without a TTY, or doesn't reset on attach, so early boot lines are lost | ~~Medium / medium~~ **Materialised, resolved by S3 (spike 1b)** | The monitor ran with piped stdout and did reset the chip, but its first app line was `alive 6s`: ~6 s of boot (i2c scan, `Embassy initialized!`, `ch422g: backlight on`, `alive 0s`) lost. Switched to S3 (single `flash --monitor --non-interactive` process), which captured everything from the ROM banner. `POST /reset` also uses S3 for this reason. The `alive 0s` default marker was chosen for this risk and is kept (see Container CLI). |
| R3 | Native USB-Serial-JTAG re-enumerates on reset (device node disappears briefly) and the monitor exits | Medium / medium | Observed indirectly in spike 1b: after a reset by a separately attached `espflash monitor`, ~6 s of output were lost before the monitor saw data, consistent with re-enumeration plus reconnect (esp-println drops output while no host has the port open). Under S3, espflash survived both observed resets (`rst:0x15` twice) without losing lines, so the flash path is covered. Remaining exposure: resets not driven by espflash (panic reboot, RST button, watchdog) may lose up to ~6 s of the next boot, or end the process. The panic text itself is printed before the reset, so `--fail-on` still sees it. The monitor exit is visible (`running=false`, CLI exit 3); `POST /reset` recovers. Bounded restart-on-exit stays deferred until step 6 shows the need. |
| R4 | macOS Camera permission (TCC) denied or attributed to the wrong app; under launchd no prompt at all | ~~High on first run~~ Low now / medium | Spike 1c: permission was already granted to the terminal app; capture worked. The 503 body carries ffmpeg's stderr. launchd is deferred (B2) and would re-open this risk. |
| R5 | Photos of an LCD are hard to judge: auto exposure/white balance drift, moiré, reflections, focus | High / medium; **mildly materialised (spike 1c)** | Spike 1c photo: sharp and well exposed, the colour-bar image clearly judgeable, but window glare top-left (over the green/blue bars) and room reflections on the right of the panel. Fixed mount and angle (plus shading the window), warm-up frames, and a judging rubric of structural checks ("backlight on, no full-screen noise, layout regions present") rather than pixel-exact comparison. ffmpeg's avfoundation input can't lock exposure/WB [VERIFY]. A UVC control tool or the boot test pattern (B3) are follow-ups if needed. Step 6 records the tuning. |
| R6 | The service and the user's manual `espflash` fight over the port | Medium / low | Documented one-holder rule, `POST /monitor/stop`. espflash's "port busy" error is clear. |
| R7 | Host python version: the Xcode CLT `python3` may be 3.9; the container tests run on 3.13 and wouldn't catch newer syntax | ~~Medium / low~~ **Low (relaxed after spike)** | Original mitigation, now dropped: "Code `bench_host.py` to 3.9 (no `match`, `from __future__ import annotations`, no `tomllib`)". The bench Mac has python 3.15.0, so the binding constraint is the container's 3.13 that runs the tests. New rule: target **>= 3.11** syntax/stdlib for host service and CLI; the README states host `python3 >= 3.11`; the startup check prints the version and refuses to start below 3.11. Remaining gap: a 3.15 host could behave differently from the 3.13 test run (deprecations/removals); step 6 is the check. |
| R8 | The capability itself: anything that can read `.bench/token` can flash the board and photograph the room | Certain (by design) / medium | Loopback only, token, Origin rejection, ELF prefix allowlist, temp copy, no shell, the service runs only while started by hand. Images and logs live in gitignored `.bench/`. B4 keeps the user in control of unattended use. |
| R9 | Host and container espflash versions differ (flag names, output format) | Medium / ~~low~~ **medium** (S3 depends on output wording) | `/health` reports the host espflash version (4.6.0 at spike 1b). The argv builders sit in one place. Flash success depends on the literal line `Flashing has completed!`; if an upgrade rewords it, flashes end in 504 (timeout) rather than a false success, and the string is one constant. Observed flag values on 4.6.0: `monitor --before` = `default-reset`, `no-reset`, `no-reset-no-sync`; `flash --after` = `hard-reset`, `no-reset` (list possibly truncated). `flash --after no-reset` without `--monitor` printed a harmless WARN about monitor options being ignored; not used under S3. |
| R11 | Serial port auto-detect picks the wrong device or none: the name changes with the USB port/hub, or a second `usbmodem` device (e.g. the board's CH343 UART port, if also cabled, or another dev board) is present | Medium / low | Auto mode accepts exactly one `/dev/cu.usbmodem*` match and otherwise errors with the candidate list (startup, `/health`, flash/reset 503). `--port` overrides. Filtering by VID 303A would need `ioreg`/`system_profiler` parsing; deferred until ambiguity actually happens. |
| R12 | Camera settings silently ignored (spike 1c: `-framerate 30` fell back to the device default ~10 fps; pixel format overridden to `uyvy422`) | Certain / low | Don't pass `-framerate` by default; warm-up expressed in frames sized for ~10 fps (default 20 ≈ 2 s); ffmpeg stderr logged on every snapshot so fallbacks are visible; JPEG magic check catches outright failure. |
| R10 | Privacy: snapshots/logs show live traffic and the room | Medium / medium | `.bench/` is gitignored. The skill forbids sharing or committing them. Same rule as the README simulator note. |

Open questions: ~~B1–B5~~ (decided, see revision log); ~~the [VERIFY] items in step 1~~ (answered
by the spike); remaining [VERIFY] items: `image2pipe` snapshot form (step 4/6), S3 backtrace
symbolisation (Decision 2, step 6), avfoundation exposure/WB locking (R5), launchd TCC (B2).

Deliberately deferred: boot test pattern (B3), launchd agent (B2), exposure/WB locking, automatic
comparison against a sim reference render (needs a headless browser and a capture of the wasm sim,
which is its own feature), monitor auto-restart (R3, only if step 6 shows the need), serial
auto-detect by USB VID (R11), a deliberate-panic ELF to test backtrace decoding.

## Revision log

- 2026-10-23: initial draft. Changes from the earlier proposal: regex matching moved from the host
  to the CLI (host serves sequence-numbered lines with long-polling); flash split into
  `espflash flash --after no-reset` + a long-running `espflash monitor` that resets on attach,
  for a clean flash exit code and a complete boot log; token distributed via the gitignored
  `.bench/token` on the bind mount; validated ELF copied to a host temp file before flashing
  (TOCTOU); `/stream.mjpg` made optional (step 8); file spool (T2) named as the transport fallback;
  boot test pattern and launchd deferred.
- 2026-10-23: plan approved by user. Decisions: B1 = (a) Logitech HD USB webcam, avfoundation
  device index `1` (default for `--camera`, still configurable); B2 = (a) manual start in a Terminal
  tab; B3 = (b) boot test pattern deferred; B4 = (a) add `Bash(tools/bench/bench:*)` to
  `.claude/settings.local.json` in step 7 (user-approved); B5 = port 7777 as the default, configurable
  (`--port` on the host, `BENCH_URL` in the container). Orchestration change: steps 2-5, 7, 8 are local
  tooling and go to `senior-engineer`, not `devops-engineer`. Step 2 does not depend on step 1's
  outcome and runs in parallel with the user's step-1 spike; a fallback forced by step 1 revises this
  doc before step 3.
- 2026-10-23: **step-1 spike results (1a, 1b, 1c, host python); step 1 closed.** Raw results:
  - 1a: Mac `python3 -m http.server --bind 127.0.0.1 7777`; in the container
    `host.containers.internal` → `192.168.127.254`, curl 200; the host server logged the client as
    `127.0.0.1`.
  - 1b: host espflash 4.6.0; port `/dev/cu.usbmodem11401`. `flash --after no-reset` → exit 0,
    "Staying in bootloader", panel dark, harmless WARN "Monitor options were provided, but
    `--monitor/-M` flag isn't set" (no espflash env/config on the host). `espflash monitor
    --non-interactive --elf ... | cat` → connected, reset the chip, first app line `alive 6s` (~6 s
    lost). `flash --monitor --non-interactive ... | cat` → "Flashing has completed!", ROM banner
    `ESP-ROM:esp32s3-20210327` / `rst:0x15 (USB_UART_CHIP_RESET)`, `I (27) boot: ...`, `INFO - ...`;
    first boot cut off after `INFO - psram: fb ptr=...` by a second `rst:0x15`, second boot ran
    through `Embassy initialized!`, `ch422g: backlight on`, `alive 0s`, `alive 1s`. Re-flash of an
    identical image reached app output ~1 s after `Connecting...`. Flag values: `monitor --before`
    default-reset/no-reset/no-reset-no-sync; `flash --after` hard-reset/no-reset (possibly
    truncated). Host shell aliases `grep`→`rg`, `cat`→`bat`.
  - 1c: Homebrew ffmpeg 9.0.2; device `1` = Logitech HD USB webcam; Camera permission already
    granted. `-list_options true` → "Unrecognized option". `-framerate 30 -video_size 1280x720` →
    "Configuration of video device failed, falling back to default", captured 1280x720 at ~9.92 fps;
    pixel format overridden to `uyvy422`; image2 single-file warning about `-update 1`; 30 warm-up
    frames ≈ 2.91 s; JPEG 33 KiB, sharp, well exposed, colour-bar image judgeable; window glare
    top-left and room reflections on the right.
  - Host python3 3.15.0.

  Changes (nothing removed outright; superseded text is struck through or quoted where noted):
  - Status line: "draft, awaiting user review" (stale since the approval entry) → "approved;
    step 1 (spike) done, revised with its results".
  - Context: port fact annotated with the observed `usbmodem11401` and that the name is
    USB-port-dependent; new bullet summarising the spike findings.
  - Options: T1, S1, S3, C1 rows annotated with the spike outcome (S1 rejected, others confirmed).
  - Decision 1: T2 fallback sentence struck through (not needed); the core/front-end split kept,
    now justified by testability; token-is-the-only-discriminator note added.
  - **Decision 2 changed from S1 to S3.** Removed: the S1 two-process design (`flash --after
    no-reset` + separate `espflash monitor --elf`, `/reset` = restart the monitor, "S3 fallback /
    S2 fallback, step 1 decides"). Replaced by: one `flash --monitor --non-interactive` process per
    session; flash success = `Flashing has completed!` line; exit before it = flash failure, after
    it = monitor died; `/reset` = re-run with the last ELF copy; `espflash monitor` unused;
    backtrace symbolisation via the positional ELF marked [VERIFY]; double boot benign.
  - Decision 3: C1 details from spike 1c (no `-framerate` by default, 1280x720, warm-up 20
    frames, `image2pipe` to stdout, ffmpeg stderr always logged).
  - "Why": S1 rationale struck through and replaced with the S3 rationale; T1 marked checked.
  - Consequences: prerequisites updated (python >= 3.11, espflash 4.6.0, ffmpeg 9.0.2, Camera
    already granted); new bullets: `/reset` is a re-flash, dependency on the espflash wording,
    the service's espflash keeps holding the port after a flash.
  - API: `/health` flat `port, port_present` **replaced** by a `serial{mode, chosen, present,
    candidates}` object, plus `python_version` and `monitor.phase`. `/flash` rewritten for S3
    (waits for the completion line; new `app_seq`; `exit_code` null on success; new 503 for
    ambiguous port and 504 for timeout). `/reset` rewritten (re-flash with last copy, was
    "restart the monitor"). `/monitor/stop` now kills the espflash process and returns 409 while
    flashing. `/log` gains `app_seq` and `phase`. `/snapshot.jpg`: ffmpeg stderr logged always.
  - Host config: serial `--port` auto-detect re-resolved on every flash/reset; `--camera` default
    changed from `0` to `1` (B1, was already decided in the approval entry but not reflected
    here); `--video-size` default 1280x720; `--framerate` not passed by default; new `--warmup`
    (default 20); startup prints versions. **Clarification:** the approval entry's "B5 ... `--port`
    on the host" meant the service port, which is `--listen`; `--port` is the serial device.
  - Container CLI: `bench flash`/`bench reset` default `--until 'alive 0s'` (was optional, no
    default); matching starts after `app_seq`; new exit code **5** (flash failed / timed out);
    exit 4 widened to request errors incl. 409/503; new `--max-boots` (default 3, boot loop →
    exit 1); new "Boots and the double boot" subsection (boot counting on `ESP-ROM:`, `--until`
    matches any boot, a cut-off boot is not a failure, R2 history of the default marker).
  - Security: new bullet "the token is the only caller discriminator" (requests arrive as
    `127.0.0.1`; no address allowlist; other containers with the repo mounted count as trusted);
    serial port never taken from a request; residual-risk bullet extended to other containers.
  - Layout: `fake_espflash.py` description updated to the S3 imitation; python target >= 3.11
    note added.
  - Plan step 1: outcome block added; original commands kept as run. The 1c [VERIFY] on rejected
    framerate/size combinations is answered: avfoundation falls back to the device default
    instead of erroring.
  - Plan step 2: post-spike argv builders (S3 only, ffmpeg without `-framerate`, `image2pipe`),
    `resolve_port` pure function, `/health` serial fields, python >= 3.11; if step 2 already
    landed with S1 builders, the change moves to the start of step 3. Verify list extended.
  - Plan step 3: rewritten for one S3 process per session with phases and `app_seq`; full
    `fake_espflash.py` mode list (ok with double boot, flash_fail, flash_hang, panic, die,
    silent, bootloop, ANSI/CRLF); verify list extended (502/504, 409 on stop-while-flashing,
    reset re-invokes with the same copy, ambiguous port 503). Removed: "flash failure → … no
    monitor" wording (under S3 there is no separate monitor to not start).
  - Plan step 4: argv updated per spike 1c (original argv kept in the text for reference);
    `image2pipe` [VERIFY] with `-update 1` temp-file fallback; verify list extended.
  - Plan step 5: verify list extended (default marker, double boot, `--until ''`, bootloop,
    exit 5 for `flash_fail`/`flash_hang`, 503 → 4, reset, pre-`app_seq` text not matched).
  - Plan step 6: port auto-detect check, full-boot and double-boot check, reset timing,
    `image2pipe` check, glare/reflection tuning note, opportunistic backtrace check.
  - Plan step 7 (outside the requested 2-6, but a direct consequence of the spike): README
    additions (python >= 3.11, port naming, `command grep`/`command cat` or no pipes because of
    the host's `rg`/`bat` aliases, `-list_devices` not `-list_options`).
  - Risks: R1 resolved; R2 materialised and resolved by S3; R3 updated with the observed ~6 s
    gap and the remaining exposure; R4 lowered (permission already granted); R5 mildly
    materialised (glare/reflections); R7 relaxed — **removed** the 3.9 coding rule, replaced by
    >= 3.11 with a startup version check; R9 impact raised to medium (output-wording dependency)
    with the observed flag values; new R11 (serial auto-detect) and R12 (silently ignored camera
    settings).
  - Open questions / deferred: B1–B5 and step-1 VERIFYs marked closed; remaining VERIFYs listed;
    deferred list gains VID-based port detection and a deliberate-panic ELF.
  - Step numbering 1-8 unchanged.

# Board bench: automated flash, serial log and webcam check from the container

Status: **approved; step 1 (spike) done, revised with its results; rev 3: tools run via uv;
rev 4: step-6 acceptance run 1 findings (snapshot framerate, controlling-terminal defect); ~~step 6
in fix loop 1 of 2, acceptance run 2 pending.~~ rev 5: step-6 acceptance run 2 results (all
checks pass except camera selection: index `1` captured the wrong camera); ~~step 6 in fix loop 2
of 2 (camera by name), short acceptance run 3 pending;~~ step 8 (`/stream.mjpg`) now in scope
for a demo, with its own 640x480 @ 30 capture settings (pending user confirmation). rev 6: QA of
fix loop 2 found the snapshot still used the configured index; camera is now selected by name
only (numeric `--camera` refused); step 6 in a user-authorised fix loop 3 (budget was 2),
short acceptance run 3 pending. rev 7: step 8 (`/stream.mjpg`) in fix loop 1 after QA (token
leak in the access log via URL-encoded `?%74oken=`; stalled ffmpeg kept a slot and the camera);
user decision: the stream is an **anonymous, read-only local view** (no token; Host-header
guard against DNS rebinding); `?token=` removed everywhere.**
Date: 2026-10-23.
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
| B1 | **Which camera?** | (a) UVC USB webcam on a fixed mount. (b) Mac built-in camera. | **(a)**. A built-in camera means the laptop lid is the camera mount and blocks video calls while the bench runs. The design handles both: the avfoundation device is a host-side setting (`--camera`, ~~name or index~~ *rev 6:* device name only). *Rev 5:* decided (a) = **"HD Pro Webcam C920"** (Logitech). ~~Device index `1`~~ was the C920 at spike time, but avfoundation indices are **not stable** (Continuity iPhone cameras come and go; acceptance run 2 captured a room camera with index `1`), so the default ~~and the recommended setting~~ is the **device name** (R17). *Rev 6:* the name is the **only** way to select the camera; a numeric `--camera` is refused at startup. | Step 1 (device name), step 6 |
| B2 | **How is the host service started?** | (a) Manually in a macOS Terminal tab (`python3 tools/bench/bench_host.py ...`; since rev 3 `uv run tools/bench/bench_host.py ...`). (b) launchd user agent (`~/Library/LaunchAgents/*.plist`). | **(a) for now.** Camera permission (TCC) is granted to the app that launches ffmpeg (Terminal/iTerm). Under launchd there is no such app and the grant is unreliable [VERIFY]. (b) is documented as a later option. | Step 7 |
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
  other Python packages are installed, and none are needed by this design. *Rev 3:* `uv` is not
  installed in the container (`which uv` → not found) and there is no `pyproject.toml`,
  `uv.toml` or `.python-version` in the repo. Devcontainer tooling is installed by local features
  under `.devcontainer/features/` (esp-rs, neovim, wasm-sim) with pinned versions as options;
  named volumes persist the cargo registry and the Claude config, and `post-create.sh` chowns
  them because they are created root-owned.
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

### Running the tools: uv (rev 3)

User request: run both tools with uv. Packaging is fixed by that request: both scripts are uv
scripts with PEP 723 inline metadata. The open choices are how uv gets into the devcontainer and
where its cache and managed pythons live.

| # | Option | Trade-off |
|---|--------|-----------|
| U1 | **New local feature `./features/uv`** with a pinned `version` option that downloads the official static (musl) release tarball from GitHub, checks its published `.sha256`, and installs `uv`/`uvx` to `/usr/local/bin`. | Same pattern as `./features/neovim` (official release binary, version as option); explicit pin and checksum; ~25 lines of shell we own. |
| U2 | Third-party devcontainer feature from ghcr (a community `uv` feature). | No code, but another third-party publisher in the build's trust chain, and its pinning/checksum behaviour has to be audited. |
| U3 | Astral's installer script in `post-create.sh` (`curl -LsSf https://astral.sh/uv/<ver>/install.sh \| sh`). | Short, but `curl \| sh` at every container create, installs to `~/.local/bin` (user dotfiles/PATH dependent), and breaks the "tooling lives in features" pattern. |
| V1 | **Named volume** `esp32-adsb-uv` at `/home/vscode/.uv`, with `UV_CACHE_DIR=/home/vscode/.uv/cache` and `UV_PYTHON_INSTALL_DIR=/home/vscode/.uv/python`. | Managed python 3.11 (~tens of MiB) and the cache survive rebuilds, so the canonical test command works offline after the first run; same pattern as the cargo registry volume (incl. the post-create chown). One more volume to know about. |
| V2 | Default locations (`~/.cache/uv`, `~/.local/share/uv/python`) in the container layer. | Nothing to configure, but every rebuild re-downloads the managed 3.11 before the tests can run, and an offline rebuild can't run the canonical tests. |

## Decision (ADR)

- **Context:** as above. The agent needs flash + log + photo from inside the container. USB can't
  enter the container, and the host has to stay safe even though the service can flash hardware
  and run processes.
- **Options considered:** transport T1–T4, serial S1–S3, camera C1–C2, uv install U1–U3 and uv
  storage V1–V2 (tables above).
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
  3. **C1, one-shot ffmpeg** with configurable warm-up frames. ~~*Spike 1c:* the webcam ignores
     `-framerate 30` (ffmpeg logs "Configuration of video device failed, falling back to default"
     and captures at the device default ~10 fps), so the default argv **does not pass
     `-framerate`**~~ *Rev 4 (acceptance run 1):* that reading of spike 1c was wrong. With
     `-framerate 30` the device logged a fallback warning **but captured**; **without**
     `-framerate`, avfoundation requests 29.97 fps, which the webcam rejects ("Selected framerate
     (29.970030) is not supported by the device", supported: 15 and 30 fps at every listed size),
     and ffmpeg exits 251 ("Input/output error"). So the default argv **passes `-framerate 30`**
     (before `-video_size`; host config `--framerate`/`BENCH_FRAMERATE`, `0` or empty omits it);
     `-video_size 1280x720` is passed (it worked). Default warm-up is **20 frames**
     (~2 s at ~10 fps). The single frame goes to stdout via `-f image2pipe -c:v mjpeg pipe:1` (no
     `-update 1` temp file needed). ffmpeg's stderr is always logged on the host, because
     unsupported settings fall back silently. *Rev 5 (acceptance run 2):* with `-framerate 30`
     the C920 still logs "Configuration of video device failed, falling back to default" and
     really runs at **~10 fps**, but it captures (the 29.97 fps hard failure is gone); pixel
     format `uyvy422` is auto-selected ("Selected pixel format (yuv420p) is not supported" is a
     benign override); `image2pipe` works (30 KiB JPEG, ffmpeg elapsed 1.91 s). So the 20-frame
     warm-up (~2 s) stays. **Frame rate (rev 5, user request):** the user wants a real
     camera frame rate (snapshots and the step-8 stream). ~~Hypothesis: the default `yuv420p`
     request fails the device configuration, which triggers the fallback to ~10 fps. New
     optional host setting `--pixel-format`/`BENCH_PIXEL_FORMAT`, passed as `-pixel_format <fmt>`
     before `-i` for snapshot **and** stream; default **TBD by the Mac experiment (candidate
     `uyvy422`)**; empty omits it. If the device then really runs at 30 fps, the warm-up is
     re-tuned or made time-based (R12, step 8a).~~ **Disproven by the Mac experiment** (C920,
     ffmpeg 9.0.2, `-t 5 -f null -`): 1280x720 @ 30 with `-pixel_format` `uyvy422`, `nv12` or
     `yuyv422` → still "configuration failed", 10 fps; 1280x720 @ 15 → failed, 10 fps;
     1920x1080 @ 30 → failed, 4.9 fps; **640x480 @ 30 → no failure, 145 frames in 5 s
     (fps=31)**. Interpretation: avfoundation receives uncompressed YUV from the C920 over
     USB 2.0, so the rate is bandwidth-capped by resolution (720p ~10 fps, 1080p ~5 fps; 480p
     fits 30 fps); the pixel format is irrelevant and **no `--pixel-format` option** is added.
     Decision (orchestrator, **to be confirmed by the user** at the checkpoint): **snapshots keep
     1280x720 with `-framerate 30`** (falls back to ~10 fps, sharp, ~2 s with the 20-frame
     warm-up — unchanged, warm-up unchanged); **the stream gets its own settings**
     `--stream-size` (default `640x480`) and `--stream-framerate` (default `30`), so the demo is
     smooth at a real 30 fps. **Camera selection (rev 5):** the camera is chosen **by
     avfoundation device name**, default `"HD Pro Webcam C920"` (B1), because indices are not
     stable (R17). At startup the host lists the avfoundation video devices
     (`ffmpeg -f avfoundation -list_devices true -i ""`, stderr parsed, video section only) and
     **refuses to start** if the configured name is missing, printing the available devices. ~~An
     all-digit `--camera` is still accepted as an index, with a stderr warning naming the device
     it currently maps to; an out-of-range index is refused. Any device whose name starts with
     `Capture screen` is refused, whether selected by name or by index (screen capture is not a
     camera; Security).~~ **Camera selection (rev 6, user decision after QA of fix loop 2): by
     name only.** Numeric avfoundation indices are **not supported at all**, because they shift
     as Continuity cameras come and go. A `--camera`/`BENCH_CAMERA` value that consists only of
     digits (`str.isdigit()`, so non-ASCII digits such as `١` or `１` count too) is **refused at
     startup with exit 2**, a "use a device name" message and the device list; no index lookup
     is attempted. Device names are kept **exactly as ffmpeg prints them** (the text after
     `[N] ` up to the line ending; case-sensitive comparison, no trimming, case folding or
     Unicode normalisation), and the configured value must equal one of them. A device whose name
     is **exactly** `Capture screen <digits>` (`re.fullmatch(r"Capture screen [0-9]+", name)`,
     not a prefix match) is refused (screen capture is not a camera; Security). **Capture (the
     snapshot and the step-8 stream) always passes the resolved device name** to ffmpeg
     (`-i "<name>:none"`), never an index; the parsed index is not used for capture at all.
     A `GET /stream.mjpg` for a human in
     a browser on the Mac is ~~an **optional last step**~~ (rev 5: **in scope**, user request for a
     demo that shares the webcam output) step 8, with one ffmpeg per connected
     client (`-f mpjpeg` pass-through, see step 8). The human can also just open Photo Booth or
     `ffplay`.
  4. **Container CLI `tools/bench/bench`** (Python stdlib, `urllib`; a uv script since rev 3,
     see 8). Regex matching for
     `--until`/`--fail-on` runs **in the container, on the CLI side**. The host only serves lines by
     sequence number with long-polling. That keeps regex evaluation (and any ReDoS) out of the host
     process and keeps the host API dumb.
  5. **Shared secret via the bind mount:** at startup the host writes a random token to
     `.bench/token` (mode 0600, `.bench/` gitignored). The CLI reads it from the same repo path. No
     copy/paste, and only the host user (and so the container user, via `keep-id`) can read it.
  6. **Project skill `.claude/skills/board-verify/SKILL.md`** encodes the agent procedure:
     health → flash → wait for marker / fail on panic → snapshot → judge → report with evidence.
  7. The boot test pattern (B3) and launchd (B2) are **deferred**.
  8. **Both tools are uv scripts** (rev 3). `bench_host.py` and `bench` start with
     ```
     #!/usr/bin/env -S uv run --script
     # /// script
     # requires-python = ">=3.11"
     # dependencies = []
     # ///
     ```
     They **stay stdlib-only**. uv would make third-party dependencies cheap, but stdlib-only remains
     a deliberate decision: no PyPI fetch on the host or behind the allow-listed CLI command, no
     dependency pinning or audit, and the scripts still run under a plain `python3 >= 3.11`
     (fallback). Adding a dependency needs a revision of this doc.
     - Host start: `uv run tools/bench/bench_host.py ...` from the repo root. This decouples the
       service from the Mac's system/Homebrew python: uv picks an installed interpreter that
       satisfies `>=3.11` (3.15.0 on the bench Mac was expected; rev 4: uv actually picked
       3.12.12 in acceptance run 1, see R15) or downloads a managed one. Host prerequisite:
       uv (`brew install uv`). The startup check that refuses python < 3.11 stays, for the
       `python3 tools/bench/bench_host.py` fallback.
     - Container: invocation stays `tools/bench/bench ...`; the shebang hands it to uv. So the
       planned allow-list entry `Bash(tools/bench/bench:*)` (B4) still matches unchanged. The
       container's python 3.13 satisfies `>=3.11`, so the CLI never needs a download.
     - Tests: canonical command
       `uv run --python 3.11 python -m unittest discover -s tools/bench/tests -t tools/bench`
       (uv fetches a managed 3.11 if needed), so the `>=3.11` claim is exercised, not just stated.
       Each step's verify also runs the suite on the container default python
       (`python3 -m unittest discover -s tools/bench/tests -t tools/bench`, 3.13): it's cheap,
       catches deprecation warnings newer than 3.11, and is the same command as the no-uv fallback.
       Tests that start the tools as subprocesses use `[sys.executable, <script>]`, never the
       shebang, so the subprocess runs on the pinned interpreter and the tests don't depend on uv
       being on `PATH`. The fakes (`fake_espflash.py`, `fake_ffmpeg.py`) are test fixtures, not
       uv scripts. If a `pyproject.toml` ever appears at the repo root, add `--no-project` to the
       uv test command.
     - Devcontainer: **U1 + V1** (exact change in plan step 2a). U1 matches the existing local
       feature pattern with a pinned version and a checksum, and keeps third-party publishers and
       `curl | sh` out of the build. V1 matches the cargo-registry precedent and makes the
       canonical test command work offline after a rebuild. Cache and python install dir are
       siblings (not nested), so `uv cache clean` doesn't delete the managed pythons.
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
  - uv (rev 3, user request): one start command independent of which python the Mac has on
    `PATH`, and a test run that actually uses the minimum supported version. U1/V1 reasons are
    under Decision 8.
- **Consequences:**
  - New host prerequisites: **uv** (`brew install uv`; rev 3), which runs the service on a python
    **>= 3.11** (bench Mac: 3.15.0; was "3.9+ if using the Xcode CLT python", relaxed in the
    step-1 revision, see R7; plain `python3 >= 3.11` still works as a fallback), `espflash` (already required; 4.6.0 on the bench Mac), `ffmpeg`
    (`brew install ffmpeg`; 9.0.2 on the bench Mac), and macOS Camera permission for the
    terminal app that starts the service (already granted on the bench Mac).
  - `POST /reset` is a re-flash of the identical image, not a bare reset. It costs ~1 s extra and
    writes nothing when the image is unchanged, but it holds the flash lock like a flash.
  - The service only works with espflash output that contains `Flashing has completed!`. An
    espflash upgrade that rewords it breaks flash-success detection loudly (timeout/502), not
    silently.
  - **A process in the container can now flash the board and take photos of the room** in front
    of the camera without a human in the loop. Mitigations: the security section and B4. *Rev 5:*
    with a wrong camera index it could also photograph a person or capture the screen (run 2,
    R17); camera by name, startup check and screen-capture refusal close that (rev 6: by name
    only, numeric values refused, capture uses the resolved name). *Rev 7:* while the service
    runs, **any local process and any podman container can watch the live camera** via the
    unauthenticated `GET /stream.mjpg` (no token needed); the user accepted this explicitly
    (Security, R18). Flashing and snapshots still need the token.
  - New container prerequisite (rev 3): uv, installed by the new local feature `./features/uv`;
    needs a container rebuild (user action). Until then the tests run with `python3 -m unittest`
    (3.13) and the CLI is called as `python3 tools/bench/bench ...`.
  - Webcam images and serial logs may show live traffic (adsb-display privacy note) and the room.
    They go to the gitignored `.bench/` and must not be committed or shared.
  - The manual README flow keeps working. The service is optional and must be stopped (or
    `POST /monitor/stop`) before the user runs `espflash` by hand, because only one process can
    hold the port. After a flash, the service's espflash process keeps holding the port (monitor
    phase) until the next flash/reset, `POST /monitor/stop`, or shutdown.

### Host service API (all JSON unless noted)

All requests need `Authorization: Bearer <token>`, **except `GET /stream.mjpg`** (rev 7: no
auth, see Security). The token is compared with `hmac.compare_digest`. ~~Only `GET /stream.mjpg`
also accepts `?token=`, for the browser.~~ Rev 7: **no endpoint accepts a token in the query
string**; a `?token=` (in any spelling) is ignored, so a token-protected endpoint called with
only a query token answers 401. The access log never logs query strings (path only).

| Endpoint | Behaviour |
|----------|-----------|
| `GET /health` | `{ok, service_version, python_version, serial:{mode, chosen, present, candidates}, espflash_version, ffmpeg_present, camera, monitor:{running, phase, exit_code}, busy}`. `serial.mode` = `fixed` (`--port` given) or `auto`; `chosen` = the port the next flash would use (`null` if auto-detect is ambiguous); `candidates` = current `/dev/cu.usbmodem*` matches; `present` = `chosen` exists. (Replaces the earlier flat `port, port_present` fields.) |
| `POST /flash` `{"elf": "<repo-relative path>"}` | Validate (see Security) → copy to a host temp file → resolve the serial port (auto mode: exactly one candidate, else **503** with the candidate list) → stop any running espflash process → start `espflash flash --monitor --non-interactive` (S3) as a new `session`. The request waits until `Flashing has completed!` appears (→ **200**), the process exits first (→ **502**), or 300 s pass (→ process killed, **504**). Returns `{ok, exit_code, output_tail, session, log_seq, app_seq}`. `log_seq` = first sequence number of this session (espflash's own output included); `app_seq` = seq of the `Flashing has completed!` line (absent on failure); `exit_code` is `null` on success (process still running as monitor). **409** if another flash or reset is running (one global lock), **400** on bad path. |
| `POST /reset` | Re-run the S3 command with the **last flashed ELF copy** (re-flash of an identical image, ~1 s; full boot log). Same response and status codes as `/flash`. 409 if busy, 409 if nothing has been flashed since startup. |
| `POST /monitor/stop` | Kill the espflash process and release the serial port (for manual use by the user). **409** while the session is still in the `flashing` phase (killing mid-write leaves a half-written image). |
| `GET /log?since=N&wait=S&max=M` | Lines with `seq > N`, long-polled up to `S` seconds (cap 30). Returns `{session, app_seq, lines:[{seq, t, text}], next, dropped, monitor:{running, phase, exit_code}}`. `phase` = `flashing` (before `Flashing has completed!`), `running` (after it, process alive), `exited`. `dropped=true` if `N` is older than the oldest buffered line. Ring buffer: 20 000 lines. Lines are UTF-8 decoded with `errors="replace"`, with ANSI escapes and trailing `\r` stripped. The host does no other parsing; boot counting and marker matching happen in the CLI. Rev 4: successful (2xx) `/log` long-polls are **not** written to the host's access log (log noise seen in acceptance run 1); non-2xx responses still are. |
| `GET /snapshot.jpg?warmup=N` | One-shot ffmpeg (timeout 20 s), returns `image/jpeg`. The output is checked for JPEG magic (`FF D8`). **503** if ffmpeg fails (the body has the stderr tail, which is where TCC denials show up). ffmpeg's stderr is also logged on the host for **every** snapshot, because unsupported framerate/size settings fall back to the device default with only a log line (spike 1c), or are rejected outright with the supported modes listed in stderr (rev 4, acceptance run 1: 29.97 fps). The camera lock is separate from the flash lock, so a snapshot can run during a flash. |
| `GET /stream.mjpg` | Step 8, ~~optional~~ in scope since rev 5. `Content-Type: multipart/x-mixed-replace; boundary=<b>`; each part `Content-Type: image/jpeg` + `Content-Length`. One ffmpeg per client (`-f mpjpeg -boundary_tag <b>`, bytes passed through; `--stream-size` 640x480, `--stream-framerate` 30), killed (process group) on client disconnect. ~~Auth: bearer **or** `?token=` (this endpoint only);~~ Rev 7: **no auth** (anonymous read-only camera view, user decision; any bearer header or `?token=` is ignored); **Host header must be exactly `127.0.0.1:<port>`, `localhost:<port>` or `[::1]:<port>`, else 403** (DNS-rebinding guard, this endpoint only); `Origin` header allowed (this endpoint only). The pump `select`s on the ffmpeg pipe **and** the client socket (~1 s tick), so a disconnect is noticed even while ffmpeg sends nothing; a **no-frame watchdog** (~10 s without pipe data) ends the response, kills the process group and frees the slot. At most **2** concurrent stream clients, **409** beyond. Does not take the snapshot camera lock (concurrent device use [VERIFY], step 8). |

Host config (CLI flags of `bench_host.py`, with env var equivalents): `--port` (serial device;
default: **auto-detect** = the single `/dev/cu.usbmodem*` match; if there are zero or several, it
errors and lists them. Auto mode is checked at startup (fail fast with the list) **and re-resolved
on every flash/reset**, because the `usbmodem…` name depends on the USB port/hub — spike 1b saw
`usbmodem11401`, not the README's `usbmodem101`. A given `--port` is used as is.),
`--camera` (env `BENCH_CAMERA`; avfoundation ~~name or index~~ device name (rev 6: name only),
default ~~`1` per B1~~ **`"HD Pro Webcam C920"`** (rev 5, B1/R17); checked against the device
list at startup: a missing name → refuse to start and print the available video devices; ~~an
all-digit value → index, accepted with a stderr warning naming the device it maps to now, refused
if out of range; `Capture screen…` devices refused by name or index~~ rev 6: an all-digit value
(any Unicode digits) → refused, exit 2, "use a device name" plus the device list; exact
case-sensitive match against the names as ffmpeg prints them; a name that is exactly
`Capture screen <digits>` → refused; snapshot and stream capture use the resolved name),
`--video-size` (default `1280x720`; snapshots), rev 5 stream
settings (step 8): `--stream-size` (env `BENCH_STREAM_SIZE`, default `640x480` — the largest
tested size the C920 delivers at a real 30 fps via avfoundation), `--stream-framerate` (env
`BENCH_STREAM_FRAMERATE`, default `30`), `--stream-clients` (max concurrent `/stream.mjpg`
clients, default `2`); ~~`--pixel-format` (env `BENCH_PIXEL_FORMAT`, `-pixel_format <fmt>`
before `-i`, default TBD, candidate `uyvy422`)~~ dropped in rev 5 (experiment: pixel format
doesn't change the rate),
`--framerate` (env `BENCH_FRAMERATE`; default **`30`**, passed as `-framerate 30` before
`-video_size`; `0` or empty omits the flag. ~~default: not passed; the webcam ignores 30 and uses
~10 fps anyway, spike 1c~~ — rev 4: without it avfoundation asks for 29.97 fps, which the webcam
rejects, acceptance run 1),
`--warmup` (default warm-up frames, `20`), `--listen` (service address, default `127.0.0.1:7777`;
this is the "configurable port" of B5, distinct from the serial `--port`), `--elf-prefix` (default
`firmware/target/`), `--espflash`/`--ffmpeg` (binary paths, which is also how the tests inject
fakes). Startup fails fast if a binary is missing, and prints the python, espflash and ffmpeg
versions. **It refuses any `--listen` address that isn't loopback.**

### Container CLI

`tools/bench/bench` (executable Python script; since rev 3 a uv script, shebang
`#!/usr/bin/env -S uv run --script`, see Decision 8). It finds the repo root from its own location,
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
  can read the token, so they count as trusted. *Rev 7:* this applies to every endpoint except
  `GET /stream.mjpg`, which has no access control beyond loopback and the Host check (below).
- Requests that carry an `Origin` header are rejected, except `GET /stream.mjpg`. That blocks
  browser-based CSRF/DNS-rebinding attempts against the loopback port even before the token check.
  Token-protected endpoints do **not** check the `Host` header (the container calls them as
  `host.containers.internal:7777`); the token covers them, and a DNS-rebinding page can't read
  `.bench/token`.
- **Tokens are accepted only in the `Authorization: Bearer` header** (rev 7). No endpoint reads a
  token from the query string, so no URL ever carries a token. The access log writes the request
  path **without the query string**, for every endpoint. (Step 8 QA, fix loop 1, found the
  earlier `token=` redaction regex missed a URL-encoded key such as `?%74oken=`, which `parse_qs`
  decoded and accepted, so the token was logged in clear; dropping query tokens and query logging
  removes that class of bug instead of patching the regex.)
- **`GET /stream.mjpg` exceptions and their cost** (rev 5, step 8 in scope for a demo: a Teams
  screen share/recording of a browser window on the Mac). ~~Browsers (`<img>`, address bar) and OBS
  browser/media sources can't send a bearer header, so this endpoint alone accepts `?token=`
  (still `hmac.compare_digest`), and alone tolerates an `Origin` header so `<img>` embedding in a
  local page works. Every other endpoint keeps bearer-only and Origin rejection; a test pins
  both exceptions to this one path. Consequences: the token in the URL **lands in browser
  history, OBS scene config and any page that embeds it, and is visible on screen** if the address
  bar is shown during the demo — with that token, anything that can reach the loopback port can
  also flash the board. Mitigations: loopback only, **no LAN exposure** (unchanged; the demo
  shares the screen, not the port); the token **rotates on every service start**, so stop/restart
  the service after a demo and old URLs die; don't show the address bar (open the stream full
  window or in a kiosk/app window); the host's access log **redacts** the `token` query value.
  The stream is read-only (camera), and the client cap bounds camera/CPU use.~~
  **Rev 7 (user decision after step 8 QA):** "anonymous public stream view is OK. Auth for
  everything else is good." Browsers and OBS can't send a bearer header, so instead of a query
  token the stream is **unauthenticated**: `GET /stream.mjpg` is a **read-only camera view that
  every local process on the Mac and every container in the podman machine can open** while the
  service runs (gvproxy delivers container requests as `127.0.0.1`, so they are
  indistinguishable from local ones). The user was told the risks before deciding — any local
  process, any podman container, and (without a guard) web pages via DNS rebinding could watch
  the webcam — and **explicitly accepted the privacy consequence** for the first two. Guards that
  stay or are added:
  - **DNS-rebinding guard (stream only):** the `Host` header must be exactly `127.0.0.1:<port>`,
    `localhost:<port>` or `[::1]:<port>` (`<port>` = the listen port), else **403**. A rebinding
    page's request carries the attacker's host name, so it is refused; a page on `localhost`
    itself is a local process and is covered by the accepted risk. Consequence: the container
    can't open the stream via `host.containers.internal:7777` (not needed; the stream is for the
    human on the Mac).
  - `Origin` still tolerated here only (so a local page can embed the `<img>`); every other
    endpoint keeps bearer-only and Origin rejection; tests pin both exceptions to this one path.
  - Loopback only, **no LAN exposure** (unchanged; the demo shares the screen, not the port).
  - Read-only: the stream can't flash, reset, or pick the camera; the camera is the host-configured
    one (screen capture refused). The client cap (2) bounds camera/CPU use, and the no-frame
    watchdog and socket-aware pump (step 8) keep a stalled ffmpeg from holding a slot or the
    camera.
  - Nothing secret is in the stream URL any more, so browser history, OBS config, embedding pages
    and a visible address bar during a demo leak nothing; no token rotation is needed after a demo.
  - Stop the service when the camera should not be watchable (camera LED shows active streams).
- Request bodies are capped at 4 KiB, and unknown JSON keys are rejected. Every subprocess has a
  timeout. Child processes are killed on shutdown (SIGINT/SIGTERM handler, `atexit`).
- **Children never get the host's terminal** (rev 4, R16). Acceptance run 1 showed espflash's
  monitor opening `/dev/tty` and switching the user's terminal to raw mode even with
  `stdin=DEVNULL`. All children are therefore started with `start_new_session=True` (new session,
  no controlling tty); stop/kill/shutdown signal the whole process group (`os.killpg`, fallback to
  the pid if the group is already gone), which also catches any grandchildren; and `main()`
  snapshots the termios settings of stdin if it is a tty and restores them on exit. This is a
  robustness fix (the service stays stoppable with Ctrl-C) and keeps a child from reading
  keystrokes typed into the service's terminal.
- The serial port is either the user-given `--port` or the single auto-detected
  `/dev/cu.usbmodem*` glob match; it is never taken from a request.
- **Camera selection is host-only, and screen capture is refused** (rev 5, R17). The camera is
  set only by the host's `--camera`/`BENCH_CAMERA` at startup, **never per request** (no query or
  body parameter selects a device). avfoundation also exposes the Mac's screens as video devices
  (`Capture screen 0`, `Capture screen 1` on the bench Mac), and a wrong index can point at a
  person/room camera or a screen; so the host resolves the configured value against the startup
  device list, ~~prefers names, warns on indices, and refuses any `Capture screen…` device by name
  or index~~ (rev 6:) accepts **names only** (an all-digit value is refused at startup, exit 2),
  refuses a device named exactly `Capture screen <digits>`, and captures with the resolved name,
  never an index. Acceptance run 2 showed the risk is real: the default index `1` photographed the room
  (the image was deleted immediately and not described). QA of fix loop 2 found the snapshot
  still passed the configured index instead of the resolved name, which would have hit a
  shifted device again; that is why indices were removed entirely (rev 6).
- **uv may fetch a python interpreter from the network** (rev 3). `uv run` / `uv run --script`
  downloads a managed CPython (python-build-standalone, from GitHub) on first use **if no
  installed interpreter satisfies `>=3.11`**. On the bench Mac (3.15.0) and in the container
  (3.13) one does, so the service and the CLI normally download nothing; the
  `uv run --python 3.11` test command does download a managed 3.11 once (cached in the V1
  volume). uv checks managed downloads against hashes built into the uv release [VERIFY for the
  pinned version]. With `dependencies = []` nothing is fetched from PyPI. To forbid interpreter
  downloads entirely, set `UV_PYTHON_DOWNLOADS=never` (host or container); the tools then use an
  installed python or fail with a clear message. The uv binary in the container comes from the
  pinned GitHub release, checked against its published `.sha256` (step 2a); on the host it comes
  from Homebrew. uv also reads `uv.toml`/`pyproject.toml` config from the working directory and
  its parents; none exist in the repo today, and with no dependencies index settings don't apply.
  The B4 allow-list entry now implicitly lets the agent run uv through the shebang; a change to
  the PEP 723 header (e.g. adding a dependency) is a reviewable diff and needs a doc revision
  (Decision 8).
- Residual risk: anything that can read `.bench/token`, meaning anything running as the user on
  the Mac or in the container (or another container with the repo mounted), can flash arbitrary ELFs from `firmware/target/` and take photos.
  That's the intended capability. *Rev 7:* watching the live stream needs no token at all — any
  local process or podman container can (accepted by the user). Stop the service when you're not
  using it.

### Layout

```
tools/bench/
  bench_host.py        # host service: Bench core + HTTP front end (single file, stdlib,
                       # uv script: PEP 723 header, started with `uv run`)
  bench                # container CLI (stdlib, uv script: shebang `uv run --script`)
  tests/               # plain unittest; fakes are plain python, not uv scripts
    fake_espflash.py   # S3 imitation: flash progress, "Flashing has completed!", double boot,
                       # periodic alive lines; failure modes; driven by env vars (see step 3)
    fake_ffmpeg.py     # writes a tiny valid JPEG, or fails like a TCC denial; rev 5: canned
                       # -list_devices stderr (step 4), N mpjpeg parts for the stream (step 8);
                       # rev 7: one part then stall without exiting (watchdog/disconnect tests)
    test_host_core.py  # path validation, argv builders, ring buffer, ANSI strip
    test_host_http.py  # server on an ephemeral 127.0.0.1 port with the fakes
    test_cli.py        # CLI against the in-container server (end to end, no hardware)
.claude/skills/board-verify/SKILL.md
.bench/                # gitignored: token, snaps/
.devcontainer/features/uv/         # rev 3: devcontainer-feature.json + install.sh (U1)
```

Tests (rev 3), inside the container, from the repo root:

- canonical (minimum version):
  `uv run --python 3.11 python -m unittest discover -s tools/bench/tests -t tools/bench`
- default python (3.13), also the no-uv fallback:
  `python3 -m unittest discover -s tools/bench/tests -t tools/bench`

Both run in every step's verify. Until the container is rebuilt with uv (step 2a), only the
second one can run; that is the interim fallback, and the 3.11 run is caught up after the rebuild.
(Before rev 3 the second command was the only test command.)

Python target for both host service and CLI: **>= 3.11** syntax/stdlib, declared in the PEP 723
header (`requires-python = ">=3.11"`) and exercised by the canonical test command (bench Mac has
3.15.0; see R7).

## Plan

Each step is one commit, and its verification runs green before the next starts. Since rev 3,
"green" for the [devops] steps means both test commands under Layout (uv on 3.11, and
`python3` on 3.13; before the step-2a rebuild only the latter).

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
     the port was `/dev/cu.usbmodem11401`, and the camera was device `1` — rev 5: at that time;
     the index is not stable, the device is "HD Pro Webcam C920", see R17.)
     - 1a **T1 confirmed**: `host.containers.internal` → `192.168.127.254`, curl 200; host logged
       the client as `127.0.0.1`.
     - 1b **S1 rejected, S3 confirmed**: espflash 4.6.0; separate monitor lost ~6 s of boot; single
       `flash --monitor --non-interactive` gives the full log with a benign double boot.
     - 1c **C1 confirmed**: ffmpeg 9.0.2, device `1` = Logitech HD USB webcam, Camera permission
       already granted. `-list_options true` is not supported by this avfoundation build (use
       `-list_devices true -i ""`). `-framerate 30` was not honoured (fallback to the device
       default ~9.92 fps) — but it still captured, and rev 4 found that omitting it fails
       outright (29.97 fps rejected), so it is now the default; 1280x720 honoured; pixel format auto-overridden to `uyvy422`; the
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
     `--after no-reset` builder, no `espflash monitor` builder); the ffmpeg argv builder ~~omits
     `-framerate` unless configured~~ (rev 4: passes `-framerate 30` by default, omitted for `0`/
     empty; see step 4) and pipes via `-f image2pipe -c:v mjpeg pipe:1`; serial-port
     resolution is a pure function (`resolve_port(configured, candidates)` → chosen port or an
     error listing candidates) plus a thin glob wrapper; `/health` reports the `serial` object and
     `python_version`; target python >= 3.11. **If step 2 already landed with the S1 builders**
     (it ran in parallel with the spike), these changes are made at the start of step 3 instead,
     and step 2's tests are updated there.
   - Rev 3 changes to this step (step 2 is in progress; apply them in this step):
     `bench_host.py` gets the shebang and PEP 723 header from Decision 8 and is executable; the
     HTTP test starts it as `[sys.executable, bench_host.py, ...]`; usage text/docstring names
     `uv run tools/bench/bench_host.py` as the start command.
   - **2a. [devops] uv in the devcontainer** (sub-part of step 2, separate commit; needed before
     the canonical uv test command can run in the container). Exact change:
     - New `.devcontainer/features/uv/devcontainer-feature.json`, modelled on
       `./features/neovim`:
       ```json
       {
         "id": "uv",
         "version": "1.0.0",
         "name": "uv (release binary)",
         "description": "Installs the official static uv/uvx release binaries into /usr/local/bin.",
         "options": {
           "version": {
             "type": "string",
             "default": "<latest uv release at implementation time, e.g. 0.x.y>",
             "description": "uv release tag (no leading v), pinned."
           }
         },
         "installsAfter": [
           "ghcr.io/devcontainers/features/common-utils"
         ]
       }
       ```
     - New `.devcontainer/features/uv/install.sh` (`set -euo pipefail`, same structure as
       neovim's): map `uname -m` `x86_64`→`x86_64`, `aarch64|arm64`→`aarch64`; `apt-get install`
       `curl ca-certificates tar gzip`; download
       `https://github.com/astral-sh/uv/releases/download/${VERSION}/uv-${ARCH}-unknown-linux-musl.tar.gz`
       and its `.sha256` into a temp dir, `sha256sum -c` it, extract, install `uv` and `uvx` to
       `/usr/local/bin` (mode 0755), print `uv --version`. Fail if `VERSION` is empty (no
       "latest"). [VERIFY asset and checksum file names for the pinned release.]
     - `.devcontainer/devcontainer.json`:
       - `features`: add, after `./features/wasm-sim`,
         ```jsonc
         // uv runs the board-bench tools (PEP 723 scripts) and their tests on python 3.11.
         "./features/uv": {
           "version": "<same pinned version>"
         }
         ```
       - `overrideFeatureInstallOrder`: **unchanged**. uv depends on nothing but curl/tar (which
         its own install.sh installs) and nothing depends on uv; `installsAfter` common-utils is
         enough.
       - `containerEnv`: add
         `"UV_CACHE_DIR": "/home/vscode/.uv/cache"` and
         `"UV_PYTHON_INSTALL_DIR": "/home/vscode/.uv/python"`.
       - `mounts`: add, next to the cargo registry volume,
         `// Persist uv's cache and managed pythons (bench tests on 3.11) across rebuilds.`
         `"source=esp32-adsb-uv,target=/home/vscode/.uv,type=volume"`.
     - `.devcontainer/post-create.sh`: add `sudo chown -R "$(id -u):$(id -g)" "$HOME/.uv"` next to
       the other volume chowns (the volume is created root-owned), and `uv --version` to the
       version printout at the end.
     - **[manual] Rebuild the container** (user action). Until then the interim fallback is
       `python3 -m unittest discover -s tools/bench/tests -t tools/bench` on 3.13; step 2 can be
       committed on that basis, and the 3.11 run is done right after the rebuild (before step 3
       starts, if the rebuild has happened by then; otherwise at the latest before step 5).
     - *Verify (after rebuild):* `uv --version` prints the pinned version; `echo $UV_CACHE_DIR`
       and `$UV_PYTHON_INSTALL_DIR` as above and both writable by `vscode`; the canonical test
       command passes (first run downloads a managed 3.11 into `/home/vscode/.uv/python`);
       after a second rebuild, the canonical command runs **without** a new download (volume
       works; check with `uv python list --only-installed` or the absence of a download line).
   - *Verify (container):* unit tests for path validation (accepts a fake ELF under
     `firmware/target/`; rejects `../`, absolute, symlink escaping the prefix, non-ELF, oversize,
     directory, NUL), argv builders (exact lists, no shell; S3 list contains `--monitor` and
     `--non-interactive` and the ELF last; ffmpeg list ~~has no `-framerate` by default~~ has
     `-framerate 30` by default since rev 4, see step 4),
     `resolve_port` (fixed port returned as is; auto with one candidate → it; zero or two
     candidates → error naming them), non-loopback `--listen` refused. HTTP
     test: 401 without/with wrong token, 403 with `Origin`, 200 `/health` with token and the
     `serial` fields present. Rev 3: `bench_host.py` starts with exactly the shebang and PEP 723
     header of Decision 8 (`requires-python = ">=3.11"`, `dependencies = []`) and is executable;
     the suite passes with **both** test commands (Layout): `uv run --python 3.11 python -m
     unittest ...` (after the 2a rebuild; interim: skipped and caught up) and
     `python3 -m unittest ...`; `uv run tools/bench/bench_host.py --help` exits 0 (after 2a).

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
   - **Rev 4 (acceptance run 1):** the "`-framerate` only if configured / not by default" argv
     above made the real webcam fail (503, ffmpeg exit 251, 29.97 fps rejected; see Decision 3).
     Current argv: `-f avfoundation -framerate 30 -video_size 1280x720 -i "1:none" -vf
     select=gte(n\,W) -frames:v 1 -f image2pipe -c:v mjpeg pipe:1`; `-framerate` comes **before**
     `-video_size`; `--framerate 0` / `BENCH_FRAMERATE=0` or empty omits it. Whether the device
     then really runs at 30 fps or still falls back to ~10 fps (spike 1c warning) is recorded in
     acceptance run 2; the warm-up default stays 20 frames until then.
   - **Rev 5 (acceptance run 2):** `-framerate 30` falls back to ~10 fps but captures; the
     warm-up default stays **20 frames** (snapshot 1.91 s inside ffmpeg). The `image2pipe` form
     is confirmed (the `-update 1` fallback is not needed). The `-i "1:none"` above becomes
     `-i "<camera>:none"` with the camera **name** by default (`"HD Pro Webcam C920:none"`); this
     is still a single argv element, so names with spaces or non-ASCII characters need no quoting.
     Startup device check (fix loop 2, implemented in parallel): a pure parser for the stderr of
     `ffmpeg -f avfoundation -list_devices true -i ""` (video section only, `[index] name`
     entries) and a pure `resolve_camera(configured, devices)` → device or an error listing the
     devices; name missing → error; ~~all-digit → index, warning with the mapped name; out of
     range → error; `Capture screen…` → error by name or index~~ (rev 6:) all-digit
     (`str.isdigit()`, incl. non-ASCII digits) → error "use a device name" with the device list,
     host exits 2; exact case-sensitive match on names as printed; a name that fullmatches
     `Capture screen [0-9]+` → error (a name that merely starts with `Capture screen` is not
     refused by this rule). **Rev 6:** the snapshot (and step-8 stream) argv builder receives the
     **resolved device name** from `resolve_camera`, never the raw `--camera` value or an index
     (QA of fix loop 2: the snapshot passed the configured index). *Verify additions
     (container):* parser tests on the run-2 device list (incl. the iPhone names with `„“` and
     `’`, kept byte-for-byte, the audio section ignored); resolve tests for each rule; the host
     refuses to start with a bogus name and prints the list (fake ffmpeg emits a canned
     `-list_devices` stderr); rev 6: `--camera 1`, `--camera ١` and `BENCH_CAMERA=1` → exit 2
     with "use a device name" and the list; `Capture screen 0` refused, while a hypothetical
     `Capture screen 0 Pro` or `Capture screens` is not treated as a screen; the snapshot argv
     seen by the fake ffmpeg contains `-i "HD Pro Webcam C920:none"` (the name, no digits-only
     device); no request parameter can change the camera.
   - *Verify (container), with `--ffmpeg tests/fake_ffmpeg.py`:* 200 + `image/jpeg` + bytes start
     with `FF D8`; fake failure → 503 with the stderr tail; timeout → 503 and the fake is killed;
     the argv builder test checks the camera/warm-up values and that ~~`-framerate` is absent by
     default and present when configured~~ (rev 4) `-framerate 30` is present by default and
     precedes `-video_size`, a configured value replaces it, and `0`/empty omits it; a fake that
     writes a "falling back to default" stderr line still returns 200, and the line appears in
     the host log.

5. **[devops] Container CLI.** `tools/bench/bench` with `health`, `flash`, `log`, `reset`, `snap`,
   the exit-code contract, and repo-root/token discovery. Rev 3: the CLI starts with the shebang
   and PEP 723 header of Decision 8 and is executable; the end-to-end tests run it as
   `[sys.executable, tools/bench/bench, ...]`.
   - *Verify (container):* `tools/bench/bench --help` executed **via its shebang** exits 0 (a
     smoke test that is skipped when `uv` is not on `PATH`, `unittest.skipUnless(shutil.which("uv"))`);
     end-to-end tests start `bench_host.py` on an ephemeral port with both
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

6. **[manual] Acceptance on the real bench.** The user installs uv (`brew install uv`; rev 3) and
   starts `uv run tools/bench/bench_host.py --camera "<B1 device>"` from the repo root in a Mac
   Terminal (was `python3 tools/bench/bench_host.py ...` before rev 3) (B1: ~~device `1`
   is the default~~ rev 5: `"HD Pro Webcam C920"` is the default, so `--camera` is only needed
   for a different camera; no `--port` needed if exactly one `/dev/cu.usbmodem*` is present), then the
   agent (or user) runs in the container:
   - `tools/bench/bench health` → exit 0, and `serial.chosen` is the board's port
     (`/dev/cu.usbmodem11401` on the spike's USB port); record `python_version` (which
     interpreter uv picked on the host, expected 3.15.0 — run 1: 3.12.12, see R15 — and that uv
     downloaded nothing);
   - `tools/bench/bench flash firmware/target/xtensa-esp32s3-none-elf/release/firmware --until
     'alive 0s' --timeout 60` → exit 0, and the log shows the boot from the first line
     (ROM banner, bootloader, `Embassy initialized!`), including the double boot without a
     false failure;
   - `tools/bench/bench reset --until 'ch422g: backlight on'` → exit 0 (a re-flash of the same
     image; record its duration);
   - `tools/bench/bench snap` → Claude `Read`s the JPEG and describes the panel correctly; confirm
     the `image2pipe` form works (step 4 VERIFY) and check the host log shows ffmpeg's stderr;
     this also confirms that the Camera permission granted to the terminal app still applies
     when ffmpeg is started under `uv run` (rev 3 [VERIFY]; expected, since TCC attributes to the
     responsible app, the terminal);
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
   - **Acceptance run 1 (2026-10-23): partial pass; fix loop 1 of 2.**
     - Host start `uv run tools/bench/bench_host.py`: uv picked **python 3.12.12** (Mac `python3`
       is 3.15.0) → R15 materialised, harmless (>= 3.11).
     - `bench health` → exit 0; serial auto → `/dev/cu.usbmodem11401`; espflash 4.6.0. The token
       landed in the shared repo `.bench/token` and the container CLI read it: token distribution
       over the bind mount works.
     - `bench flash firmware/target/xtensa-esp32s3-none-elf/release/firmware --until 'alive 0s'
       --timeout 60` → exit 0. Full log from `Flashing has completed!`; `--- boot 1 ---` (cut off
       after `psram: fb ptr`), `--- boot 2 ---` through `Embassy initialized!`,
       `ch422g: backlight on`, `alive 0s`; summary "--until matched in boot 2 at seq 142".
       **Double-boot handling confirmed on hardware.**
     - `bench reset --until 'ch422g: backlight on'` → exit 0 in **3.94 s** (re-flash of the same
       image).
     - `bench snap` → **FAILED**: 503, ffmpeg exit 251, "Selected framerate (29.970030) is not
       supported by the device" (supported: 640x480, 1280x720, 1760x1328, 1328x1760, 1552x1552,
       1920x1080, 1080x1920, each at 15 and 30 fps) → "Error opening input: Input/output error".
       Fix: `-framerate 30` by default (Decision 3, step 4, R12).
     - **New defect:** after the first `/flash` the Mac terminal running the service was left in
       raw mode (staircase output, Ctrl-C dead); recovered with `pkill -f bench_host.py` and
       `reset`. Fix: children in their own session, process-group kill, termios restore (R16,
       Security).
     - Log noise: successful `GET /log` long-polls are no longer access-logged (API table).
     - Not yet run: bad-path rejection, stop-service → exit 4, camera tuning, flash→marker timing.
   - **Acceptance run 2 ~~(pending, after the fixes land)~~ (2026-10-23): pass except camera
     selection; fix loop 2 of 2.** Planned scope (kept): re-run **everything** above, including
     `bench snap`, plus: snapshot timing; `image2pipe` form (step 4 [VERIFY]); Camera permission
     under `uv run` (rev 3 [VERIFY]); ffmpeg stderr visible in the host log (incl. whether
     `-framerate 30` is honoured or falls back to ~10 fps); espflash's monitor still runs and
     streams the log without a controlling tty, and the host terminal stays cooked and Ctrl-C
     stops the service cleanly with no orphaned espflash/ffmpeg (R16 [VERIFY]). If run 2 fails,
     that is fix loop 2 of 2. Results (host `uv run tools/bench/bench_host.py`, python 3.12.12,
     espflash 4.6.0, ffmpeg 9.0.2):
     - `bench health` → exit 0.
     - `bench flash firmware/target/xtensa-esp32s3-none-elf/release/firmware` (default
       `--until 'alive 0s'`) → exit 0 in **3.92 s** wall (flash→marker); double boot recognised,
       matched in boot 2.
     - `bench reset --until 'ch422g: backlight on'` → exit 0 in **3.92 s** (reset→marker).
     - Bad paths `../README.md` and `/etc/passwd` → HTTP 400 → exit 4. Service stopped →
       `bench health` exit 4 (connection refused).
     - **R16 resolved:** espflash's monitor runs and streams the log without a controlling
       terminal; the host terminal stays clean and Ctrl-C stops the service cleanly. The request
       log is clean (no `/log` noise).
     - `bench snap` → exit 0 in **2.19 s** (ffmpeg elapsed 1.91 s, 30 KiB JPEG). `-framerate 30`:
       "Configuration of video device failed, falling back to default", runs at ~10 fps;
       `uyvy422` auto-selected, "Selected pixel format (yuv420p) is not supported" is a benign
       override. **`image2pipe` VERIFY closed** (step 4); **Camera permission under `uv run`
       VERIFY closed** (TCC grant of the terminal applies); ffmpeg stderr appears in the host log.
     - **DEFECT: wrong camera.** The default `--camera 1` captured a different camera (a room
       view, not the panel). avfoundation video devices on the bench Mac at the time: `[0]`
       MacBook Pro-Kamera, `[1]` HD Pro Webcam C920, `[2]` Kamera von „Sergej’s iPhone (5)“
       (Continuity), `[3]` MacBook Pro-Schreibtischansicht-Kamera, `[4]` Schreibtischansicht-Kamera
       von „Sergej’s iPhone (5)“, `[5]` Capture screen 0, `[6]` Capture screen 1. This listing shows
       `[1]` as the C920, yet index `1` captured another camera: the index→device mapping is not
       stable (Continuity devices come and go; the exact shift at capture time was not
       reconstructed), so B1's "device index `1`" is not a usable identifier.
       The accidental photo was deleted immediately and not described. With
       `--camera "HD Pro Webcam C920"` the snapshot showed the panel: colour bars, **no glare**
       (the spike's glare is gone at the current mount; R5). Fix (fix loop 2 of 2, implemented in
       parallel): camera by name, default `"HD Pro Webcam C920"`, startup device check, ~~index
       warning,~~ screen-capture refusal (Decision 3, host config, Security, step 4, R17).
     - **QA of fix loop 2 (rev 6):** the snapshot passed the configured index instead of the
       resolved device name, so it would have hit a shifted device. The step-6 fix-loop budget
       was exhausted (3 of 2); the **user authorised a third fix** with a simplification: camera
       by **name only**, numeric `--camera` refused at startup (exit 2, "use a device name" +
       list), capture always with the resolved name, exact `Capture screen <digits>` refusal,
       names kept exactly as ffmpeg prints them (Decision 3).
     - Not observed: a panic (backtrace symbolisation stays [VERIFY]).
   - **Acceptance run 3 (pending, after fix loop ~~2~~ 3 lands; short, user-run):**
     - start `uv run tools/bench/bench_host.py` **without** `--camera` (default name) →
       `bench snap` → the snapshot shows the panel (rev 6: the host log's ffmpeg line shows the
       device **name** in `-i`, not an index);
     - start with a bogus name (e.g. `--camera "No Such Camera"`) → the host refuses to start and
       prints the available video devices;
     - (rev 6) start with `--camera 1` → refused at startup, exit 2, "use a device name" and the
       device list.
     - (The frame-rate experiment the user ran by hand alongside run 2/3 is recorded in Decision
       3 and the rev-5 revision log; it is not a run-3 pass criterion.)
     - ~~If run 3 fails, the fix loop budget (2 of 2) is spent: stop and revise this doc with the
       user instead of a third fix loop.~~ Rev 6: the budget is already exceeded (fix loop 3 of 2,
       user-authorised). If run 3 fails, stop and revise this doc with the user; no further fix
       loop without the user's explicit OK.

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
     Rev 3 additions: host prerequisite **uv** (`brew install uv`) replaces "`python3 >= 3.11`" as
     the primary prerequisite (uv selects or fetches a python >= 3.11); start command
     `uv run tools/bench/bench_host.py [--camera ...]` from the repo root, with
     `python3 tools/bench/bench_host.py` (python >= 3.11) as the documented fallback; note that
     uv may download a python on first use if none >= 3.11 is installed, and
     `UV_PYTHON_DOWNLOADS=never` to forbid it; the container side is unchanged
     (`tools/bench/bench ...`, uv comes with the devcontainer) and the test commands from Layout.
     Rev 4 additions: a "terminal broken" troubleshooting note — if the service's terminal ever
     ends up in raw mode (staircase output, Ctrl-C ignored; R16), run `pkill -f bench_host.py`
     from another terminal tab, then `reset` in the broken one; the `--framerate` /
     `BENCH_FRAMERATE` setting (default 30, `0` omits it) next to `--camera`, with the hint that
     ffmpeg's 503 stderr lists the camera's supported modes.
     Rev 5 additions: `--camera`/`BENCH_CAMERA` takes the avfoundation **device name** (default
     `"HD Pro Webcam C920"`); ~~**prefer names over indices**~~ (rev 6: **names only**; a numeric
     value is refused at startup), because indices shift when
     Continuity cameras (iPhone) appear or disappear and a wrong index can photograph a person,
     the room or the screen. How to find the name:
     `ffmpeg -f avfoundation -list_devices true -i "" 2>&1 | command grep -A10 'video devices'`
     (`command grep` because of the bench Mac's `grep`→`rg` alias). The host checks the name at
     startup and refuses to start, listing the devices, if it is missing; ~~an index is accepted
     with a warning naming the device; `Capture screen…` devices are always refused~~ (rev 6:) a
     numeric value is refused (exit 2, "use a device name"); the name must match exactly as
     ffmpeg prints it (case, punctuation such as `„“’` included; quote it in the shell);
     `Capture screen <N>` devices are always refused. The camera
     can only be set on the host, never from the container.
     Rev 7 additions (written with step 8's README part): the stream URL is the literal
     `http://127.0.0.1:7777/stream.mjpg` (no token, no `$(...)` substitution — QA found the README
     showed the URL as a `$(...)` shell string, which doesn't work in a browser address bar or an
     OBS source); `localhost:7777` and `[::1]:7777` also work, any other host name gets 403; the
     security note says plainly that **while the service runs, any local process and any podman
     container can watch the camera without a token** (accepted), and to stop the service when
     that is not wanted; the old "hide the address bar / restart to rotate the token after a demo"
     caution is dropped (nothing secret in the URL).
   - *Verify:* a fresh agent session that is only given the skill completes step 6's flash→snap
     sequence unaided; the README commands match what was run in step 6.

8. **[devops] ~~[optional]~~ `GET /stream.mjpg`** for a human in a Mac browser (rev 5: **in
   scope**, no longer optional — user request, for a demo that shares the webcam output via a Teams
   screen share/recording of a browser window on the Mac; loopback only, no LAN exposure). ~~One
   ffmpeg per client (`-f mpjpeg`), `multipart/x-mixed-replace`, killed on client disconnect,
   `?token=` accepted.~~ Rev 5 spec:
   - **8a. Stream capture settings (separate from snapshots).** The Mac frame-rate experiment
     (Decision 3, revision log) showed the C920 reaches a real 30 fps via avfoundation only at
     640x480 (uncompressed YUV over USB 2.0; 720p ~10 fps, 1080p ~5 fps; pixel format
     irrelevant). So the stream has its own host options `--stream-size` (env
     `BENCH_STREAM_SIZE`, default `640x480`) and `--stream-framerate` (env
     `BENCH_STREAM_FRAMERATE`, default `30`; `0`/empty omits it, like `--framerate`); snapshots
     keep `--video-size 1280x720`, `--framerate 30` and the 20-frame warm-up unchanged. The
     stream has no warm-up (the first second may be under-exposed; acceptable for a live view).
     Pending the user's confirmation at the checkpoint (orchestrator decision).
   - **Response:** `200`, `Content-Type: multipart/x-mixed-replace; boundary=<b>`,
     `Cache-Control: no-store`; each part `Content-Type: image/jpeg` + `Content-Length`, then the
     JPEG. `<b>` is a fixed constant (e.g. `benchframe`) passed to ffmpeg as
     `-boundary_tag <b>`, so the header and the body can't disagree (ffmpeg's default is
     `ffmpeg`).
   - **Framing decision: ffmpeg `-f mpjpeg` pass-through**, not image2pipe + JPEG re-framing.
     Argv: `-f avfoundation -framerate <stream-framerate> -video_size <stream-size>
     -i "<camera>:none" -c:v mjpeg -f mpjpeg -boundary_tag <b> pipe:1` (defaults: `30`,
     `640x480`; same camera resolution and screen-capture refusal as snapshots; rev 6: `<camera>`
     is the resolved device name, never an index). Why: ffmpeg already writes each part
     with the boundary, `Content-type: image/jpeg` and `Content-length` (header names are
     case-insensitive), so the host only copies bytes from the pipe to the socket — no JPEG
     scanning, no buffering, least host code. Re-framing (split image2pipe output on
     `FF D8`…`FF D9`, write our own part headers) gives exact header control and per-frame
     validation but is ~30 more lines of parsing for no demo benefit; it stays the **fallback**
     if Safari/Chrome/OBS reject ffmpeg's part headers [VERIFY on the Mac].
   - **Lifecycle:** one ffmpeg per client, started with `start_new_session=True` (rev 4); a write
     error to the client (`BrokenPipeError`/`ConnectionResetError`) or service shutdown kills the
     **process group** (`os.killpg`, pid fallback) and frees the client slot; ffmpeg exiting ends
     the response and logs the stderr tail. No timeout on the stream itself (it runs until the
     client leaves); each `write` uses a socket timeout so a stalled client is dropped.
     **Rev 7 (step 8 QA, fix loop 1):** QA found that an ffmpeg that stalls after the first frame
     kept the client slot and the camera after the client disconnected, because the pump blocked
     on the pipe read and only noticed the disconnect on the next write. Fix: the pump
     `select`s on the ffmpeg stdout pipe **and** the client socket with a ~1 s tick; the client
     socket becoming readable with EOF (or an error) ends the stream immediately; a **no-frame
     watchdog** ends the stream if the pipe delivers no data for ~10 s. Every exit path (client
     gone, watchdog, ffmpeg exit, write error, shutdown) kills the process group and frees the
     slot exactly once.
   - **Client cap:** at most `--stream-clients` (default **2**) concurrent streams; beyond that
     **409** ("stream clients busy").
   - **Interaction with `/snapshot.jpg`:** a running stream does **not** take the snapshot camera
     lock; the snapshot opens the device concurrently (avfoundation generally allows several
     clients of one camera [VERIFY on the Mac]). If the Mac shows that a second open fails or
     disturbs the stream, the fallback is: an active stream makes `/snapshot.jpg` return **409**
     (the CLI already retries 409 and then exits 4) — revise the doc before switching.
   - **Auth:** ~~bearer header **or** `?token=` (this endpoint only); `Origin` tolerated (this
     endpoint only); access log redacts `token=`.~~ Rev 7: **none** (anonymous read-only view,
     user decision); `Host` must be exactly `127.0.0.1:<port>`, `localhost:<port>` or
     `[::1]:<port>`, else 403 (this endpoint only); `Origin` tolerated (this endpoint only); no
     endpoint accepts `?token=`; the access log never contains query strings. Security and R18
     record the consequences.
   - README (in this step): how to open the stream on the Mac
     (~~`http://127.0.0.1:7777/stream.mjpg?token=$(cat .bench/token)` — with `command cat` on the
     bench Mac~~ rev 7: `http://127.0.0.1:7777/stream.mjpg`, a literal URL), ~~the demo caution
     (don't show the address bar; restart the service afterwards to rotate the token),~~ (rev 7:)
     the note that any local process/container can watch while the service runs, OBS
     browser/media source setup, the client cap.
   - *Verify (container):* ~~with the fake ffmpeg emitting two frames, the response has two
     multipart parts and the fake is gone after the client closes.~~ With `fake_ffmpeg.py`
     extended to emit N mpjpeg parts using the `-boundary_tag` from its argv (and exit 2 if
     `-f mpjpeg`/`-boundary_tag` are missing): the response `Content-Type` is
     `multipart/x-mixed-replace` with a boundary equal to the argv's `-boundary_tag`; a stdlib
     multipart parse yields N parts, each `image/jpeg` with a correct `Content-Length` and
     `FF D8` magic; closing the client kills the fake (no process left, group killed); a third
     concurrent client → 409 and the first two keep streaming; ~~`?token=` accepted here, and
     `?token=` on `/health`, `/snapshot.jpg`, `/flash` → 401;~~ `Origin` accepted here and still
     403 on every other endpoint; ~~the access log line contains no token value;~~ rev 7:
     **anonymous** `GET /stream.mjpg` (no `Authorization`, no query) with `Host: 127.0.0.1:<port>`
     → 200, and likewise with `localhost:<port>` and `[::1]:<port>`; `Host` `evil.example:<port>`,
     `host.containers.internal:<port>`, `127.0.0.1` with a wrong or missing port, or no `Host`
     → 403 and no ffmpeg started; a wrong bearer token on the stream is ignored (still 200);
     `?token=<valid>` and `?%74oken=<valid>` on `/health`, `/snapshot.jpg`, `/flash` → **401**
     (query tokens ignored everywhere); token-protected endpoints with a bearer token and
     `Host: host.containers.internal:<port>` → 200 (no Host check there); the captured access log
     contains no `?`, no query string and no token value for any of these requests;
     **disconnect/stall:** with a fake ffmpeg mode that writes one part and then stalls without
     exiting, closing the client kills the fake's process group and frees the slot within ~1–2 s
     (a new client then gets 200, not 409); with the client still connected, the no-frame
     watchdog ends the response after its timeout (test-shortened), kills the fake and frees the
     slot; no orphaned fake processes after any of these; the stream argv
     builder test pins `-framerate 30 -video_size 640x480` by default, configured
     `--stream-size`/`--stream-framerate` replace them, `0`/empty framerate omits it, and the
     snapshot argv is unchanged (`1280x720`); a snapshot during an active stream succeeds with
     the fakes.
   - **[manual]** ~~The stream renders in Safari/Chrome at
     `http://127.0.0.1:7777/stream.mjpg?token=...`.~~ The stream renders in Safari and Chrome on the
     Mac and in an OBS browser source and media source at 640x480, with the observed fps
     recorded (expected ~30, per the experiment) and no "configuration failed" line in the host
     log; closing
     the tab stops ffmpeg (camera LED off); a snapshot during a running stream works or the 409
     fallback is decided (concurrency [VERIFY]); a Teams share of the browser window shows the
     stream ~~with the address bar hidden~~ (rev 7: the address bar may show; the URL
     `http://127.0.0.1:7777/stream.mjpg` carries no secret); the stream opens without any token;
     `curl -H 'Host: example.com' http://127.0.0.1:7777/stream.mjpg` → 403.

## Risks

| # | Risk | Likelihood / impact | Mitigation |
|---|------|---------------------|------------|
| R1 | `host.containers.internal` doesn't reach host loopback under applehv/libkrun | ~~Medium / high~~ **Resolved (spike 1a)** | Step 1a checked it: it works (via `192.168.127.254`). T2 not needed. Side effect recorded under Security: requests arrive as `127.0.0.1`, so the token is the only discriminator. Not opening a non-loopback port stays a hard rule. |
| R2 | `espflash monitor` won't run without a TTY, or doesn't reset on attach, so early boot lines are lost | ~~Medium / medium~~ **Materialised, resolved by S3 (spike 1b)** | The monitor ran with piped stdout and did reset the chip, but its first app line was `alive 6s`: ~6 s of boot (i2c scan, `Embassy initialized!`, `ch422g: backlight on`, `alive 0s`) lost. Switched to S3 (single `flash --monitor --non-interactive` process), which captured everything from the ROM banner. `POST /reset` also uses S3 for this reason. The `alive 0s` default marker was chosen for this risk and is kept (see Container CLI). |
| R3 | Native USB-Serial-JTAG re-enumerates on reset (device node disappears briefly) and the monitor exits | Medium / medium | Observed indirectly in spike 1b: after a reset by a separately attached `espflash monitor`, ~6 s of output were lost before the monitor saw data, consistent with re-enumeration plus reconnect (esp-println drops output while no host has the port open). Under S3, espflash survived both observed resets (`rst:0x15` twice) without losing lines, so the flash path is covered. Remaining exposure: resets not driven by espflash (panic reboot, RST button, watchdog) may lose up to ~6 s of the next boot, or end the process. The panic text itself is printed before the reset, so `--fail-on` still sees it. The monitor exit is visible (`running=false`, CLI exit 3); `POST /reset` recovers. Bounded restart-on-exit stays deferred until step 6 shows the need. |
| R4 | macOS Camera permission (TCC) denied or attributed to the wrong app; under launchd no prompt at all | ~~High on first run~~ Low now / medium | Spike 1c: permission was already granted to the terminal app; capture worked. The 503 body carries ffmpeg's stderr. launchd is deferred (B2) and would re-open this risk. *Rev 5:* acceptance run 2 confirmed the grant also applies when ffmpeg runs under `uv run` from the terminal. |
| R5 | Photos of an LCD are hard to judge: auto exposure/white balance drift, moiré, reflections, focus | High / medium; **mildly materialised (spike 1c)** | Spike 1c photo: sharp and well exposed, the colour-bar image clearly judgeable, but window glare top-left (over the green/blue bars) and room reflections on the right of the panel. Fixed mount and angle (plus shading the window), warm-up frames, and a judging rubric of structural checks ("backlight on, no full-screen noise, layout regions present") rather than pixel-exact comparison. ffmpeg's avfoundation input can't lock exposure/WB [VERIFY]. A UVC control tool or the boot test pattern (B3) are follow-ups if needed. Step 6 records the tuning. *Rev 5 (acceptance run 2):* at the current mount the C920 photo of the colour bars shows **no glare**; the panel is judgeable. Exposure/WB locking stays [VERIFY]/deferred. |
| R6 | The service and the user's manual `espflash` fight over the port | Medium / low | Documented one-holder rule, `POST /monitor/stop`. espflash's "port busy" error is clear. |
| R7 | Host python version: the Xcode CLT `python3` may be 3.9; the container tests run on 3.13 and wouldn't catch newer syntax | ~~Medium / low~~ **Low (relaxed after spike)** | Original mitigation, now dropped: "Code `bench_host.py` to 3.9 (no `match`, `from __future__ import annotations`, no `tomllib`)". The bench Mac has python 3.15.0, so the binding constraint is the container's 3.13 that runs the tests. New rule: target **>= 3.11** syntax/stdlib for host service and CLI; the README states host `python3 >= 3.11`; the startup check prints the version and refuses to start below 3.11. Remaining gap: a 3.15 host could behave differently from the 3.13 test run (deprecations/removals); step 6 is the check. *Rev 3:* the lower bound is now exercised (canonical tests on 3.11 via uv, plus 3.13); the upper gap stays, and can optionally be narrowed by also running the suite with `uv run --python 3.15 ...` if uv offers that version (not required). |
| R8 | The capability itself: anything that can read `.bench/token` can flash the board and photograph the room | Certain (by design) / medium | Loopback only, token, Origin rejection, ELF prefix allowlist, temp copy, no shell, the service runs only while started by hand. Images and logs live in gitignored `.bench/`. B4 keeps the user in control of unattended use. *Rev 7:* watching the live stream needs no token (R18). |
| R9 | Host and container espflash versions differ (flag names, output format) | Medium / ~~low~~ **medium** (S3 depends on output wording) | `/health` reports the host espflash version (4.6.0 at spike 1b). The argv builders sit in one place. Flash success depends on the literal line `Flashing has completed!`; if an upgrade rewords it, flashes end in 504 (timeout) rather than a false success, and the string is one constant. Observed flag values on 4.6.0: `monitor --before` = `default-reset`, `no-reset`, `no-reset-no-sync`; `flash --after` = `hard-reset`, `no-reset` (list possibly truncated). `flash --after no-reset` without `--monitor` printed a harmless WARN about monitor options being ignored; not used under S3. |
| R11 | Serial port auto-detect picks the wrong device or none: the name changes with the USB port/hub, or a second `usbmodem` device (e.g. the board's CH343 UART port, if also cabled, or another dev board) is present | Medium / low | Auto mode accepts exactly one `/dev/cu.usbmodem*` match and otherwise errors with the candidate list (startup, `/health`, flash/reset 503). `--port` overrides. Filtering by VID 303A would need `ioreg`/`system_profiler` parsing; deferred until ambiguity actually happens. |
| R12 | Camera settings silently ignored (spike 1c: `-framerate 30` fell back to the device default ~10 fps; pixel format overridden to `uyvy422`) — or, **rev 4**, hard-rejected: without `-framerate` avfoundation requests 29.97 fps and the webcam refuses it (acceptance run 1: 503, ffmpeg exit 251) | Certain / ~~low~~ **medium** (materialised as a hard failure) | ~~Don't pass `-framerate` by default~~ (rev 4: that was the cause of the failure). Pass `-framerate 30` by default (a rate the device lists as supported), configurable via `--framerate`/`BENCH_FRAMERATE`, `0`/empty omits it; warm-up expressed in frames sized for ~10 fps (default 20 ≈ 2 s; revisit if run 2 shows a true 30 fps); ffmpeg stderr logged on every snapshot so fallbacks are visible, and the 503 body carries the stderr tail (which named the supported modes in run 1); JPEG magic check catches outright failure. *Rev 5 (acceptance run 2):* with `-framerate 30` the C920 captures but still falls back to ~10 fps ("Configuration of video device failed"); `uyvy422` is auto-selected after "Selected pixel format (yuv420p) is not supported". Snapshots work (2.19 s); warm-up stays 20 frames. ~~Hypothesis for the fallback: avfoundation first requests `yuv420p`, which the C920 doesn't support, so the device configuration fails and the device falls back to its default (~10 fps); an explicit `-pixel_format` may give a real 30 fps.~~ **Disproven** by the Mac experiment (`uyvy422`/`nv12`/`yuyv422` at 1280x720 @ 30 all still fall back to 10 fps). The cap is resolution-bound (uncompressed YUV over USB 2.0): 1280x720 ~10 fps (at 30 or 15 requested), 1920x1080 ~4.9 fps, **640x480 @ 30 works (fps=31)**. So: no pixel-format option; snapshots keep 1280x720 (sharpness matters more than rate; ~10 fps, 20-frame warm-up ≈ 2 s, unchanged); the stream uses `--stream-size 640x480 --stream-framerate 30` (step 8a). The "configuration failed" warning at 720p is expected and benign. |
| R10 | Privacy: snapshots/logs show live traffic and the room | Medium / medium | `.bench/` is gitignored. The skill forbids sharing or committing them. Same rule as the README simulator note. |
| R13 | uv missing (rev 3): not installed on the Mac, or the container not yet rebuilt with `./features/uv`, so `uv run ...` and the CLI shebang fail (`env: 'uv': No such file or directory`) | Medium until step 2a/6 / low | Both tools stay runnable with a plain `python3 >= 3.11` (`python3 tools/bench/bench_host.py`, `python3 tools/bench/bench ...`, `python3 -m unittest ...`); README and skill name the fallback; the shebang smoke test is skipped, not failed, without uv. Note: the B4 allow-list entry matches `tools/bench/bench`, not `python3 tools/bench/bench`, so the fallback is prompted. |
| R14 | Managed-python download (rev 3): `uv run --python 3.11` (or a host without python >= 3.11) needs network on first use; offline, or GitHub unreachable, it fails | Low / low | Only the canonical test command needs it in the container; the V1 volume keeps the managed 3.11 across rebuilds, so it is a one-time download. The 3.13 test command and the CLI need no download. On the host, 3.15.0 satisfies `>=3.11`, so nothing is fetched. `UV_PYTHON_DOWNLOADS=never` turns a silent fetch into a clear error where that is preferred. |
| R15 | uv's interpreter choice drifts (rev 3): with uv's default "prefer managed" policy, once a managed 3.11 is installed the CLI shebang may run on 3.11 instead of the system 3.13, and on the host a later managed install could shadow Homebrew python | Low / low; **materialised, harmless (acceptance run 1)** | Any interpreter >= 3.11 is supported and tested at both ends of the range we control (3.11, 3.13); `/health` reports the host's `python_version`. Rev 4: on the bench Mac `uv run tools/bench/bench_host.py` picked **python 3.12.12** although the Mac's `python3` is 3.15.0 (presumably a uv-managed or other installed interpreter that uv prefers; not investigated). Harmless, since 3.12 is inside the tested range; it also means the R7 "3.15 upper gap" is not what actually runs on the host today. |
| R16 | A child tool grabs the controlling terminal (rev 4, **materialised in acceptance run 1**): after the first `/flash`, the Mac terminal running `bench_host.py` went into raw mode (staircase output, Ctrl-C dead); the user had to `pkill -f bench_host.py` from another tab and run `reset`. Cause: espflash's monitor (crossterm) opens the controlling terminal via `/dev/tty`, despite `stdin=DEVNULL`, and enables raw mode | Certain without mitigation / medium (host terminal unusable, service can't be stopped with Ctrl-C) | Fix (implemented in parallel to rev 4, see Security): every child (espflash, ffmpeg) is started with `start_new_session=True`, so it has no controlling terminal and `/dev/tty` can't be opened; stop/kill signal the child's **process group** (`os.killpg`, falling back to the pid); `main()` snapshots the termios state of a tty stdin at startup and restores it on exit, as a backstop. README step 7 documents recovery (`pkill -f bench_host.py` from another tab, then `reset`). ~~**Residual [VERIFY in acceptance run 2]:** espflash's monitor might refuse to run, or behave differently, without a controlling tty; if so, revise this doc (options: a pty for the child, or `flash` + S2) rather than improvising.~~ **Resolved (acceptance run 2, rev 5):** espflash runs and streams the full log without a controlling terminal; the host terminal stays clean and Ctrl-C stops the service cleanly. Accepted residual (fix loop 1 QA): a SIGKILL of the host orphans the children. |
| R17 | Camera index instability (rev 5, **materialised in acceptance run 2**): avfoundation indices shift when devices appear/disappear (Continuity iPhone cameras, desk-view cameras), so a fixed index selects the wrong camera — a person/room camera, or a screen (`Capture screen N` devices are listed as video devices). Run 2: the default `--camera 1` photographed the room instead of the panel | Certain with indices / **high** (privacy: photo of a person, the room or the screen contents; and wrong-evidence verdicts if the agent judged a non-panel image) | Camera chosen by **name**, default `"HD Pro Webcam C920"`; startup lists the avfoundation video devices and refuses to start if the name is missing (prints the list); ~~an all-digit index is accepted only with a stderr warning naming the device it maps to now, out of range refused; `Capture screen…` refused by name or index;~~ *rev 6:* **name only** — an all-digit value (incl. non-ASCII digits) is refused at startup (exit 2, "use a device name" + list), names compared exactly as ffmpeg prints them, a device named exactly `Capture screen <digits>` refused, and snapshot/stream capture always pass the resolved name (QA of fix loop 2 caught the snapshot still passing the configured index); camera configurable only on the host, never per request (Decision 3, host config, Security). Residual: the name→device mapping is checked only at startup, so if the C920 is unplugged while the service runs, ffmpeg fails (503) rather than picking another device — names don't shift the way indices do. Two devices with the same name (a second C920) would be ambiguous [not handled; refuse if it ever happens]. The run-2 accidental photo was deleted immediately and not described. Acceptance run 3 checks the default, the bogus-name refusal and (rev 6) the `--camera 1` refusal. Residual (rev 6): an avfoundation device whose real name is all digits could not be selected; accepted (not seen). |
| R18 | ~~Stream token exposure (rev 5, step 8): `?token=` puts the service token into URLs — browser history, OBS scene config, pages embedding the `<img>`, and the screen during a Teams demo if the address bar is visible; Origin tolerance on this endpoint lets any local web page embed the stream if it has the token~~ **Rev 7: anonymous local camera access.** `GET /stream.mjpg` needs no token, so while the service runs **any process on the Mac and any container in the podman machine** (all arrive as `127.0.0.1`) can watch the live webcam — the room, and anyone in it. (The rev-5 token-in-URL risk is gone: no endpoint accepts a query token. It had materialised in step 8 QA as a clear-text token in the access log via `?%74oken=`.) | ~~Medium during demos / medium (the same token authorises `/flash`)~~ Certain while the service runs / medium (privacy); **accepted by the user** after being told the risks | ~~Query token and Origin tolerance limited to `GET /stream.mjpg` (tested); loopback only, no LAN exposure; token rotates per service start — restart after a demo; hide the address bar while sharing; access log redacts the token; client cap 2. A separate read-only stream token would remove the flash capability from leaked URLs — deferred unless demos become routine.~~ Read-only (no flash/reset/camera choice); loopback only, no LAN exposure; DNS rebinding from web pages blocked by the stream-only Host check (exact `127.0.0.1:<port>`/`localhost:<port>`/`[::1]:<port>`, else 403); all other endpoints stay bearer-only with Origin rejection; no tokens in URLs and no query strings in the access log; client cap 2; camera LED shows activity; stop the service when the camera must not be watchable; README states this plainly. Residual: a local web page served from `localhost` itself can embed the stream (it is a local process; covered by the acceptance). |
| R19 | Concurrent camera use (rev 5, step 8): a snapshot while a stream runs may fail to open the device, or disturb the stream, if avfoundation doesn't share the camera as expected | Low–medium / low | Default: stream doesn't take the snapshot lock [VERIFY on the Mac in step 8]; fallback: an active stream makes `/snapshot.jpg` return 409 (CLI retries, then exit 4); snapshot failures already surface as 503 with ffmpeg's stderr. |
| R20 | Stalled stream ffmpeg (rev 7, **found in step 8 QA**): an ffmpeg that stops producing data after the first frame (camera hiccup, device grabbed) kept the client slot and the camera after the client disconnected, because the pump blocked on the pipe read and noticed the disconnect only on its next write; two such stalls exhaust the 2-client cap (409 for everyone) until restart | Low–medium / medium (stream unusable, camera LED stays on) | **Mitigated:** the pump `select`s on pipe and client socket (~1 s tick), so a disconnect ends the stream promptly; a no-frame watchdog (~10 s) ends a stalled stream even with the client still attached; every exit path kills the process group and frees the slot once (step 8 lifecycle, tested with a stalling fake). Residual: a camera that delivers frames very slowly but within the watchdog window keeps its slot (by design). |

Open questions: ~~B1–B5~~ (decided, see revision log); ~~the [VERIFY] items in step 1~~ (answered
by the spike); remaining [VERIFY] items: ~~`image2pipe` snapshot form (step 4/6)~~ (closed in
run 2), S3 backtrace
symbolisation (Decision 2, step 6), avfoundation exposure/WB locking (R5), launchd TCC (B2);
rev 3: pinned uv version and its release asset/checksum names (step 2a), uv's hash check of
managed python downloads (Security), ~~Camera permission under `uv run` (step 6)~~ (closed in
run 2); rev 4: ~~espflash monitor without a controlling tty (R16), whether `-framerate 30` is
honoured or falls back (R12), both in acceptance run 2~~ (closed in run 2: espflash fine without a
tty; `-framerate 30` falls back to ~10 fps but captures). Rev 5: camera by name with startup check (R17) —
acceptance run 3 (rev 6: name only, incl. the `--camera 1` refusal); user confirmation of the split capture settings (snapshot 1280x720 ~10 fps,
stream 640x480 @ 30; step 8a); real stream fps in browsers/OBS (step 8 manual); concurrent
snapshot + stream on one camera (R19); browser/OBS acceptance of ffmpeg's mpjpeg part headers
(step 8).

Deliberately deferred: boot test pattern (B3), launchd agent (B2), exposure/WB locking, automatic
comparison against a sim reference render (needs a headless browser and a capture of the wasm sim,
which is its own feature), monitor auto-restart (R3, only if step 6 shows the need), serial
auto-detect by USB VID (R11), a deliberate-panic ELF to test backtrace decoding; rev 3:
third-party Python dependencies (stdlib-only stays, Decision 8), test runs on 3.15 (R7); rev 5:
~~a separate read-only stream token (R18)~~ (superseded in rev 7: the stream needs no token at
all), disambiguation of two cameras with the same name (R17),
LAN exposure of the stream (not needed: the demo shares the screen); compressed capture modes
from the C920 (MJPEG/H.264 on the device, which would lift the USB 2.0 bandwidth cap) — not
reachable via avfoundation as far as observed (it delivered only uncompressed YUV); a
time-based warm-up (only relevant if the snapshot rate changes).

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
- 2026-10-23: **rev 3, "use uv for bench tool"** (user request; interpretation agreed with the
  user). Both tools become uv scripts; tests run via uv pinned to 3.11; uv added to the
  devcontainer. Nothing removed; superseded commands are kept with a "before rev 3" note.
  - Status line: "rev 3: tools run via uv" appended.
  - Decisions table B2: option (a) start command annotated with `uv run tools/bench/bench_host.py`.
  - Context: container bullet notes uv not installed, no `pyproject.toml`/`uv.toml`/
    `.python-version`, and the local-feature/volume pattern of the devcontainer.
  - Options: new "Running the tools: uv (rev 3)" subsection with U1–U3 (uv install: local
    feature / third-party feature / installer script) and V1–V2 (cache and managed pythons: named
    volume / default locations).
  - Decision: "Options considered" lists U/V; item 4 notes the CLI is a uv script; **new item 8**:
    shebang + PEP 723 header (`requires-python = ">=3.11"`, `dependencies = []`), stdlib-only kept
    as an explicit decision, host start via `uv run`, container invocation unchanged (B4
    allow-list still matches), canonical uv test command on 3.11 plus the 3.13 run, tests spawn
    the tools with `sys.executable`, fakes are not uv scripts, `--no-project` note, U1 + V1
    chosen. "Why" gains a uv bullet. Consequences: host prerequisite uv (plain python3 >= 3.11
    as fallback); new container-prerequisite bullet (rebuild, interim fallback).
  - Container CLI: intro notes the uv shebang.
  - Security: new bullet on uv fetching a managed python on first use (when, where from, hash
    check [VERIFY], `UV_PYTHON_DOWNLOADS=never`, uv binary provenance, uv config files, allow-list
    implication).
  - Layout: tool comments mention uv scripts; `tests/` comment; new `.devcontainer/features/uv/`
    entry. The single test command is **replaced** by two (canonical uv 3.11, and the old
    `python3 -m unittest` kept as the default-python run and no-uv fallback); python target line
    mentions the PEP 723 declaration.
  - Plan intro: "green" means both test commands.
  - Plan step 2: rev-3 bullet (header, executable, `sys.executable` in the HTTP test, usage text);
    **new sub-part 2a** (uv feature, devcontainer.json/post-create changes, manual rebuild,
    interim fallback, verify); verify list extended (header, both test commands, `uv run ...
    --help`).
  - Plan step 5: CLI header, `sys.executable` in end-to-end tests, shebang smoke test skipped
    without uv.
  - Plan step 6: `brew install uv` and `uv run` start command (old command noted); record host
    `python_version`; Camera permission under `uv run` [VERIFY].
  - Plan step 7: README rev-3 additions (uv prerequisite, start command, python3 fallback,
    download note, test commands).
  - Risks: R7 notes the lower bound is now tested and an optional 3.15 run; new R13 (uv missing),
    R14 (managed-python download offline), R15 (uv interpreter choice drifts).
  - Open questions / deferred: rev-3 VERIFYs added; third-party deps and 3.15 test runs deferred.
  - Step numbering 1-8 unchanged (2a is a sub-part of step 2).
- 2026-10-23 (step 4, orchestrator note): concurrent `GET /snapshot.jpg` while one is in flight
  returns **409** (fail-fast, same as the flash lock), not a queue — confirmed by the user. Step 5's
  `bench snap` must treat 409 as "camera busy": retry a few times with a short back-off before exiting 4.
  QA minor notes left as is: `warmup` has no upper cap (bounded by the 20 s ffmpeg timeout); full
  ffmpeg stderr is logged per call.
- 2026-10-23 (step 5, orchestrator note after QA): CLI exit-code contract clarified — usage/argument
  errors and unexpected/malformed host responses exit **4** (never 1 or 2, so an agent can't confuse a
  typo with a timeout or a crash with a panic; `BENCH_DEBUG` shows tracebacks); `bench snap` checks
  JPEG magic client-side; `bench log --follow` without `--until` exits 0 on timeout (documented in
  `--help`); the CLI's POST /flash|/reset read timeout (330 s) is asserted > the host's 300 s flash
  timeout. `snap` retries 409 five times with back-off 0.5–4 s, then exits 4.
- 2026-10-23: **rev 4, step-6 acceptance run 1 on the real Mac bench** (fix loop 1 of 2; the code
  fixes are implemented and QA'd in parallel, this revision is doc only). Run results are recorded
  in the new "Acceptance run 1" block of plan step 6. Nothing removed outright; superseded text is
  struck through.
  - Status line: rev-4 note appended (run 1 findings, fix loop 1 of 2, run 2 pending).
  - **Decision 3 changed:** "the default argv does not pass `-framerate`" struck through. Spike 1c
    had been misread: `-framerate 30` gave a fallback warning but captured, while omitting it makes
    avfoundation request 29.97 fps, which the webcam rejects (run 1: 503, ffmpeg exit 251). New
    default `-framerate 30` before `-video_size`, configurable via `--framerate`/`BENCH_FRAMERATE`,
    `0`/empty omits it.
  - Decision 8 (host start): notes uv picked 3.12.12, not the expected 3.15.0.
  - API: `/log` row notes that successful long-polls are no longer access-logged (non-2xx still
    are); `/snapshot.jpg` row notes that unsupported settings can also be rejected outright.
  - Plan step 1 outcome (1c): annotated that `-framerate 30` still captured and that omitting it
    fails, so it is now the default (original wording kept).
  - Host config: `--framerate` default changed from "not passed" (struck through) to `30`, env
    `BENCH_FRAMERATE`, `0`/empty omits.
  - Security: new bullet "children never get the host's terminal" (`start_new_session=True`,
    process-group kill with pid fallback, termios snapshot/restore in `main()`).
  - Plan step 2: post-spike argv note and verify list — "omits `-framerate` unless configured" /
    "no `-framerate` by default" struck through, replaced by `-framerate 30` by default.
  - Plan step 4: new rev-4 bullet with the current argv; verify item "`-framerate` absent by
    default" struck through, replaced by "present by default, before `-video_size`, configurable,
    `0`/empty omits". The 409 concurrency rule and the QA minor notes (orchestrator notes above)
    are unchanged.
  - Plan step 6: run-1 interpreter noted in the health bullet; new "Acceptance run 1" results block
    (health/token, flash with double boot confirmed on hardware, reset 3.94 s, snap failure,
    raw-terminal defect, log noise, items not yet run) and an "Acceptance run 2 (pending)" block
    (re-run everything incl. snap; snapshot timing; `image2pipe`, TCC under `uv run`, ffmpeg
    stderr in the host log, framerate honoured or not, espflash without a controlling tty).
  - Plan step 7: README rev-4 additions (terminal recovery: `pkill -f bench_host.py` from another
    tab, then `reset`; `--framerate`/`BENCH_FRAMERATE`).
  - Risks: R12 extended (hard rejection without `-framerate`; impact low → medium; old mitigation
    "don't pass `-framerate` by default" struck through and replaced); R15 marked materialised,
    harmless (uv picked 3.12.12); **new R16** (child grabs the controlling terminal; mitigation;
    residual: espflash might not run without a controlling tty, [VERIFY] in run 2).
  - Open questions: rev-4 VERIFYs added (R16 tty, R12 framerate honoured).
  - Step numbering 1-8 unchanged.
- 2026-10-23 (fix loop 1 QA, orchestrator note): QA PASS for the rev-4 fixes (framerate default,
  new-session children with process-group kill, termios restore, quiet /log). Residual notes, accepted:
  children now live in their own session, so a SIGKILL of the host orphans espflash/ffmpeg (Ctrl-C and
  SIGTERM go through the host's handler, which kills the groups — verified for SIGTERM); the termios
  restore uses `tcsetattr` and could raise SIGTTOU if the host were run as a background job (foreground
  Terminal start per B2 is unaffected; relevant only if launchd/background start is revisited).
- 2026-10-23: **rev 5, step-6 acceptance run 2 on the real Mac bench; camera-selection defect (fix
  loop 2 of 2, code implemented in parallel, this revision is doc only); step 8 brought into scope
  with its own capture settings after a frame-rate experiment (user requests, same day).** Results
  recorded in the new run-2 block of plan step 6. Nothing pre-existing deleted; superseded text is
  struck through.
  - **Frame-rate experiment** (user, Mac, C920 "HD Pro Webcam C920", ffmpeg 9.0.2,
    `-t 5 -f null -`): 1280x720 @ `-framerate 30` with `-pixel_format` `uyvy422` / `nv12` /
    `yuyv422` → all "Configuration of video device failed, falling back to default", 10 fps;
    1280x720 @ 15 → failed, 10 fps; 1920x1080 @ 30 → failed, 4.9 fps; 640x480 @ 30 → no
    failure, 145 frames in 5 s, fps=31. Interpretation: avfoundation gets uncompressed YUV from
    the C920 over USB 2.0, so the rate is bandwidth-capped by resolution. The pixel-format
    hypothesis (yuv420p unsupported → configuration failed → fallback) is **disproven**.
  - Sequence within this revision, for the record: the user first wanted the stream in scope
    (step 8); then answered "frame rate OK as is, no `--pixel-format`", then corrected that (wants
    a proper frame rate) and asked for an optional `--pixel-format` (env `BENCH_PIXEL_FORMAT`,
    default TBD, candidate `uyvy422`) plus the experiment; the experiment disproved it, so the
    `--pixel-format` option (drafted earlier in this same revision in Decision 3, host config,
    step 8a, step 8 verify, R12, open questions) was **removed again** — the Decision 3 and host
    config drafts are kept struck through, the rest was replaced. Final decision (orchestrator,
    **to be confirmed by the user** at the checkpoint): snapshots unchanged (1280x720,
    `-framerate 30` → ~10 fps, 20-frame warm-up); the stream gets `--stream-size` (default
    `640x480`) and `--stream-framerate` (default `30`), env `BENCH_STREAM_SIZE` /
    `BENCH_STREAM_FRAMERATE`.
  - Status line: rev-4 "step 6 in fix loop 1 of 2, acceptance run 2 pending" struck through;
    rev-5 note added (run 2 passes except camera selection; fix loop 2 of 2; run 3 pending).
  - B1: decided device recorded as "HD Pro Webcam C920"; "device index `1`" struck through as
    not a stable identifier (R17).
  - Decision 3: rev-5 run-2 findings (`-framerate 30` falls back to ~10 fps but captures,
    `uyvy422` auto-selected / benign `yuv420p` override, `image2pipe` works, 20-frame warm-up
    kept); new **frame-rate** paragraph (user wants a real frame rate; the yuv420p /
    `--pixel-format` draft struck through as disproven; experiment results; snapshots keep
    1280x720, stream gets `--stream-size 640x480` / `--stream-framerate 30`, pending user
    confirmation); new **camera selection** paragraph (by name,
    default "HD Pro Webcam C920", startup device listing, refuse missing name with the list,
    index with warning, out of range refused, `Capture screen…` refused); "optional last step"
    for the stream struck through → in scope, mpjpeg pass-through.
  - Consequences: the "photos of the room" bullet notes person/screen capture via a wrong
    index and its mitigation.
  - API: `/stream.mjpg` row "Step 8, optional" replaced ("optional" struck through) by the full
    rev-5 behaviour (content types, mpjpeg + `-boundary_tag`, process-group kill, `?token=` and
    Origin only here, 2-client cap with 409, stream size/framerate, no snapshot camera lock
    [VERIFY]).
  - Host config: `--camera` default `1` struck through → `"HD Pro Webcam C920"`, env
    `BENCH_CAMERA`, startup check rules; `--video-size` marked as the snapshot size; new
    `--stream-size` (env `BENCH_STREAM_SIZE`, default `640x480`), `--stream-framerate` (env
    `BENCH_STREAM_FRAMERATE`, default `30`), `--stream-clients` (default 2); the drafted
    `--pixel-format` struck through as dropped.
  - Security: new bullet "camera selection is host-only, and screen capture is refused" (never
    per request; `Capture screen N` devices; run-2 room photo deleted and not described); new
    bullet on the `/stream.mjpg` exceptions (query token, Origin tolerance) and their cost
    (token in history/OBS config/on screen during a Teams demo; rotates per start; hide address
    bar; access log redacts the token; loopback only, no LAN exposure).
  - Layout: `fake_ffmpeg.py` comment extended (canned `-list_devices` stderr, mpjpeg parts).
  - Plan step 1 outcome: "camera was device `1`" annotated (at that time; not stable; R17).
  - Plan step 4: new rev-5 bullet (framerate/warm-up result, `image2pipe` confirmed, `-i` uses
    the camera name, pure device-list parser and `resolve_camera`, verify additions).
  - Plan step 6: start note "device `1` is the default" struck through → name default;
    "(pending, after the fixes land)" struck through and replaced by the run-2 results (health;
    flash 3.92 s with double boot matched in boot 2; reset 3.92 s; bad paths `../README.md` and
    `/etc/passwd` → 400 → exit 4; service stopped → exit 4; R16 resolved; clean request log;
    snap 2.19 s / ffmpeg 1.91 s / 30 KiB; `image2pipe` and TCC-under-uv VERIFYs closed; ffmpeg
    stderr in the host log; wrong-camera defect with the device list; C920 by name shows the
    panel with no glare; no panic observed). The planned run-2 scope text is kept. New
    "Acceptance run 3 (pending)" block: default name → panel, bogus name → refusal with list;
    the frame-rate experiment noted as separate; run-3 failure → revise with the user (fix
    loop budget spent).
  - Plan step 7: README rev-5 additions (camera name, prefer names, how to list devices with
    `command grep -A10 'video devices'`, startup refusal, index warning, screen refusal,
    host-only setting).
  - **Plan step 8 changed from optional to in scope** ("[optional]", the old one-paragraph spec,
    the old container verify and the old manual check struck through). New: 8a stream capture
    settings (`--stream-size 640x480`, `--stream-framerate 30`, no warm-up; snapshots
    unchanged); response format; framing decision **mpjpeg
    pass-through with a fixed `-boundary_tag`** over image2pipe re-framing (less host code;
    re-framing is the fallback if browsers/OBS reject ffmpeg's part headers); lifecycle (new
    session, process-group kill on disconnect/shutdown, write timeout); client cap 2 → 409;
    snapshot concurrency decision (no shared lock, [VERIFY], 409 fallback); auth exceptions;
    README items; container verify list (boundary match, multipart parse, content types,
    disconnect kills fake, cap, token-in-query and Origin only here, token redaction,
    stream argv defaults/overrides with the snapshot argv unchanged, snapshot during stream);
    manual verify (Safari/Chrome/OBS at 640x480, fps ~30 recorded, no "configuration failed",
    camera LED off on close, concurrency, Teams share without the address bar).
  - Risks: R4 notes TCC under `uv run` confirmed; R5 notes no glare at the current mount; R12
    extended with run-2 results, the yuv420p hypothesis struck through as disproven, the
    experiment's resolution-bound rates and the snapshot/stream split; R16 residual struck through → resolved in run 2 (SIGKILL-orphan residual
    accepted); **new R17** (camera index instability → wrong camera or screen; mitigations);
    **new R18** (stream token exposure); **new R19** (concurrent camera use by snapshot and
    stream).
  - Open questions: `image2pipe`, TCC under `uv run`, R16 tty and R12 framerate VERIFYs struck
    through as closed in run 2; rev-5 items added (camera by name in run 3, user confirmation
    of the snapshot/stream capture split, real stream fps in browsers/OBS, R19 concurrency,
    mpjpeg header acceptance).
  - Deferred: read-only stream token (R18), same-name camera disambiguation (R17), LAN exposure
    of the stream, compressed C920 capture modes (MJPEG/H.264; not reachable via avfoundation as
    far as observed), time-based warm-up.
  - Step numbering 1-8 unchanged (8a is a sub-part of step 8).
- 2026-10-23 (orchestrator note): user confirmed the rev-5 capture split — sharp 1280x720 snapshots,
  640x480@30 stream — and step 8 (stream) in scope; demo is a Teams share of a Mac browser window
  (loopback only). The "to be confirmed by user" marks in rev 5 are resolved by this entry.
- 2026-10-23: **rev 6, camera by name only** (user decision after QA of fix loop 2; doc only,
  code fix implemented separately). QA context: QA of fix loop 2 found that the snapshot passed
  the **configured index** to ffmpeg instead of the resolved device name, so it would have hit a
  shifted device again (the run-2 defect). The step-6 fix-loop budget was exhausted (this is fix
  loop 3 of 2); the **user authorised a third fix** on condition of this simplification. Nothing
  deleted; superseded text is struck through.
  - Rule: the camera is selected **by name only**. An all-digit `--camera`/`BENCH_CAMERA`
    (`str.isdigit()`, incl. non-ASCII digits) is refused at startup with **exit 2**, "use a
    device name" and the device list. Capture (snapshot and step-8 stream) always uses the
    resolved device name. Screen-capture refusal is an exact `Capture screen <digits>` match
    (`re.fullmatch`), not a prefix match. Device names are kept exactly as ffmpeg prints them
    (exact, case-sensitive comparison; no trimming or normalisation).
  - **Removed** (struck through) everywhere: "all-digit value accepted as an index with a stderr
    warning naming the mapped device", "out-of-range index refused", "`Capture screen…` refused
    by name or index" (prefix rule), "prefer names over indices" — in B1 ("name or index", "and
    the recommended setting"), Decision 3, host config `--camera`, Security camera bullet, R17
    mitigation, step 4 rev-5 note, step 6 run-2 fix summary ("index warning"), step 7 README
    rev-5 additions.
  - Added: Status line rev-6 note (rev-5 "fix loop 2 of 2, run 3 pending" struck through);
    Decision 3 rev-6 paragraph; Consequences note; step 4 rev-6 note (argv builder takes the
    resolved name from `resolve_camera`) and verify additions (`--camera 1`/`١`/`BENCH_CAMERA=1`
    → exit 2; exact screen match; snapshot argv carries the name); step 6 "QA of fix loop 2"
    bullet; run-3 block retitled to "after fix loop 3", new check **"start with `--camera 1` →
    refused"** and a name-in-`-i` check on the snapshot, the "budget spent" sentence struck
    through and replaced (no further fix loop without the user's explicit OK); step 7 README
    (names only, exact match, quote names in the shell); step 8 stream argv uses the resolved
    name; R17 mitigation rewritten and a residual added (an all-digit real device name cannot be
    selected; accepted); open questions note for run 3.
  - Step numbering 1-8 unchanged.
- 2026-10-23 (fix loop 3 QA, orchestrator note): QA PASS for names-only camera selection. The code
  refuses `str.isnumeric()` values (a superset of `isdigit()`; also refuses e.g. `½`, `²`) — accepted.
  `--camera` help text updated to "exact avfoundation video device name (indices are refused)".
- 2026-10-23 (step 7, orchestrator note): skill dry-run by a fresh agent given only SKILL.md on the
  real board → PASS (flash → `alive 0s` in boot 2 → snapshot judged correctly). Its 12 feedback
  points were applied to the skill (real uv-missing error/exit 127, build command
  `cd firmware && cargo build --release`, always rebuild + record dirty state and ELF mtime,
  expectation and exit codes in the report, timeout scopes and Bash timeout ≥ 400 s, snapshot
  INCONCLUSIVE/retake rule, strict PASS bar, snap/health exit codes, explicit permission to delete a
  wrong-camera photo). Design's `lcd: scanout started` marker: the firmware logs either
  `lcd: scanout started …` (default) or `lcd: bounce scanout started …` (bounce feature), so the skill
  names the common substring `scanout started`. B4 allow-list added to the git-excluded
  `.claude/settings.local.json`: `Bash(tools/bench/bench:*)` and `Bash(python3 tools/bench/bench:*)`
  (the python3 form until the container has uv).
- 2026-10-23: **rev 7, step 8 QA (fix loop 1) and user decision: anonymous stream view** (doc
  only; the code fix is implemented in parallel). QA findings: (1) the service token was logged
  in clear for a URL-encoded key `?%74oken=` — the access-log redaction regex didn't match it,
  while `parse_qs` decoded and accepted it; (2) an ffmpeg that stalled after the first frame kept
  the client slot and the camera after the client disconnected (blocking pipe read; the
  disconnect was only noticed on the next write); (3) the README stream URL was a `$(...)` shell
  string, unusable in a browser/OBS. The user was told the risks of dropping stream auth (any
  local process, any podman container, and web pages via DNS rebinding could watch the webcam)
  and decided: "anonymous public stream view is OK. Auth for everything else is good." Nothing
  deleted; superseded text is struck through.
  - Status line: rev-7 note appended.
  - API intro: "only `/stream.mjpg` also accepts `?token=`" struck through; new rule: bearer
    header only, no endpoint accepts a query token (a query-only token → 401), stream needs no
    auth, access log never logs query strings.
  - API `/stream.mjpg` row: "bearer **or** `?token=`" struck through → no auth; exact Host check
    (`127.0.0.1:<port>`, `localhost:<port>`, `[::1]:<port>`, else 403); `select` on pipe + socket
    (~1 s) and no-frame watchdog (~10 s). Origin exemption unchanged.
  - Consequences: the "photos of the room" bullet notes that any local process/podman container
    can watch the stream (accepted).
  - Security: token-only-discriminator bullet notes the stream exception; Origin bullet adds that
    token-protected endpoints don't check `Host` (the container uses
    `host.containers.internal:7777`); **new bullet** "tokens only in the Bearer header, no query
    strings in the access log" (with the `%74oken` finding); the rev-5 stream-exceptions text
    (query token, history/OBS/on-screen exposure, rotate after demo, hide the address bar,
    redaction) **struck through** and replaced by the rev-7 text (unauthenticated read-only view,
    explicit user acceptance, Host-check DNS-rebinding guard, remaining guards); residual-risk
    bullet notes the anonymous stream.
  - Layout: `fake_ffmpeg.py` comment gains a stall mode.
  - Plan step 7: README rev-7 additions (literal URL `http://127.0.0.1:7777/stream.mjpg`, no
    `$(...)`; allowed host names; plain statement of the anonymous access; token-rotation/address-bar
    caution dropped).
  - Plan step 8: lifecycle gains the rev-7 select/watchdog fix; Auth bullet struck through and
    replaced; README bullet's `?token=$(cat .bench/token)` URL and the demo caution struck through,
    replaced by the literal URL and the anonymous-access note; container verify: the `?token=`
    accepted / token-not-logged items struck through, new items (anonymous 200 for the three
    allowed Hosts; 403 for other/missing Host incl. `host.containers.internal`; wrong bearer
    ignored on the stream; `?token=` and `?%74oken=` → 401 on other endpoints; bearer + container
    Host → 200 elsewhere; no query strings in the access log; disconnect during a stall frees the
    slot in ~1–2 s; watchdog ends a stalled stream; no orphans); manual verify: "address bar
    hidden" struck through, no-token open and a Host-403 curl added.
  - Risks: R8 notes the anonymous stream; **R18 rewritten** (old token-exposure text struck
    through → anonymous local camera access, accepted by the user, mitigations); **new R20**
    (stalled stream ffmpeg; mitigated by select + watchdog).
  - Deferred: "a separate read-only stream token" struck through as superseded.
  - Step numbering 1-8 unchanged.
- 2026-10-23 (step 8 close, orchestrator note): the token-free stream was blocked twice by Claude
  Code's auto-mode safety check ("security weaken") when delegated; after the user's explicit
  in-session approval ("tokenfree stream approved") the orchestrator made the `_authorized` change
  itself and reviewed the engineer's test/README follow-up. Re-QA PASS (Host-check bypass attempts,
  auth matrix, stall/disconnect, SIGTERM, leaks). QA low notes: the access log writes a literal `?...`
  marker when a query was present (no query content is ever logged — this refines "contains no `?`");
  `log_message` no longer crashes on malformed request lines (`getattr` for command/path); duplicate
  Host headers: only the first is checked — browsers can't send duplicates, accepted.
  Manual acceptance on the Mac by the user: stream renders in the browser at 30 fps (640x480), camera
  LED goes off when the tab closes, snapshot during an open stream works (R19 resolved). Snapshots now
  show visible moiré at the current mount (structural judging unaffected; mitigate by angle/distance).

---
name: board-verify
description: Flash the firmware to the real ESP32-S3 board, read its serial log and photograph the panel via the host bench service, then judge the result. Use after a firmware change that needs on-board verification (boot, display output, a fix that "waits on the user's board") instead of asking the user to flash and report.
---

# Board verify

Flash a built ELF, watch the boot log, photograph the panel, judge against an expectation.
Everything goes through `tools/bench/bench`, which talks to a service the user runs on the Mac
(USB and camera can't enter the container). Details: `docs/design/board-bench.md`.

## Preconditions

1. `tools/bench/bench health` must exit 0 (serial port chosen and present, tools present).
2. If `tools/bench/bench` prints `env: 'uv': No such file or directory` (shell exit 127) or any
   other "uv missing" error, the container predates the uv feature. That is **not** a failed
   health check: use `python3 tools/bench/bench ...` for every call.
3. If health exits 4 (service not running, no token, port/tools missing, board unplugged; the
   reason is on stderr/in the JSON), **say so, report it and ask the user** to start the
   service or to flash and report by hand. Don't start the service or work around it.

## Procedure

1. **Write the expectation first** (before flashing, before any image): the log marker that
   proves boot and what the panel should show.
   - Default marker `alive 0s` (proves boot and a running main loop). Use another or an extra
     marker only when the change under test logs its own. For display work
     `ch422g: backlight on` and `scanout started` (logged as `lcd: scanout started …`, or
     `lcd: bounce scanout started …` on the bounce path) come before `alive 0s` anyway.
2. **Rebuild** unless told otherwise (never verify a stale ELF):
   `cd firmware && cargo build --release`, giving
   `firmware/target/xtensa-esp32s3-none-elf/release/firmware` (relative to the repo root; named
   copies under `firmware/target/` also work). Record `git rev-parse --short HEAD`,
   whether the tree is dirty (`git status --porcelain`) and the ELF mtime (`ls -l`).
3. `tools/bench/bench flash <elf> --until '<marker>' --timeout 60`. Set the Bash tool timeout
   to at least 400000 ms for flash/reset calls: `--timeout` only covers the wait for the marker
   after flashing completes; flashing itself has the host's 300 s cap. The default `--fail-on`
   already catches `panic|Guru Meditation|abort\(\)|Exception`. Exit codes:
   - **0** marker matched. Continue.
   - **1** `--fail-on` matched or boot loop (more than `--max-boots`, default 3).
   - **2** timeout without the marker.
   - **3** monitor died after a successful flash.
   - **4** transport/auth/argument error (service down, bad path, busy, 503).
   - **5** flash failed (espflash exited before finishing, or 300 s timeout).

   On anything but 0: report the printed log excerpt and **stop**. No snapshot, no retry loop.
4. `tools/bench/bench snap --delay 1` prints an absolute path under `.bench/snaps/`; pass it to
   `Read` as is. Exit 0 = JPEG written; **4 = any failure** (service unreachable, auth, camera
   busy after retries, ffmpeg error, non-JPEG), message on stderr: report it and ask the user.
5. Judge the image against the expectation from step 1.
   - Unusable photo (blurry, off-frame, glare hiding content): retake once with
     `bench snap --delay 2`. Still unusable: report `visual: INCONCLUSIVE` (neither PASS nor
     FAIL) and ask the user to check the camera.
   - A dark panel is a finding: FAIL unless a dark panel was expected.
   - **PASS only if every element of the written expectation is visible and nothing contradicts
     it.** List anything unexpected as a finding. If the expectation was too vague to judge a
     detail, say so instead of guessing.

## Report

- **PASS** / **FAIL** / visual INCONCLUSIVE (a visual PASS needs steps 4 and 5 in this run)
- **Expectation (written before flashing)**
- ELF path, git rev, dirty or clean, ELF mtime
- bench exit code(s)
- log excerpt (incl. the `--until`/`--fail-on` summary line)
- snapshot path, findings

## Rules

- Never claim a visual PASS without having read an image in this run.
- The double boot is normal: after flashing the chip boots twice (`--- boot 1 ---` is cut off,
  `--- boot 2 ---` completes). Only `--fail-on` text or a boot loop is a failure.
- The service owns the serial port. Never run `espflash` yourself.
- `tools/bench/bench reset` re-flashes the **last flashed image** (full boot log, ~4 s). It is
  not a bare reset, and exits 4 if nothing was flashed since the service started.
- Don't commit `.bench/` (token, snapshots; gitignored). Never print the token.
- Privacy: images and logs may show live traffic or the room. Describe nothing beyond the panel
  and don't share or paste images. If a photo shows a person or room instead of the panel,
  **`rm` that one file under `.bench/snaps/`** (explicitly allowed and required, even in
  otherwise read-only verification), don't describe it, report "wrong camera" and stop. The user
  fixes `--camera` on the host; it can't be changed from the container.
- If the user gave a sim reference screenshot, compare against it; there is no headless browser
  here to make one.

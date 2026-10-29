# esp32-adsb

ADS-B receiver/decoder on the ESP32, written in Rust (`no_std`, [esp-hal](https://github.com/esp-rs/esp-hal)).

## Dev container

`.devcontainer/` provides a Debian trixie container built from dev container features:

| Feature | Provides |
| --- | --- |
| `devcontainers/features/common-utils` | zsh as default shell |
| `devcontainers/features/rust` | stable Rust, rust-analyzer, clippy, rustfmt |
| `devcontainers/features/node` | Node LTS (needed by Mason-installed LSPs in Neovim) |
| `rio/features/chezmoi` | chezmoi |
| `./features/esp-rs` (local) | `espup` (Xtensa `esp` toolchain + RISC-V targets, all chips), `espflash`, `cargo-espflash`, `esp-generate` |
| `./features/wasm-sim` (local) | `wasm32-unknown-unknown` target, `trunk`, `wasm-bindgen-cli` (versions are feature options; trunk downloads a matching wasm-bindgen/wasm-opt itself if the pinned one differs from `Cargo.lock`) |
| `./features/neovim` (local) | Neovim (official release binary) |

On first create, `post-create.sh` applies the dotfiles from
[terminatorausbikinibottom/dotfiles](https://github.com/terminatorausbikinibottom/dotfiles)
with chezmoi and runs their `zsh-install-plugins.sh`.
The espup environment (`export-esp.sh`) is sourced system-wide from
`/etc/zsh/zshenv` and `/etc/bash.bashrc`, so the dotfiles don't need to know about it.

Port 8080 (simulator, `trunk serve`) is in `forwardPorts`, so VS Code forwards it to the host's
`localhost`. Without VS Code (devcontainer CLI), add `"-p", "127.0.0.1:8080:8080"` to `runArgs`
(always with the `127.0.0.1:` prefix) and run `trunk serve --address 0.0.0.0` in the container.

### Podman

VS Code: set `"dev.containers.dockerPath": "podman"` in your user settings.
CLI: `devcontainer up --workspace-folder . --docker-path podman`.

`runArgs` uses `--userns=keep-id:uid=1000,gid=1000` so the bind-mounted workspace is
owned by the container's `vscode` user.

### Flashing

The podman machine on macOS (libkrun/applehv) cannot pass USB serial devices into
containers. Build in the container, flash from the host — `target/` lives in the
shared workspace:

```sh
# host, once
brew install espflash        # or: cargo binstall espflash
# host, per flash (use your usbmodem port; the suffix depends on the USB port, e.g. usbmodem101 or usbmodem11401)
espflash flash --monitor --port /dev/cu.usbmodem101 \
  firmware/target/xtensa-esp32s3-none-elf/release/firmware
```

If the bench service below is running, stop it first (Ctrl-C): only one process can hold the
serial port.

The Waveshare ESP32-S3-Touch-LCD-7 enumerates as two serial ports:

| Connector | macOS device | What it is |
| --- | --- | --- |
| USB | `/dev/cu.usbmodem<N>` (VID 303A; the number depends on the USB port) | native USB-Serial-JTAG — **use this one** |
| UART | `/dev/tty.usbmodem<serial>` | USB-UART bridge to UART0 |

`esp-println` is in `auto` mode: while a host is attached to the native USB port, all log
output goes there and UART0 stays silent. Flashing over the native port also resets reliably
into the app. Unplug the USB cable if you want logs on the UART port instead.

On a Linux host you can pass the device instead, e.g. add
`"--device=/dev/ttyUSB0"` (or `/dev/ttyACM0`) to `runArgs`, and `cargo run` works inside the container.

### Bench service (automated)

Lets the agent in the container flash the board, read the serial log and photograph the panel
without a human (skill `.claude/skills/board-verify`). A small service runs on the Mac; the
container talks to it at `http://host.containers.internal:7777`.

Host prerequisites (once):

```sh
brew install uv ffmpeg       # plus espflash, see above
```

Start it from the repo root in a macOS Terminal tab (leave it running):

```sh
uv run tools/bench/bench_host.py
# fallback without uv (python >= 3.11): python3 tools/bench/bench_host.py
```

- **Camera permission:** macOS asks to let the terminal app use the camera on the first snapshot;
  allow it (the grant belongs to Terminal/iTerm, which is why it is not a launchd agent).
- **Camera by name only.** Default `"HD Pro Webcam C920"`; set another with `--camera "<name>"` or
  `BENCH_CAMERA`. Numeric indices are refused, because they shift when an iPhone (Continuity)
  camera appears. List the names, and quote the one you pick exactly as printed:
  `ffmpeg -f avfoundation -list_devices true -i "" 2>&1 | command grep -A10 'video devices'`
  (`command grep` because `grep` may be aliased to `rg`). The service refuses to start if the
  name is missing, and always refuses `Capture screen N` devices. The camera can only be set on
  the host.
- **Serial port:** auto-detected (the single `/dev/cu.usbmodem*`; zero or several is an error
  listing them). Override with `--port /dev/cu.usbmodem<N>`.
- **One holder for the serial port.** After a flash the service keeps the port open (monitor)
  until the next flash/reset or until you stop it. Stop the service before running `espflash`
  by hand.
- **Terminal recovery (just in case):** if the service's terminal ever shows staircase output and
  ignores Ctrl-C, run `pkill -f bench_host.py` from another tab, then `reset` in the broken one.
- **uv downloads:** uv may fetch a python >= 3.11 on first use if none is installed; set
  `UV_PYTHON_DOWNLOADS=never` (host or container) to forbid it. The tools then use an installed
  python or fail with a clear message.
- **launchd:** not recommended yet (Camera permission / TCC is unreliable without a terminal app).
- Other options: `uv run tools/bench/bench_host.py --help` (`--video-size`, `--framerate`,
  `--warmup`, `--listen`, ...). A 503 from a snapshot includes ffmpeg's stderr, which lists the
  camera's supported modes.

Security: the service binds to `127.0.0.1` only (non-loopback `--listen` is refused) and needs a
bearer token that it regenerates on every start into `.bench/token` (mode 0600, gitignored,
shared with the container through the bind mount). Whoever can read that file (anything running
as you on the Mac, or any container that mounts this repo) can flash ELFs from `firmware/target/`
and take camera photos. Stop the service when you are not using it. Snapshots and logs may show
live traffic or your room: they stay in `.bench/`; don't commit or share them.

Live stream (demo): on the Mac run `open http://127.0.0.1:7777/stream.mjpg` for a 640x480 @ 30 fps
live view (`--stream-size`, `--stream-framerate`, `--stream-clients`; at most 2 viewers, then
409; closing the tab stops ffmpeg). The stream needs no token: while the service runs, anything on
the Mac and any podman container can view the camera (loopback only, not the LAN). The Host check
(only `127.0.0.1`, `localhost` and `[::1]` with the service port) blocks DNS-rebinding reads; any
web page can still embed the stream (switching the camera on and occupying the 2 slots) but
cannot read the pixels. Flashing, logs and snapshots still need the token; a snapshot works
while streaming. The access log never prints query strings.

Use from the container (uv comes with the devcontainer; until it is rebuilt, prefix with
`python3`):

```sh
tools/bench/bench health
tools/bench/bench flash firmware/target/xtensa-esp32s3-none-elf/release/firmware --until 'alive 0s' --timeout 60
tools/bench/bench log --session --follow --timeout 10
tools/bench/bench reset --until 'ch422g: backlight on'   # re-flashes the last image
tools/bench/bench snap --delay 1                         # prints .bench/snaps/<time>.jpg
```

Build first with `cd firmware && cargo build --release` (ELF at
`firmware/target/xtensa-esp32s3-none-elf/release/firmware`). `build.rs` reads the config from
`firmware/.env` (gitignored; `cp .env.example .env` and fill in the keys); a variable set in the
shell wins over the file, e.g. `ADSB_DEMO=busy cargo build --release`. If `tools/bench/bench` fails with
`env: 'uv': No such file or directory` (exit 127), the container has no uv yet: use
`python3 tools/bench/bench ...`. `snap` and `health` exit 0 or 4 (any failure, message on stderr).

Exit codes of `flash`/`reset`/`log`: 0 `--until` matched, 1 `--fail-on` matched or boot loop,
2 timeout, 3 monitor died, 4 transport/argument error, 5 flash failed. The chip boots twice after
a flash; this is normal.

Tests (from the repo root, in the container):

```sh
uv run --python 3.11 python -m unittest discover -s tools/bench/tests -t tools/bench
python3 -m unittest discover -s tools/bench/tests -t tools/bench   # no uv needed
```

The uv feature (`.devcontainer/features/uv`) needs a devcontainer rebuild; before that,
`uv` is missing in the container and only the `python3` forms work.

## Simulator

Browser simulator of the UI (`sim/`), driven by fictitious scenarios or, optionally, a live feed.

```sh
cd sim && trunk serve                                         # 127.0.0.1:8080, open http://127.0.0.1:8080
cd sim-bridge && cp .env.example .env && cargo run       # only needed for live mode
```

The bridge has its own `sim-bridge/.env` (gitignored; see `.env.example` for the keys), read from
the directory it is started in or from `ADSB_SIM_ENV_FILE`. It listens on loopback only and trunk
proxies `/ws` and `/local` to it. Without a `.env` the simulator runs scenarios only.

Privacy: live mode shows the real receiver area and traffic. Don't share live screenshots or
the `.env`.

## Creating the firmware project

Inside the container:

```sh
esp-generate --headless -s -o esp32s3 -o esp32s3-wroom-1-octal-psram -o embassy -o unstable-hal -o alloc -o wifi -o log -o esp-backtrace firmware
esp-generate list-options
```

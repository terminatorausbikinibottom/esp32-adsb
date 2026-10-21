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
# host, per flash
espflash flash --monitor --port /dev/cu.usbmodem101 \
  firmware/target/xtensa-esp32s3-none-elf/release/firmware
```

The Waveshare ESP32-S3-Touch-LCD-7 enumerates as two serial ports:

| Connector | macOS device | What it is |
| --- | --- | --- |
| USB | `/dev/cu.usbmodem101` (VID 303A) | native USB-Serial-JTAG — **use this one** |
| UART | `/dev/tty.usbmodem<serial>` | USB-UART bridge to UART0 |

`esp-println` is in `auto` mode: while a host is attached to the native USB port, all log
output goes there and UART0 stays silent. Flashing over the native port also resets reliably
into the app. Unplug the USB cable if you want logs on the UART port instead.

On a Linux host you can pass the device instead, e.g. add
`"--device=/dev/ttyUSB0"` (or `/dev/ttyACM0`) to `runArgs`, and `cargo run` works inside the container.

## Creating the firmware project

Inside the container:

```sh
esp-generate --headless -s -o esp32s3 -o esp32s3-wroom-1-octal-psram -o embassy -o unstable-hal -o alloc -o wifi -o log -o esp-backtrace firmware
esp-generate list-options
```

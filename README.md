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
| `./features/neovim` (local) | Neovim (official release binary) |

On first create, `post-create.sh` applies the dotfiles from
[terminatorausbikinibottom/dotfiles](https://github.com/terminatorausbikinibottom/dotfiles)
with chezmoi and runs their `zsh-install-plugins.sh`.
The espup environment (`export-esp.sh`) is sourced system-wide from
`/etc/zsh/zshenv` and `/etc/bash.bashrc`, so the dotfiles don't need to know about it.

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
espflash flash --monitor target/<triple>/release/<app>
```

On a Linux host you can pass the device instead, e.g. add
`"--device=/dev/ttyUSB0"` (or `/dev/ttyACM0`) to `runArgs`, and `cargo run` works inside the container.

## Creating the firmware project

Inside the container:

```sh
esp-generate --headless -o esp32 -o embassy -o unstable-hal -o alloc esp32-adsb   # pick your chip
esp-generate list-options
```

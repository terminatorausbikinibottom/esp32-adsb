#!/usr/bin/env bash
# Runs once after the container is created.
set -euo pipefail

DOTFILES_REPO="${DOTFILES_REPO:-terminatorausbikinibottom}"

# The cargo registry volume is created root-owned on first use.
sudo chown -R "$(id -u):$(id -g)" /usr/local/cargo/registry
# Same for the Claude Code config volume.
sudo chown -R "$(id -u):$(id -g)" "$HOME/.claude"

# ~/.ssh is root-owned (created by the read-only config/key bind mounts), so ssh can't record
# host keys. Pin GitHub's published host keys system-wide instead (fetched over verified HTTPS).
curl -fsSL https://api.github.com/meta \
  | python3 -c 'import json, sys; [print("github.com", k) for k in json.load(sys.stdin)["ssh_keys"]]' \
  | sudo tee /etc/ssh/ssh_known_hosts >/dev/null

# Dotfiles via chezmoi (--force: replace the image's stock ~/.zshrc etc. without prompting).
chezmoi init --apply --force "$DOTFILES_REPO"

# Shell tooling the dotfiles expect (fzf, zoxide, eza, bat, ripgrep, direnv, starship, ...).
if [[ -x "$HOME/.local/bin/zsh-install-plugins.sh" ]]; then
  "$HOME/.local/bin/zsh-install-plugins.sh"
fi

echo
echo "esp-rs toolchain:"
. /usr/local/share/esp-rs/export-esp.sh
rustup toolchain list
espflash --version
esp-generate --version
claude --version

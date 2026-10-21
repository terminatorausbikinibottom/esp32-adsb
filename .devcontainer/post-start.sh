#!/usr/bin/env bash
# Runs on every container start: clone/update the agentic harness and wire it into ~/.claude.
set -euo pipefail

HARNESS_REPO="${HARNESS_REPO:-git@github.com:terminatorausbikinibottom/agentic-harness.git}"
# install.sh insists on exactly this location.
HARNESS_DIR="$HOME/agentic-harness"

# No tty here: don't stop at the github.com host-key prompt.
export GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=accept-new"

if [[ -d "$HARNESS_DIR/.git" ]]; then
  # Best effort: offline or local changes must not block startup.
  git -C "$HARNESS_DIR" pull --ff-only || echo "post-start: harness pull failed; using existing checkout" >&2
else
  git clone "$HARNESS_REPO" "$HARNESS_DIR"
fi

# --yes: no tty to confirm; conflicting ~/.claude entries are moved to a backup dir, not deleted.
"$HARNESS_DIR/scripts/install.sh" --yes

# GitHub CLI (`gh`) in the dev container

Status: **draft, awaiting user review.** Date: 2026-10-21. Branch: `feature/adsb-display`.

Conventions (same as [adsb-display.md](adsb-display.md)):

- **[VERIFY]** marks a fact I could not confirm from inside the container (no network access
  during design). Confirm it at implementation time. Don't treat it as fact.
- Step tags: **[devops]** = repo, tooling or container config; **[devops/CI]** = touches
  `.github/workflows` (CI/CD); **[manual]** = needs the user (container rebuild, interactive login).

---

## Decisions needed from user

| # | Decision | Options | Recommendation | Blocks |
|---|----------|---------|----------------|--------|
| G1 | **How does `gh` get credentials?** | (a) `gh auth login` once inside the container, config persisted in a named volume. (b) Pass a host token through: `remoteEnv` `GH_TOKEN=${localEnv:GH_TOKEN}`. (c) Both. | **(a)**, see [Auth](#auth). (b) stays documented as an alternative. | Step 3 |
| G2 | **Build the dev container in CI** to check `gh` (and the rest of the toolchain)? | (a) No, local verification only. (b) New CI job with `devcontainers/ci`. | **(a) for now.** Revisit if the devcontainer starts breaking unnoticed. | Step 6 (optional) |

---

## Context

The `gh` CLI is wanted inside the dev container: create and inspect PRs, watch CI runs
(`gh run watch`, needed for the "push and check the pipeline goes green" verification in
adsb-display step 0.5), read issues. Today it isn't installed (no `gh` binary in the image).

Current setup (`.devcontainer/`):

- `devcontainer.json` uses the plain image `mcr.microsoft.com/devcontainers/base:trixie` (Debian
  13). **No Dockerfile.** Everything is installed by dev container features:
  - published ones (`common-utils`, `rust`, `node`, `python`, `claude-code`, `chezmoi`);
  - two local ones (`./features/esp-rs`, `./features/neovim`). `neovim/install.sh` shows the local
    pattern for an arch-aware release-binary download (`x86_64` / `aarch64|arm64`).
- `devcontainer-lock.json` pins every published feature by version and digest.
- Persistent state uses **named volumes** (`esp32-adsb-cargo-registry`,
  `esp32-adsb-claude-config` at `~/.claude`). `post-create.sh` `chown`s each volume to `vscode`
  because podman creates them root-owned. It also prints tool versions as a smoke check
  (`espflash --version`, `claude --version`, ...), with `set -euo pipefail`.
- Git talks to GitHub over **SSH**: `~/.ssh/config` and `~/.ssh/personal` are bind-mounted
  read-only from the host. There's no HTTPS git credential helper, and no GitHub token in the
  container.
- The runtime is rootless podman (`--userns=keep-id`). The host the user works on now is
  **aarch64 Linux**. The adsb-display doc also describes a macOS podman machine (Apple Silicon,
  also arm64). CI runners (`ubuntu-latest`) are amd64. So the install must work on **arm64 and
  amd64**.
- CI (`.github/workflows/ci.yml`) builds the Rust projects only. It **does not build the dev
  container**, so CI won't notice a broken devcontainer change.
- `README.md` has a table of the features and what they provide.

## Options

### Install

| # | Option | Trade-off |
|---|--------|-----------|
| A | **Official feature `ghcr.io/devcontainers/features/github-cli:1`**, gh version pinned through the feature's `version` option, feature digest pinned in `devcontainer-lock.json`. | One line, matches how every other published tool is installed here, multi-arch, maintained upstream. You trust one more feature publisher (the same one that already ships `common-utils`/`rust`/`node`/`python`). |
| B | **Local feature `./features/gh`** that adds GitHub's apt repo (`cli.github.com/packages`, signed keyring) and runs `apt-get install gh`. | Full control and the official package source, but ~25 lines of our own script to maintain (keyring path, `signed-by`, arch). apt pinning to an exact version needs `gh=<ver>`, and old versions can drop out of the repo. |
| C | **Local feature downloading the release tarball** (`gh_<ver>_linux_{amd64,arm64}.tar.gz`) like `features/neovim`, with checksum verification. | Exact pin, no apt repo, but we own the arch mapping and the checksum handling, and there are no updates unless we bump. |
| D | **Debian's own `gh` package** (`apt-get install gh` from trixie main). | Simplest, but the version lags upstream by a lot [VERIFY the trixie version], and some newer subcommands/flags may be missing. |
| E | Install in `post-create.sh`. | Runs on every container create instead of being cached in the image layer, and mixes image setup into the user-setup script. Rejected. |

### Auth

| # | Option | Trade-off |
|---|--------|-----------|
| a | **Interactive `gh auth login`** (device flow: prints a code, you open github.com/login/device on the host) once per volume; `~/.config/gh` is a named volume `esp32-adsb-gh-config`. | Same pattern as the Claude Code config volume. Survives rebuilds, no host setup. The token is stored **in plain text** in `hosts.yml` inside the volume, because there's no keyring daemon in the container [VERIFY gh's fallback message]. |
| b | **`remoteEnv: { "GH_TOKEN": "${localEnv:GH_TOKEN}" }`**, the user exports a (fine-grained) PAT on the host. | Nothing stored in the container, and the token is easy to scope and rotate. It needs the variable exported in the environment that **starts** VS Code / `devcontainer up`, which is easy to get wrong. When unset on the host it becomes an empty `GH_TOKEN`, and I'm not sure gh treats empty as unset [VERIFY]. The token is visible to every process in the container (`env`). |
| c | Both. | Most flexible; `GH_TOKEN` overrides the stored login when set, which can confuse "which identity am I?". |

Never: a token in `containerEnv`, `devcontainer.json`, a Dockerfile, the lock file or any file in
the repo. `containerEnv` also ends up in the container's config (`podman inspect`), so it's wrong
even with `${localEnv:...}`. `remoteEnv` is applied per process by the tooling.

## Decision (ADR)

- **Context:** as above. `gh` must work on arm64 and amd64, fit the features-only setup, be
  pinned like everything else, and get credentials without baking secrets into the image or repo.
- **Options considered:** install A–E, auth a–c (tables above).
- **Decision:**
  1. **Install with option A**, the official `github-cli` feature, with an explicit
     `"version": "<X.Y.Z>"` (the current gh release at implementation time [VERIFY]) instead of
     `latest`, and `"installDirectlyFromGitHubRelease": true` if the option exists in the
     resolved feature version [VERIFY option name and default]. The feature digest goes into
     `devcontainer-lock.json`. No `overrideFeatureInstallOrder` entry is needed: it doesn't
     depend on our local features.
  2. **Auth with option a** (G1, pending user confirmation): a named volume
     `esp32-adsb-gh-config` at `/home/vscode/.config/gh`, `chown`ed in `post-create.sh` like the
     other volumes, and a one-time `gh auth login` by the user. `post-create.sh` sets
     `gh config set git_protocol ssh` so `gh repo clone` / `gh pr checkout` use the existing SSH
     setup rather than adding an HTTPS credential path. We **don't** run `gh auth setup-git`.
     Option b is documented in the README as an alternative and not wired in, which avoids the
     empty-`GH_TOKEN` question.
  3. **Verify locally** (`gh --version` in the `post-create.sh` smoke list, plus a manual auth
     check). No CI devcontainer build for now (G2).
- **Why:**
  - A is the least code and the existing convention: published tools come from published
    features, and local features exist only where no good published one exists (esp-rs) or for
    a specific install source (neovim).
  - B/C give the same result with more of our own code.
  - D trades freshness for nothing we need.
  - Auth (a) mirrors the Claude Code volume and needs nothing on the host, so it works the
    same on the Linux and macOS hosts.
- **Consequences:**
  - One more published feature and one more named volume.
  - A plaintext token in a podman volume on the host. Acceptable for a dev machine, and it's
    removed with `podman volume rm esp32-adsb-gh-config` or `gh auth logout`.
  - **Agents in this container (Claude Code) can use `gh` with the user's identity**: push, open
    PRs, comment, and with broad scopes more. That's a capability increase. Mitigations are in
    the risks below. Whether an agent may run a given `gh` command stays governed by the user's
    Claude Code permission settings, which this change does not touch.
  - Upgrading gh = bump the `version` option and refresh the lock file. That's deliberate,
    not incidental.

## Plan

Before step 1: run `git status` / `git diff -- .devcontainer`. It's clean at the time of writing,
but if the user has local modifications, make additive edits only and don't commit their
unrelated changes (same rule as adsb-display 0.6). The container rebuild is always done by the
user. It can't be done from inside the container.

1. **[devops]** Add the feature to `devcontainer.json`.
   - New entry in `features`:
     `"ghcr.io/devcontainers/features/github-cli:1": { "version": "<X.Y.Z>" }`, plus
     `installDirectlyFromGitHubRelease` if confirmed.
   - Look up `<X.Y.Z>` on github.com/cli/cli/releases and the feature's options in
     github.com/devcontainers/features `src/github-cli/devcontainer-feature.json` [VERIFY both].
   - *Verify:* `devcontainer read-configuration --workspace-folder .` (on the host) parses
     without error, and `git diff` shows only the added entry.

2. **[devops] [manual]** Lock the feature and rebuild.
   - The user rebuilds (VS Code "Rebuild Container", or
     `devcontainer up --workspace-folder . --docker-path podman --remove-existing-container`).
   - Confirm that `devcontainer-lock.json` gained a `github-cli` entry (version + `resolved`
     digest + `integrity`), and commit it with step 1. If the CLI doesn't write it automatically,
     run `devcontainer upgrade --workspace-folder .` [VERIFY the command/flag].
   - *Verify (arm64 host):* in the new container,
     - `gh --version` prints `<X.Y.Z>`;
     - `uname -m` → `aarch64`;
     - `file "$(command -v gh)"` shows an ARM aarch64 ELF.

3. **[devops]** Persist auth config (after G1 is confirmed as (a)).
   - Add a mount:
     `"source=esp32-adsb-gh-config,target=/home/vscode/.config/gh,type=volume"`.
   - In `post-create.sh`:
     - `chown` it next to the existing two volumes (`sudo chown -R "$(id -u):$(id -g)" "$HOME/.config/gh"`);
     - add `gh config set git_protocol ssh`, idempotent;
     - add `gh --version` to the version printout at the end, so a missing `gh` fails
       post-create loudly.
   - Check that the chezmoi dotfiles don't manage `~/.config/gh/`. If they do, decide with the
     user which one owns it.
   - *Verify after rebuild:*
     - `stat -c %U ~/.config/gh` → `vscode`;
     - `gh config get git_protocol` → `ssh`;
     - post-create output shows the gh version.

4. **[manual]** First login and functional check.
   - The user runs `gh auth login --hostname github.com --git-protocol ssh --skip-ssh-key`
     and completes the device flow in the host browser [VERIFY `--skip-ssh-key` flag name; it
     stops gh from offering to upload an SSH key]. Use minimal scopes; the default `repo`,
     `read:org`, `gist` is the upper bound. Don't add `workflow`, `delete_repo` or `admin:*`.
   - *Verify:*
     - `gh auth status` shows the account;
     - `gh repo view` resolves the remote of this repo;
     - `gh run list --limit 1` lists the latest CI run.
   - Then **rebuild the container again** and check that `gh auth status` still passes, which
     proves the volume works.

5. **[devops]** Document it in `README.md`.
   - Add a row `devcontainers/features/github-cli` | `gh` (GitHub CLI) to the feature table.
   - Add a short "GitHub CLI" subsection covering:
     - one-time `gh auth login` (step 4 command and scope advice);
     - the token lives in the `esp32-adsb-gh-config` volume; reset with
       `gh auth logout` / `podman volume rm esp32-adsb-gh-config`;
     - alternative: `GH_TOKEN` via `remoteEnv`/`localEnv` with a fine-grained PAT scoped to this
       repo, exported in the shell that starts VS Code / `devcontainer up`;
     - agents in the container can act as you through `gh`.
   - The table is already missing the `python` and `claude-code` rows. Fixing that is a separate,
     optional one-line change; don't bundle it silently.
   - *Verify:* the README renders, and the commands in it match what was run in step 4.

6. **[devops/CI] [optional, deferred per G2]** CI smoke build of the dev container.
   - Add a job with `devcontainers/ci` that builds `.devcontainer` and runs
     `gh --version && espflash --version`. An arm64 leg on `ubuntu-24.04-arm` would cover both
     architectures [VERIFY runner availability for this repo's plan].
   - Cost: the esp-rs feature (`espup` with all targets) makes this build slow (likely 10+ min,
     [VERIFY]). Run it only on changes under `.devcontainer/**` (path filter).
   - Requires the user's OK to push (same rule as adsb-display 0.5).
   - *Verify:* the job goes green on a push touching `.devcontainer/`, and turns red with a
     deliberately broken `version` value, which is then reverted.

Optional amd64 check without CI: on an amd64 machine, or with
`devcontainer build --platform linux/amd64` under qemu emulation on the arm64 host [VERIFY flag
and that qemu-user-static is available in podman], confirm the build succeeds and
`gh --version` works. Not required for done, because the feature is multi-arch upstream.

Steps 1+2 are one commit, 3 is one commit (or folded into the same one), and 5 is one commit.

## Risks

| # | Risk | Likelihood / impact | Mitigation |
|---|------|---------------------|------------|
| G-R1 | **Agents act as the user on GitHub.** Claude Code in the container gets authenticated `gh` (push, PRs, comments, maybe more). | Certain (by design) / medium–high | Minimal token scopes (step 4). Prefer a fine-grained PAT limited to this repo if the user picks option b. Claude Code permission settings remain the gate for running `gh`; this design doesn't change them. Document it in the README. |
| G-R2 | Plaintext token in the `esp32-adsb-gh-config` volume on the host | High / low–medium | Dev machine only, volume is user-owned. `gh auth logout` / `podman volume rm` to revoke locally, and revoke on github.com if the machine is compromised. |
| G-R3 | Volume created root-owned → `gh auth login` fails to write config | High without the chown / low | `chown` in `post-create.sh` (step 3), same as the existing volumes. |
| G-R4 | The feature's `version` / `installDirectlyFromGitHubRelease` options differ from what's assumed here | Medium / low | [VERIFY] against the feature's `devcontainer-feature.json` in step 1. Falling back to option B or C costs ~1 hour. |
| G-R5 | Pinned gh version disappears or breaks the install (e.g. apt repo drops an old version) | Low / low | Release-binary install (if the option exists) doesn't depend on the apt repo. Otherwise bump the pin. |
| G-R6 | Mounting a volume at `~/.config/gh` conflicts with chezmoi dotfiles or creates `~/.config` root-owned | Low / low | Check the dotfiles in step 3. If `~/.config` ends up root-owned, also `chown` `~/.config` itself (non-recursively). |
| G-R7 | No CI coverage for the devcontainer: a later change breaks `gh` (or anything else) unnoticed | Medium / low | `gh --version` in the post-create smoke list fails loudly on the next rebuild. Step 6 is available if this bites. |
| G-R8 | amd64 path not exercised locally (host is aarch64) | Low / low | Upstream feature is multi-arch. Optional emulated build or step 6 covers it. |
| G-R9 | `GH_TOKEN` option (b), if chosen later: unset host var → empty `GH_TOKEN`, which may block the stored login or produce confusing errors | Medium / low | Not wired by default. If adopted, [VERIFY] gh's empty-var behaviour first, and don't combine with (a) without documenting precedence. |

Open questions: G1, G2. The [VERIFY] items in steps 1, 2, 4 and 6.

## Revision log

- 2026-10-21: initial draft.

---
name: notify
description: Send a push notification with Markdown text and/or images (e.g. a bench snapshot) to the user's phone via their ntfy server. Use when the user asks to be notified, to "send me" a result, screenshot or bench photo, or to ping them when a long task finishes.
---

# Notify

Publishes to the user's self-hosted ntfy server (default `https://ntfy.zz-systems.net`, topic
`claude`) with `.claude/skills/notify/notify.py` (stdlib Python, no uv needed).

## Usage

```sh
python3 .claude/skills/notify/notify.py --title "Build done" --text "**OK** - 42 tests passed"
python3 .claude/skills/notify/notify.py --title "Bench snapshot" --image .bench/snaps/<time>.jpg
python3 .claude/skills/notify/notify.py --text-file /path/report.md --image a.jpg --image b.png
some-command | python3 .claude/skills/notify/notify.py --title "Log tail" --text -
```

- Text is sent as **one Markdown message** (`Markdown: yes`; the ntfy web app renders it, the
  Android/iOS apps may show it as plain text).
- **Each image is a separate message** (ntfy allows one attachment per message), titled
  `<title> (i/n)` when there are several.
- Options: `--priority 1..5`, `--tags warning,robot` (emoji shortcodes), `--server`, `--topic`.
  Env overrides: `NTFY_SERVER`, `NTFY_TOPIC`, `NTFY_TOKEN` (sent as a bearer token).
- Exit 0 all sent, 1 any message failed (server reply on stderr), 2 usage error.

## Rules

1. **Look at every image before sending it** (Read tool). Bench snapshots can show the room or
   live traffic (README: don't share them). If anything but the panel/board is visible, ask
   first.
2. Send only what the user asked for, or a short status when they asked to be pinged. Never
   send secrets, tokens, `.env` contents or `.bench/token`.
3. For a bench photo, take it first with `tools/bench/bench snap --delay 1` (or
   `python3 tools/bench/bench snap ...` if uv is missing; see `board-verify`).
4. Report what was sent: message ids and the attachment URL. The server's retention is short
   (attachments ~3 h, messages ~12 h).

## Errors

- `HTTP 400 ... attachments not allowed` (code 40014): attachments are off on the server
  (`attachment-cache-dir` / `base-url` in `server.yml`). Tell the user; offer text only.
- `HTTP 401/403`: the topic needs auth. Ask the user to set `NTFY_TOKEN`; don't guess one.
- `HTTP 413`: image above the server's attachment size limit. Downscale or send fewer.

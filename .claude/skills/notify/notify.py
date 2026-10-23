#!/usr/bin/env python3
"""Send Markdown text and/or images to an ntfy topic (the user's phone).

Text goes as one Markdown message; each image goes as its own message, because ntfy
allows one attachment per message. Stdlib only.

Exit codes: 0 everything sent, 1 any message failed, 2 usage error.
"""

import argparse
import base64
import json
import mimetypes
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

DEFAULT_SERVER = "https://ntfy.zz-systems.net"
DEFAULT_TOPIC = "claude"
TIMEOUT_S = 30


def header_value(text):
    """HTTP headers are latin-1; ntfy decodes RFC 2047 encoded-words for anything else."""
    if text.isascii():
        return text
    return "=?UTF-8?B?" + base64.b64encode(text.encode()).decode() + "?="


def publish(url, body, headers):
    req = urllib.request.Request(url, data=body, method="PUT")
    for k, v in headers.items():
        if v:
            req.add_header(k, header_value(v))
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace").strip()
        raise RuntimeError(f"HTTP {e.code}: {detail}") from None
    except urllib.error.URLError as e:
        raise RuntimeError(f"cannot reach {url}: {e.reason}") from None


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    src = p.add_mutually_exclusive_group()
    src.add_argument("--text", help="Markdown text ('-' reads stdin)")
    src.add_argument("--text-file", type=Path, help="file with Markdown text")
    p.add_argument("--image", type=Path, action="append", default=[],
                   help="image to attach (repeat for several)")
    p.add_argument("--title", help="notification title")
    p.add_argument("--priority", choices=["1", "2", "3", "4", "5"],
                   help="1 min .. 5 max (default: server default, 3)")
    p.add_argument("--tags", help="comma-separated ntfy tags / emoji shortcodes")
    p.add_argument("--server", default=os.environ.get("NTFY_SERVER", DEFAULT_SERVER))
    p.add_argument("--topic", default=os.environ.get("NTFY_TOPIC", DEFAULT_TOPIC))
    args = p.parse_args()

    if args.text == "-":
        text = sys.stdin.read()
    elif args.text_file:
        text = args.text_file.read_text()
    else:
        text = args.text
    text = text.strip() if text else ""
    if not text and not args.image:
        p.error("nothing to send: give --text, --text-file and/or --image")
    for img in args.image:
        if not img.is_file():
            p.error(f"no such image: {img}")

    url = args.server.rstrip("/") + "/" + args.topic
    common = {"Priority": args.priority, "Tags": args.tags}
    token = os.environ.get("NTFY_TOKEN")
    if token:
        common["Authorization"] = f"Bearer {token}"

    failed = 0
    if text:
        try:
            r = publish(url, text.encode(), {**common, "Title": args.title, "Markdown": "yes"})
            print(f"text: sent id={r['id']}")
        except RuntimeError as e:
            print(f"text: FAILED {e}", file=sys.stderr)
            failed += 1

    n = len(args.image)
    for i, img in enumerate(args.image, 1):
        ctype = mimetypes.guess_type(img.name)[0] or "application/octet-stream"
        title = args.title or img.name
        if n > 1:
            title = f"{title} ({i}/{n})"
        try:
            r = publish(url, img.read_bytes(), {
                **common, "Title": title, "Filename": img.name,
                "Message": img.name, "Content-Type": ctype,
            })
            att = r.get("attachment", {})
            print(f"image {img}: sent id={r['id']} url={att.get('url')}")
        except RuntimeError as e:
            print(f"image {img}: FAILED {e}", file=sys.stderr)
            failed += 1

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

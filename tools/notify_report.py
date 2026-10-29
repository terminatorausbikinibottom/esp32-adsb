#!/usr/bin/env python3
"""Send a status message with the notify skill and append it as a slide to the progress deck (Marp).

Usage:
  python3 tools/notify_report.py --title T --text-file msg.md [--image a.jpg ...] [--tags x,y]
         [--time "2026-10-23 18:01 UTC (original)"] [--report docs/presentation/radar-ui-progress.md]

Images are copied next to the report (<report dir>/img/) so the Markdown stays self-contained.
The entry is appended even if sending fails; the entry then says so.
"""
import argparse, datetime, os, shutil, subprocess, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NOTIFY = os.path.join(REPO, ".claude/skills/notify/notify.py")

p = argparse.ArgumentParser()
p.add_argument("--title", required=True)
p.add_argument("--text-file", required=True)
p.add_argument("--image", action="append", default=[])
p.add_argument("--tags", default="")
p.add_argument("--time", help="time label for the entry (default: now, UTC)")
p.add_argument("--report", default=os.path.join(REPO, "docs/presentation/radar-ui-progress.md"))
a = p.parse_args()

text = open(a.text_file, encoding="utf-8").read().strip()
now = datetime.datetime.now(datetime.timezone.utc)
label = a.time or now.strftime("%Y-%m-%d %H:%M UTC")

cmd = [sys.executable, NOTIFY, "--title", a.title, "--text-file", a.text_file]
if a.tags:
    cmd += ["--tags", a.tags]
for img in a.image:
    cmd += ["--image", img]
res = subprocess.run(cmd, capture_output=True, text=True)
sys.stdout.write(res.stdout)
sys.stderr.write(res.stderr)

img_dir = os.path.join(os.path.dirname(a.report), "img")
os.makedirs(img_dir, exist_ok=True)
links = []
for img in a.image:
    name = now.strftime("%Y%m%dT%H%M%S") + "-" + os.path.basename(img)
    shutil.copy2(img, os.path.join(img_dir, name))
    links.append(f"![bg right:45% contain](img/{name})")

# The report is a Marp deck: each entry is one slide (image as a right-hand background).
status = "" if res.returncode == 0 else f"\n\n> notify failed (exit {res.returncode}); not delivered to the phone."
entry = (f"\n---\n\n<!-- _class: log -->\n\n## {a.title}\n\n<span class=\"time\">{label}</span>\n\n{text}\n"
         + "".join(f"\n{l}\n" for l in links) + status + "\n")
with open(a.report, "a", encoding="utf-8") as f:
    f.write(entry)
print(f"appended to {os.path.relpath(a.report, REPO)}")
sys.exit(res.returncode)

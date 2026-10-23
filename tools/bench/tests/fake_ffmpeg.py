#!/usr/bin/env python3
"""Test imitation of the one-shot `ffmpeg -f avfoundation ...` snapshot. Not a uv script.

Run as [sys.executable, this_file, ...ffmpeg args] or directly (python3 shebang).
Env: FAKE_FFMPEG_MODE  ok | fail | hang | fallback | notjpeg
     FAKE_FFMPEG_DEVICES  video device names for -list_devices, separated by "|" (default: MacBook Pro-Kamera|HD Pro Webcam C920)
     FAKE_FFMPEG_LOG   file that gets one JSON line {pid, argv} per invocation
"""
import json
import os
import sys
import time

MODE = os.environ.get("FAKE_FFMPEG_MODE", "ok")
JPEG = b"\xff\xd8\xff\xe0" + b"\0" * 16 + b"\xff\xd9"  # not decodable; only the magic matters

if len(sys.argv) == 2 and sys.argv[1] == "-version":
    print("ffmpeg version fake")
    sys.exit(0)
if "-list_devices" in sys.argv:  # like the real one: listing on stderr, then exit 1 (empty input)
    names = os.environ.get("FAKE_FFMPEG_DEVICES", "MacBook Pro-Kamera|HD Pro Webcam C920").split("|")
    pre = "[AVFoundation indev @ 0x1] "
    lines = ["AVFoundation video devices:"] + [f"[{i}] {n}" for i, n in enumerate(names)]
    lines += ["AVFoundation audio devices:", "[0] Mikrofon"]
    sys.stderr.write("".join(pre + l + "\n" for l in lines) + 'Error opening input file .\n')
    sys.exit(1)
if os.environ.get("FAKE_FFMPEG_LOG"):
    with open(os.environ["FAKE_FFMPEG_LOG"], "a") as f:
        f.write(json.dumps({"pid": os.getpid(), "argv": sys.argv[1:], "sid": os.getsid(0), "pgid": os.getpgid(0)}) + "\n")
if MODE == "fail":
    sys.stderr.write("[avfoundation] Cannot use camera: not authorized\n")
    sys.exit(1)
if MODE == "hang":
    time.sleep(60)
if MODE == "fallback":
    sys.stderr.write("Configuration of video device failed, falling back to default\n")
sys.stdout.buffer.write(b"not a jpeg" if MODE == "notjpeg" else JPEG)
sys.stdout.buffer.flush()

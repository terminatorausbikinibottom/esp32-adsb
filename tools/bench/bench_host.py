#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Board bench host service (stdlib only). See docs/design/board-bench.md.

Start from the repo root: `uv run tools/bench/bench_host.py [--port ...]`
(fallback without uv: `python3 tools/bench/bench_host.py`, python >= 3.11).

Step 2 skeleton: config, loopback-only bind, token + auth, GET /health, and the pure helpers
(ELF staging, argv builders, serial port resolution) that later steps build on.
"""
import argparse
import glob
import hmac
import ipaddress
import json
import os
import secrets
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SERVICE_VERSION = "0.1"
MIN_PYTHON = (3, 11)
REPO_ROOT = Path(__file__).resolve().parents[2]
MAX_BODY = 4096
MAX_ELF = 64 * 1024 * 1024
ELF_MAGIC = b"\x7fELF"
PORT_GLOB = "/dev/cu.usbmodem*"


class BenchError(Exception):
    """An error that maps to an HTTP status."""

    def __init__(self, status, message, **extra):
        super().__init__(message)
        self.status = status
        self.extra = extra


class PortError(BenchError):
    def __init__(self, candidates):
        super().__init__(
            503,
            f"expected exactly one {PORT_GLOB} device, found {len(candidates)}: {candidates}",
            candidates=list(candidates),
        )


# ---- pure helpers ----------------------------------------------------------------------------


def parse_listen(value):
    """'host:port' -> (host, port). Only loopback IP literals are accepted."""
    host, sep, port = value.rpartition(":")
    host = host.strip("[]")
    try:
        if not sep or not (0 <= int(port) <= 65535) or not ipaddress.ip_address(host).is_loopback:
            raise ValueError
    except ValueError:
        raise ValueError(f"--listen must be a loopback IP address and port, got {value!r}") from None
    return host, int(port)


def resolve_port(configured, candidates):
    """The serial port the next flash uses: the configured one as is, else the single candidate."""
    if configured:
        return configured
    if len(candidates) == 1:
        return candidates[0]
    raise PortError(candidates)


def list_candidates(pattern=PORT_GLOB):
    return sorted(glob.glob(pattern))


def stage_elf(repo_root, elf, elf_prefix, dest_dir):
    """Validate a repo-relative ELF path and copy it into dest_dir; returns the copy's path.

    The checks run on the bytes that are copied (one open, one read), so the file can't be
    swapped between check and use. Flash the returned copy, never the request string.
    """
    if not isinstance(elf, str) or not elf or "\0" in elf or os.path.isabs(elf):
        raise BenchError(400, "elf must be a non-empty relative path")
    base = os.path.realpath(os.path.join(repo_root, elf_prefix))
    target = os.path.realpath(os.path.join(repo_root, elf))
    if os.path.commonpath([base, target]) != base:
        raise BenchError(400, f"elf must be under {elf_prefix}")
    try:
        # O_NONBLOCK so a FIFO can't hang the open; it is rejected as non-regular below.
        fd = os.open(target, os.O_RDONLY | os.O_NONBLOCK)
    except OSError as e:
        raise BenchError(400, f"cannot open elf: {e.strerror}") from None
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        os.close(fd)
        raise BenchError(400, "elf is not a regular file")
    with os.fdopen(fd, "rb") as f:
        data = f.read(MAX_ELF + 1)
    if len(data) > MAX_ELF:
        raise BenchError(400, "elf is larger than 64 MiB")
    if not data.startswith(ELF_MAGIC):
        raise BenchError(400, "not an ELF file")
    fd, copy = tempfile.mkstemp(dir=dest_dir, suffix=".elf")
    with os.fdopen(fd, "wb") as out:
        out.write(data)
    return copy


def espflash_argv(espflash, port, elf):
    """The single S3 command: flash then stay in the monitor. `elf` is the staged copy, last."""
    return [espflash, "flash", "--chip", "esp32s3", "--port", port, "--monitor", "--non-interactive", elf]


def ffmpeg_argv(ffmpeg, camera, video_size, warmup, framerate=None):
    """One-shot snapshot: skip `warmup` frames, write one JPEG to stdout."""
    argv = [ffmpeg, "-f", "avfoundation"]
    if framerate:
        argv += ["-framerate", str(framerate)]
    argv += ["-video_size", video_size, "-i", f"{camera}:none"]
    # The backslash is ffmpeg filter-graph escaping for the comma; there is no shell involved.
    argv += ["-vf", f"select=gte(n\\,{int(warmup)})", "-frames:v", "1"]
    return argv + ["-f", "image2pipe", "-c:v", "mjpeg", "pipe:1"]


def write_token(bench_dir):
    """Generate a fresh token and write it to <bench_dir>/token with mode 0600."""
    os.makedirs(bench_dir, mode=0o700, exist_ok=True)
    token = secrets.token_urlsafe(32)
    fd = os.open(os.path.join(bench_dir, "token"), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        os.fchmod(f.fileno(), 0o600)  # an existing file keeps its old mode otherwise
        f.write(token)
    return token


def tool_version(binary, flag):
    try:
        out = subprocess.run([binary, flag], capture_output=True, text=True, timeout=5,
                             stdin=subprocess.DEVNULL).stdout
        return out.strip().splitlines()[0] if out.strip() else None
    except (OSError, subprocess.SubprocessError):
        return None


# ---- core ------------------------------------------------------------------------------------


class Bench:
    def __init__(self, *, repo_root=REPO_ROOT, port=None, camera="1", video_size="1280x720",
                 framerate=None, warmup=20, elf_prefix="firmware/target/", espflash="espflash",
                 ffmpeg="ffmpeg", candidates=list_candidates):
        self.repo_root, self.port, self.camera = repo_root, port, camera
        self.video_size, self.framerate, self.warmup = video_size, framerate, warmup
        self.elf_prefix, self.espflash, self.ffmpeg = elf_prefix, espflash, ffmpeg
        self.candidates = candidates
        self.espflash_version = tool_version(espflash, "--version")

    def serial_status(self):
        cands = self.candidates()
        try:
            chosen = resolve_port(self.port, cands)
        except PortError:
            chosen = None  # ambiguous auto-detect
        return {
            "mode": "fixed" if self.port else "auto",
            "chosen": chosen,
            "present": bool(chosen) and os.path.exists(chosen),
            "candidates": cands,
        }

    def health(self):
        return {
            "ok": True,
            "service_version": SERVICE_VERSION,
            "python_version": sys.version.split()[0],
            "serial": self.serial_status(),
            "espflash_version": self.espflash_version,
            "ffmpeg_present": shutil.which(self.ffmpeg) is not None,
            "camera": self.camera,
            # Stubs until step 3 adds sessions.
            "monitor": {"running": False, "phase": None, "exit_code": None},
            "busy": False,
        }


# ---- HTTP front end --------------------------------------------------------------------------


class Handler(BaseHTTPRequestHandler):
    server_version = "bench-host"

    def log_message(self, fmt, *args):
        # The peer address is always 127.0.0.1 (gvproxy), so it says nothing about the caller.
        sys.stderr.write(f"{self.command} {self.path} -> {args[1] if len(args) > 1 else '?'}\n")

    def _send(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _authorized(self):
        if self.headers.get("Origin") is not None:  # browsers: CSRF / DNS rebinding
            self._send(403, {"ok": False, "error": "Origin header not allowed"})
            return False
        got = self.headers.get("Authorization", "").encode()
        want = b"Bearer " + self.server.token.encode()
        if not hmac.compare_digest(got, want):
            self._send(401, {"ok": False, "error": "missing or wrong token"})
            return False
        return True

    def _body_ok(self):
        try:
            n = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            n = -1
        if n < 0:
            self._send(400, {"ok": False, "error": "bad Content-Length"})
        elif n > MAX_BODY:
            self._send(413, {"ok": False, "error": f"body larger than {MAX_BODY} bytes"})
        else:
            return True
        return False

    def _route(self, method):
        if not self._authorized() or not self._body_ok():
            return
        path = self.path.split("?", 1)[0]
        try:
            if (method, path) == ("GET", "/health"):
                self._send(200, self.server.bench.health())
            else:
                self._send(404, {"ok": False, "error": "not found"})
        except BenchError as e:
            self._send(e.status, {"ok": False, "error": str(e), **e.extra})

    def do_GET(self):
        self._route("GET")

    def do_POST(self):
        self._route("POST")


def make_server(bench, host, port, token=None):
    class Server(ThreadingHTTPServer):
        address_family = socket.AF_INET6 if ":" in host else socket.AF_INET

    srv = Server((host, port), Handler)
    srv.bench, srv.token = bench, token
    return srv


# ---- entry point -----------------------------------------------------------------------------


def parse_args(argv):
    env = os.environ.get
    p = argparse.ArgumentParser(description="Board bench host service (loopback only).")
    p.add_argument("--port", default=env("BENCH_PORT"), help="serial device (default: auto-detect %s)" % PORT_GLOB)
    p.add_argument("--camera", default=env("BENCH_CAMERA", "1"), help="avfoundation name or index")
    p.add_argument("--video-size", default=env("BENCH_VIDEO_SIZE", "1280x720"))
    p.add_argument("--framerate", default=env("BENCH_FRAMERATE"), help="not passed to ffmpeg by default")
    p.add_argument("--warmup", type=int, default=int(env("BENCH_WARMUP", "20")), help="warm-up frames")
    p.add_argument("--listen", default=env("BENCH_LISTEN", "127.0.0.1:7777"), help="loopback host:port")
    p.add_argument("--elf-prefix", default=env("BENCH_ELF_PREFIX", "firmware/target/"))
    p.add_argument("--espflash", default=env("BENCH_ESPFLASH", "espflash"))
    p.add_argument("--ffmpeg", default=env("BENCH_FFMPEG", "ffmpeg"))
    return p.parse_args(argv)


def main(argv=None, bench_dir=REPO_ROOT / ".bench"):
    args = parse_args(argv)
    if sys.version_info < MIN_PYTHON:
        return _fail(f"python >= {MIN_PYTHON[0]}.{MIN_PYTHON[1]} required, running {sys.version.split()[0]}")
    try:
        host, port = parse_listen(args.listen)
        for b in (args.espflash, args.ffmpeg):
            if shutil.which(b) is None:
                raise ValueError(f"binary not found: {b}")
        resolve_port(args.port, list_candidates())  # fail fast; re-resolved on every flash/reset
    except (ValueError, PortError) as e:
        return _fail(str(e))
    bench = Bench(port=args.port, camera=args.camera, video_size=args.video_size,
                  framerate=args.framerate, warmup=args.warmup, elf_prefix=args.elf_prefix,
                  espflash=args.espflash, ffmpeg=args.ffmpeg)
    print(f"python {sys.version.split()[0]}, {bench.espflash_version}, "
          f"{tool_version(args.ffmpeg, '-version')}", file=sys.stderr)
    try:
        srv = make_server(bench, host, port)  # bind before touching the token of a running instance
    except OSError as e:
        return _fail(f"cannot listen on {host}:{port}: {e.strerror}")
    srv.token = write_token(bench_dir)
    signal.signal(signal.SIGTERM, _terminate)
    print(f"listening on {host}:{port}, token in {bench_dir / 'token'}", file=sys.stderr)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        srv.server_close()
    return 0


def _terminate(*_):
    raise KeyboardInterrupt


def _fail(msg):
    print(f"bench_host: {msg}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())

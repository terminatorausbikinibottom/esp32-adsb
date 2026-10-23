#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Board bench host service (stdlib only). See docs/design/board-bench.md.

Start from the repo root: `uv run tools/bench/bench_host.py [--port ...]`
(fallback without uv: `python3 tools/bench/bench_host.py`, python >= 3.11).

Implemented: config, loopback-only bind, token + auth, GET /health, the pure helpers (ELF
staging, argv builders, serial port resolution), and (step 3) /flash, /reset, /monitor/stop, /log
around one espflash process per session, and (step 4) GET /snapshot.jpg. Stream comes later.
"""
import atexit
import argparse
import glob
import hmac
import ipaddress
import json
import os
import re
import secrets
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
from collections import deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from itertools import islice
from pathlib import Path

SERVICE_VERSION = "0.1"
MIN_PYTHON = (3, 11)
REPO_ROOT = Path(__file__).resolve().parents[2]
MAX_BODY = 4096
MAX_ELF = 64 * 1024 * 1024
ELF_MAGIC = b"\x7fELF"
PORT_GLOB = "/dev/cu.usbmodem*"
COMPLETE = "Flashing has completed!"  # espflash's flash-success line (R9: one constant)
ANSI_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
RING_LINES = 20000
FLASH_TIMEOUT = 300
LOG_WAIT_CAP = 30
SNAPSHOT_TIMEOUT = 20
JPEG_MAGIC = b"\xff\xd8"
TAIL_LINES = 20


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


def _cmd(binary):
    """A binary is a path, or an argv prefix (tests run the fake as [sys.executable, fake])."""
    return [binary] if isinstance(binary, str) else list(binary)


def espflash_argv(espflash, port, elf):
    """The single S3 command: flash then stay in the monitor. `elf` is the staged copy, last."""
    return [*_cmd(espflash), "flash", "--chip", "esp32s3", "--port", port, "--monitor", "--non-interactive", elf]


def ffmpeg_argv(ffmpeg, camera, video_size, warmup, framerate=None):
    """One-shot snapshot: skip `warmup` frames, write one JPEG to stdout."""
    argv = [*_cmd(ffmpeg), "-f", "avfoundation"]
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
        out = subprocess.run([*_cmd(binary), flag], capture_output=True, text=True, timeout=5,
                             stdin=subprocess.DEVNULL).stdout
        return out.strip().splitlines()[0] if out.strip() else None
    except (OSError, subprocess.SubprocessError):
        return None


# ---- core ------------------------------------------------------------------------------------


class Session:
    """One espflash process. phase: flashing -> running (completion line seen) -> exited."""

    def __init__(self, id, proc, log_seq):
        self.id, self.proc, self.log_seq = id, proc, log_seq
        self.phase, self.app_seq, self.exit_code, self.reader = "flashing", None, None, None


class Bench:
    def __init__(self, *, repo_root=REPO_ROOT, port=None, camera="1", video_size="1280x720",
                 framerate=None, warmup=20, elf_prefix="firmware/target/", espflash="espflash",
                 ffmpeg="ffmpeg", candidates=list_candidates, flash_timeout=FLASH_TIMEOUT,
                 ring_lines=RING_LINES, snapshot_timeout=SNAPSHOT_TIMEOUT):
        self.repo_root, self.port, self.camera = repo_root, port, camera
        self.video_size, self.framerate, self.warmup = video_size, framerate, warmup
        self.elf_prefix, self.espflash, self.ffmpeg = elf_prefix, espflash, ffmpeg
        self.candidates, self.flash_timeout = candidates, flash_timeout
        self.snapshot_timeout = snapshot_timeout
        self.camera_lock = threading.Lock()  # separate from flash_lock: snapshots work during a flash
        self.espflash_version = tool_version(espflash, "--version")
        self.flash_lock = threading.Lock()  # one flash/reset/stop at a time
        self.cond = threading.Condition()  # guards ring, next_seq and session fields
        self.ring = deque(maxlen=ring_lines)  # {seq, t, text}; seq is global across sessions
        self.next_seq = 1
        self.session = None
        self.sessions = 0
        self.last_elf = None  # staged copy kept for /reset
        self.stage_dir = None

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

    def monitor_status(self):
        """phase is None until the first flash; running = the espflash process is alive."""
        s = self.session
        if s is None:
            return {"running": False, "phase": None, "exit_code": None}
        return {"running": s.phase != "exited", "phase": s.phase, "exit_code": s.exit_code}

    def health(self):
        return {
            "ok": True,
            "service_version": SERVICE_VERSION,
            "python_version": sys.version.split()[0],
            "serial": self.serial_status(),
            "espflash_version": self.espflash_version,
            "ffmpeg_present": shutil.which(_cmd(self.ffmpeg)[0]) is not None,
            "camera": self.camera,
            "monitor": self.monitor_status(),
            "busy": self.flash_lock.locked(),
        }

    # -- flash / reset / stop

    def flash(self, elf):
        def stage():
            if self.stage_dir is None:
                self.stage_dir = tempfile.mkdtemp(prefix="bench-")
            return stage_elf(self.repo_root, elf, self.elf_prefix, self.stage_dir)
        return self._start(stage)

    def reset(self):
        return self._start(None)

    def _start(self, stage):
        if not self.flash_lock.acquire(blocking=False):
            raise BenchError(409, "another flash or reset is running")
        try:
            if stage is None and self.last_elf is None:
                raise BenchError(409, "nothing has been flashed since startup")
            new = stage() if stage else None
            try:
                port = resolve_port(self.port, self.candidates())  # re-resolved every time
                self._stop_process()
                s = self._spawn(espflash_argv(self.espflash, port, new or self.last_elf))
            except BaseException:
                if new:
                    os.unlink(new)
                raise
            if new:  # the previous copy is only dropped once the new one is in use
                old, self.last_elf = self.last_elf, new
                if old:
                    os.unlink(old)
            return self._await_flash(s)
        finally:
            self.flash_lock.release()

    def _spawn(self, argv):
        try:
            proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT)
        except OSError as e:
            raise BenchError(500, f"cannot start espflash: {e.strerror}") from None
        with self.cond:
            self.sessions += 1
            s = self.session = Session(self.sessions, proc, self.next_seq)
        s.reader = threading.Thread(target=self._read, args=(s,), daemon=True)
        s.reader.start()
        return s

    def _read(self, s):
        for raw in s.proc.stdout:
            text = ANSI_RE.sub("", raw.decode("utf-8", errors="replace")).rstrip("\r\n")
            with self.cond:
                seq, self.next_seq = self.next_seq, self.next_seq + 1
                self.ring.append({"seq": seq, "t": round(time.time(), 3), "text": text})
                if s.phase == "flashing" and COMPLETE in text:
                    s.phase, s.app_seq = "running", seq
                self.cond.notify_all()
        s.proc.stdout.close()
        code = s.proc.wait()
        with self.cond:
            s.exit_code, s.phase = code, "exited"
            self.cond.notify_all()

    def _tail(self, s):
        return [l["text"] for l in self.ring if l["seq"] >= s.log_seq][-TAIL_LINES:]

    def _await_flash(self, s):
        with self.cond:
            done = self.cond.wait_for(lambda: s.phase != "flashing", self.flash_timeout)
        if not done:
            self._stop_process()
        with self.cond:
            info = {"exit_code": s.exit_code, "output_tail": self._tail(s), "session": s.id,
                    "log_seq": s.log_seq}
            if not done:
                raise BenchError(504, f"no '{COMPLETE}' within {self.flash_timeout}s; killed", **info)
            if s.app_seq is None:
                raise BenchError(502, f"espflash exited with {s.exit_code} before '{COMPLETE}'", **info)
            return {"ok": True, **info, "app_seq": s.app_seq}

    def _stop_process(self):
        s = self.session
        if s is None:
            return
        if s.proc.poll() is None:
            s.proc.terminate()
            try:
                s.proc.wait(2)
            except subprocess.TimeoutExpired:
                s.proc.kill()
                s.proc.wait(5)
        s.reader.join(5)

    def stop_monitor(self):
        # Holding the flash lock also covers "still flashing": a half-written image is never killed.
        if not self.flash_lock.acquire(blocking=False):
            raise BenchError(409, "flash or reset in progress")
        try:
            s = self.session
            was_alive = s is not None and s.phase != "exited"
            self._stop_process()
            return {"ok": True, "stopped": was_alive}
        finally:
            self.flash_lock.release()

    def close(self):
        """Kill the child and delete staged copies. Idempotent; runs on shutdown and at exit."""
        self._stop_process()
        if self.stage_dir:
            shutil.rmtree(self.stage_dir, ignore_errors=True)
            self.stage_dir = self.last_elf = None

    # -- snapshot

    def snapshot(self, warmup=None):
        """One JPEG from a one-shot ffmpeg. 503 (with the stderr tail) on any failure."""
        if not self.camera_lock.acquire(blocking=False):
            raise BenchError(409, "another snapshot is running")
        try:
            argv = ffmpeg_argv(self.ffmpeg, self.camera, self.video_size,
                               self.warmup if warmup is None else warmup, self.framerate)
            try:
                proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE)
            except OSError as e:
                raise BenchError(503, f"cannot start ffmpeg: {e.strerror}") from None
            try:
                out, err = proc.communicate(timeout=self.snapshot_timeout)
                problem = None
            except subprocess.TimeoutExpired:
                proc.kill()
                out, err = proc.communicate()
                problem = f"ffmpeg timed out after {self.snapshot_timeout}s; killed"
            finally:
                if proc.poll() is None:  # e.g. KeyboardInterrupt during communicate
                    proc.kill()
                    proc.wait()
            text = err.decode("utf-8", errors="replace")
            # Always logged: an unsupported setting only shows up here (R12), a TCC denial too (R4).
            sys.stderr.write(f"ffmpeg exit {proc.returncode}:\n{text}\n")
            if problem is None and proc.returncode != 0:
                problem = f"ffmpeg exited with {proc.returncode}"
            elif problem is None and not out.startswith(JPEG_MAGIC):
                problem = "ffmpeg output is not a JPEG"
            if problem:
                tail = text.splitlines()[-TAIL_LINES:]
                raise BenchError(503, problem, stderr_tail=tail)
            return out
        finally:
            self.camera_lock.release()

    # -- log

    def log(self, since=0, wait=0, max_lines=1000):
        with self.cond:
            if wait > 0:
                self.cond.wait_for(lambda: self.next_seq - 1 > since, min(wait, LOG_WAIT_CAP))
            oldest = self.ring[0]["seq"] if self.ring else self.next_seq
            start = max(0, since + 1 - oldest)
            lines = list(islice(self.ring, start, start + max_lines))
            s = self.session
            return {
                "session": s.id if s else None,
                "app_seq": s.app_seq if s else None,
                "lines": lines,
                "next": lines[-1]["seq"] if lines else since,
                "dropped": since + 1 < oldest,
                "monitor": self.monitor_status(),
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

    def _send_jpeg(self, data):
        self.send_response(200)
        self.send_header("Content-Type", "image/jpeg")
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

    def _json_body(self, allowed):
        raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        if not raw.strip():
            return {}
        try:
            body = json.loads(raw)
        except ValueError:
            raise BenchError(400, "body is not valid JSON") from None
        if not isinstance(body, dict) or set(body) - allowed:
            raise BenchError(400, f"body must be a JSON object with keys from {sorted(allowed)}")
        return body

    @staticmethod
    def _num(query, name, default, cast, lo):
        try:
            value = cast(query.get(name, [default])[0])
        except ValueError:
            value = None
        if value is None or not value >= lo:  # also rejects nan
            raise BenchError(400, f"bad query parameter {name}")
        return value

    def _route(self, method):
        if not self._authorized() or not self._body_ok():
            return
        url = urllib.parse.urlsplit(self.path)
        path, bench = url.path, self.server.bench
        try:
            if (method, path) == ("GET", "/health"):
                self._send(200, bench.health())
            elif (method, path) == ("GET", "/log"):
                q = urllib.parse.parse_qs(url.query)
                self._send(200, bench.log(self._num(q, "since", 0, int, 0), self._num(q, "wait", 0, float, 0),
                                          self._num(q, "max", 1000, int, 1)))
            elif (method, path) == ("GET", "/snapshot.jpg"):
                q = urllib.parse.parse_qs(url.query)
                warmup = self._num(q, "warmup", bench.warmup, int, 0) if "warmup" in q else None
                self._send_jpeg(bench.snapshot(warmup))
            elif (method, path) == ("POST", "/flash"):
                self._send(200, bench.flash(self._json_body({"elf"}).get("elf")))
            elif (method, path) == ("POST", "/reset"):
                self._json_body(set())
                self._send(200, bench.reset())
            elif (method, path) == ("POST", "/monitor/stop"):
                self._json_body(set())
                self._send(200, bench.stop_monitor())
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
    atexit.register(bench.close)
    signal.signal(signal.SIGTERM, _terminate)
    print(f"listening on {host}:{port}, token in {bench_dir / 'token'}", file=sys.stderr)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        srv.server_close()
        bench.close()
    return 0


def _terminate(*_):
    raise KeyboardInterrupt


def _fail(msg):
    print(f"bench_host: {msg}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())

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
around one espflash process per session, (step 4) GET /snapshot.jpg and (step 8) GET /stream.mjpg
(anonymous, loopback Host names only; everything else needs the bearer token).
"""
import atexit
import argparse
import contextlib
import glob
import hmac
import http
import ipaddress
import json
import os
import re
import secrets
import select
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
ANSI_BYTES_RE = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
RING_LINES = 20000
MAX_LINE = 4096  # bytes per ring line; longer serial lines are split
LINE_SPLIT_MARK = " [line split]"
PARTIAL_LINE_QUIET = 0.5  # seconds of silence before a line without newline is shown
FLASH_TIMEOUT = 300
LOG_WAIT_CAP = 30
SNAPSHOT_TIMEOUT = 20
JPEG_MAGIC = b"\xff\xd8"
TAIL_LINES = 20
STREAM_BOUNDARY = "benchframe"  # also the Content-Type boundary; ffmpeg writes it via -boundary_tag
STREAM_WRITE_TIMEOUT = 10  # a client that stops reading is dropped after this many seconds
STREAM_STALL_TIMEOUT = 10  # no ffmpeg output for this long: the stream is dead, end it


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
    if framerate and str(framerate) != "0":  # "0"/empty = leave it to ffmpeg
        argv += ["-framerate", str(framerate)]
    argv += ["-video_size", video_size, "-i", f"{camera}:none"]
    # The backslash is ffmpeg filter-graph escaping for the comma; there is no shell involved.
    argv += ["-vf", f"select=gte(n\\,{int(warmup)})", "-frames:v", "1"]
    return argv + ["-f", "image2pipe", "-c:v", "mjpeg", "pipe:1"]


def stream_argv(ffmpeg, camera, video_size, framerate=None):
    """Live view: ffmpeg's mpjpeg muxer writes the multipart framing, the host copies bytes."""
    argv = [*_cmd(ffmpeg), "-hide_banner", "-nostats", "-f", "avfoundation"]  # no \r stats in the stderr file
    if framerate and str(framerate) != "0":
        argv += ["-framerate", str(framerate)]
    argv += ["-video_size", video_size, "-i", f"{camera}:none"]
    return argv + ["-c:v", "mjpeg", "-f", "mpjpeg", "-boundary_tag", STREAM_BOUNDARY, "pipe:1"]


def write_token(bench_dir):
    """Generate a fresh token and write it to <bench_dir>/token with mode 0600."""
    if os.path.islink(bench_dir):
        raise OSError(f"{bench_dir} is a symlink; refusing to write the token through it")
    os.makedirs(bench_dir, mode=0o700, exist_ok=True)
    token = secrets.token_urlsafe(32)
    # temp file + rename: an existing `token` (even a symlink) is replaced, never written through
    fd, tmp = tempfile.mkstemp(dir=bench_dir, prefix=".token-")  # mode 0600
    try:
        with os.fdopen(fd, "w") as f:
            f.write(token)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, os.path.join(bench_dir, "token"))
    except BaseException:
        os.unlink(tmp)
        raise
    return token


def _safe_cut(b):
    """Where to cut `b` (longer than MAX_LINE): not inside an ANSI escape, not inside a UTF-8 character."""
    cut = MAX_LINE
    esc = b.rfind(b"\x1b", 0, cut)
    if esc > 0 and (m := ANSI_BYTES_RE.match(b, esc)) and m.end() > cut:
        cut = esc
    while cut > 1 and b[cut] & 0xC0 == 0x80:  # b[cut] would start the rest with a continuation byte
        cut -= 1
    return cut


def tool_version(binary, flag):
    try:
        out = subprocess.run([*_cmd(binary), flag], capture_output=True, text=True, timeout=5,
                             stdin=subprocess.DEVNULL, start_new_session=True).stdout
        return out.strip().splitlines()[0] if out.strip() else None
    except (OSError, subprocess.SubprocessError):
        return None


def parse_video_devices(text):
    """{index: name} from the video section of `ffmpeg -f avfoundation -list_devices true -i ""` stderr."""
    devices, in_video = {}, False
    for line in text.splitlines():
        body = re.sub(r"^\[AVFoundation[^\]]*\] ", "", line.rstrip("\r\n"))  # names stay exactly as printed
        if body.strip().endswith("video devices:"):
            in_video = True
        elif body.strip().endswith("audio devices:"):
            in_video = False
        elif in_video and (m := re.match(r"\[(\d+)\] (.+)$", body)):
            devices[int(m[1])] = m[2]
    return devices


def list_video_devices(ffmpeg):
    try:  # the listing itself exits non-zero (empty input), so only stderr matters
        err = subprocess.run([*_cmd(ffmpeg), "-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""],
                             capture_output=True, text=True, timeout=10, stdin=subprocess.DEVNULL,
                             start_new_session=True).stderr
    except (OSError, subprocess.SubprocessError) as e:
        raise ValueError(f"cannot list cameras with ffmpeg: {e}")
    devices = parse_video_devices(err)
    if not devices:
        raise ValueError("cannot list cameras: no video devices in ffmpeg output")
    return devices


def resolve_camera(camera, devices):
    """The device name to capture from; ValueError if not a listed camera. Names only: indices shift."""
    available = "available video devices: " + ", ".join(f"[{i}] {n}" for i, n in devices.items())
    if camera.isnumeric():  # superset of isdigit/isdecimal, catches fullwidth etc.
        raise ValueError(f"camera '{camera}': camera indices are not stable; use a device name; {available}")
    if camera not in devices.values():
        raise ValueError(f"camera '{camera}' not found; {available}")
    if re.fullmatch(r"Capture screen \d+", camera):
        raise ValueError(f"camera '{camera}' is a screen capture, not a camera; {available}")
    return camera


def _signal_group(proc, sig):
    """Signal the child's whole process group (children run in their own session), else just the pid."""
    try:
        os.killpg(proc.pid, sig)
    except OSError:
        try:
            proc.send_signal(sig)
        except OSError:
            pass


def _client_gone(sock):
    """The client socket is readable: EOF or an error means it left (a GET sends nothing more)."""
    try:
        return not sock.recv(4096)
    except OSError:
        return True


# ---- core ------------------------------------------------------------------------------------


class Session:
    """One espflash process. phase: flashing -> running (completion line seen) -> exited."""

    def __init__(self, id, proc, log_seq):
        self.id, self.proc, self.log_seq = id, proc, log_seq
        self.phase, self.app_seq, self.exit_code, self.reader = "flashing", None, None, None


class Bench:
    def __init__(self, *, repo_root=REPO_ROOT, port=None, camera="HD Pro Webcam C920", video_size="1280x720",
                 framerate="30", warmup=20, elf_prefix="firmware/target/", espflash="espflash",
                 ffmpeg="ffmpeg", candidates=list_candidates, flash_timeout=FLASH_TIMEOUT,
                 ring_lines=RING_LINES, snapshot_timeout=SNAPSHOT_TIMEOUT,
                 stream_size="640x480", stream_framerate="30", stream_clients=2,
                 stream_stall_timeout=STREAM_STALL_TIMEOUT):
        self.repo_root, self.port, self.camera = repo_root, port, camera
        self.video_size, self.framerate, self.warmup = video_size, framerate, warmup
        self.elf_prefix, self.espflash, self.ffmpeg = elf_prefix, espflash, ffmpeg
        self.candidates, self.flash_timeout = candidates, flash_timeout
        self.snapshot_timeout = snapshot_timeout
        self.stream_size, self.stream_framerate, self.stream_clients = stream_size, stream_framerate, stream_clients
        self.stream_stall_timeout = stream_stall_timeout
        self.streams, self.streams_lock, self.closed = set(), threading.Lock(), False
        self.camera_name = camera  # resolved device name (main() sets it after validation); the one capture uses
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
            "camera_name": self.camera_name,
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
            if self.closed:
                raise BenchError(503, "service is shutting down")
            try:
                new = stage() if stage else None
            except OSError:
                if self.closed:  # close() removed the stage dir under us
                    raise BenchError(503, "service is shutting down") from None
                raise
            try:
                port = resolve_port(self.port, self.candidates())  # re-resolved every time
                self._stop_process()
                s = self._spawn(espflash_argv(self.espflash, port, new or self.last_elf))
            except BaseException:
                if new:
                    with contextlib.suppress(FileNotFoundError):  # close() may have removed the stage dir
                        os.unlink(new)
                raise
            if new:  # the previous copy is only dropped once the new one is in use
                old, self.last_elf = self.last_elf, new
                if old:
                    with contextlib.suppress(FileNotFoundError):  # close() may have removed the stage dir
                        os.unlink(old)
            if self.closed:  # close() ran after the spawn: it already killed the session
                self.last_elf = None
                raise BenchError(503, "service is shutting down")
            return self._await_flash(s)
        finally:
            self.flash_lock.release()

    def _spawn(self, argv):
        with self.streams_lock:  # close() sets `closed` under it, so nothing spawns after close()
            if self.closed:
                raise BenchError(503, "service is shutting down")
            try:
                proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                        stderr=subprocess.STDOUT, start_new_session=True)
            except OSError as e:
                raise BenchError(500, f"cannot start espflash: {e.strerror}") from None
            with self.cond:  # still under streams_lock: close() then always sees this session
                self.sessions += 1
                s = self.session = Session(self.sessions, proc, self.next_seq)
            s.reader = threading.Thread(target=self._read, args=(s,), daemon=True)
            s.reader.start()
        return s

    def _read(self, s):
        fd, buf = s.proc.stdout.fileno(), b""
        while True:
            # a pending partial line (a prompt, a panic cut short) is flushed after a quiet period
            if not select.select([fd], [], [], PARTIAL_LINE_QUIET if buf else None)[0]:
                self._add_line(s, buf)
                buf = b""
                continue
            chunk = os.read(fd, 65536)
            if not chunk:
                break
            *lines, buf = (buf + chunk).split(b"\n")
            for line in lines:
                self._add_line(s, line)
            while len(buf) > MAX_LINE:  # no newline in sight: bounded memory
                cut = _safe_cut(buf)
                self._add_line(s, buf[:cut], truncated=True)
                buf = buf[cut:]
        if buf:
            self._add_line(s, buf)
        s.proc.stdout.close()
        code = s.proc.wait()
        with self.cond:
            s.exit_code, s.phase = code, "exited"
            self.cond.notify_all()

    def _add_line(self, s, raw, truncated=False):
        """Append one ring line; a line over MAX_LINE becomes several, all but the last marked."""
        pieces = []
        while len(raw) > MAX_LINE:
            cut = _safe_cut(raw)
            pieces.append(raw[:cut])
            raw = raw[cut:]
        pieces.append(raw)
        for n, piece in enumerate(pieces):
            text = ANSI_RE.sub("", piece.decode("utf-8", errors="replace")).rstrip("\r\n")
            if truncated or n < len(pieces) - 1:
                text += LINE_SPLIT_MARK
            with self.cond:
                seq, self.next_seq = self.next_seq, self.next_seq + 1
                self.ring.append({"seq": seq, "t": round(time.time(), 3), "text": text})
                if s.phase == "flashing" and COMPLETE in text:
                    s.phase, s.app_seq = "running", seq
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
            _signal_group(s.proc, signal.SIGTERM)
            try:
                s.proc.wait(2)
            except subprocess.TimeoutExpired:
                _signal_group(s.proc, signal.SIGKILL)
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
        with self.streams_lock:
            self.closed = True
            for proc in self.streams:  # the handler threads see EOF and clean up
                _signal_group(proc, signal.SIGKILL)
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
            argv = ffmpeg_argv(self.ffmpeg, self.camera_name, self.video_size,
                               self.warmup if warmup is None else warmup, self.framerate)
            try:
                proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, start_new_session=True)
            except OSError as e:
                raise BenchError(503, f"cannot start ffmpeg: {e.strerror}") from None
            try:
                out, err = proc.communicate(timeout=self.snapshot_timeout)
                problem = None
            except subprocess.TimeoutExpired:
                _signal_group(proc, signal.SIGKILL)
                out, err = proc.communicate()
                problem = f"ffmpeg timed out after {self.snapshot_timeout}s; killed"
            finally:
                if proc.poll() is None:  # e.g. KeyboardInterrupt during communicate
                    _signal_group(proc, signal.SIGKILL)
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

    # -- stream (no camera_lock: it may run beside a snapshot, R19)

    def stream_start(self, client):
        """Start one ffmpeg for one client socket; returns (proc, stderr_file, first_chunk), or None if
        the client left while waiting for the first frame (checked once a second).

        503 before any response header if ffmpeg can't start or yields nothing; 409 over the cap.
        """
        argv = stream_argv(self.ffmpeg, self.camera_name, self.stream_size, self.stream_framerate)
        with self.streams_lock:
            if self.closed:
                raise BenchError(503, "service is shutting down")
            if len(self.streams) >= self.stream_clients:
                raise BenchError(409, "stream clients busy")
            err = tempfile.TemporaryFile()
            try:
                proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=err,
                                        bufsize=0, start_new_session=True)
            except OSError as e:
                err.close()
                raise BenchError(503, f"cannot start ffmpeg: {e.strerror}") from None
            self.streams.add(proc)
        deadline, ready = time.monotonic() + self.snapshot_timeout, False
        while not ready and time.monotonic() < deadline:
            r = select.select([proc.stdout, client], [], [], 1)[0]
            ready = proc.stdout in r
            if not ready and client in r and _client_gone(client):
                self.stream_stop(proc, err)
                return None
        first = os.read(proc.stdout.fileno(), 65536) if ready else b""
        if not first:
            problem = "ffmpeg exited before the first frame" if ready else f"no frame within {self.snapshot_timeout}s"
            raise BenchError(503, problem, stderr_tail=self.stream_stop(proc, err))
        return proc, err, first

    def stream_stop(self, proc, err):
        """Kill the stream's process group, log and return the stderr tail. Idempotent."""
        _signal_group(proc, signal.SIGKILL)
        proc.wait()
        proc.stdout.close()
        with self.streams_lock:
            self.streams.discard(proc)
        if err.closed:
            return []
        err.seek(0)
        text = err.read().decode("utf-8", errors="replace")
        err.close()
        sys.stderr.write(f"ffmpeg stream exit {proc.returncode}:\n{text}\n")
        return text.splitlines()[-TAIL_LINES:]

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
        # command/path are unset when the request line itself was malformed (e.g. 414).
        command, path = getattr(self, "command", None), getattr(self, "path", "")
        if fmt.startswith("code "):  # log_error(code, message): message quotes the request line, query included
            code = args[0] if args and isinstance(args[0], int) else 0
            reason = http.HTTPStatus(code).phrase if code in set(http.HTTPStatus) else ""
            sys.stderr.write(f"error -> {code} {reason}\n")
            return
        if command is None:  # malformed request line: nothing trustworthy to print but the status
            sys.stderr.write(f"bad request -> {args[1] if len(args) > 1 else '?'}\n")
            return
        if command == "GET" and path.startswith("/log") and str(args[1]).startswith("2"):
            return  # long-poll noise; only failures are logged
        # The peer address is always 127.0.0.1 (gvproxy), so it says nothing about the caller.
        # Query strings are never logged: they can carry the token in any encoding.
        url = urllib.parse.urlsplit(path)
        sys.stderr.write(f"{command} {url.path}{'?...' if url.query else ''} -> {args[1] if len(args) > 1 else '?'}\n")

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

    def _stream(self, bench):
        self.close_connection = True
        started = bench.stream_start(self.connection)  # raises before any header is sent
        if started is None:
            return
        proc, err, chunk = started
        try:
            self.send_response(200)
            self.send_header("Content-Type", f"multipart/x-mixed-replace; boundary={STREAM_BOUNDARY}")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.connection.settimeout(STREAM_WRITE_TIMEOUT)
            while chunk:
                self.wfile.write(chunk)
                chunk = self._next_chunk(bench, proc)
        except OSError:  # client left (BrokenPipe, reset, stalled)
            pass
        finally:
            bench.stream_stop(proc, err)

    def _next_chunk(self, bench, proc):
        """Next ffmpeg bytes; b"" when ffmpeg ended, the client left, or ffmpeg went silent too long."""
        last = time.monotonic()
        while time.monotonic() - last < bench.stream_stall_timeout:
            ready = select.select([proc.stdout, self.connection], [], [], 1)[0]
            if proc.stdout in ready:
                return os.read(proc.stdout.fileno(), 65536)
            if ready and _client_gone(self.connection):
                break
        return b""

    def _authorized(self, stream):
        """`stream` (GET /stream.mjpg only) is an anonymous camera view (user decision, design rev 7):
        no token, Origin allowed for embedding, but the Host must be a loopback name (DNS rebinding)."""
        if stream:
            port = self.server.server_address[1]
            if self.headers.get("Host") not in {f"127.0.0.1:{port}", f"localhost:{port}", f"[::1]:{port}"}:
                self._send(403, {"ok": False, "error": "stream is only served to loopback Host names"})
                return False
            return True
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
        url = urllib.parse.urlsplit(self.path)
        path, bench = url.path, self.server.bench
        if not self._authorized((method, path) == ("GET", "/stream.mjpg")) or not self._body_ok():
            return
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
            elif (method, path) == ("GET", "/stream.mjpg"):
                self._stream(bench)
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
    p.add_argument("--camera", default=env("BENCH_CAMERA", "HD Pro Webcam C920"), help="exact avfoundation video device name (indices are refused)")
    p.add_argument("--video-size", default=env("BENCH_VIDEO_SIZE", "1280x720"))
    p.add_argument("--framerate", default=env("BENCH_FRAMERATE", "30"),
                   help="avfoundation input framerate (default 30; the camera rejects ffmpeg's 29.97 default); 0 or empty = don't pass")
    p.add_argument("--stream-size", default=env("BENCH_STREAM_SIZE", "640x480"), help="GET /stream.mjpg capture size (640x480 is what the C920 delivers at 30 fps)")
    p.add_argument("--stream-framerate", default=env("BENCH_STREAM_FRAMERATE", "30"), help="stream input framerate; 0 or empty = don't pass")
    p.add_argument("--stream-clients", type=int, default=int(env("BENCH_STREAM_CLIENTS", "2")), help="max concurrent stream clients (409 beyond)")
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
        camera_name = resolve_camera(args.camera, list_video_devices(args.ffmpeg))
    except (ValueError, PortError) as e:
        return _fail(str(e))
    restore_tty = _save_termios()
    bench = Bench(port=args.port, camera=args.camera, video_size=args.video_size,
                  framerate=args.framerate, warmup=args.warmup, elf_prefix=args.elf_prefix,
                  espflash=args.espflash, ffmpeg=args.ffmpeg, stream_size=args.stream_size,
                  stream_framerate=args.stream_framerate, stream_clients=args.stream_clients)
    bench.camera_name = camera_name
    print(f"python {sys.version.split()[0]}, {bench.espflash_version}, "
          f"{tool_version(args.ffmpeg, '-version')}", file=sys.stderr)
    try:
        srv = make_server(bench, host, port)  # bind before touching the token of a running instance
    except OSError as e:
        return _fail(f"cannot listen on {host}:{port}: {e.strerror}")
    try:
        srv.token = write_token(bench_dir)
    except OSError as e:
        srv.server_close()
        return _fail(f"cannot write the token: {e}")
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
        restore_tty()
    return 0


def _save_termios():
    """Snapshot the terminal settings; the returned callable restores them (no-op without a tty)."""
    try:
        import termios
        fd = sys.stdin.fileno()
        saved = termios.tcgetattr(fd)
    except Exception:
        return lambda: None

    def restore():
        try:
            termios.tcsetattr(fd, termios.TCSADRAIN, saved)
        except Exception:
            pass
    return restore


def _terminate(*_):
    raise KeyboardInterrupt


def _fail(msg):
    print(f"bench_host: {msg}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())

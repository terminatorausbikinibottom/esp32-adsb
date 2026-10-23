import contextlib
import email
import http.client
import io
import json
import os
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

import bench_host as bh
from tests.test_host_flash import pid_alive

FAKE = str(Path(__file__).resolve().parent / "fake_ffmpeg.py")
TOKEN = "t0ken-SECRET"


class StreamArgvTest(unittest.TestCase):
    def test_defaults_and_overrides(self):
        a = bh.stream_argv("ffmpeg", "Cam", "640x480", "30")
        self.assertEqual(a[a.index("-framerate"):a.index("-framerate") + 4], ["-framerate", "30", "-video_size", "640x480"])
        self.assertIn("Cam:none", a)
        self.assertEqual(a[-5:], ["-f", "mpjpeg", "-boundary_tag", bh.STREAM_BOUNDARY, "pipe:1"])
        self.assertIn("320x240", bh.stream_argv("ffmpeg", "Cam", "320x240", "15"))
        for off in ("0", "", None):
            self.assertNotIn("-framerate", bh.stream_argv("ffmpeg", "Cam", "640x480", off))

    def test_snapshot_argv_unchanged(self):
        a = bh.ffmpeg_argv("ffmpeg", "Cam", "1280x720", 20, "30")
        self.assertIn("1280x720", a)
        self.assertNotIn("mpjpeg", a)


class StreamTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.fakelog = Path(tmp.name) / "fake.log"
        self.env = mock.patch.dict(os.environ, {"FAKE_FFMPEG_LOG": str(self.fakelog), "FAKE_FFMPEG_CHILD": "1"})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.bench = bh.Bench(ffmpeg=[sys.executable, FAKE], espflash=sys.executable,
                              snapshot_timeout=3, candidates=list)
        self.bench.camera_name = "Resolved Cam"
        srv = bh.make_server(self.bench, "127.0.0.1", 0, TOKEN)
        self.addCleanup(srv.server_close)
        self.port = srv.server_address[1]
        threading.Thread(target=srv.serve_forever, args=(0.02,), daemon=True).start()
        self.addCleanup(srv.shutdown)
        self.addCleanup(self.assert_no_orphans)
        self.conns = []
        self.addCleanup(lambda: [c.close() for c in self.conns])
        err = mock.patch("sys.stderr", io.StringIO())
        self.log = err.start()
        self.addCleanup(err.stop)

    def invocations(self):
        if not self.fakelog.exists():
            return []
        return [json.loads(l) for l in self.fakelog.read_text().splitlines()]

    def wait_dead(self, timeout=3):
        end = time.time() + timeout
        while time.time() < end and any(pid_alive(i["pid"]) for i in self.invocations()):
            time.sleep(0.02)

    def assert_no_orphans(self):
        self.bench.close()
        self.wait_dead()
        self.assertEqual([i["pid"] for i in self.invocations() if pid_alive(i["pid"])], [])

    def mode(self, **kv):
        p = mock.patch.dict(os.environ, kv)
        p.start()
        self.addCleanup(p.stop)

    def open(self, path="/stream.mjpg", headers=None):
        h = {} if headers is None else headers
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        self.conns.append(c)
        c.request("GET", path, headers=h)
        return c, c.getresponse()

    def read_until(self, r, n_boundaries):
        data = b""
        while data.count(b"--" + bh.STREAM_BOUNDARY.encode()) < n_boundaries:
            chunk = r.read1(65536)
            self.assertTrue(chunk, "stream ended early")
            data += chunk
        return data

    def bearer(self):
        return {"Authorization": f"Bearer {TOKEN}"}

    def test_parts_parse(self):
        self.mode(FAKE_FFMPEG_STREAM_FRAMES="4")
        _, r = self.open(headers=self.bearer())
        self.assertEqual(r.status, 200)
        self.assertEqual(r.getheader("Content-Type"), f"multipart/x-mixed-replace; boundary={bh.STREAM_BOUNDARY}")
        self.assertEqual(r.getheader("Cache-Control"), "no-store")
        body = r.read()  # ends when the fake exits
        tag = f"--{bh.STREAM_BOUNDARY}\r\n".encode()
        self.assertTrue(body.startswith(tag))
        parts = [p for p in body.split(tag) if p]
        self.assertEqual(len(parts), 4)
        for p in parts:
            head, _, payload = p.partition(b"\r\n\r\n")
            msg = email.message_from_bytes(head)
            self.assertEqual(msg["Content-Type"], "image/jpeg")
            self.assertEqual(int(msg["Content-Length"]), len(payload.removesuffix(b"\r\n")))
            self.assertTrue(payload.startswith(b"\xff\xd8"))
        argv = self.invocations()[0]["argv"]
        self.assertIn("Resolved Cam:none", argv)
        self.assertEqual(argv[argv.index("-framerate") + 1:argv.index("-framerate") + 4], ["30", "-video_size", "640x480"])
        self.assertEqual(argv[argv.index("-f", argv.index("-i")) + 1], "mpjpeg")
        self.assertEqual(argv[argv.index("-boundary_tag") + 1], bh.STREAM_BOUNDARY)
        self.assertNotEqual(self.invocations()[0]["sid"], os.getsid(0))

    def test_client_disconnect_kills_group(self):
        self.mode(FAKE_FFMPEG_STREAM_FRAMES="0")
        c, r = self.open(headers=self.bearer())
        self.read_until(r, 2)
        pids = [i["pid"] for i in self.invocations()]
        self.assertEqual(len(pids), 2)  # fake + its child
        self.assertTrue(all(pid_alive(p) for p in pids))
        c.close()
        r.close()
        self.wait_dead()
        self.assertEqual([p for p in pids if pid_alive(p)], [])
        self.assertEqual(self.bench.streams, set())

    def test_cap_409_and_slot_freed(self):
        self.mode(FAKE_FFMPEG_STREAM_FRAMES="0", FAKE_FFMPEG_CHILD="0")
        (c1, r1), (c2, r2) = self.open(headers=self.bearer()), self.open(headers=self.bearer())
        self.read_until(r1, 2), self.read_until(r2, 2)
        _, r3 = self.open(headers=self.bearer())
        self.assertEqual(r3.status, 409)
        self.assertIn("busy", r3.read().decode())
        self.read_until(r1, 2), self.read_until(r2, 2)  # the first two keep streaming
        c1.close(), r1.close()
        end = time.time() + 3
        while len(self.bench.streams) > 1 and time.time() < end:
            time.sleep(0.02)
        _, r4 = self.open(headers=self.bearer())
        self.assertEqual(r4.status, 200)

    def test_start_failure_503_before_headers(self):
        self.mode(FAKE_FFMPEG_MODE="fail")
        _, r = self.open(headers=self.bearer())
        self.assertEqual((r.status, r.getheader("Content-Type")), (503, "application/json"))
        self.assertIn("not authorized", r.read().decode())
        self.assertEqual(self.bench.streams, set())
        self.bench.ffmpeg = ["/nonexistent/ffmpeg"]
        _, r = self.open(headers=self.bearer())
        self.assertEqual(r.status, 503)

    def test_shutdown_kills_streams(self):
        self.mode(FAKE_FFMPEG_STREAM_FRAMES="0")
        _, r = self.open(headers=self.bearer())
        self.read_until(r, 2)
        self.bench.close()
        self.wait_dead()
        self.assertEqual([i for i in self.invocations() if pid_alive(i["pid"])], [])
        while r.read1(65536):  # the response ends
            pass
        _, r2 = self.open(headers=self.bearer())  # and none starts afterwards
        self.assertEqual(r2.status, 503)

    def started(self):
        return [i for i in self.invocations() if i["argv"] != ["child"]]

    def test_anonymous_stream_and_hosts(self):
        self.mode(FAKE_FFMPEG_STREAM_FRAMES="1")
        for host in (f"127.0.0.1:{self.port}", f"localhost:{self.port}", f"[::1]:{self.port}"):
            _, r = self.open(headers={"Host": host})
            self.assertEqual(r.status, 200, host)
            r.read()
        _, r = self.open()  # http.client's default Host is 127.0.0.1:<port>
        self.assertEqual(r.status, 200)
        r.read()
        _, r = self.open(headers={"Authorization": "Bearer wrong"})  # ignored here
        self.assertEqual(r.status, 200)
        r.read()
        _, r = self.open(headers={"Origin": "http://evil.example"})  # embedding allowed
        self.assertEqual(r.status, 200)
        r.read()
        self.assertEqual(len(self.started()), 6)

    def test_bad_host_403_and_nothing_started(self):
        before = len(self.started())
        for host in (f"host.containers.internal:{self.port}", f"evil.example:{self.port}", "127.0.0.1",
                     "localhost", f"127.0.0.1:{self.port + 1}", ""):
            _, r = self.open(headers={"Host": host, **self.bearer()})
            self.assertEqual(r.status, 403, host)
            r.read()
        self.assertEqual(len(self.started()), before)

    def test_other_endpoints_keep_token_and_origin_rules(self):
        for path in ("/health", "/snapshot.jpg", "/log", "/flash", "/nope"):
            for method in ("GET", "POST"):
                for q in (f"?token={TOKEN}", f"?%74oken={TOKEN}"):
                    _, r = self.open_method(method, path + q)
                    self.assertEqual(r.status, 401, (method, path, q))
                _, r = self.open_method(method, path, {"Origin": "http://x", **self.bearer()})
                self.assertEqual(r.status, 403, (method, path))
        self.assertEqual(self.invocations(), [])  # nothing was started

    def open_method(self, method, path, headers=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        self.conns.append(c)
        c.request(method, path, headers=headers or {}, body=b"{}" if method == "POST" else None)
        return c, c.getresponse()

    def test_access_log_has_no_query_or_token(self):
        self.mode(FAKE_FFMPEG_STREAM_FRAMES="1")
        _, r = self.open(path="/stream.mjpg?x=1")
        r.read()
        enc = "".join(f"%{ord(c):02x}" for c in TOKEN)
        for q in (f"token={TOKEN}", f"%74oken={TOKEN}", f"token={enc}", f"TOKEN={TOKEN}", f"token%3D{TOKEN}",
                  f"x=1&token={TOKEN}"):
            for path in ("/stream.mjpg", "/health"):
                _, r = self.open_method("GET", f"{path}?{q}")
                r.read()
        time.sleep(0.2)
        log = self.log.getvalue()
        self.assertIn("GET /stream.mjpg?... -> 200", log)
        self.assertIn("GET /health?... -> 401", log)
        self.assertNotIn("SECRET", log)
        self.assertNotIn("x=1", log)

    def test_snapshot_during_stream(self):
        self.mode(FAKE_FFMPEG_STREAM_FRAMES="0", FAKE_FFMPEG_CHILD="0")
        _, r = self.open(headers=self.bearer())
        self.read_until(r, 2)
        _, s = self.open_method("GET", "/snapshot.jpg", self.bearer())
        self.assertEqual(s.status, 200)
        self.assertTrue(s.read().startswith(b"\xff\xd8"))

    def test_stalled_ffmpeg_after_client_close_frees_slot(self):
        self.mode(FAKE_FFMPEG_STREAM_FRAMES="1", FAKE_FFMPEG_STREAM_STALL="1")
        c, r = self.open(headers=self.bearer())
        self.read_until(r, 2)
        pids = [i["pid"] for i in self.invocations()]
        c.close()
        r.close()
        self.wait_dead(3)
        self.assertEqual([p for p in pids if pid_alive(p)], [])
        self.assertEqual(self.bench.streams, set())

    def test_stall_watchdog_ends_stream(self):
        self.mode(FAKE_FFMPEG_STREAM_FRAMES="1", FAKE_FFMPEG_STREAM_STALL="1")
        self.bench.stream_stall_timeout = 1
        _, r = self.open(headers=self.bearer())
        self.read_until(r, 2)
        t = time.time()
        while r.read1(65536):  # client stays connected; the service ends the response
            pass
        self.assertLess(time.time() - t, 5)
        self.wait_dead()
        self.assertEqual(self.bench.streams, set())

    def test_client_leaves_during_first_frame_wait(self):
        self.mode(FAKE_FFMPEG_STREAM_FRAMES="1", FAKE_FFMPEG_STREAM_STALL="1")
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        c.request("GET", "/stream.mjpg")
        c.close()  # before any frame is read; a frame may already be in, either way the slot must free
        end = time.time() + 4
        while self.bench.streams and time.time() < end:
            time.sleep(0.05)
        self.assertEqual(self.bench.streams, set())

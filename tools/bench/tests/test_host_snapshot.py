import contextlib
import io
import json
import os
import sys
import tempfile
import threading
import time
import unittest
import http.client
from pathlib import Path
from unittest import mock

import bench_host as bh
from tests.test_host_flash import pid_alive

FAKE = str(Path(__file__).resolve().parent / "fake_ffmpeg.py")
TOKEN = "t0ken"


class SnapshotTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.fakelog = Path(tmp.name) / "fake.log"
        env = mock.patch.dict(os.environ, {"FAKE_FFMPEG_LOG": str(self.fakelog)})
        env.start()
        self.addCleanup(env.stop)
        self.bench = bh.Bench(ffmpeg=[sys.executable, FAKE], espflash=sys.executable,
                              snapshot_timeout=1.5, candidates=list)
        srv = bh.make_server(self.bench, "127.0.0.1", 0, TOKEN)
        self.addCleanup(srv.server_close)
        self.port = srv.server_address[1]
        threading.Thread(target=srv.serve_forever, args=(0.02,), daemon=True).start()
        self.addCleanup(srv.shutdown)
        self.addCleanup(self.assert_no_orphans)

    def mode(self, m):
        p = mock.patch.dict(os.environ, {"FAKE_FFMPEG_MODE": m})
        p.start()
        self.addCleanup(p.stop)

    def invocations(self):
        if not self.fakelog.exists():
            return []
        return [json.loads(l) for l in self.fakelog.read_text().splitlines()]

    def assert_no_orphans(self):
        self.assertEqual([i["pid"] for i in self.invocations() if pid_alive(i["pid"])], [])

    def get(self, path="/snapshot.jpg", auth=True):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=15)
        c.request("GET", path, headers={"Authorization": f"Bearer {TOKEN}"} if auth else {})
        r = c.getresponse()
        body = r.read()
        c.close()
        return r.status, r.getheader("Content-Type"), body

    def get_logged(self, path="/snapshot.jpg"):
        with contextlib.redirect_stderr(io.StringIO()) as err:
            res = self.get(path)
        return res, err.getvalue()

    def test_capture_uses_camera_name(self):
        self.bench.camera_name = "Resolved Cam"
        self.get_logged()
        argv = self.invocations()[0]["argv"]
        self.assertIn("Resolved Cam:none", argv)

    def test_ok_jpeg(self):
        (status, ctype, body), _ = self.get_logged()
        self.assertEqual((status, ctype), (200, "image/jpeg"))
        self.assertTrue(body.startswith(b"\xff\xd8"))
        argv = self.invocations()[0]["argv"]
        self.assertIn("HD Pro Webcam C920:none", argv)
        self.assertIn("select=gte(n\\,20)", argv)
        self.assertEqual(argv[argv.index("-framerate") + 1], "30")
        self.assertEqual(argv.index("-framerate") + 2, argv.index("-video_size"))
        inv = self.invocations()[0]  # own session: no controlling terminal to put in raw mode
        self.assertNotEqual(inv["sid"], os.getsid(0))
        self.assertEqual(inv["pgid"], inv["pid"])

    def test_warmup_param(self):
        self.get_logged("/snapshot.jpg?warmup=3")
        self.assertIn("select=gte(n\\,3)", self.invocations()[0]["argv"])

    def test_bad_warmup_400(self):
        self.assertEqual(self.get("/snapshot.jpg?warmup=x")[0], 400)
        self.assertEqual(self.get("/snapshot.jpg?warmup=-1")[0], 400)

    def test_requires_auth(self):
        self.assertEqual(self.get(auth=False)[0], 401)
        self.assertEqual(self.invocations(), [])

    def test_fail_503_with_stderr_tail_and_logged(self):
        self.mode("fail")
        (status, _, body), log = self.get_logged()
        self.assertEqual(status, 503)
        self.assertIn("not authorized", json.loads(body)["stderr_tail"][-1])
        self.assertIn("not authorized", log)

    def test_not_jpeg_503(self):
        self.mode("notjpeg")
        (status, _, body), _ = self.get_logged()
        self.assertEqual(status, 503)
        self.assertIn("not a JPEG", json.loads(body)["error"])

    def test_fallback_line_still_200_and_logged(self):
        self.mode("fallback")
        (status, _, body), log = self.get_logged()
        self.assertEqual(status, 200)
        self.assertTrue(body.startswith(b"\xff\xd8"))
        self.assertIn("falling back to default", log)

    def test_timeout_503_and_killed(self):
        self.mode("hang")
        t = time.time()
        (status, _, body), _ = self.get_logged()
        self.assertEqual(status, 503)
        self.assertIn("timed out", json.loads(body)["error"])
        self.assertLess(time.time() - t, 10)
        (inv,) = self.invocations()
        self.assertFalse(pid_alive(inv["pid"]))

    def test_second_snapshot_while_running_409(self):
        self.mode("hang")
        self.bench.snapshot_timeout = 3
        first = threading.Thread(target=self.get_logged)
        first.start()
        deadline = time.time() + 5
        while not self.bench.camera_lock.locked() and time.time() < deadline:
            time.sleep(0.01)
        self.assertEqual(self.get("/snapshot.jpg")[0], 409)
        first.join()

    def test_camera_lock_independent_of_flash_lock(self):
        with self.bench.flash_lock:
            (status, _, _), _ = self.get_logged()
        self.assertEqual(status, 200)

    def test_missing_binary_503(self):
        self.bench.ffmpeg = "/nonexistent/ffmpeg"
        (status, _, body), _ = self.get_logged()
        self.assertEqual(status, 503)
        self.assertFalse(self.bench.health()["ffmpeg_present"])

    def test_health_present_with_argv_prefix(self):
        self.assertTrue(self.bench.health()["ffmpeg_present"])


if __name__ == "__main__":
    unittest.main()

import http.client
import json
import sys
import threading
import unittest

import bench_host as bh

TOKEN = "t0ken"


class HealthHttpTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # sys.executable stands in for both binaries: it answers `--version`, and exists.
        cls.cands = ["/dev/cu.usbmodemA"]
        bench = bh.Bench(espflash=sys.executable, ffmpeg=sys.executable, candidates=lambda: cls.cands)
        cls.srv = bh.make_server(bench, "127.0.0.1", 0, TOKEN)
        cls.port = cls.srv.server_address[1]
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()
        cls.bench = bench

    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown()
        cls.srv.server_close()

    def req(self, method="GET", path="/health", headers=None, body=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        c.request(method, path, body=body, headers=headers or {})
        r = c.getresponse()
        data = json.loads(r.read())
        c.close()
        return r.status, data

    def auth(self, **extra):
        return {"Authorization": f"Bearer {TOKEN}", **extra}

    def test_no_token_401(self):
        self.assertEqual(self.req()[0], 401)

    def test_wrong_token_401(self):
        self.assertEqual(self.req(headers={"Authorization": "Bearer nope"})[0], 401)
        self.assertEqual(self.req(headers={"Authorization": TOKEN})[0], 401)

    def test_origin_403_even_with_valid_token(self):
        self.assertEqual(self.req(headers=self.auth(Origin="http://evil.example"))[0], 403)

    def test_origin_403_without_token(self):
        self.assertEqual(self.req(headers={"Origin": "http://evil.example"})[0], 403)

    def test_health_ok(self):
        status, h = self.req(headers=self.auth())
        self.assertEqual(status, 200)
        self.assertTrue(h["ok"])
        self.assertEqual(h["python_version"].split(".")[0], "3")
        self.assertEqual(h["serial"], {"mode": "auto", "chosen": "/dev/cu.usbmodemA",
                                       "present": False, "candidates": ["/dev/cu.usbmodemA"]})
        self.assertTrue(h["ffmpeg_present"])
        self.assertTrue(h["espflash_version"])
        self.assertEqual(h["monitor"], {"running": False, "phase": None, "exit_code": None})
        self.assertFalse(h["busy"])

    def test_health_ambiguous_auto_chosen_null(self):
        self.addCleanup(setattr, type(self), "cands", self.cands)
        type(self).cands = ["/dev/cu.usbmodemA", "/dev/cu.usbmodemB"]
        serial = self.req(headers=self.auth())[1]["serial"]
        self.assertIsNone(serial["chosen"])
        self.assertEqual(len(serial["candidates"]), 2)

    def test_health_fixed_port(self):
        self.addCleanup(setattr, self.bench, "port", None)
        self.bench.port = sys.executable  # an existing path
        serial = self.req(headers=self.auth())[1]["serial"]
        self.assertEqual((serial["mode"], serial["chosen"], serial["present"]), ("fixed", sys.executable, True))

    def test_body_cap_413(self):
        status, _ = self.req("POST", "/health", self.auth(), body=b"x" * (bh.MAX_BODY + 1))
        self.assertEqual(status, 413)

    def test_unknown_path_404(self):
        self.assertEqual(self.req(path="/nope", headers=self.auth())[0], 404)


if __name__ == "__main__":
    unittest.main()

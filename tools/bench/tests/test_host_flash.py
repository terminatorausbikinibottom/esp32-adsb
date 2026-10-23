import http.client
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

import bench_host as bh

HERE = Path(__file__).resolve().parent
FAKE = str(HERE / "fake_espflash.py")
TOKEN = "t0ken"
ELF = "firmware/target/fw.elf"


def pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class FlashBase(unittest.TestCase):
    cands = ["/dev/cu.usbmodemA"]
    bench_kw = {}

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(os.path.realpath(tmp.name))
        (self.root / "firmware/target").mkdir(parents=True)
        (self.root / ELF).write_bytes(b"\x7fELF" + b"\0" * 64)
        self.fakelog = self.root / "fake.log"
        env = mock.patch.dict(os.environ, {"FAKE_ESPFLASH_LOG": str(self.fakelog)})
        env.start()
        self.addCleanup(env.stop)
        self.bench = bh.Bench(repo_root=str(self.root), espflash=[sys.executable, FAKE],
                              candidates=lambda: self.cands, **{"flash_timeout": 10, **self.bench_kw})
        srv = bh.make_server(self.bench, "127.0.0.1", 0, TOKEN)
        self.addCleanup(srv.server_close)
        self.port = srv.server_address[1]
        threading.Thread(target=srv.serve_forever, args=(0.02,), daemon=True).start()
        self.addCleanup(srv.shutdown)
        self.addCleanup(self.assert_no_orphans)  # runs first (LIFO), after close below
        self.addCleanup(self.bench.close)

    def pids(self):
        if not self.fakelog.exists():
            return []
        return [json.loads(l)["pid"] for l in self.fakelog.read_text().splitlines()]

    def invocations(self):
        return [json.loads(l)["argv"] for l in self.fakelog.read_text().splitlines()]

    def assert_no_orphans(self):
        self.assertEqual([p for p in self.pids() if pid_alive(p)], [])

    def req(self, method, path, body=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=15)
        raw = body if isinstance(body, bytes) else (json.dumps(body).encode() if body is not None else None)
        c.request(method, path, body=raw, headers={"Authorization": f"Bearer {TOKEN}"})
        r = c.getresponse()
        data = json.loads(r.read())
        c.close()
        return r.status, data

    def flash(self, elf=ELF):
        return self.req("POST", "/flash", {"elf": elf})

    def log(self, since=0, wait=0, **kw):
        return self.req("GET", f"/log?since={since}&wait={wait}")[1]

    def wait_for_line(self, text, since=0, timeout=10):
        deadline = time.time() + timeout
        seen = []
        while time.time() < deadline:
            d = self.log(since, wait=1)
            seen += d["lines"]
            since = d["next"]
            if any(text in l["text"] for l in seen):
                return seen, d
        self.fail(f"{text!r} not seen in {[l['text'] for l in seen]}")


class FlashTest(FlashBase):
    def test_ok_flash_and_double_boot(self):
        status, r = self.flash()
        self.assertEqual(status, 200)
        self.assertIsNone(r["exit_code"])
        self.assertEqual(r["session"], 1)
        lines, d = self.wait_for_line("alive 0s", since=r["log_seq"] - 1)
        texts = [l["text"] for l in lines]
        self.assertEqual(lines[r["app_seq"] - r["log_seq"]]["text"], "INFO - Flashing has completed!")
        self.assertEqual(sum(t.startswith("ESP-ROM:") for t in texts), 2)
        seqs = [l["seq"] for l in lines]
        self.assertEqual(seqs, list(range(r["log_seq"], r["log_seq"] + len(seqs))))
        self.assertEqual(d["monitor"], {"running": True, "phase": "running", "exit_code": None})
        self.assertEqual(d["app_seq"], r["app_seq"])
        self.assertEqual(self.req("GET", "/health")[1]["monitor"]["phase"], "running")
        argv = self.invocations()[0]
        self.assertEqual(argv[-1], self.bench.last_elf)
        self.assertEqual(argv[argv.index("--port") + 1], "/dev/cu.usbmodemA")

    def test_child_in_own_session_without_tty(self):
        self.flash()
        inv = json.loads(self.fakelog.read_text().splitlines()[0])
        self.assertNotEqual(inv["sid"], os.getsid(0))
        self.assertEqual(inv["pgid"], inv["pid"])
        self.assertFalse(inv["tty"])  # crossterm would otherwise open /dev/tty and go raw

    def test_log_requests_not_logged_when_ok(self):
        import contextlib, io
        with contextlib.redirect_stderr(io.StringIO()) as err:
            self.assertEqual(self.req("GET", "/log?since=0&wait=0")[0], 200)
            self.req("GET", "/health")
            self.req("GET", "/log?since=x")  # non-2xx stays visible
        out = err.getvalue()
        self.assertIn("GET /health -> 200", out)
        self.assertNotIn("/log?since=0", out)
        self.assertIn("/log?... -> 4", out)

    def test_fake_pins_argv(self):
        p = subprocess.run([sys.executable, FAKE, "flash", "--monitor"], capture_output=True, timeout=10)
        self.assertEqual(p.returncode, 2)

    def test_fake_runs_via_path_without_uv(self):
        b = bh.Bench(repo_root=str(self.root), espflash=FAKE, port="/dev/x", flash_timeout=10)
        self.addCleanup(b.close)
        self.assertIn("app_seq", b.flash(ELF))
        self.assertEqual(self.invocations()[0][-1], b.last_elf)

    def test_ansi_and_crlf_stripped(self):
        with mock.patch.dict(os.environ, {"FAKE_ESPFLASH_ANSI": "1", "FAKE_ESPFLASH_CRLF": "1"}):
            _, r = self.flash()
        lines, _ = self.wait_for_line("alive 0s", since=r["log_seq"] - 1)
        for l in lines:
            self.assertNotIn("\x1b", l["text"])
            self.assertNotIn("\r", l["text"])
        self.assertIn("INFO - alive 0s", [l["text"] for l in lines])

    def test_flash_fail_502(self):
        with mock.patch.dict(os.environ, {"FAKE_ESPFLASH_MODE": "flash_fail"}):
            status, r = self.flash()
        self.assertEqual(status, 502)
        self.assertEqual(r["exit_code"], 1)
        self.assertIn("Error: espflash::connection_failed", r["output_tail"])
        self.assertNotIn("app_seq", r)
        d = self.log()
        self.assertEqual(d["monitor"], {"running": False, "phase": "exited", "exit_code": 1})
        self.assertIsNone(d["app_seq"])

    def test_flash_hang_504_kills_fake(self):
        self.bench.flash_timeout = 0.5
        with mock.patch.dict(os.environ, {"FAKE_ESPFLASH_MODE": "flash_hang"}):
            status, r = self.flash()
        self.assertEqual(status, 504)
        self.assertEqual(self.log()["monitor"]["phase"], "exited")
        self.assertEqual([p for p in self.pids() if pid_alive(p)], [])

    def test_concurrent_409_and_stop_while_flashing_409(self):
        self.bench.flash_timeout = 1.5
        result = []
        with mock.patch.dict(os.environ, {"FAKE_ESPFLASH_MODE": "flash_hang"}):
            t = threading.Thread(target=lambda: result.append(self.flash()))
            t.start()
            deadline = time.time() + 5
            while not self.req("GET", "/health")[1]["busy"] and time.time() < deadline:
                time.sleep(0.01)
            self.assertEqual(self.flash()[0], 409)
            self.assertEqual(self.req("POST", "/reset")[0], 409)
            self.assertEqual(self.req("POST", "/monitor/stop")[0], 409)
            self.assertEqual(self.log()["monitor"]["phase"], "flashing")
            t.join(10)
        self.assertEqual(result[0][0], 504)

    def test_stop_while_running_kills_fake(self):
        self.flash()
        pid = self.pids()[0]
        self.assertTrue(pid_alive(pid))
        status, r = self.req("POST", "/monitor/stop", {})
        self.assertEqual((status, r["stopped"]), (200, True))
        self.assertFalse(pid_alive(pid))
        self.assertFalse(self.log()["monitor"]["running"])
        self.assertFalse(self.req("POST", "/monitor/stop")[1]["stopped"])

    def test_stop_without_session(self):
        self.assertEqual(self.req("POST", "/monitor/stop")[1], {"ok": True, "stopped": False})

    def test_monitor_dies_after_completion(self):
        with mock.patch.dict(os.environ, {"FAKE_ESPFLASH_MODE": "die"}):
            self.assertEqual(self.flash()[0], 200)
            _, d = self.wait_for_line("alive 0s")
            deadline = time.time() + 5
            while d["monitor"]["running"] and time.time() < deadline:
                d = self.log(d["next"], wait=1)
        self.assertEqual(d["monitor"], {"running": False, "phase": "exited", "exit_code": 1})
        self.assertIsNotNone(d["app_seq"])

    def test_dropped_when_since_older_than_buffer(self):
        self.bench.ring = type(self.bench.ring)(maxlen=5)
        self.flash()
        self.wait_for_line("alive 2s")
        d = self.log(0)
        self.assertTrue(d["dropped"])
        self.assertEqual(len(d["lines"]), 5)
        self.assertFalse(self.log(d["lines"][0]["seq"] - 1)["dropped"])

    def test_long_poll_returns_early_and_empty_after_wait(self):
        self.flash()
        d = self.wait_for_line("alive 0s")[1]
        t0 = time.time()
        d2 = self.log(self.log()["next"], wait=5)
        self.assertTrue(d2["lines"])
        self.assertLess(time.time() - t0, 3)
        self.req("POST", "/monitor/stop")
        nxt = self.log()["next"]
        t0 = time.time()
        d3 = self.log(nxt, wait=0.3)
        self.assertEqual((d3["lines"], d3["next"]), ([], nxt))
        self.assertGreaterEqual(time.time() - t0, 0.25)

    def test_log_max(self):
        self.flash()
        self.wait_for_line("alive 0s")
        d = self.req("GET", "/log?since=0&max=3")[1]
        self.assertEqual([l["seq"] for l in d["lines"]], [1, 2, 3])
        self.assertEqual(d["next"], 3)

    def test_reset_before_flash_409(self):
        self.assertEqual(self.req("POST", "/reset")[0], 409)

    def test_reset_reuses_copy_in_new_session(self):
        _, r1 = self.flash()
        status, r2 = self.req("POST", "/reset")
        self.assertEqual((status, r2["session"]), (200, 2))
        self.assertGreater(r2["log_seq"], r1["app_seq"])
        a1, a2 = self.invocations()
        self.assertEqual(a1, a2)
        self.assertEqual(a1[-1], self.bench.last_elf)
        self.assertFalse(pid_alive(self.pids()[0]))

    def test_next_flash_deletes_previous_copy(self):
        self.flash()
        first = self.bench.last_elf
        self.flash()
        self.assertFalse(os.path.exists(first))
        self.assertTrue(os.path.exists(self.bench.last_elf))
        self.bench.close()
        self.assertFalse(os.path.exists(self.bench.stage_dir or "/nonexistent"))

    def test_bad_flash_keeps_last_copy_for_reset(self):
        self.flash()
        copy = self.bench.last_elf
        self.assertEqual(self.flash("../README.md")[0], 400)
        self.assertEqual(self.bench.last_elf, copy)
        self.assertEqual(self.req("POST", "/reset")[0], 200)

    def test_ambiguous_port_503_keeps_running_monitor(self):
        self.flash()
        pid = self.pids()[0]
        self.cands = ["/dev/cu.usbmodemA", "/dev/cu.usbmodemB"]
        status, r = self.flash()
        self.assertEqual(status, 503)
        self.assertEqual(r["candidates"], self.cands)
        self.assertTrue(pid_alive(pid))
        self.assertEqual(len(self.pids()), 1)

    def test_port_reresolved_each_flash(self):
        self.flash()
        self.cands = ["/dev/cu.usbmodemB"]
        self.flash()
        ports = [a[a.index("--port") + 1] for a in self.invocations()]
        self.assertEqual(ports, ["/dev/cu.usbmodemA", "/dev/cu.usbmodemB"])

    def test_body_validation(self):
        self.assertEqual(self.req("POST", "/flash", {"elf": ELF, "port": "/dev/x"})[0], 400)
        self.assertEqual(self.req("POST", "/flash", b"{nope")[0], 400)
        self.assertEqual(self.req("POST", "/flash", [ELF])[0], 400)
        self.assertEqual(self.req("POST", "/flash", {})[0], 400)
        self.assertEqual(self.req("POST", "/reset", {"x": 1})[0], 400)
        self.assertEqual(self.req("GET", "/log?since=abc")[0], 400)
        self.assertEqual(self.req("GET", "/log?wait=nan")[0], 400)
        self.assertEqual(self.pids(), [])

    def test_modes_are_just_buffered(self):
        for mode, needle in [("panic", "Backtrace:"), ("silent", "Embassy initialized!")]:
            with self.subTest(mode=mode), mock.patch.dict(os.environ, {"FAKE_ESPFLASH_MODE": mode}):
                _, r = self.flash()
                lines, _ = self.wait_for_line(needle, since=r["log_seq"] - 1)
                texts = [l["text"] for l in lines]
                self.assertEqual("INFO - alive 0s" in texts, mode == "panic")
        with mock.patch.dict(os.environ, {"FAKE_ESPFLASH_MODE": "bootloop"}):
            _, r = self.flash()
            time.sleep(0.5)
            texts = [l["text"] for l in self.log(r["log_seq"] - 1)["lines"]]
            self.assertGreater(sum(t.startswith("ESP-ROM:") for t in texts), 3)
            self.assertNotIn("INFO - alive 0s", texts)

class SerialReaderTest(FlashBase):
    def test_newline_less_flood_is_bounded(self):
        with mock.patch.dict(os.environ, {"FAKE_ESPFLASH_MODE": "flood"}):
            _, r = self.flash()
        deadline = time.time() + 10
        while time.time() < deadline and sum(len(l["text"]) for l in self.log(r["app_seq"])["lines"]) < (1 << 20):
            time.sleep(0.1)
        lines = self.log(r["app_seq"], max=5000)["lines"]
        self.assertGreaterEqual(sum(len(l["text"].removesuffix(bh.LINE_SPLIT_MARK)) for l in lines), 1 << 20)
        self.assertTrue(all(len(l["text"]) <= bh.MAX_LINE + len(bh.LINE_SPLIT_MARK) for l in lines))
        self.assertTrue(lines[0]["text"].endswith(bh.LINE_SPLIT_MARK))

    def test_partial_line_shows_after_quiet_period(self):
        with mock.patch.dict(os.environ, {"FAKE_ESPFLASH_MODE": "partial"}):
            _, r = self.flash()
        t = time.time()
        lines, _ = self.wait_for_line("Guru Meditation: no newline", since=r["app_seq"])
        self.assertGreaterEqual(time.time() - t, 0.3)  # not before the quiet period
        self.assertEqual([l["text"] for l in lines].count("Guru Meditation: no newline"), 1)

    def test_blank_lines_and_split_complete_line(self):
        s = bh.Session(1, mock.Mock(), 1)
        self.bench._add_line(s, b"")
        self.bench._add_line(s, b"a" * (bh.MAX_LINE + 1))
        texts = [l["text"] for l in self.bench.ring]
        self.assertEqual(texts, ["", "a" * bh.MAX_LINE + bh.LINE_SPLIT_MARK, "a"])


class CloseRaceTest(FlashBase):
    def test_close_during_stage_spawns_nothing(self):
        staging, release = threading.Event(), threading.Event()
        real = bh.stage_elf

        def slow(*a):
            staging.set()
            release.wait(5)
            return real(*a)

        result = []
        with mock.patch.object(bh, "stage_elf", slow):
            t = threading.Thread(target=lambda: result.append(self.flash()))
            t.start()
            self.assertTrue(staging.wait(5))
            self.bench.close()
            release.set()
            t.join(10)
        self.assertEqual(result[0][0], 503)
        self.assertEqual(self.pids(), [])  # espflash was never started
        self.assertIsNone(self.bench.session)


class MalformedRequestLogTest(FlashBase):
    def test_log_has_no_query_text(self):
        import contextlib, io, socket
        lines = [b"GET /health?q=SECRETa b HTTP/1.1\r\n\r\n", b"GET /health?q=SECRETb HTTP/9.9\r\n\r\n",
                 b"FOO /x?q=SECRETc HTTP/1.1\r\n\r\n", b"GET /x?q=SECRETd\r\n\r\n",
                 b"GET /" + b"a" * 70000 + b"?q=SECRETe HTTP/1.1\r\n\r\n", b"SECRETf\r\n\r\n"]
        with contextlib.redirect_stderr(io.StringIO()) as err:
            for raw in lines:
                with socket.create_connection(("127.0.0.1", self.port), timeout=5) as c:
                    try:
                        c.sendall(raw)
                    except OSError:
                        pass
                    try:
                        while c.recv(4096):
                            pass
                    except OSError:
                        pass
            time.sleep(0.2)
        out = err.getvalue()
        self.assertIn("-> 4", out)  # something was logged, with a status
        self.assertNotIn("SECRET", out)
        self.assertNotIn("q=", out)


FAKE_FFMPEG = str(Path(__file__).resolve().parent / "fake_ffmpeg.py")


class ShutdownTest(FlashBase):
    def test_main_kills_child_on_interrupt(self):
        def fake_make_server(bench, host, port, token=None):
            class Srv:
                def serve_forever(s):  # a flash is in flight, then Ctrl-C / SIGTERM
                    bench.repo_root = str(self.root)
                    bench.flash(ELF)
                    self.assertEqual(len(self.pids()), 1)
                    raise KeyboardInterrupt

                def server_close(s):
                    pass
            return Srv()

        with mock.patch.object(bh, "make_server", fake_make_server), \
                mock.patch.object(bh.atexit, "register") as register:
            rc = bh.main(["--listen", "127.0.0.1:0", "--port", "/dev/x", "--espflash", FAKE,
                          "--ffmpeg", FAKE_FFMPEG], bench_dir=self.root / ".bench")
        self.assertEqual(rc, 0)
        self.assertEqual([p for p in self.pids() if pid_alive(p)], [])  # killed by main's finally
        self.assertEqual(register.call_args.args[0].__name__, "close")  # and atexit is armed


if __name__ == "__main__":
    unittest.main()

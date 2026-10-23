"""End to end: the CLI as a subprocess against an in-process host service with both fakes.

The CLI is copied into <temp repo>/tools/bench/bench so its own repo-root discovery finds the
temp root and its token, with no test-only override.
"""
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from importlib.machinery import SourceFileLoader
from pathlib import Path
from unittest import mock

import bench_host as bh
from tests.test_host_flash import pid_alive

HERE = Path(__file__).resolve().parent
CLI_SRC = HERE.parent / "bench"
ESPFLASH, FFMPEG = str(HERE / "fake_espflash.py"), str(HERE / "fake_ffmpeg.py")
TOKEN = "t0ken"
ELF = "firmware/target/fw.elf"


class CliBase(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(os.path.realpath(tmp.name))
        (self.root / "firmware/target").mkdir(parents=True)
        (self.root / ELF).write_bytes(b"\x7fELF" + b"\0" * 64)
        (self.root / ".bench").mkdir()
        (self.root / ".bench/token").write_text(TOKEN)
        (self.root / "tools/bench").mkdir(parents=True)
        self.cli = self.root / "tools/bench/bench"
        shutil.copy(CLI_SRC, self.cli)
        self.port_file = self.root / "ttyfake"  # health wants the chosen port to exist
        self.port_file.touch()
        self.cands = [str(self.port_file)]
        self.fakelog = self.root / "fake.log"
        env = mock.patch.dict(os.environ, {"FAKE_ESPFLASH_LOG": str(self.fakelog),
                                           "FAKE_FFMPEG_LOG": str(self.fakelog)})
        env.start()
        self.addCleanup(env.stop)
        self.bench = bh.Bench(repo_root=str(self.root), espflash=[sys.executable, ESPFLASH],
                              ffmpeg=[sys.executable, FFMPEG], candidates=lambda: self.cands,
                              flash_timeout=10, snapshot_timeout=3)
        self.bench.espflash_version = "fake"  # the fake has no --version output
        srv = bh.make_server(self.bench, "127.0.0.1", 0, TOKEN)
        self.url = f"http://127.0.0.1:{srv.server_address[1]}"
        threading.Thread(target=srv.serve_forever, args=(0.02,), daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        self.addCleanup(self.assert_no_orphans)  # runs after bench.close (LIFO)
        self.addCleanup(self.bench.close)

    def assert_no_orphans(self):
        if self.fakelog.exists():
            pids = [json.loads(l)["pid"] for l in self.fakelog.read_text().splitlines()]
            self.assertEqual([p for p in pids if pid_alive(p)], [])

    def mode(self, espflash=None, ffmpeg=None):
        if espflash:
            os.environ["FAKE_ESPFLASH_MODE"] = espflash  # restored by the patch.dict in setUp
        if ffmpeg:
            os.environ["FAKE_FFMPEG_MODE"] = ffmpeg

    def run_cli(self, *args, url=None, **env):
        e = {**os.environ, "BENCH_URL": url or self.url, "BENCH_SNAP_BACKOFF": "0.02", **env}
        p = subprocess.run([sys.executable, str(self.cli), *args], capture_output=True, text=True,
                           env=e, timeout=60)
        p.all = p.stdout + p.stderr
        return p


class FlashTest(CliBase):
    def test_default_until_with_double_boot(self):
        p = self.run_cli("flash", ELF)
        self.assertEqual(p.returncode, 0, p.all)
        self.assertIn("--- boot 1 ---", p.stdout)
        self.assertIn("--- boot 2 ---", p.stdout)  # the cut-off first boot is not a failure
        self.assertIn("INFO - alive 0s", p.stdout)
        self.assertIn("--until matched in boot 2", p.stderr)

    def test_explicit_until(self):
        p = self.run_cli("flash", ELF, "--until", "backlight on")
        self.assertEqual(p.returncode, 0, p.all)
        self.assertIn("--until matched", p.stderr)

    def test_until_empty_returns_after_flash(self):
        p = self.run_cli("flash", ELF, "--until", "")
        self.assertEqual(p.returncode, 0, p.all)
        self.assertIn("Flashing has completed!", p.stdout)
        self.assertNotIn("bench: --until", p.stderr)

    def test_absolute_in_repo_path_converted(self):
        p = self.run_cli("flash", str(self.root / ELF))
        self.assertEqual(p.returncode, 0, p.all)

    def test_bad_path_rejected_4(self):
        p = self.run_cli("flash", "../README.md")
        self.assertEqual(p.returncode, 4, p.all)
        self.assertIn("400", p.stderr)

    def test_ansi_crlf_output_clean(self):
        os.environ["FAKE_ESPFLASH_ANSI"] = os.environ["FAKE_ESPFLASH_CRLF"] = "1"
        self.addCleanup(os.environ.pop, "FAKE_ESPFLASH_ANSI", None)
        self.addCleanup(os.environ.pop, "FAKE_ESPFLASH_CRLF", None)
        p = self.run_cli("flash", ELF)
        self.assertEqual(p.returncode, 0, p.all)
        self.assertNotIn("\x1b", p.stdout)

    def test_pre_app_seq_text_not_matched(self):
        # "Connecting..." only appears in espflash's own output, before the completion line.
        p = self.run_cli("flash", ELF, "--until", "Connecting", "--timeout", "1")
        self.assertEqual(p.returncode, 2, p.all)
        self.assertNotIn("--until matched", p.stderr)

    def test_panic_exit_1(self):
        self.mode("panic")
        p = self.run_cli("flash", ELF, "--until", "never-appears")
        self.assertEqual(p.returncode, 1, p.all)
        self.assertIn("--fail-on matched", p.stderr)
        self.assertIn("panicked at", p.stdout)

    def test_fail_on_disabled(self):
        self.mode("panic")
        p = self.run_cli("flash", ELF, "--until", "never-appears", "--fail-on", "", "--timeout", "1")
        self.assertEqual(p.returncode, 2, p.all)

    def test_bootloop_exit_1(self):
        self.mode("bootloop")
        p = self.run_cli("flash", ELF)
        self.assertEqual(p.returncode, 1, p.all)
        self.assertIn("boot loop", p.stderr)

    def test_no_marker_timeout_2(self):
        self.mode("silent")
        t = time.time()
        p = self.run_cli("flash", ELF, "--timeout", "1")
        self.assertEqual(p.returncode, 2, p.all)
        self.assertLess(time.time() - t, 15)

    def test_monitor_dies_3(self):
        self.mode("die")
        p = self.run_cli("flash", ELF, "--until", "never-appears")
        self.assertEqual(p.returncode, 3, p.all)

    def test_flash_fail_5_prints_tail(self):
        self.mode("flash_fail")
        p = self.run_cli("flash", ELF)
        self.assertEqual(p.returncode, 5, p.all)
        self.assertIn("espflash::connection_failed", p.stdout)

    def test_flash_hang_5(self):
        self.mode("flash_hang")
        self.bench.flash_timeout = 1
        p = self.run_cli("flash", ELF)
        self.assertEqual(p.returncode, 5, p.all)

    def test_ambiguous_port_4_lists_candidates(self):
        self.cands = ["/dev/cu.usbmodemA", "/dev/cu.usbmodemB"]
        p = self.run_cli("flash", ELF)
        self.assertEqual(p.returncode, 4, p.all)
        self.assertIn("/dev/cu.usbmodemA", p.stderr)
        self.assertIn("/dev/cu.usbmodemB", p.stderr)

    def test_bad_regex_4(self):
        self.assertEqual(self.run_cli("flash", ELF, "--until", "(").returncode, 4)


class ResetAndLogTest(CliBase):
    def test_reset_after_flash(self):
        self.assertEqual(self.run_cli("flash", ELF).returncode, 0)
        p = self.run_cli("reset")
        self.assertEqual(p.returncode, 0, p.all)
        self.assertIn("alive 0s", p.stdout)

    def test_reset_before_flash_4(self):
        p = self.run_cli("reset")
        self.assertEqual(p.returncode, 4, p.all)
        self.assertIn("409", p.stderr)

    def test_log_dump_and_until(self):
        self.assertEqual(self.run_cli("flash", ELF).returncode, 0)
        dump = self.run_cli("log")
        self.assertEqual(dump.returncode, 0, dump.all)
        self.assertIn("alive 0s", dump.stdout)
        p = self.run_cli("log", "--until", "alive 2s", "--timeout", "10")
        self.assertEqual(p.returncode, 0, p.all)

    def test_log_session_starts_after_app_seq(self):
        self.assertEqual(self.run_cli("flash", ELF, "--until", "alive 1s").returncode, 0)
        p = self.run_cli("log", "--session")
        self.assertEqual(p.returncode, 0, p.all)
        self.assertNotIn("Connecting", p.stdout)
        self.assertIn("--- boot 1 ---", p.stdout)

    def test_log_timeout_2(self):
        self.assertEqual(self.run_cli("flash", ELF).returncode, 0)
        p = self.run_cli("log", "--since", "0", "--until", "never-appears", "--timeout", "1")
        self.assertEqual(p.returncode, 2, p.all)


class SnapAndHealthTest(CliBase):
    def test_snap_writes_jpeg_and_prints_path(self):
        p = self.run_cli("snap")
        self.assertEqual(p.returncode, 0, p.all)
        path = Path(p.stdout.strip())
        self.assertEqual(path.parent, self.root / ".bench/snaps")
        self.assertTrue(path.read_bytes().startswith(b"\xff\xd8"))

    def test_snap_explicit_path_and_warmup(self):
        out = self.root / "x/y.jpg"
        p = self.run_cli("snap", str(out), "--warmup", "3", "--delay", "0.01")
        self.assertEqual(p.returncode, 0, p.all)
        self.assertEqual(p.stdout.strip(), str(out))
        self.assertTrue(out.read_bytes().startswith(b"\xff\xd8"))
        argv = json.loads(self.fakelog.read_text().splitlines()[-1])["argv"]
        self.assertIn("select=gte(n\\,3)", argv)

    def test_snap_retries_while_camera_busy(self):
        self.bench.camera_lock.acquire()
        threading.Timer(0.3, self.bench.camera_lock.release).start()
        p = self.run_cli("snap", BENCH_SNAP_BACKOFF="0.2")
        self.assertEqual(p.returncode, 0, p.all)

    def test_snap_gives_up_when_always_busy(self):
        self.bench.camera_lock.acquire()
        self.addCleanup(self.bench.camera_lock.release)
        p = self.run_cli("snap")
        self.assertEqual(p.returncode, 4, p.all)
        self.assertIn("409", p.stderr)

    def test_snap_failure_4(self):
        self.mode(ffmpeg="fail")
        p = self.run_cli("snap")
        self.assertEqual(p.returncode, 4, p.all)
        self.assertIn("503", p.stderr)

    def test_health_ok(self):
        p = self.run_cli("health")
        self.assertEqual(p.returncode, 0, p.all)
        self.assertTrue(json.loads(p.stdout)["ok"])

    def test_health_ambiguous_4_prints_candidates(self):
        self.cands = ["/dev/cu.usbmodemA", "/dev/cu.usbmodemB"]
        p = self.run_cli("health")
        self.assertEqual(p.returncode, 4, p.all)
        self.assertIn("/dev/cu.usbmodemB", p.stderr)

    def test_health_port_absent_4(self):
        self.cands = ["/dev/cu.usbmodem-not-there"]
        self.assertEqual(self.run_cli("health").returncode, 4)


class TransportTest(CliBase):
    def test_wrong_token_4(self):
        (self.root / ".bench/token").write_text("wrong")
        p = self.run_cli("health")
        self.assertEqual(p.returncode, 4, p.all)
        self.assertIn("401", p.stderr)

    def test_missing_token_4(self):
        (self.root / ".bench/token").unlink()
        self.assertEqual(self.run_cli("health").returncode, 4)

    def test_unreachable_4(self):
        self.assertEqual(self.run_cli("health", url="http://127.0.0.1:1").returncode, 4)


class UsageAndShapeTest(CliBase):
    def test_usage_errors_exit_4(self):
        for args in ([], ["bogus"], ["flash"], ["log", "--since", "x"], ["flash", ELF, "--timeout", "soon"],
                     ["log", "--since", "1", "--session"]):
            p = self.run_cli(*args)
            self.assertEqual(p.returncode, 4, (args, p.all))
            self.assertIn("usage:", p.stderr)

    def test_help_exits_0(self):
        self.assertEqual(self.run_cli("log", "--help").returncode, 0)

    def test_malformed_200_exits_4_without_traceback(self):
        class Stub(BaseHTTPRequestHandler):
            def do_GET(self):
                self.reply(b'{"weird": 1}')

            do_POST = do_GET

            def reply(self, body):
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *a):
                pass

        srv = ThreadingHTTPServer(("127.0.0.1", 0), Stub)
        threading.Thread(target=srv.serve_forever, args=(0.02,), daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        url = f"http://127.0.0.1:{srv.server_address[1]}"
        for args in (["flash", ELF], ["reset"], ["log", "--until", "x", "--timeout", "1"], ["snap"]):
            p = self.run_cli(*args, url=url)
            self.assertEqual(p.returncode, 4, (args, p.all))
            self.assertNotIn("Traceback", p.stderr)

    def test_snap_non_jpeg_exits_4(self):
        self.mode(ffmpeg="notjpeg")  # host rejects it with 503 already; the stub covers a 200
        self.assertEqual(self.run_cli("snap").returncode, 4)

    def test_flash_request_timeout_exceeds_host_flash_timeout(self):
        cli = SourceFileLoader("bench_cli", str(CLI_SRC)).load_module()
        self.assertGreater(cli.FLASH_REQUEST_TIMEOUT, bh.FLASH_TIMEOUT)


class ShebangTest(unittest.TestCase):
    def test_header_and_mode(self):
        head = CLI_SRC.read_text().splitlines()[:5]
        self.assertEqual(head, ["#!/usr/bin/env -S uv run --script", "# /// script",
                                '# requires-python = ">=3.11"', "# dependencies = []", "# ///"])
        self.assertTrue(CLI_SRC.stat().st_mode & stat.S_IXUSR)

    @unittest.skipUnless(shutil.which("uv"), "uv not on PATH")
    def test_help_via_shebang(self):
        p = subprocess.run([str(CLI_SRC), "--help"], capture_output=True, text=True, timeout=120)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("flash", p.stdout)


if __name__ == "__main__":
    unittest.main()

import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import bench_host as bh

PREFIX = "firmware/target/"


class StageElfTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = os.path.realpath(self._tmp.name)
        self.dest = os.path.join(self.root, "dest")
        os.mkdir(self.dest)
        self.tdir = os.path.join(self.root, "firmware", "target")
        os.makedirs(self.tdir)
        self.elf = os.path.join(self.tdir, "fw.elf")
        Path(self.elf).write_bytes(b"\x7fELF" + b"\0" * 100)

    def stage(self, elf):
        return bh.stage_elf(self.root, elf, PREFIX, self.dest)

    def reject(self, elf):
        with self.assertRaises(bh.BenchError) as cm:
            self.stage(elf)
        self.assertEqual(cm.exception.status, 400)

    def test_accepts_and_copies(self):
        copy = self.stage("firmware/target/fw.elf")
        self.assertEqual(os.path.dirname(copy), self.dest)
        self.assertEqual(Path(copy).read_bytes(), Path(self.elf).read_bytes())

    def test_rejects_traversal(self):
        Path(self.root, "README.md").write_bytes(b"\x7fELF")
        self.reject("../README.md")
        self.reject("firmware/target/../../README.md")

    def test_rejects_absolute(self):
        self.reject(self.elf)

    def test_rejects_symlink_escape(self):
        Path(self.root, "outside.elf").write_bytes(b"\x7fELF")
        os.symlink(os.path.join(self.root, "outside.elf"), os.path.join(self.tdir, "link.elf"))
        self.reject("firmware/target/link.elf")

    def test_accepts_symlink_inside_prefix(self):
        os.symlink(self.elf, os.path.join(self.tdir, "alias.elf"))
        self.stage("firmware/target/alias.elf")

    def test_rejects_non_elf(self):
        Path(self.tdir, "x.txt").write_bytes(b"hello")
        self.reject("firmware/target/x.txt")

    def test_rejects_oversize(self):
        Path(self.tdir, "big.elf").write_bytes(b"\x7fELF" + b"\0" * 100)
        old = bh.MAX_ELF
        bh.MAX_ELF = 50
        self.addCleanup(setattr, bh, "MAX_ELF", old)
        self.reject("firmware/target/big.elf")

    def test_rejects_directory_nul_empty_missing(self):
        os.mkdir(os.path.join(self.tdir, "dir"))
        self.reject("firmware/target/dir")
        self.reject("firmware/target/fw.elf\0.txt")
        self.reject("")
        self.reject("firmware/target/missing.elf")

    def test_rejects_fifo(self):
        os.mkfifo(os.path.join(self.tdir, "fifo"))
        self.reject("firmware/target/fifo")

    def test_rejects_prefix_sibling(self):
        os.makedirs(os.path.join(self.root, "firmware", "target2"))
        Path(self.root, "firmware", "target2", "a.elf").write_bytes(b"\x7fELF")
        self.reject("firmware/target2/a.elf")


class ArgvTest(unittest.TestCase):
    def test_espflash_exact(self):
        self.assertEqual(
            bh.espflash_argv("espflash", "/dev/cu.usbmodem1", "/tmp/x.elf"),
            ["espflash", "flash", "--chip", "esp32s3", "--port", "/dev/cu.usbmodem1",
             "--monitor", "--non-interactive", "/tmp/x.elf"],
        )

    def test_ffmpeg_default_has_no_framerate(self):
        argv = bh.ffmpeg_argv("ffmpeg", "1", "1280x720", 20)
        self.assertNotIn("-framerate", argv)
        self.assertEqual(
            argv,
            ["ffmpeg", "-f", "avfoundation", "-video_size", "1280x720", "-i", "1:none",
             "-vf", "select=gte(n\\,20)", "-frames:v", "1", "-f", "image2pipe", "-c:v", "mjpeg", "pipe:1"],
        )

    def test_ffmpeg_framerate_when_configured(self):
        argv = bh.ffmpeg_argv("ffmpeg", "Cam", "640x480", 5, framerate="30")
        self.assertEqual(argv[argv.index("-framerate") + 1], "30")
        self.assertIn("Cam:none", argv)


class ResolvePortTest(unittest.TestCase):
    def test_fixed_as_is(self):
        self.assertEqual(bh.resolve_port("/dev/x", []), "/dev/x")
        self.assertEqual(bh.resolve_port("/dev/x", ["/dev/a", "/dev/b"]), "/dev/x")

    def test_auto_single(self):
        self.assertEqual(bh.resolve_port(None, ["/dev/a"]), "/dev/a")

    def test_auto_zero_or_many_names_candidates(self):
        for cands in ([], ["/dev/a", "/dev/b"]):
            with self.assertRaises(bh.PortError) as cm:
                bh.resolve_port(None, cands)
            self.assertEqual(cm.exception.status, 503)
            self.assertEqual(cm.exception.extra["candidates"], cands)
            for c in cands:
                self.assertIn(c, str(cm.exception))

    def test_glob_wrapper(self):
        with tempfile.TemporaryDirectory() as d:
            Path(d, "cu.usbmodem2").touch()
            Path(d, "cu.usbmodem1").touch()
            Path(d, "other").touch()
            self.assertEqual(bh.list_candidates(os.path.join(d, "cu.usbmodem*")),
                             [os.path.join(d, "cu.usbmodem1"), os.path.join(d, "cu.usbmodem2")])


class ListenTest(unittest.TestCase):
    def test_loopback_ok(self):
        self.assertEqual(bh.parse_listen("127.0.0.1:7777"), ("127.0.0.1", 7777))
        self.assertEqual(bh.parse_listen("[::1]:0"), ("::1", 0))

    def test_non_loopback_refused(self):
        for v in ("0.0.0.0:7777", "192.168.1.5:1", "example.com:80", "localhost:80", "7777", ":80",
                  "127.0.0.1:x", "127.0.0.1:70000", "[::]:1"):
            with self.assertRaises(ValueError, msg=v):
                bh.parse_listen(v)

    def test_main_refuses_before_touching_anything(self):
        self.assertEqual(bh.main(["--listen", "0.0.0.0:7777"]), 2)


class TokenTest(unittest.TestCase):
    def test_token_file_mode_and_regeneration(self):
        with tempfile.TemporaryDirectory() as d:
            bench_dir = os.path.join(d, ".bench")
            t1 = bh.write_token(bench_dir)
            path = os.path.join(bench_dir, "token")
            self.assertEqual(os.stat(path).st_mode & 0o777, 0o600)
            self.assertEqual(Path(path).read_text(), t1)
            os.chmod(path, 0o644)
            t2 = bh.write_token(bench_dir)
            self.assertNotEqual(t1, t2)
            self.assertEqual(os.stat(path).st_mode & 0o777, 0o600)
            self.assertGreaterEqual(len(t2), 43)


class BindFailureTest(unittest.TestCase):
    def test_port_in_use_keeps_existing_token(self):
        holder = bh.make_server(bh.Bench(), "127.0.0.1", 0)
        self.addCleanup(holder.server_close)
        port = holder.server_address[1]
        with tempfile.TemporaryDirectory() as d:
            token = Path(d, "token")
            token.write_text("running-instance")
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                rc = bh.main(["--listen", f"127.0.0.1:{port}", "--port", "/dev/x",
                              "--espflash", sys.executable, "--ffmpeg", sys.executable],
                             bench_dir=Path(d))
            self.assertEqual(rc, 2)
            self.assertEqual(token.read_text(), "running-instance")
            self.assertIn(f"127.0.0.1:{port}", err.getvalue())
            self.assertNotIn("Traceback", err.getvalue())


class ScriptTest(unittest.TestCase):
    script = Path(bh.__file__)

    def test_uv_header_and_executable(self):
        self.assertTrue(os.access(self.script, os.X_OK))
        self.assertEqual(
            self.script.read_text().splitlines()[:5],
            ["#!/usr/bin/env -S uv run --script", "# /// script", '# requires-python = ">=3.11"',
             "# dependencies = []", "# ///"],
        )

    def run_script(self, *args):
        return subprocess.run([sys.executable, str(self.script), *args], capture_output=True,
                              text=True, timeout=20, stdin=subprocess.DEVNULL)

    def test_help_exits_0(self):
        self.assertEqual(self.run_script("--help").returncode, 0)

    def test_non_loopback_listen_exits_2(self):
        r = self.run_script("--listen", "0.0.0.0:7777")
        self.assertEqual(r.returncode, 2)
        self.assertIn("loopback", r.stderr)


if __name__ == "__main__":
    unittest.main()

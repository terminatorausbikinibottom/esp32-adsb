import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import bench_host as bh

PREFIX = "firmware/target/"
FAKE_FFMPEG = str(Path(__file__).resolve().parent / "fake_ffmpeg.py")
REAL_LIST = """\
[AVFoundation indev @ 0x1] AVFoundation video devices:
[AVFoundation indev @ 0x1] [0] MacBook Pro-Kamera
[AVFoundation indev @ 0x1] [1] HD Pro Webcam C920
[AVFoundation indev @ 0x1] [2] Kamera von \u201eSergej\u2019s iPhone (5)\u201c
[AVFoundation indev @ 0x1] [3] MacBook Pro-Schreibtischansicht-Kamera
[AVFoundation indev @ 0x1] [4] Schreibtischansicht-Kamera von \u201eSergej\u2019s iPhone (5)\u201c
[AVFoundation indev @ 0x1] [5] Capture screen 0
[AVFoundation indev @ 0x1] [6] Capture screen 1
[AVFoundation indev @ 0x1] AVFoundation audio devices:
[AVFoundation indev @ 0x1] [0] Mikrofon von \u201eSergej\u2019s iPhone (5)\u201c
[AVFoundation indev @ 0x1] [1] MacBook Pro-Mikrofon
"""


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

    def test_ffmpeg_default_has_framerate_30(self):
        argv = bh.ffmpeg_argv("ffmpeg", "1", "1280x720", 20, framerate=bh.parse_args([]).framerate)
        self.assertEqual(
            argv,
            ["ffmpeg", "-f", "avfoundation", "-framerate", "30", "-video_size", "1280x720", "-i", "1:none",
             "-vf", "select=gte(n\\,20)", "-frames:v", "1", "-f", "image2pipe", "-c:v", "mjpeg", "pipe:1"],
        )

    def test_ffmpeg_framerate_disabled(self):
        for off in (None, "", "0"):
            self.assertNotIn("-framerate", bh.ffmpeg_argv("ffmpeg", "1", "1280x720", 20, framerate=off))
        self.assertNotIn("-framerate", bh.ffmpeg_argv("ffmpeg", "1", "1280x720", 20))

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


class TokenSymlinkTest(unittest.TestCase):
    def test_planted_symlinks_are_not_followed(self):
        with tempfile.TemporaryDirectory() as d:
            victim = Path(d, "victim")
            victim.write_text("keep")
            bench_dir = Path(d, ".bench")
            bench_dir.mkdir()
            (bench_dir / "token").symlink_to(victim)
            token = bh.write_token(bench_dir)
            self.assertEqual(victim.read_text(), "keep")
            self.assertFalse((bench_dir / "token").is_symlink())
            self.assertEqual((bench_dir / "token").read_text(), token)
            self.assertEqual(os.listdir(bench_dir), ["token"])  # no temp file left behind
            linked = Path(d, "linked")
            linked.symlink_to(bench_dir)
            with self.assertRaises(OSError):
                bh.write_token(linked)


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
                              "--espflash", sys.executable, "--ffmpeg", FAKE_FFMPEG],
                             bench_dir=Path(d))
            self.assertEqual(rc, 2)
            self.assertEqual(token.read_text(), "running-instance")
            self.assertIn(f"127.0.0.1:{port}", err.getvalue())
            self.assertNotIn("Traceback", err.getvalue())


class CameraTest(unittest.TestCase):
    def test_parse_real_output_video_section_only(self):
        d = bh.parse_video_devices(REAL_LIST)
        self.assertEqual(sorted(d), [0, 1, 2, 3, 4, 5, 6])
        self.assertEqual(d[1], "HD Pro Webcam C920")
        self.assertEqual(d[2], "Kamera von \u201eSergej\u2019s iPhone (5)\u201c")
        self.assertNotIn("MacBook Pro-Mikrofon", d.values())

    def test_parse_keeps_name_whitespace(self):
        d = bh.parse_video_devices("[AVFoundation indev @ 0x1] AVFoundation video devices:\n"
                                   "[AVFoundation indev @ 0x1] [0]  Cam \n")
        self.assertEqual(d, {0: " Cam "})

    def test_parse_edge_cases(self):
        self.assertEqual(bh.parse_video_devices(""), {})
        self.assertEqual(bh.parse_video_devices("[AVFoundation indev @ 0x1] AVFoundation video devices:\n"), {})
        self.assertEqual(bh.parse_video_devices(
            "[AVFoundation indev @ 0x1] AVFoundation audio devices:\n[AVFoundation indev @ 0x1] [0] Mic\n"), {})

    def run_main(self, camera, devices=None, extra=()):
        env = {"FAKE_FFMPEG_DEVICES": devices} if devices else {}
        err = io.StringIO()
        with tempfile.TemporaryDirectory() as d, mock.patch.dict(os.environ, env), \
                mock.patch.object(bh, "make_server", side_effect=OSError(98, "stop here")), \
                contextlib.redirect_stderr(err):
            rc = bh.main(["--listen", "127.0.0.1:0", "--port", "/dev/x", "--espflash", sys.executable,
                          "--ffmpeg", FAKE_FFMPEG, *(["--camera", camera] if camera else []), *extra],
                         bench_dir=Path(d))
        return rc, err.getvalue()

    def test_default_camera_is_c920(self):
        rc, err = self.run_main(None)
        self.assertIn("stop here", err)  # got past camera check to the bind
        self.assertNotIn("warning", err)

    def test_name_present_starts(self):
        rc, err = self.run_main("MacBook Pro-Kamera")
        self.assertIn("stop here", err)

    def test_name_absent_lists_devices(self):
        rc, err = self.run_main("Nope")
        self.assertEqual(rc, 2)
        self.assertIn("'Nope' not found", err)
        self.assertIn("HD Pro Webcam C920", err)

    def test_index_refused(self):
        for cam in ("1", "9", "\uff11"):  # ASCII and fullwidth digits
            rc, err = self.run_main(cam)
            self.assertEqual(rc, 2, cam)
            self.assertIn("indices are not stable; use a device name", err)
            self.assertIn("HD Pro Webcam C920", err)

    def test_screen_capture_refused_but_lookalike_accepted(self):
        devs = "Cam|Capture screen 0|Capture screenshot cam"
        rc, err = self.run_main("Capture screen 0", devices=devs)
        self.assertEqual(rc, 2)
        self.assertIn("screen capture", err)
        rc, err = self.run_main("Capture screenshot cam", devices=devs)
        self.assertIn("stop here", err)

    def test_listing_failure_refuses(self):
        with mock.patch.object(bh, "list_video_devices", side_effect=ValueError("cannot list cameras")):
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                self.assertEqual(bh.main(["--listen", "127.0.0.1:0", "--port", "/dev/x",
                                          "--espflash", sys.executable, "--ffmpeg", sys.executable]), 2)
            self.assertIn("cannot list cameras", err.getvalue())

    def test_listing_unparseable_refuses(self):
        with self.assertRaises(ValueError):
            bh.list_video_devices(sys.executable)


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



class SaveTermiosTest(unittest.TestCase):
    def test_no_tty_is_noop(self):
        with mock.patch.object(bh.sys, "stdin", io.StringIO()):
            bh._save_termios()()  # must not raise

    def test_restores_saved_attrs(self):
        import termios
        m, s = os.openpty()
        self.addCleanup(os.close, m)
        self.addCleanup(os.close, s)
        before = termios.tcgetattr(s)
        stdin = os.fdopen(os.dup(s))
        self.addCleanup(stdin.close)
        with mock.patch.object(bh.sys, "stdin", stdin):
            restore = bh._save_termios()
        raw = termios.tcgetattr(s)
        raw[1] &= ~termios.OPOST
        termios.tcsetattr(s, termios.TCSANOW, raw)
        restore()
        self.assertEqual(termios.tcgetattr(s), before)

if __name__ == "__main__":
    unittest.main()

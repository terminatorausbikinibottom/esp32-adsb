#!/usr/bin/env python3
"""Test imitation of `espflash flash --monitor --non-interactive` (S3). Not a uv script.

Run as [sys.executable, this_file, ...espflash args] or directly (python3 shebang).
Env: FAKE_ESPFLASH_MODE  ok | flash_fail | flash_hang | panic | die | silent | bootloop | flood | partial
     FAKE_ESPFLASH_INTERVAL  seconds between `alive N` lines (default 0.05)
     FAKE_ESPFLASH_ANSI=1 / FAKE_ESPFLASH_CRLF=1  colour codes / \\r\\n line endings
     FAKE_ESPFLASH_LOG  file that gets one JSON line {pid, argv} per invocation
"""
import json
import os
import sys
import time

MODE = os.environ.get("FAKE_ESPFLASH_MODE", "ok")
INTERVAL = float(os.environ.get("FAKE_ESPFLASH_INTERVAL", "0.05"))
EOL = "\r\n" if os.environ.get("FAKE_ESPFLASH_CRLF") else "\n"
ANSI = bool(os.environ.get("FAKE_ESPFLASH_ANSI"))


def out(text):
    if ANSI:
        text = f"\x1b[32m{text}\x1b[0m"
    sys.stdout.write(text + EOL)
    sys.stdout.flush()


def banner():
    out("ESP-ROM:esp32s3-20210327")
    out("rst:0x15 (USB_UART_CHIP_RESET),boot:0x8 (SPI_FAST_FLASH_BOOT)")
    out("I (27) boot: ESP-IDF v5.1 2nd stage bootloader")


def first_boot():
    banner()
    out("INFO - i2c scan done")
    out("INFO - psram: fb ptr=0x3c000000")  # cut off by the second reset


def second_boot():
    banner()
    out("INFO - Embassy initialized!")
    out("INFO - ch422g: backlight on")
    out("INFO - alive 0s")


def forever(n=1):
    while True:
        time.sleep(INTERVAL)
        out(f"INFO - alive {n}s")
        n += 1


def _has_tty():
    try:
        open("/dev/tty").close()
        return True
    except OSError:
        return False


def main():
    argv = sys.argv[1:]
    log = os.environ.get("FAKE_ESPFLASH_LOG")
    if log and "flash" in argv:  # not the --version probe
        with open(log, "a") as f:
            f.write(json.dumps({"pid": os.getpid(), "argv": argv, "sid": os.getsid(0), "pgid": os.getpgid(0), "tty": _has_tty()}) + "\n")
    if not {"flash", "--monitor", "--non-interactive"} <= set(argv):
        print("fake_espflash: argv lacks flash/--monitor/--non-interactive", file=sys.stderr)
        return 2
    out("INFO - Connecting...")
    out("INFO - Writing to flash 50%")
    if MODE == "flash_fail":
        out("Error: espflash::connection_failed")
        return 1
    if MODE == "flash_hang":
        time.sleep(3600)
    out("INFO - Flashing has completed!")
    if MODE == "bootloop":
        for _ in range(6):
            first_boot()
            time.sleep(INTERVAL)
        time.sleep(3600)
    if MODE == "flood":  # 1 MiB without a newline
        sys.stdout.write("x" * (1 << 20))
        sys.stdout.flush()
        time.sleep(3600)
    if MODE == "partial":  # a last line that never gets its newline
        sys.stdout.write("Guru Meditation: no newline")
        sys.stdout.flush()
        time.sleep(3600)
    first_boot()
    if MODE == "silent":
        banner()
        out("INFO - Embassy initialized!")
        time.sleep(3600)
    second_boot()
    if MODE == "panic":
        out("INFO - panicked at src/main.rs:42:5:")
        out("Backtrace: 0x42001234:0x3fc90000")
        time.sleep(3600)
    if MODE == "die":
        time.sleep(0.1)
        return 1
    forever()


if __name__ == "__main__":
    sys.exit(main())

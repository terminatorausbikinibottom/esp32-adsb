# esp-hal `Dpi::send` patch

**Status (2026-10-29):** not in use. The firmware builds against stock esp-hal 1.2.2 from crates.io.
The vendored copy (`firmware/vendor/esp-hal`) was removed; it is in the git history before this
change. To use the patch again: copy esp-hal 1.2.2 from `~/.cargo/registry/src/*/esp-hal-1.2.2` to
`firmware/vendor/esp-hal` (keep its license files), apply the diff below, and add
`[patch.crates-io] esp-hal = { path = "vendor/esp-hal" }` to `firmware/Cargo.toml`.

## Patch

Upstream: `esp-hal` 1.2.2 from crates.io (checksum `c9e3dfd511fed638fe41fdb4595f8c7f0cbb488d0f0cf2bbc063d76d805057cf`),
used through `[patch.crates-io]` in `firmware/Cargo.toml`. Only differences from the published crate:
`Cargo.lock` and `.cargo-ok` were not copied, and this file was added. The only source change is in
`src/lcd_cam/lcd/dpi.rs`, `Dpi::send`:

```diff
@@ -480,15 +480,6 @@
         next_frame_en: bool,
         mut buf: TX,
     ) -> Result<DpiTransfer<'d, TX, Dm>, (DmaError, Self, TX)> {
-        let result = unsafe {
-            self.tx_channel
-                .prepare_transfer(DmaPeripheral::LCD_CAM, &mut buf)
-        }
-        .and_then(|_| self.tx_channel.start_transfer());
-        if let Err(err) = result {
-            return Err((err, self, buf));
-        }
-
         // Reset LCD control unit and Async Tx FIFO
@@ -503,6 +494,18 @@
             w.lcd_next_frame_en().bit(next_frame_en)
         });

+        // Start the DMA only after the resets above (like ESP-IDF's esp_lcd_rgb_panel): resetting the
+        // Async Tx FIFO after the DMA started drops a start-up dependent number of bytes, which
+        // shifts the whole image horizontally.
+        let result = unsafe {
+            self.tx_channel
+                .prepare_transfer(DmaPeripheral::LCD_CAM, &mut buf)
+        }
+        .and_then(|_| self.tx_channel.start_transfer());
+        if let Err(err) = result {
+            return Err((err, self, buf));
+        }
+
         // Start the transfer.
```

## Why

Upstream starts the GDMA channel, then resets the LCD and its async TX FIFO, then sets `lcd_start`. The
reset discards whatever the DMA already pushed into the FIFO, so how many bytes of the framebuffer
are lost before the first pixel depends on start-up timing (observed: constant horizontal image
offset per build, different between builds). ESP-IDF's `lcd_rgb_panel_start_transmission` resets
first, then starts GDMA, then starts the LCD; this patch uses that order.

## Upstream

Issue: to be filed in esp-rs/esp-hal (text in the step 1.3 fix loop 2 report). Add the link here once filed.

## Why stock esp-hal is enough

The default per-frame restart (`firmware/src/restart.rs`) resets the LCD and FIFO before every start, so
the `Dpi::send` ordering does not matter for the default build. Only `Dpi::send`'s first frame, and the
`no-resync`/`restart-vsync` variants (no reset before start), depend on the patch: with stock esp-hal
they can show a horizontal image shift. Restore the patch (see the status above) before using them.

**2026-10-23 (radar-ui RD.0):** stock esp-hal 1.2.2 passed on the board (39 fps, no shift), so the patch was
switched off in `firmware/Cargo.toml`. **2026-10-29:** the vendored copy was removed.

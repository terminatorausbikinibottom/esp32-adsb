# Vendored esp-hal

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

Drop this vendored copy and the `[patch.crates-io]` entry once a released esp-hal contains the fix.

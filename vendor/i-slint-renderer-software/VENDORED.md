# Vendored i-slint-renderer-software

Upstream: `i-slint-renderer-software` 1.18.1 from crates.io, used through `[patch.crates-io]` in
`sim/Cargo.toml`, `adsb-app/Cargo.toml` and `firmware/Cargo.toml`. Only differences from the
published crate: `Cargo.lock` and `.cargo-ok` were not copied, and this file was added. The only
source change is in `scene.rs`, `Scene::next_line` (path that skips lines):

```diff
@@ -113 +113 @@
-                if item.pos.y_length() + item.size.height_length() < self.current_line {
+                if item.pos.y_length() + item.size.height_length() <= self.current_line {
```

Why: when the dirty region has a gap between rectangles (for example the clock at the top and a
blip lower down), an item that ended on the line the renderer lands on stayed in the active list.
Debug builds panic at `lib.rs` (`scene.current_line < span.pos.y + span.size.height`). Release
builds draw wrong frames instead: 10 of 880 per-second frames of the busy, edge and overflight
scenarios differed from the fixed renderer (radar-ui RD.5). The sweep in `sim/tests/snapshots.rs`
fails without this patch in a debug build.

Drop it: when a Slint release contains the fix, remove the three `[patch.crates-io]`
`i-slint-renderer-software` lines, bump the pinned `slint` version, and delete this directory.

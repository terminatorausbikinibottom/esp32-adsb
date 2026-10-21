use alloc::rc::Rc;
use core::ops::Range;
use slint::platform::software_renderer::Rgb565Pixel;
use slint::platform::software_renderer::{LineBufferProvider, MinimalSoftwareWindow};

pub const WIDTH: usize = 800;
pub const HEIGHT: usize = 480;

struct Lines<'a, F> {
    buf: &'a mut [Rgb565Pixel],
    sink: F,
}

impl<F: FnMut(usize, Range<usize>, &[Rgb565Pixel])> LineBufferProvider for Lines<'_, F> {
    type TargetPixel = Rgb565Pixel;

    fn process_line(
        &mut self,
        line: usize,
        range: Range<usize>,
        render_fn: impl FnOnce(&mut [Rgb565Pixel]),
    ) {
        render_fn(&mut self.buf[range.clone()]);
        (self.sink)(line, range.clone(), &self.buf[range]);
    }
}

/// Render the dirty region of `window` one line at a time into `line_buf` (at least [`WIDTH`]
/// pixels) and hand each finished span to `sink(y, x_range, pixels)`. The sink must keep a
/// persistent framebuffer: only changed spans are delivered. Returns whether anything was drawn.
pub fn render_by_line(
    window: &Rc<MinimalSoftwareWindow>,
    line_buf: &mut [Rgb565Pixel],
    sink: impl FnMut(usize, Range<usize>, &[Rgb565Pixel]),
) -> bool {
    assert!(line_buf.len() >= WIDTH);
    window.draw_if_needed(|r| {
        r.render_by_line(Lines {
            buf: line_buf,
            sink,
        });
    })
}

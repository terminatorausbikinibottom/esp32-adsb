//! Browser glue: rAF loop, canvas blit, pointer events and the control strip from `index.html`.
//! Scenario mode only; live/replay (S.5/S.6) would add input sources next to `kind`.
use crate::Sim;
use adsb_app::{HEIGHT, PointerEvent, PointerPhase, WIDTH};
use adsb_core::scenario::Kind;
use core::time::Duration;
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{Clamped, JsCast, closure::Closure, prelude::*};
use web_sys::{
    CanvasRenderingContext2d, Document, HtmlCanvasElement, HtmlInputElement, HtmlSelectElement,
    ImageData, MouseEvent,
};

const SEED: u64 = 1;
const MAX_DT_MS: f64 = 1000.0;
type Frame = Closure<dyn FnMut(f64)>;

struct State {
    sim: Sim,
    kind: Kind,
    /// Scenario time in ms; advances by wall dt x speed.
    virt_ms: f64,
    /// Whole scenario second last fed to the sim (the UI ticks at 1 Hz, like the device).
    ticked_s: u64,
    /// Length of the current scenario (cached; building a `Scenario` is not free).
    duration_ms: u64,
    last_frame: f64,
    pressed: bool,
    speed: f64,
    paused: bool,
    ctx: CanvasRenderingContext2d,
    canvas: HtmlCanvasElement,
    out: web_sys::HtmlElement,
}

fn perf_ms() -> f64 {
    web_sys::window().unwrap().performance().unwrap().now()
}

fn clock() -> Duration {
    Duration::from_secs_f64(perf_ms() / 1000.0)
}

fn by_id<T: JsCast>(doc: &Document, id: &str) -> T {
    doc.get_element_by_id(id)
        .unwrap_or_else(|| panic!("missing #{id}"))
        .dyn_into()
        .unwrap_or_else(|_| panic!("#{id} has the wrong type"))
}

fn kind_of(name: &str) -> Kind {
    match name {
        "busy" => Kind::Busy,
        "edge" => Kind::Edge,
        _ => Kind::Overflight,
    }
}

impl State {
    fn blit(&self) {
        let rgba = self.sim.rgba();
        let img = ImageData::new_with_u8_clamped_array_and_sh(
            Clamped(&rgba),
            WIDTH as u32,
            HEIGHT as u32,
        )
        .unwrap();
        self.ctx.put_image_data(&img, 0.0, 0.0).unwrap();
    }

    fn show(&self) {
        let t = self.ticked_s;
        let text = format!(
            "t = {t} s ({})",
            if self.paused { "paused" } else { "running" }
        );
        self.out.set_text_content(Some(&text));
    }

    fn restart(&mut self, kind: Kind, at_ms: u64) {
        self.kind = kind;
        self.duration_ms = adsb_core::scenario::Scenario::new(kind, SEED).duration_ms();
        self.sim.seek(kind, SEED, at_ms);
        self.virt_ms = at_ms as f64;
        self.ticked_s = at_ms / 1000;
        self.blit();
        self.show();
    }

    fn frame(&mut self, now: f64) {
        // A hidden tab stops rAF; don't let the first frame back jump the scenario by minutes.
        let dt = (now - self.last_frame).clamp(0.0, MAX_DT_MS);
        self.last_frame = now;
        let mut changed = self.sim.render();
        if !self.paused {
            self.virt_ms += dt * self.speed;
            if self.virt_ms >= self.duration_ms as f64 {
                self.restart(self.kind, 0);
                return;
            }
            let s = (self.virt_ms / 1000.0) as u64;
            if s > self.ticked_s {
                self.ticked_s = s;
                self.sim.advance_to(s * 1000);
                changed = true;
                self.show();
            }
        }
        if changed {
            self.blit();
        }
    }

    fn pointer(&mut self, ev: &MouseEvent, phase: PointerPhase) {
        match phase {
            PointerPhase::Down => self.pressed = true,
            // Slint has no hover state today: moves only matter while dragging.
            PointerPhase::Move if !self.pressed => return,
            PointerPhase::Up if !self.pressed => return,
            PointerPhase::Up => self.pressed = false,
            PointerPhase::Move => {}
        }
        // The canvas is CSS-scaled: map client pixels back to 800x480.
        let r = self.canvas.get_bounding_client_rect();
        let x = (ev.client_x() as f64 - r.left()) * WIDTH as f64 / r.width();
        let y = (ev.client_y() as f64 - r.top()) * HEIGHT as f64 / r.height();
        let changed = self.sim.pointer(PointerEvent {
            x: x as f32,
            y: y as f32,
            phase,
        });
        if changed {
            self.blit();
        }
    }

    fn dump(&self) {
        let msg = format!(
            "frame hash (FNV-1a 64, RGB565 LE): {:016x}  scenario={:?} t={} s",
            self.sim.frame_hash(),
            self.kind,
            self.ticked_s
        );
        web_sys::console::log_1(&msg.clone().into());
        by_id::<web_sys::HtmlElement>(&web_sys::window().unwrap().document().unwrap(), "hash")
            .set_text_content(Some(&msg));
    }
}

fn on<E: JsCast + 'static>(
    target: &web_sys::EventTarget,
    event: &str,
    st: &Rc<RefCell<State>>,
    mut f: impl FnMut(&mut State, &E) + 'static,
) {
    let st = st.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        f(&mut st.borrow_mut(), e.unchecked_ref());
    });
    target
        .add_event_listener_with_callback(event, cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget();
}

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
    let win = web_sys::window().unwrap();
    let doc = win.document().unwrap();
    let canvas: HtmlCanvasElement = by_id(&doc, "screen");
    canvas.set_width(WIDTH as u32);
    canvas.set_height(HEIGHT as u32);
    let ctx = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    let st = Rc::new(RefCell::new(State {
        sim: Sim::with_clock(Kind::Overflight, SEED, clock),
        kind: Kind::Overflight,
        virt_ms: 0.0,
        ticked_s: 0,
        duration_ms: 0,
        last_frame: perf_ms(),
        pressed: false,
        speed: 1.0,
        paused: false,
        ctx,
        canvas: canvas.clone(),
        out: by_id(&doc, "time"),
    }));
    st.borrow_mut().restart(Kind::Overflight, 0);

    let c: &web_sys::EventTarget = &canvas;
    on::<MouseEvent>(c, "mousedown", &st, |s, e| s.pointer(e, PointerPhase::Down));
    // Window-level so a release outside the canvas still ends the press.
    on::<MouseEvent>(&win, "mouseup", &st, |s, e| s.pointer(e, PointerPhase::Up));
    on::<MouseEvent>(c, "mousemove", &st, |s, e| s.pointer(e, PointerPhase::Move));

    let scenario: HtmlSelectElement = by_id(&doc, "scenario");
    on::<web_sys::Event>(&scenario.clone(), "change", &st, move |s, _| {
        s.restart(kind_of(&scenario.value()), 0)
    });
    let speed: HtmlSelectElement = by_id(&doc, "speed");
    on::<web_sys::Event>(&speed.clone(), "change", &st, move |s, _| {
        s.speed = speed.value().parse().unwrap_or(1.0)
    });
    let pause: web_sys::HtmlElement = by_id(&doc, "pause");
    on::<web_sys::Event>(&pause.clone(), "click", &st, move |s, _| {
        s.paused = !s.paused;
        pause.set_text_content(Some(if s.paused { "Resume" } else { "Pause" }));
        s.show();
    });
    let goto_t: HtmlInputElement = by_id(&doc, "goto-t");
    let goto: web_sys::HtmlElement = by_id(&doc, "goto");
    on::<web_sys::Event>(&goto, "click", &st, move |s, _| {
        let secs = goto_t.value().parse::<u64>().unwrap_or(0);
        s.restart(s.kind, secs * 1000);
    });
    let dump: web_sys::HtmlElement = by_id(&doc, "dump");
    on::<web_sys::Event>(&dump, "click", &st, |s, _| s.dump());

    web_sys::console::log_1(&"sim started (scenario mode, fictitious receiver)".into());

    // rAF loop: the closure re-registers itself.
    let f: Rc<RefCell<Option<Frame>>> = Rc::new(RefCell::new(None));
    let g = f.clone();
    *g.borrow_mut() = Some(Closure::new(move |now: f64| {
        st.borrow_mut().frame(now);
        web_sys::window()
            .unwrap()
            .request_animation_frame(f.borrow().as_ref().unwrap().as_ref().unchecked_ref())
            .unwrap();
    }));
    win.request_animation_frame(g.borrow().as_ref().unwrap().as_ref().unchecked_ref())
        .unwrap();
}

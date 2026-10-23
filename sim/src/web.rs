//! Browser glue: rAF loop, canvas blit, pointer events and the control strip from `index.html`.
//! Input sources: scenario (default) and live (WebSocket to the sim-bridge); replay is S.6.
use crate::Sim;
use crate::live::{Link, Reconnect, validate};
use adsb_app::{HEIGHT, PointerEvent, PointerPhase, WIDTH};
use adsb_core::scenario::Kind;
use adsb_core::tracker::Config;
use core::time::Duration;
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};
use wasm_bindgen::{Clamped, JsCast, closure::Closure, prelude::*};
use web_sys::{
    CanvasRenderingContext2d, Document, HtmlCanvasElement, HtmlInputElement, HtmlSelectElement,
    ImageData, MouseEvent, WebSocket,
};

const SEED: u64 = 1;
const MAX_DT_MS: f64 = 1000.0;
const BANNER_SIM: &str = "SIM: fictitious location (scenario mode, no real receiver data)";
const BANNER_LIVE: &str = "LIVE \u{2014} local data, don't share screenshots";
const TICK_MS: u64 = 1000;
type Frame = Closure<dyn FnMut(f64)>;

struct Live {
    link: Reconnect,
    ws: Option<WebSocket>,
    /// Identifies the current socket; events from older ones are ignored.
    epoch: u32,
    last_tick_ms: u64,
    /// Kept alive while `ws` may call them; replaced (and so freed) on the next connect.
    _handlers: Option<Handlers>,
}

type Handlers = (
    Closure<dyn FnMut()>,
    Closure<dyn FnMut(web_sys::MessageEvent)>,
    Closure<dyn FnMut()>,
);

struct State {
    this: Weak<RefCell<State>>,
    /// Receiver and radius from `/local/config.json`; kept in memory only, never displayed.
    live_cfg: Option<(Config, f64)>,
    live: Option<Live>,
    banner: web_sys::HtmlElement,
    live_opt: web_sys::HtmlElement,
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

fn now_ms() -> u64 {
    perf_ms() as u64
}

fn kind_of(name: &str) -> Kind {
    match name {
        "busy" => Kind::Busy,
        "edge" => Kind::Edge,
        _ => Kind::Overflight,
    }
}

fn detach(ws: &WebSocket) {
    ws.set_onopen(None);
    ws.set_onmessage(None);
    ws.set_onclose(None);
    ws.set_onerror(None);
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
        if let Some(l) = &self.live {
            let text = match l.link.link() {
                Link::Open => "LIVE: connected".to_string(),
                Link::Waiting { until_ms } => format!(
                    "LIVE: disconnected, retrying in {} s",
                    until_ms.saturating_sub(now_ms()).div_ceil(1000)
                ),
                _ => "LIVE: connecting".to_string(),
            };
            self.out.set_text_content(Some(&text));
            return;
        }
        let t = self.ticked_s;
        let text = format!(
            "t = {t} s ({})",
            if self.paused { "paused" } else { "running" }
        );
        self.out.set_text_content(Some(&text));
    }

    fn restart(&mut self, kind: Kind, at_ms: u64) {
        self.stop_live();
        self.banner.set_text_content(Some(BANNER_SIM));
        self.kind = kind;
        self.duration_ms = adsb_core::scenario::Scenario::new(kind, SEED).duration_ms();
        self.sim.seek(kind, SEED, at_ms);
        self.virt_ms = at_ms as f64;
        self.ticked_s = at_ms / 1000;
        self.blit();
        self.show();
    }

    fn stop_live(&mut self) {
        if let Some(l) = self.live.take()
            && let Some(ws) = l.ws
        {
            detach(&ws);
            let _ = ws.close();
        }
    }

    fn start_live(&mut self) {
        let Some((cfg, radius)) = self.live_cfg else {
            return;
        };
        self.stop_live();
        self.sim.start_live(cfg, radius);
        self.banner.set_text_content(Some(BANNER_LIVE));
        let mut link = Reconnect::default();
        link.start();
        self.live = Some(Live {
            link,
            ws: None,
            epoch: 0,
            last_tick_ms: now_ms(),
            _handlers: None,
        });
        self.connect();
        self.sim.tick_live(now_ms(), false);
        self.blit();
        self.show();
    }

    /// Open a socket to the bridge through the page's own origin (trunk proxy, same-origin).
    fn connect(&mut self) {
        let Some(l) = self.live.as_mut() else { return };
        // Detach before freeing: a dropped closure must never be called.
        if let Some(old) = l.ws.take() {
            detach(&old);
        }
        l.epoch += 1;
        let epoch = l.epoch;
        let loc = web_sys::window().unwrap().location();
        let scheme = if loc.protocol().as_deref() == Ok("https:") {
            "wss"
        } else {
            "ws"
        };
        let url = format!("{scheme}://{}/ws/live", loc.host().unwrap_or_default());
        let Ok(ws) = WebSocket::new(&url) else {
            l.link.on_closed(now_ms());
            return;
        };
        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);
        let hook = |this: &Weak<RefCell<State>>| {
            let this = this.clone();
            move |f: &dyn Fn(&mut State)| {
                if let Some(st) = this.upgrade() {
                    f(&mut st.borrow_mut());
                }
            }
        };
        let h = hook(&self.this);
        let open = Closure::<dyn FnMut()>::new(move || h(&|s| s.ws_open(epoch)));
        ws.set_onopen(Some(open.as_ref().unchecked_ref()));
        let h = hook(&self.this);
        let msg =
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
                let bytes = js_sys::Uint8Array::new(&e.data()).to_vec();
                h(&|s| s.ws_data(epoch, &bytes));
            });
        ws.set_onmessage(Some(msg.as_ref().unchecked_ref()));
        // `error` is always followed by `close`; either ends the attempt.
        let h = hook(&self.this);
        let closed = Closure::<dyn FnMut()>::new(move || h(&|s| s.ws_closed(epoch)));
        ws.set_onclose(Some(closed.as_ref().unchecked_ref()));
        ws.set_onerror(Some(closed.as_ref().unchecked_ref()));
        l._handlers = Some((open, msg, closed));
        l.ws = Some(ws);
    }

    fn ws_open(&mut self, epoch: u32) {
        if let Some(l) = self.live.as_mut().filter(|l| l.epoch == epoch) {
            l.link.on_open();
            self.sim.reset_clock();
            self.show();
        }
    }

    fn ws_data(&mut self, epoch: u32, bytes: &[u8]) {
        if let Some(l) = self.live.as_mut().filter(|l| l.epoch == epoch) {
            l.link.on_data();
            self.sim.feed(bytes, now_ms());
        }
    }

    fn ws_closed(&mut self, epoch: u32) {
        let Some(l) = self.live.as_mut().filter(|l| l.epoch == epoch) else {
            return;
        };
        l.link.on_closed(now_ms());
        l.ws = None;
        // Feed is known down: show No data now instead of waiting for the 30 s rule.
        self.sim.tick_live(now_ms(), false);
        self.blit();
        self.show();
    }

    fn live_frame(&mut self) {
        let now = now_ms();
        let l = self.live.as_mut().unwrap();
        if l.link.poll(now) {
            self.connect();
        }
        let l = self.live.as_mut().unwrap();
        let mut changed = self.sim.render();
        if now >= l.last_tick_ms + TICK_MS {
            l.last_tick_ms = now;
            self.sim.tick_live(now, l.link.feed_ok());
            changed = true;
            self.show();
        }
        if changed {
            self.blit();
        }
    }

    fn set_live_config(&mut self, cfg: Option<(Config, f64)>) {
        self.live_cfg = cfg;
        if cfg.is_some() {
            self.live_opt.remove_attribute("disabled").unwrap();
            self.live_opt.set_text_content(Some("live"));
        } else {
            self.live_opt.set_text_content(Some(
                "live (unavailable: no local receiver config from the bridge)",
            ));
        }
    }

    fn frame(&mut self, now: f64) {
        if self.live.is_some() {
            self.last_frame = now;
            return self.live_frame();
        }
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
            "frame hash (FNV-1a 64, RGB565 LE): {:016x}  {}",
            self.sim.frame_hash(),
            if self.live.is_some() {
                "live".to_string()
            } else {
                format!("scenario={:?} t={} s", self.kind, self.ticked_s)
            }
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

/// Ask the bridge for the receiver. Any failure (404 without a `.env`, bridge down, bad JSON,
/// implausible values) leaves live mode disabled. Nothing from the response is logged.
fn fetch_config(st: &Rc<RefCell<State>>) {
    let s = st.clone();
    let on_json = Closure::<dyn FnMut(JsValue)>::new(move |v: JsValue| {
        let num = |k: &str| js_sys::Reflect::get(&v, &k.into()).ok()?.as_f64();
        let cfg = (|| validate(num("lat")?, num("lon")?, num("elev_m")?, num("radius_km")?))();
        s.borrow_mut().set_live_config(cfg);
    });
    let s = st.clone();
    let on_err = Rc::new(Closure::<dyn FnMut(JsValue)>::new(move |_| {
        s.borrow_mut().set_live_config(None)
    }));
    let (e, s) = (on_err.clone(), st.clone());
    let on_resp = Closure::<dyn FnMut(JsValue)>::new(move |v: JsValue| {
        let r: web_sys::Response = v.unchecked_into();
        // Status first: `json()` on an empty 404 body rejects, and nobody would catch that.
        match r.ok().then(|| r.json()) {
            Some(Ok(p)) => {
                let _ = p.then2(&on_json, &e);
            }
            _ => s.borrow_mut().set_live_config(None),
        }
    });
    let _ = web_sys::window()
        .unwrap()
        .fetch_with_str("/local/config.json")
        .then2(&on_resp, &on_err);
    on_resp.forget();
    std::mem::forget(on_err);
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
    let st = Rc::new_cyclic(|this| {
        RefCell::new(State {
            this: this.clone(),
            live_cfg: None,
            live: None,
            banner: by_id(&doc, "banner"),
            live_opt: by_id(&doc, "mode-live"),
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
        })
    });
    st.borrow_mut().restart(Kind::Overflight, 0);

    let c: &web_sys::EventTarget = &canvas;
    on::<MouseEvent>(c, "mousedown", &st, |s, e| s.pointer(e, PointerPhase::Down));
    // Window-level so a release outside the canvas still ends the press.
    on::<MouseEvent>(&win, "mouseup", &st, |s, e| s.pointer(e, PointerPhase::Up));
    on::<MouseEvent>(c, "mousemove", &st, |s, e| s.pointer(e, PointerPhase::Move));

    let scenario: HtmlSelectElement = by_id(&doc, "scenario");
    on::<web_sys::Event>(&scenario.clone(), "change", &st, move |s, _| match scenario
        .value()
        .as_str()
    {
        "live" => s.start_live(),
        name => s.restart(kind_of(name), 0),
    });
    let speed: HtmlSelectElement = by_id(&doc, "speed");
    on::<web_sys::Event>(&speed.clone(), "change", &st, move |s, _| {
        s.speed = speed.value().parse().unwrap_or(1.0)
    });
    let pause: web_sys::HtmlElement = by_id(&doc, "pause");
    on::<web_sys::Event>(&pause.clone(), "click", &st, move |s, _| {
        if s.live.is_some() {
            return;
        }
        s.paused = !s.paused;
        pause.set_text_content(Some(if s.paused { "Resume" } else { "Pause" }));
        s.show();
    });
    let goto_t: HtmlInputElement = by_id(&doc, "goto-t");
    let goto: web_sys::HtmlElement = by_id(&doc, "goto");
    on::<web_sys::Event>(&goto, "click", &st, move |s, _| {
        if s.live.is_some() {
            return;
        }
        let secs = goto_t.value().parse::<u64>().unwrap_or(0);
        s.restart(s.kind, secs * 1000);
    });
    let dump: web_sys::HtmlElement = by_id(&doc, "dump");
    on::<web_sys::Event>(&dump, "click", &st, |s, _| s.dump());

    fetch_config(&st);
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

//! The graph view (like Obsidian's): notes as dots, links as lines, drawn on
//! a canvas from `knowledge::graph` and laid out by `graph::layout`. The
//! layout moves until it settles, then nothing runs until you interact.
//! Everything happens on decrypted notes in the browser: the server learns
//! nothing new.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::graph::Graph;
use crate::graph::layout::{self, Point, Sim, View};
use crate::knowledge;
use crate::state::{self, Route, step, ui};
use crate::util;
use crate::views::icons::Icon;

/// Folder colours (top-level folders, in name order); notes at the root use
/// the theme's muted colour.
const PALETTE: [&str; 8] = [
    "#7c6cf0", "#2bb3a3", "#e0864a", "#d45d9e", "#4a9be0", "#8fb33a", "#c9a227", "#e05a5a",
];

enum Drag {
    None,
    /// A note held under the pointer (a click if it didn't move).
    Node {
        i: usize,
        start: Point,
        moved: bool,
    },
    Pan {
        last: Point,
        start: Point,
    },
    /// Two fingers: zoom and move.
    Pinch {
        dist: f64,
        mid: Point,
    },
}

struct Scene {
    graph: Graph,
    sim: Sim,
    radii: Vec<f64>,
    neighbors: Vec<Vec<usize>>,
    colors: Vec<Option<usize>>,
    focus: Option<String>,
    hover: Option<usize>,
    view: View,
    /// Keep everything in view while the layout moves (until you zoom or pan).
    follow: bool,
    size: Point,
    dpr: f64,
    compact: bool,
    drag: Drag,
    pointers: Vec<(i32, Point)>,
    frame: Option<i32>,
}

impl Scene {
    fn new(compact: bool, focus: Option<String>) -> Scene {
        Scene {
            graph: Graph::default(),
            sim: Sim::new(0, vec![], |_| None),
            radii: vec![],
            neighbors: vec![],
            colors: vec![],
            focus,
            hover: None,
            view: View {
                scale: 1.0,
                tx: 0.0,
                ty: 0.0,
            },
            follow: true,
            size: (0.0, 0.0),
            dpr: 1.0,
            compact,
            drag: Drag::None,
            pointers: vec![],
            frame: None,
        }
    }

    /// Show `g`, keeping where the notes already shown are.
    fn set_graph(&mut self, g: Graph) {
        if g.nodes
            .iter()
            .map(|n| &n.id)
            .eq(self.graph.nodes.iter().map(|n| &n.id))
            && g.edges == self.graph.edges
        {
            // Same shape (a title or an icon changed): nothing moves.
            self.graph = g;
            return;
        }
        let first = self.graph.nodes.is_empty();
        let old: HashMap<&str, Point> = self
            .graph
            .nodes
            .iter()
            .zip(&self.sim.pos)
            .map(|(n, p)| (n.id.as_str(), *p))
            .collect();
        let neighbors = g.neighbors();
        // New notes start next to a neighbour already shown.
        let start = |i: usize| {
            old.get(g.nodes[i].id.as_str()).copied().or_else(|| {
                neighbors[i]
                    .iter()
                    .find_map(|&j| old.get(g.nodes[j].id.as_str()))
                    .map(|p| (p.0 + 6.0, p.1 + 6.0 + i as f64 * 0.01))
            })
        };
        let mut sim = Sim::new(g.nodes.len(), g.edges.clone(), start);
        if !first {
            sim.alpha = 0.35;
        }
        let degrees = g.degrees();
        let mut folders: Vec<&str> = g.nodes.iter().map(|n| n.folder()).collect();
        folders.sort_unstable();
        folders.dedup();
        folders.retain(|f| !f.is_empty());
        self.colors = g
            .nodes
            .iter()
            .map(|n| {
                folders
                    .binary_search(&n.folder())
                    .ok()
                    .map(|k| k % PALETTE.len())
            })
            .collect();
        self.radii = degrees.iter().map(|&d| layout::radius(d)).collect();
        self.neighbors = neighbors;
        self.sim = sim;
        self.graph = g;
        self.hover = None;
        self.drag = Drag::None;
    }

    fn fitted(&self) -> View {
        let margin = if self.compact { 22.0 } else { 56.0 };
        View::fit(&self.sim.pos, self.size.0, self.size.1, margin)
    }

    fn hit(&self, at: Point) -> Option<usize> {
        layout::hit(&self.sim.pos, &self.radii, &self.view, at, 6.0)
    }

    fn lit(&self, i: usize) -> bool {
        self.hover
            .is_some_and(|h| h == i || self.neighbors[h].contains(&i))
    }
}

/// The scene and the browser objects tied to one canvas.
struct Runtime {
    scene: RefCell<Scene>,
    canvas: RefCell<Option<HtmlCanvasElement>>,
    resize: RefCell<Option<(web_sys::ResizeObserver, Closure<dyn FnMut(js_sys::Array)>)>>,
    wheel: RefCell<Option<Closure<dyn FnMut(web_sys::WheelEvent)>>>,
    closed: std::cell::Cell<bool>,
}

type Rt = Rc<Runtime>;

/// Draw on the next frame (and keep stepping the layout until it settles).
fn schedule(rt: &Rt) {
    if rt.closed.get() || rt.scene.borrow().frame.is_some() {
        return;
    }
    let next = rt.clone();
    // Runs once (and is freed); after the view is gone it does nothing.
    let cb = Closure::once_into_js(move || on_frame(&next));
    if let Ok(id) = util::window().request_animation_frame(cb.unchecked_ref()) {
        rt.scene.borrow_mut().frame = Some(id);
    }
}

fn on_frame(rt: &Rt) {
    if rt.closed.get() {
        return;
    }
    let moving = {
        let mut s = rt.scene.borrow_mut();
        s.frame = None;
        if !s.sim.settled() {
            // As many steps as fit in about half a frame (at least one).
            let start = js_sys::Date::now();
            for _ in 0..12 {
                s.sim.tick();
                if s.sim.settled() || js_sys::Date::now() - start > 8.0 {
                    break;
                }
            }
            if s.follow {
                s.view = s.fitted();
            }
        }
        !s.sim.settled()
    };
    draw(rt);
    if moving {
        schedule(rt);
    }
}

struct Theme {
    fg: String,
    muted: String,
    accent: String,
    edge: String,
    font: String,
}

impl Theme {
    fn read() -> Theme {
        let style = util::document()
            .document_element()
            .and_then(|e| util::window().get_computed_style(&e).ok().flatten());
        let var = |name: &str, fallback: &str| {
            style
                .as_ref()
                .and_then(|s| s.get_property_value(name).ok())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| fallback.to_string())
        };
        Theme {
            fg: var("--fg", "#222"),
            muted: var("--muted", "#888"),
            accent: var("--accent", "#5b5bd6"),
            edge: var("--border-strong", "#bbb"),
            font: var("--sans", "system-ui, sans-serif"),
        }
    }
}

fn draw(rt: &Rt) {
    let Some(canvas) = rt.canvas.borrow().clone() else {
        return;
    };
    let Ok(Some(ctx)) = canvas.get_context("2d") else {
        return;
    };
    let ctx: CanvasRenderingContext2d = ctx.unchecked_into();
    let s = rt.scene.borrow();
    let theme = Theme::read();
    let (dpr, v) = (s.dpr, s.view);
    let _ = ctx.set_transform(dpr, 0.0, 0.0, dpr, 0.0, 0.0);
    ctx.clear_rect(0.0, 0.0, s.size.0, s.size.1);
    let _ = ctx.set_transform(
        dpr * v.scale,
        0.0,
        0.0,
        dpr * v.scale,
        dpr * v.tx,
        dpr * v.ty,
    );
    let pos = &s.sim.pos;
    // Links, those of the note under the pointer on top.
    let lit_edge = |a: usize, b: usize| s.hover.is_some_and(|h| h == a || h == b);
    for lit in [false, true] {
        ctx.begin_path();
        for &(a, b) in s
            .graph
            .edges
            .iter()
            .filter(|&&(a, b)| lit_edge(a, b) == lit)
        {
            ctx.move_to(pos[a].0, pos[a].1);
            ctx.line_to(pos[b].0, pos[b].1);
        }
        ctx.set_global_alpha(if s.hover.is_some() && !lit { 0.35 } else { 0.9 });
        ctx.set_stroke_style_str(if lit { &theme.accent } else { &theme.edge });
        ctx.set_line_width(if lit { 1.8 } else { 1.0 } / v.scale);
        ctx.stroke();
    }
    // Notes.
    let focus = s
        .focus
        .as_deref()
        .and_then(|f| s.graph.nodes.iter().position(|n| n.id == f));
    ctx.set_text_align("center");
    ctx.set_text_baseline("middle");
    for (i, n) in s.graph.nodes.iter().enumerate() {
        let (x, y) = pos[i];
        let r = s.radii[i];
        let dim = s.hover.is_some() && !s.lit(i);
        ctx.set_global_alpha(if dim { 0.25 } else { 1.0 });
        let fill = if Some(i) == s.hover || Some(i) == focus {
            theme.accent.as_str()
        } else {
            s.colors[i].map_or(theme.muted.as_str(), |k| PALETTE[k])
        };
        ctx.begin_path();
        let _ = ctx.arc(x, y, r, 0.0, std::f64::consts::TAU);
        ctx.set_fill_style_str(fill);
        ctx.fill();
        if let Some(icon) = &n.icon
            && r * v.scale >= 6.0
        {
            ctx.set_font(&format!("{}px {}", r * 1.4, theme.font));
            let _ = ctx.fill_text(icon, x, y);
        }
    }
    // Names: all when zoomed in (or few notes), else the lit ones.
    let all = v.scale >= 0.9 || s.graph.nodes.len() <= 30;
    let px = if s.compact { 11.0 } else { 12.0 } / v.scale;
    ctx.set_font(&format!("{px}px {}", theme.font));
    ctx.set_text_baseline("top");
    for (i, n) in s.graph.nodes.iter().enumerate() {
        if !(all || s.lit(i) || Some(i) == focus) {
            continue;
        }
        let dim = s.hover.is_some() && !s.lit(i);
        ctx.set_global_alpha(if dim { 0.3 } else { 1.0 });
        ctx.set_fill_style_str(if dim { &theme.muted } else { &theme.fg });
        let (x, y) = pos[i];
        let _ = ctx.fill_text(n.title(), x, y + s.radii[i] + 3.0 / v.scale);
    }
    ctx.set_global_alpha(1.0);
}

fn dist(a: Point, b: Point) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn mid(a: Point, b: Point) -> Point {
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

fn at(ev: &web_sys::MouseEvent) -> Point {
    (ev.offset_x() as f64, ev.offset_y() as f64)
}

/// A vault's notes and links on a canvas: the whole vault, or with `focus`
/// the notes within `depth` links of that note ("local graph").
#[component]
pub fn GraphCanvas(
    vault: String,
    #[prop(optional)] focus: Option<String>,
    #[prop(default = 1)] depth: usize,
    #[prop(into, default = Signal::stored(String::new()))] query: Signal<String>,
    #[prop(into, default = Signal::stored(true))] orphans: Signal<bool>,
    #[prop(optional)] compact: bool,
) -> impl IntoView {
    let ui = ui();
    let vid = StoredValue::new(vault);
    let focus_id = StoredValue::new(focus.clone());
    let data = Memo::new(move |_| {
        ui.tree_rev.track();
        ui.content_rev.track();
        let g = knowledge::graph(&vid.get_value());
        let g = match focus_id.get_value() {
            Some(f) => g.around(&f, depth),
            None => g,
        };
        g.filtered(&query.get(), orphans.get())
    });
    let rt: Rt = Rc::new(Runtime {
        scene: RefCell::new(Scene::new(compact, focus)),
        canvas: RefCell::new(None),
        resize: RefCell::new(None),
        wheel: RefCell::new(None),
        closed: std::cell::Cell::new(false),
    });
    let canvas_ref = NodeRef::<html::Canvas>::new();

    // The canvas follows its box's size (sharp on high-density screens).
    let mount = rt.clone();
    Effect::new(move |_| {
        let Some(canvas) = canvas_ref.get() else {
            return;
        };
        let rt = mount.clone();
        if rt.canvas.borrow().is_some() {
            return;
        }
        let canvas: HtmlCanvasElement = canvas.unchecked_into();
        *rt.canvas.borrow_mut() = Some(canvas.clone());
        let sized = rt.clone();
        let on_resize = Closure::wrap(Box::new(move |_: js_sys::Array| {
            let Some(c) = sized.canvas.borrow().clone() else {
                return;
            };
            let (w, h) = (c.client_width() as f64, c.client_height() as f64);
            let dpr = util::window().device_pixel_ratio().max(1.0);
            c.set_width((w * dpr).round() as u32);
            c.set_height((h * dpr).round() as u32);
            {
                let mut s = sized.scene.borrow_mut();
                s.size = (w, h);
                s.dpr = dpr;
                if s.follow {
                    s.view = s.fitted();
                }
            }
            draw(&sized);
        }) as Box<dyn FnMut(js_sys::Array)>);
        if let Ok(observer) = web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()) {
            observer.observe(&canvas);
            *rt.resize.borrow_mut() = Some((observer, on_resize));
        }
        // Wheel zoom: a listener of our own, as it must be able to stop the
        // page from scrolling.
        let zoomed = rt.clone();
        let on_wheel = Closure::wrap(Box::new(move |ev: web_sys::WheelEvent| {
            ev.prevent_default();
            let lines = if ev.delta_mode() == 1 { 16.0 } else { 1.0 };
            let factor = (-ev.delta_y() * lines * 0.0015).exp();
            {
                let mut s = zoomed.scene.borrow_mut();
                s.view.zoom_at(at(&ev), factor);
                s.follow = false;
            }
            schedule(&zoomed);
        }) as Box<dyn FnMut(web_sys::WheelEvent)>);
        let opts = web_sys::AddEventListenerOptions::new();
        opts.set_passive(false);
        let _ = canvas.add_event_listener_with_callback_and_add_event_listener_options(
            "wheel",
            on_wheel.as_ref().unchecked_ref(),
            &opts,
        );
        *rt.wheel.borrow_mut() = Some(on_wheel);
        schedule(&rt);
    });

    let fed = rt.clone();
    Effect::new(move |_| {
        let g = data.get();
        fed.scene.borrow_mut().set_graph(g);
        schedule(&fed);
    });

    let kept = StoredValue::new_local(rt.clone());
    on_cleanup(move || {
        kept.try_with_value(|rt| {
            rt.closed.set(true);
            if let Some((observer, _)) = rt.resize.borrow_mut().take() {
                observer.disconnect();
            }
            if let (Some(c), Some(w)) =
                (rt.canvas.borrow_mut().take(), rt.wheel.borrow_mut().take())
            {
                let _ = c.remove_event_listener_with_callback("wheel", w.as_ref().unchecked_ref());
            }
        });
    });

    let open = move |id: String| state::navigate(Route::Note(vid.get_value(), id));
    let (down, moved, up, left) = (rt.clone(), rt.clone(), rt.clone(), rt.clone());
    let on_down = move |ev: web_sys::PointerEvent| {
        let p = at(&ev);
        if let Some(c) = down.canvas.borrow().as_ref() {
            let _ = c.set_pointer_capture(ev.pointer_id());
        }
        {
            let mut s = down.scene.borrow_mut();
            s.pointers.retain(|(id, _)| *id != ev.pointer_id());
            s.pointers.push((ev.pointer_id(), p));
            if let [(_, a), (_, b)] = s.pointers[..] {
                if let Drag::Node { i, .. } = s.drag {
                    s.sim.fixed[i] = None;
                    s.sim.alpha_target = 0.0;
                }
                s.drag = Drag::Pinch {
                    dist: dist(a, b),
                    mid: mid(a, b),
                };
            } else if let Some(i) = s.hit(p) {
                s.sim.fixed[i] = Some(s.sim.pos[i]);
                s.sim.alpha_target = 0.3;
                s.sim.reheat(0.3);
                s.drag = Drag::Node {
                    i,
                    start: p,
                    moved: false,
                };
            } else {
                s.drag = Drag::Pan { last: p, start: p };
            }
        }
        schedule(&down);
    };
    let on_move = move |ev: web_sys::PointerEvent| {
        let p = at(&ev);
        let mut redraw = true;
        {
            let mut s = moved.scene.borrow_mut();
            if let Some(slot) = s.pointers.iter_mut().find(|(id, _)| *id == ev.pointer_id()) {
                slot.1 = p;
            }
            let pinch = match s.pointers[..] {
                [(_, a), (_, b)] => Some((dist(a, b), mid(a, b))),
                _ => None,
            };
            if let Drag::Node { start, moved, .. } = &mut s.drag {
                *moved |= dist(*start, p) > 3.0;
            }
            match s.drag {
                Drag::Node { i, .. } => {
                    let w = s.view.to_world(p);
                    s.sim.fixed[i] = Some(w);
                }
                Drag::Pan { last, start } => {
                    s.view.tx += p.0 - last.0;
                    s.view.ty += p.1 - last.1;
                    s.drag = Drag::Pan { last: p, start };
                    s.follow = false;
                }
                Drag::Pinch { dist: d0, mid: m0 } => {
                    if let Some((d1, m1)) = pinch {
                        s.view.zoom_at(m0, d1 / d0.max(1.0));
                        s.view.tx += m1.0 - m0.0;
                        s.view.ty += m1.1 - m0.1;
                        s.drag = Drag::Pinch { dist: d1, mid: m1 };
                        s.follow = false;
                    }
                }
                Drag::None => {
                    let hover = s.hit(p);
                    redraw = hover != s.hover;
                    s.hover = hover;
                    if let Some(c) = moved.canvas.borrow().as_ref() {
                        let _ = c.class_list().toggle_with_force("on-node", hover.is_some());
                    }
                }
            }
        }
        if redraw {
            schedule(&moved);
        }
    };
    let on_up = move |ev: web_sys::PointerEvent| {
        let clicked = {
            let mut s = up.scene.borrow_mut();
            s.pointers.retain(|(id, _)| *id != ev.pointer_id());
            match std::mem::replace(&mut s.drag, Drag::None) {
                Drag::Node { i, moved, .. } => {
                    s.sim.fixed[i] = None;
                    s.sim.alpha_target = 0.0;
                    (!moved).then(|| s.graph.nodes[i].id.clone())
                }
                _ => None,
            }
        };
        schedule(&up);
        if let Some(id) = clicked {
            open(id);
        }
    };
    let on_leave = move |_: web_sys::PointerEvent| {
        let changed = {
            let mut s = left.scene.borrow_mut();
            std::mem::take(&mut s.hover).is_some()
        };
        if changed {
            schedule(&left);
        }
    };
    let (zin, zout, zfit) = (rt.clone(), rt.clone(), rt.clone());
    let zoom = move |rt: &Rt, factor: f64| {
        {
            let mut s = rt.scene.borrow_mut();
            let c = (s.size.0 / 2.0, s.size.1 / 2.0);
            s.view.zoom_at(c, factor);
            s.follow = false;
        }
        schedule(rt);
    };

    let count = move || {
        let g = data.get();
        format!(
            "Graph of {} and {}",
            util::count(g.nodes.len(), "note", "notes"),
            util::count(g.edges.len(), "link", "links")
        )
    };
    view! {
        <div class="graph-wrap" class:compact=compact>
            <canvas node_ref=canvas_ref role="img" aria-label=count
                data-nodes=move || data.get().nodes.len().to_string()
                data-edges=move || data.get().edges.len().to_string()
                on:pointerdown=on_down on:pointermove=on_move on:pointerup=on_up.clone()
                on:pointercancel=on_up on:pointerleave=on_leave></canvas>
            {move || data.get().nodes.is_empty().then(|| view! {
                <p class="graph-empty muted small">{if focus_id.get_value().is_some() { "Not in the graph." } else { "No notes to show." }}</p>
            })}
            <div class="graph-tools">
                <button class="icon-btn tiny" title="Zoom in" aria-label="Zoom in" on:click=move |_| zoom(&zin, 1.3)><Icon name="plus"/></button>
                <button class="icon-btn tiny" title="Zoom out" aria-label="Zoom out" on:click=move |_| zoom(&zout, 1.0 / 1.3)><Icon name="minus"/></button>
                <button class="icon-btn tiny graph-fit" title="Show everything" aria-label="Show everything" on:click=move |_| {
                    {
                        let mut s = zfit.scene.borrow_mut();
                        s.follow = true;
                        s.view = s.fitted();
                    }
                    schedule(&zfit);
                }><Icon name="maximize"/></button>
            </div>
            // The same notes for screen readers (and to reach them by keyboard).
            <ul class="sr-only graph-list" aria-label="Notes in the graph">
                {move || {
                    let g = data.get();
                    let degrees = g.degrees();
                    g.nodes.into_iter().zip(degrees).map(|(n, d)| {
                        let id = n.id.clone();
                        view! { <li><button on:click=move |_| open(id.clone())>{format!("{} ({d} links)", n.title())}</button></li> }
                    }).collect_view()
                }}
            </ul>
        </div>
    }
}

/// `/v/{vault}/graph`: the vault's whole graph, with a filter.
#[component]
pub fn GraphPage(vault: String) -> impl IntoView {
    let ui = ui();
    let query = RwSignal::new(String::new());
    let orphans = RwSignal::new(true);
    let v = vault.clone();
    let name = Memo::new(move |_| {
        ui.vaults.get();
        step(|c| c.vault_name(&v))
    });
    view! {
        <div class="page graph-page">
            <header class="page-head">
                <button class="icon-btn mobile-only" title="Menu" aria-label="Menu" on:click=move |_| ui.sidebar.set(true)><Icon name="menu"/></button>
                <h1 class="grow ellipsis">"Graph"<span class="muted">{move || format!(" · {}", name.get())}</span></h1>
                <input class="input graph-filter" type="search" placeholder="Filter notes" aria-label="Filter notes"
                    prop:value=move || query.get() on:input=move |ev| query.set(event_target_value(&ev))/>
                <label class="check graph-orphans" title="Notes that link with no other note">
                    <input type="checkbox" prop:checked=move || orphans.get() on:change=move |ev| orphans.set(event_target_checked(&ev))/>
                    " Unlinked notes"
                </label>
            </header>
            <div class="graph-body">
                <GraphCanvas vault=vault query=query orphans=orphans/>
            </div>
        </div>
    }
}

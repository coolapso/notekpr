//! Drawing one frame on the canvas.

use coolpsonotes_load_events::{Action, LevelResult, Phase, Report, Stats};
use web_sys::CanvasRenderingContext2d as Ctx;

use crate::state::{App, SLOTS};
use crate::wrap;

const SANS: &str = "Inter, system-ui, -apple-system, 'Segoe UI', sans-serif";
const MONO: &str = "ui-monospace, 'SF Mono', 'JetBrains Mono', Menlo, Consolas, monospace";

const BG: &str = "#0a0d13";
const PANEL: &str = "#10151f";
const PANEL_2: &str = "#141b27";
const LINE: &str = "#1f2836";
const TEXT: &str = "#d6dde8";
const DIM: &str = "#7c889c";
const FAINT: &str = "#4a5568";
const CYAN: &str = "#4cc9f0";
const GREEN: &str = "#3ddc97";
const RED: &str = "#ff5d73";
const AMBER: &str = "#ffb547";
const PURPLE: &str = "#a78bfa";

pub fn person_color(id: u32, light: u8) -> String {
    format!("hsl({:.0},72%,{light}%)", (id as f64 * 137.508) % 360.0)
}

fn phase_color(p: Option<Phase>) -> &'static str {
    match p {
        Some(Phase::Busy) | Some(Phase::Done) => GREEN,
        Some(Phase::Crash) => RED,
        Some(Phase::Recovering) => AMBER,
        Some(Phase::Checking) => CYAN,
        Some(Phase::Teaming) => PURPLE,
        Some(Phase::Arriving) => "#60a5fa",
        _ => DIM,
    }
}

fn kind_color(k: Action) -> &'static str {
    match k {
        Action::Type => CYAN,
        Action::Note | Action::CreateVault => PURPLE,
        Action::Picture => AMBER,
        Action::Quick => "#2dd4bf",
        Action::Rename => "#60a5fa",
        Action::Trash => DIM,
        Action::Share | Action::ShareNote => "#f472b6",
        Action::Revision => GREEN,
        Action::SignUp => TEXT,
    }
}

struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

fn rrect(c: &Ctx, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    c.begin_path();
    c.move_to(x + r, y);
    let _ = c.arc_to(x + w, y, x + w, y + h, r);
    let _ = c.arc_to(x + w, y + h, x, y + h, r);
    let _ = c.arc_to(x, y + h, x, y, r);
    let _ = c.arc_to(x, y, x + w, y, r);
    c.close_path();
}

fn panel(c: &Ctx, r: &Rect) {
    rrect(c, r.x, r.y, r.w, r.h, 12.0);
    c.set_fill_style_str(PANEL);
    c.fill();
    c.set_stroke_style_str(LINE);
    c.set_line_width(1.0);
    c.stroke();
}

fn text(c: &Ctx, s: &str, x: f64, y: f64, font: &str, color: &str, align: &str) {
    c.set_font(font);
    c.set_fill_style_str(color);
    c.set_text_align(align);
    let _ = c.fill_text(s, x, y);
}

fn width(c: &Ctx, s: &str, font: &str) -> f64 {
    c.set_font(font);
    c.measure_text(s).map(|m| m.width()).unwrap_or(0.0)
}

fn dot(c: &Ctx, x: f64, y: f64, r: f64, color: &str) {
    c.begin_path();
    let _ = c.arc(x, y, r.max(0.1), 0.0, std::f64::consts::TAU);
    c.set_fill_style_str(color);
    c.fill();
}

fn clock(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// "34 ms", "2.4 s", "150 s".
fn duration(ms: f32) -> String {
    if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else if ms < 10_000.0 {
        format!("{:.1} s", ms / 1000.0)
    } else {
        format!("{:.0} s", ms / 1000.0)
    }
}

fn mb(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

pub fn frame(app: &mut App, c: &Ctx, w: f64, h: f64, now: f64) {
    app.tick(now);
    c.set_fill_style_str(BG);
    c.fill_rect(0.0, 0.0, w, h);
    c.set_text_baseline("alphabetic");

    let pad = 16.0;
    let head = 58.0;
    let right_w = (w * 0.36).clamp(360.0, 640.0);
    let charts_h = if h > 760.0 { 156.0 } else { 128.0 };
    let world = Rect {
        x: pad,
        y: head + 8.0,
        w: w - right_w - pad * 3.0,
        h: h - head - 8.0 - charts_h - pad * 2.0,
    };
    let charts = Rect {
        x: pad,
        y: h - charts_h - pad,
        w: world.w,
        h: charts_h,
    };
    let screens = Rect {
        x: w - right_w - pad,
        y: head + 8.0,
        w: right_w,
        h: h - head - 8.0 - pad,
    };

    header(app, c, w, head);
    draw_world(app, c, &world, now);
    draw_charts(app, c, &charts);
    draw_screens(app, c, &screens, now);
    if let Some(r) = app.report.clone() {
        report(c, &world, &r, now - app.report_at);
    }
    if !app.connected {
        let msg = "connecting to the load test…";
        let fw = width(c, msg, &format!("600 13px {SANS}")) + 28.0;
        rrect(c, (w - fw) / 2.0, head + 14.0, fw, 30.0, 15.0);
        c.set_fill_style_str(AMBER);
        c.fill();
        text(
            c,
            msg,
            w / 2.0,
            head + 34.0,
            &format!("600 13px {SANS}"),
            BG,
            "center",
        );
    }
}

fn header(app: &App, c: &Ctx, w: f64, head: f64) {
    text(
        c,
        "coolpsonotes",
        18.0,
        36.0,
        &format!("700 20px {SANS}"),
        TEXT,
        "left",
    );
    let tw = width(c, "coolpsonotes", &format!("700 20px {SANS}"));
    text(
        c,
        "under load",
        26.0 + tw,
        36.0,
        &format!("400 20px {SANS}"),
        DIM,
        "left",
    );
    let x = 40.0 + tw + width(c, "under load", &format!("400 20px {SANS}"));
    let label = app.phase.map(|p| p.label()).unwrap_or("Waiting");
    let font = &format!("700 12px {SANS}");
    let pw = width(c, &label.to_uppercase(), font) + 26.0;
    rrect(c, x, 17.0, pw, 26.0, 13.0);
    c.set_fill_style_str(phase_color(app.phase));
    c.set_global_alpha(0.16);
    c.fill();
    c.set_global_alpha(1.0);
    dot(c, x + 12.0, 30.0, 3.5, phase_color(app.phase));
    text(
        c,
        &label.to_uppercase(),
        x + 20.0,
        34.5,
        font,
        phase_color(app.phase),
        "left",
    );
    let mut right = Vec::new();
    if let Some(hl) = &app.hello {
        let t = app.latest().map(|s| s.t_ms).unwrap_or(0);
        match app.level {
            Some((level, of, users, _)) if of > 1 => right.push(format!(
                "{} · level {level}/{of} · {users} of {} people",
                clock(t),
                hl.users
            )),
            _ => right.push(format!("{} · {} people", clock(t), hl.users)),
        }
        if hl.cpus > 0.0 {
            right.push(format!("server: {} CPU · {:.0} MB", hl.cpus, mb(hl.memory)));
        } else {
            right.push("server: local process, no limits".into());
        }
        right.push(format!("seed {}", hl.seed));
    }
    let right = right.join("   ");
    let rf = format!("500 13px {MONO}");
    text(c, &right, w - 18.0, 34.5, &rf, DIM, "right");
    // The phase's detail, cut to the room left.
    let df = format!("400 13px {SANS}");
    let room = w - 18.0 - width(c, &right, &rf) - 24.0 - (x + pw + 12.0);
    let mut detail = app.detail.clone();
    if width(c, &detail, &df) > room {
        while !detail.is_empty() && width(c, &format!("{detail}…"), &df) > room {
            detail.pop();
        }
        detail.push('…');
    }
    text(c, &detail, x + pw + 12.0, 34.5, &df, DIM, "left");
    c.set_fill_style_str(LINE);
    c.fill_rect(0.0, head, w, 1.0);
}

fn draw_world(app: &mut App, c: &Ctx, r: &Rect, now: f64) {
    panel(c, r);
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0 + 6.0);
    let (rx, ry) = (r.w * 0.40, r.h * 0.37);
    if app.layout_dirty {
        app.place(cx, cy, rx, ry);
    }
    let stats = app.latest().cloned().unwrap_or_default();
    let down =
        !app.server_up && app.hello.is_some() && app.phase.is_some_and(|p| p != Phase::Starting);
    let since = now - app.server_changed;
    let shake = if down && since < 1500.0 {
        (now / 28.0).sin() * 5.0 * (1.0 - since / 1500.0)
    } else {
        0.0
    };
    let server = (cx + shake, cy);

    // Glow around the server, brighter the busier it is.
    let busy = (stats.http_rps as f64 / 600.0).clamp(0.08, 1.0);
    if let Ok(g) = c.create_radial_gradient(server.0, server.1, 10.0, server.0, server.1, rx * 0.9)
    {
        let col = if down {
            "rgba(255,93,115,"
        } else {
            "rgba(76,201,240,"
        };
        let _ = g.add_color_stop(0.0, &format!("{col}{:.3})", 0.22 * busy));
        let _ = g.add_color_stop(1.0, &format!("{col}0)"));
        c.set_fill_style_canvas_gradient(&g);
        c.fill_rect(r.x, r.y, r.w, r.h);
    }
    // Rings.
    c.set_stroke_style_str(LINE);
    c.set_line_width(1.0);
    for k in [0.55, 1.0] {
        c.begin_path();
        let _ = c.ellipse(cx, cy, rx * k, ry * k, 0.0, 0.0, std::f64::consts::TAU);
        c.stroke();
    }
    // Spokes from everyone to the server.
    c.set_stroke_style_str("rgba(150,170,200,0.05)");
    c.begin_path();
    for p in app.people.iter().filter(|p| p.arrived) {
        c.move_to(p.pos.0, p.pos.1);
        c.line_to(server.0, server.1);
    }
    c.stroke();

    // Team names, outside their people: all of them while they fit, then
    // only the teams on the screens.
    let members = app.team_members();
    let on_screens: std::collections::BTreeSet<u32> =
        app.screens.iter().flatten().map(|s| s.team).collect();
    let few = members.len() <= 40;
    for (team, ids) in &members {
        if !few && !on_screens.contains(team) {
            continue;
        }
        let (mut sx, mut sy) = (0.0, 0.0);
        for &id in ids {
            let p = &app.people[id as usize].pos;
            sx += p.0;
            sy += p.1;
        }
        let n = ids.len().max(1) as f64;
        let (mx, my) = (sx / n, sy / n);
        let (dx, dy) = (mx - cx, my - cy);
        let len = (dx * dx + dy * dy).sqrt().max(1.0);
        let (lx, ly) = (cx + dx / len * (len + 34.0), cy + dy / len * (len + 30.0));
        let name = app
            .teams
            .get(team)
            .map(|t| t.name.clone())
            .unwrap_or_default();
        if !name.is_empty() {
            let align = if lx < cx - 20.0 {
                "right"
            } else if lx > cx + 20.0 {
                "left"
            } else {
                "center"
            };
            text(
                c,
                &name,
                lx,
                ly + 4.0,
                &format!("600 11px {SANS}"),
                FAINT,
                align,
            );
        }
    }

    // What is flying: changes up to the server, and out to the others.
    let _ = c.set_global_composite_operation("lighter");
    for p in &app.particles {
        let t = ((now - p.t0) / p.dur).clamp(0.0, 1.0);
        let e = t * t * (3.0 - 2.0 * t);
        let a = app.position(p.from, server);
        let b = app.position(p.to, server);
        let (mx, my) = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let (px, py) = (mx - dy * p.bend * 0.35, my + dx * p.bend * 0.35);
        let u = 1.0 - e;
        let x = u * u * a.0 + 2.0 * u * e * px + e * e * b.0;
        let y = u * u * a.1 + 2.0 * u * e * py + e * e * b.1;
        let color = if p.failed {
            RED.to_string()
        } else if p.kind == Action::Picture {
            AMBER.to_string()
        } else {
            match p.person {
                Some(id) => person_color(id, 64),
                None => kind_color(p.kind).to_string(),
            }
        };
        let fade = if t > 0.85 { (1.0 - t) / 0.15 } else { 1.0 };
        c.set_global_alpha(0.22 * fade);
        dot(c, x, y, p.size * 2.6, &color);
        c.set_global_alpha(fade);
        if p.kind == Action::Picture {
            c.set_fill_style_str(&color);
            c.fill_rect(x - p.size, y - p.size, p.size * 2.0, p.size * 2.0);
        } else {
            dot(c, x, y, p.size, &color);
        }
    }
    c.set_global_alpha(1.0);
    let _ = c.set_global_composite_operation("source-over");

    // People.
    let n = app.people.len();
    let radius = app.dot_radius.max(2.5);
    let label_all = n <= 50;
    let on_screen: std::collections::BTreeSet<u32> = app
        .screens
        .iter()
        .flatten()
        .flat_map(|s| s.cursors.iter().map(|c| c.person))
        .collect();
    for (i, p) in app.people.iter().enumerate() {
        let id = i as u32;
        let (x, y) = p.pos;
        if !p.arrived {
            dot(c, x, y, radius * 0.5, FAINT);
            continue;
        }
        let typing = p.typing_until > now;
        if typing {
            let pulse = 0.5 + 0.5 * ((now / 140.0) + i as f64).sin();
            c.set_global_alpha(0.25 + 0.35 * pulse);
            c.begin_path();
            let _ = c.arc(
                x,
                y,
                radius * 1.4 + 2.0 + pulse * 2.0,
                0.0,
                std::f64::consts::TAU,
            );
            c.set_stroke_style_str(&person_color(id, 66));
            c.set_line_width(1.5);
            c.stroke();
            c.set_global_alpha(1.0);
        }
        if p.online {
            dot(c, x, y, radius, &person_color(id, 58));
        } else if !p.was_online {
            // Signed up, still connecting.
            dot(c, x, y, radius * 0.8, &person_color(id, 30));
        } else {
            dot(c, x, y, radius, PANEL_2);
            c.begin_path();
            let _ = c.arc(x, y, radius, 0.0, std::f64::consts::TAU);
            c.set_stroke_style_str(RED);
            c.set_line_width(1.5);
            c.stroke();
        }
        if radius >= 9.0 {
            let initial: String = p.name.chars().take(1).collect();
            text(
                c,
                &initial,
                x,
                y + 4.0,
                &format!("700 10px {SANS}"),
                BG,
                "center",
            );
        }
        if label_all || (typing && on_screen.contains(&id)) {
            let (dx, dy) = (x - cx, y - cy);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            let (lx, ly) = (x + dx / len * (radius + 6.0), y + dy / len * (radius + 6.0));
            let align = if dx < 0.0 { "right" } else { "left" };
            let color = if typing {
                person_color(id, 72)
            } else {
                DIM.to_string()
            };
            text(
                c,
                &p.name,
                lx,
                ly + 4.0,
                &format!("500 10px {SANS}"),
                &color,
                align,
            );
        }
    }

    // The server.
    let (bw, bh) = (196.0, 128.0);
    let (bx, by) = (server.0 - bw / 2.0, server.1 - bh / 2.0);
    rrect(c, bx, by, bw, bh, 14.0);
    c.set_fill_style_str(PANEL_2);
    c.fill();
    let border = if down {
        if (now / 300.0) as i64 % 2 == 0 {
            RED
        } else {
            "#7a2533"
        }
    } else {
        CYAN
    };
    c.set_stroke_style_str(border);
    c.set_line_width(2.0);
    c.stroke();
    if down {
        text(
            c,
            "DOWN",
            server.0,
            by + 52.0,
            &format!("800 26px {SANS}"),
            RED,
            "center",
        );
        text(
            c,
            "killed with SIGKILL",
            server.0,
            by + 76.0,
            &format!("500 11px {SANS}"),
            DIM,
            "center",
        );
        let left = app
            .down_for
            .map(|(secs, at)| (secs as f64 - (now - at) / 1000.0).max(0.0));
        let msg = match left {
            Some(l) if l >= 0.5 => format!("back in {l:.0} s"),
            _ => "starting again…".to_string(),
        };
        text(
            c,
            &msg,
            server.0,
            by + 98.0,
            &format!("700 14px {MONO}"),
            AMBER,
            "center",
        );
    } else {
        text(
            c,
            "SERVER",
            bx + 14.0,
            by + 22.0,
            &format!("700 10px {SANS}"),
            DIM,
            "left",
        );
        text(
            c,
            &format!("{:.0} req/s", stats.http_rps),
            bx + bw - 14.0,
            by + 22.0,
            &format!("600 11px {MONO}"),
            CYAN,
            "right",
        );
        let hello = app.hello.clone();
        let cpu_limit = hello
            .as_ref()
            .map(|h| h.cpus as f64 * 100.0)
            .filter(|l| *l > 0.0);
        let mem_limit = hello.as_ref().map(|h| h.memory).filter(|m| *m > 0);
        bar(
            c,
            bx + 14.0,
            by + 40.0,
            bw - 28.0,
            "CPU",
            &match cpu_limit {
                Some(l) => format!("{:.0}% of {:.0}%", stats.cpu_pct, l),
                None => format!("{:.0}%", stats.cpu_pct),
            },
            stats.cpu_pct as f64 / cpu_limit.unwrap_or(100.0),
            GREEN,
        );
        bar(
            c,
            bx + 14.0,
            by + 72.0,
            bw - 28.0,
            "MEMORY",
            &match mem_limit {
                Some(l) => format!("{:.0} / {:.0} MB", mb(stats.mem_bytes), mb(l)),
                None => format!("{:.0} MB", mb(stats.mem_bytes)),
            },
            stats.mem_bytes as f64 / mem_limit.unwrap_or(512 << 20) as f64,
            PURPLE,
        );
        text(
            c,
            "stores only ciphertext",
            server.0,
            by + bh - 12.0,
            &format!("italic 400 11px {SANS}"),
            DIM,
            "center",
        );
    }
    let troubled = down || matches!(app.phase, Some(Phase::Crash) | Some(Phase::Recovering));
    if stats.queued > 0 && troubled {
        let msg = format!(
            "{} changes waiting on people's devices",
            thousands(stats.queued as u64)
        );
        let font = format!("600 12px {SANS}");
        let fw = width(c, &msg, &font) + 24.0;
        rrect(c, server.0 - fw / 2.0, by + bh + 12.0, fw, 26.0, 13.0);
        c.set_fill_style_str("rgba(255,181,71,0.15)");
        c.fill();
        text(c, &msg, server.0, by + bh + 29.0, &font, AMBER, "center");
    }

    // Legend and feed.
    let lf = format!("500 11px {SANS}");
    let mut lx = r.x + 16.0;
    let ly = r.y + 22.0;
    for (i, hue) in [12u32, 77, 140].iter().enumerate() {
        dot(
            c,
            lx + i as f64 * 8.0,
            ly - 4.0,
            3.5,
            &person_color(*hue, 62),
        );
    }
    let label = "edits, in their writer's colour: up to the server, then out to the others";
    text(c, label, lx + 24.0, ly, &lf, DIM, "left");
    lx += width(c, label, &lf) + 50.0;
    c.set_fill_style_str(AMBER);
    c.fill_rect(lx - 3.5, ly - 7.5, 7.0, 7.0);
    text(c, "a picture", lx + 9.0, ly, &lf, DIM, "left");
    lx += width(c, "a picture", &lf) + 30.0;
    c.begin_path();
    let _ = c.arc(lx, ly - 4.0, 3.5, 0.0, std::f64::consts::TAU);
    c.set_stroke_style_str(RED);
    c.set_line_width(1.5);
    c.stroke();
    text(c, "offline", lx + 9.0, ly, &lf, DIM, "left");
    levels_panel(app, c, r);
    let shown = app.feed.len().min(6);
    if shown > 0 {
        let ff = format!("500 12px {SANS}");
        let fw = app
            .feed
            .iter()
            .take(shown)
            .map(|(m, _, _)| width(c, m, &ff))
            .fold(0.0, f64::max)
            + 44.0;
        let fh = shown as f64 * 18.0 + 12.0;
        rrect(c, r.x + 8.0, r.y + r.h - fh - 6.0, fw, fh, 10.0);
        c.set_fill_style_str("rgba(10,13,19,0.86)");
        c.fill();
    }
    let mut fy = r.y + r.h - 14.0;
    for (i, (msg, kind, at)) in app.feed.iter().take(6).enumerate() {
        let age = (now - at) / 1000.0;
        let alpha = (1.0 - i as f64 * 0.15).max(0.15) * if age < 0.4 { age / 0.4 } else { 1.0 };
        c.set_global_alpha(alpha.clamp(0.0, 1.0));
        dot(c, r.x + 20.0, fy - 4.0, 3.0, kind_color(*kind));
        text(
            c,
            msg,
            r.x + 30.0,
            fy,
            &format!("500 12px {SANS}"),
            TEXT,
            "left",
        );
        fy -= 18.0;
    }
    c.set_global_alpha(1.0);
}

/// A row of a levels table: (text, right edge) per column.
fn level_cells(l: &LevelResult) -> Vec<String> {
    vec![
        l.level.to_string(),
        l.users.to_string(),
        format!("{:.0}", l.avg_http_rps),
        format!("{:.0}", l.peak_http_rps),
        format!("{:.0}%", l.peak_cpu_pct),
        format!("{:.0} MB", mb(l.peak_mem_bytes)),
        format!(
            "{} / {}",
            duration(l.propagation_p50_ms),
            duration(l.propagation_p99_ms)
        ),
        format!("{} s", l.downtime_s),
        l.online_s
            .map(|s| format!("{s:.1} s"))
            .unwrap_or_else(|| "—".into()),
        l.recovery_s
            .map(|s| format!("{s:.1} s"))
            .unwrap_or_else(|| "never".into()),
        thousands(l.max_queued as u64),
    ]
}

const LEVEL_HEADS: [&str; 11] = [
    "LEVEL",
    "PEOPLE",
    "REQ/S",
    "PEAK",
    "CPU",
    "MEMORY",
    "KEYSTROKE p50/p99",
    "DOWN",
    "ONLINE IN",
    "CAUGHT UP",
    "MOST QUEUED",
];

/// Which columns, and their right edges as fractions of the width (the
/// first is left-aligned): all of them, or a compact set for the corner.
fn level_columns(full: bool) -> Vec<(usize, f64)> {
    if full {
        vec![
            (0, 0.0),
            (1, 0.12),
            (2, 0.21),
            (3, 0.29),
            (4, 0.36),
            (5, 0.46),
            (6, 0.61),
            (7, 0.68),
            (8, 0.78),
            (9, 0.88),
            (10, 1.0),
        ]
    } else {
        vec![
            (0, 0.0),
            (1, 0.19),
            (2, 0.32),
            (4, 0.43),
            (6, 0.70),
            (7, 0.80),
            (9, 1.0),
        ]
    }
}

#[allow(clippy::too_many_arguments)]
fn level_table(
    c: &Ctx,
    x: f64,
    y: f64,
    w: f64,
    rows: &[LevelResult],
    now_row: Option<Vec<String>>,
    row_h: f64,
    full: bool,
) -> f64 {
    let cols = level_columns(full);
    let hf = format!("700 9px {SANS}");
    for &(i, f) in &cols {
        let (cx, align) = if f == 0.0 {
            (x, "left")
        } else {
            (x + f * w, "right")
        };
        text(c, LEVEL_HEADS[i], cx, y, &hf, DIM, align);
    }
    let mut ry = y + row_h;
    let rf = format!("500 11px {MONO}");
    let all: Vec<(Vec<String>, &str)> = rows
        .iter()
        .map(|l| (level_cells(l), TEXT))
        .chain(now_row.map(|r| (r, AMBER)))
        .collect();
    for (cells, color) in &all {
        for &(i, f) in &cols {
            let (cx, align) = if f == 0.0 {
                (x, "left")
            } else {
                (x + f * w, "right")
            };
            text(c, &cells[i], cx, ry, &rf, color, align);
        }
        ry += row_h;
    }
    ry - y
}

/// The levels so far, in the world's top-right corner.
fn levels_panel(app: &App, c: &Ctx, r: &Rect) {
    let Some((level, of, users, down)) = app.level else {
        return;
    };
    if of < 2 || app.report.is_some() {
        return;
    }
    let shown: Vec<LevelResult> = app.levels.iter().rev().take(5).rev().cloned().collect();
    let current = (app.levels.iter().all(|l| l.level != level)).then(|| {
        vec![
            level.to_string(),
            users.to_string(),
            "…".into(),
            "".into(),
            "".into(),
            "".into(),
            "".into(),
            format!("{down} s"),
            "".into(),
            "now".into(),
            "".into(),
        ]
    });
    let rows = shown.len() + usize::from(current.is_some());
    let w = 400.0_f64.min(r.w * 0.45);
    let h = 34.0 + rows as f64 * 15.0;
    let (x, y) = (r.x + r.w - w - 12.0, r.y + 10.0);
    rrect(c, x, y, w, h, 10.0);
    c.set_fill_style_str("rgba(10,13,19,0.88)");
    c.fill();
    level_table(
        c,
        x + 12.0,
        y + 18.0,
        w - 24.0,
        &shown,
        current,
        15.0,
        false,
    );
}

#[allow(clippy::too_many_arguments)]
fn bar(c: &Ctx, x: f64, y: f64, w: f64, label: &str, value: &str, frac: f64, color: &str) {
    text(c, label, x, y, &format!("700 9px {SANS}"), DIM, "left");
    text(
        c,
        value,
        x + w,
        y,
        &format!("500 10px {MONO}"),
        TEXT,
        "right",
    );
    rrect(c, x, y + 6.0, w, 7.0, 3.5);
    c.set_fill_style_str(LINE);
    c.fill();
    let f = frac.clamp(0.0, 1.0);
    if f > 0.0 {
        rrect(c, x, y + 6.0, (w * f).max(7.0), 7.0, 3.5);
        c.set_fill_style_str(if f > 0.9 { RED } else { color });
        c.fill();
    }
}

fn draw_charts(app: &App, c: &Ctx, r: &Rect) {
    let s: Vec<&Stats> = app.stats.iter().collect();
    let last = s.last().copied().cloned().unwrap_or_default();
    let hello = app.hello.clone();
    let cpu_limit = hello
        .as_ref()
        .map(|h| h.cpus as f64 * 100.0)
        .filter(|l| *l > 0.0);
    let mem_limit = hello.as_ref().map(|h| mb(h.memory)).filter(|m| *m > 0.0);
    let series = |f: &dyn Fn(&Stats) -> f64| -> Vec<f64> { s.iter().map(|x| f(x)).collect() };
    // People who have arrived so far (later levels' seats are still empty).
    let here = app.people.iter().filter(|p| p.arrived).count();
    let marks: Vec<f64> = match (s.first(), s.last()) {
        (Some(a), Some(b)) if b.t_ms > a.t_ms => app
            .level_starts
            .iter()
            .skip(1)
            .map(|&t| (t.saturating_sub(a.t_ms)) as f64 / (b.t_ms - a.t_ms) as f64)
            .filter(|f| (0.0..=1.0).contains(f))
            .collect(),
        _ => Vec::new(),
    };
    let cards: Vec<Card> = vec![
        Card {
            title: "SERVER CPU",
            value: match cpu_limit {
                Some(l) => format!("{:.0}% of {:.0}%", last.cpu_pct, l),
                None => format!("{:.0}%", last.cpu_pct),
            },
            sub: String::new(),
            lines: vec![(series(&|x| x.cpu_pct as f64), GREEN)],
            limit: cpu_limit,
            marks: marks.clone(),
        },
        Card {
            title: "SERVER MEMORY",
            value: format!("{:.0} MB", mb(last.mem_bytes)),
            sub: mem_limit
                .map(|m| format!("limit {m:.0} MB"))
                .unwrap_or_default(),
            lines: vec![(series(&|x| mb(x.mem_bytes)), PURPLE)],
            limit: mem_limit,
            marks: marks.clone(),
        },
        Card {
            title: "REQUESTS / S",
            value: format!("{:.0}", last.http_rps),
            sub: format!("p99 {:.1} ms at the server", last.http_p99_ms),
            lines: vec![(series(&|x| x.http_rps as f64), CYAN)],
            limit: None,
            marks: marks.clone(),
        },
        Card {
            title: "EDITS / S",
            value: format!("{:.0}", last.actions_ps),
            sub: format!("{} edits in all", thousands(last.edits)),
            lines: vec![(series(&|x| x.actions_ps as f64), "#60a5fa")],
            limit: None,
            marks: marks.clone(),
        },
        Card {
            title: "KEYSTROKE → OTHERS' SCREENS",
            value: format!("{:.0} ms", last.propagation_p50_ms),
            sub: format!("p99 {:.0} ms", last.propagation_p99_ms),
            lines: vec![
                (series(&|x| x.propagation_p99_ms as f64), "#f472b6"),
                (series(&|x| x.propagation_p50_ms as f64), AMBER),
            ],
            limit: None,
            marks: marks.clone(),
        },
        Card {
            title: "PEOPLE ONLINE",
            value: format!("{} / {here}", last.online),
            sub: format!(
                "{} notes · {} pictures · {} errors",
                last.notes, last.pictures, last.errors
            ),
            lines: vec![(series(&|x| x.online as f64), GREEN)],
            limit: Some(here.max(1) as f64),
            marks: marks.clone(),
        },
    ];
    let cols = if r.w > 840.0 { 6 } else { 3 };
    let rows = cards.len().div_ceil(cols);
    let gap = 12.0;
    let cw = (r.w - gap * (cols as f64 - 1.0)) / cols as f64;
    let ch = (r.h - gap * (rows as f64 - 1.0)) / rows as f64;
    for (i, card) in cards.iter().enumerate() {
        let x = r.x + (i % cols) as f64 * (cw + gap);
        let y = r.y + (i / cols) as f64 * (ch + gap);
        chart(c, &Rect { x, y, w: cw, h: ch }, card);
    }
}

struct Card {
    title: &'static str,
    value: String,
    sub: String,
    lines: Vec<(Vec<f64>, &'static str)>,
    limit: Option<f64>,
    /// Where levels began, as fractions of the run so far.
    marks: Vec<f64>,
}

/// At most `n` points: each the highest of its share of `v`.
fn downsample(v: &[f64], n: usize) -> Vec<f64> {
    if v.len() <= n {
        return v.to_vec();
    }
    (0..n)
        .map(|i| {
            let a = i * v.len() / n;
            let b = ((i + 1) * v.len() / n).max(a + 1);
            v[a..b].iter().copied().fold(f64::MIN, f64::max)
        })
        .collect()
}

fn chart(c: &Ctx, r: &Rect, card: &Card) {
    panel(c, r);
    text(
        c,
        card.title,
        r.x + 12.0,
        r.y + 20.0,
        &format!("700 9px {SANS}"),
        DIM,
        "left",
    );
    text(
        c,
        &card.value,
        r.x + 12.0,
        r.y + 44.0,
        &format!("700 18px {MONO}"),
        TEXT,
        "left",
    );
    if !card.sub.is_empty() {
        text(
            c,
            &card.sub,
            r.x + 12.0,
            r.y + 60.0,
            &format!("400 10px {SANS}"),
            DIM,
            "left",
        );
    }
    let (gx, gy, gw) = (r.x + 10.0, r.y + 68.0, r.w - 20.0);
    let gh = r.h - 78.0;
    if gh < 12.0 {
        return;
    }
    // The whole run, a bucket per two pixels (each bucket's highest value).
    let n = ((gw / 2.0) as usize).max(2);
    let lines: Vec<(Vec<f64>, &str)> = card
        .lines
        .iter()
        .map(|(s, color)| (downsample(s, n), *color))
        .collect();
    let mut max = card.limit.unwrap_or(0.0);
    for (s, _) in &lines {
        for v in s {
            max = max.max(*v);
        }
    }
    let max = max.max(1.0) * 1.1;
    if let Some(l) = card.limit {
        let y = gy + gh - gh * (l / max);
        let _ = c.set_line_dash(&js_sys::Array::of2(&4.0.into(), &4.0.into()));
        c.set_stroke_style_str(RED);
        c.set_global_alpha(0.5);
        c.begin_path();
        c.move_to(gx, y);
        c.line_to(gx + gw, y);
        c.stroke();
        c.set_global_alpha(1.0);
        let _ = c.set_line_dash(&js_sys::Array::new());
    }
    // Where levels began.
    for f in &card.marks {
        let x = gx + gw * f;
        c.set_fill_style_str(LINE);
        c.fill_rect(x, gy, 1.0, gh);
    }
    for (pts, color) in &lines {
        if pts.len() < 2 {
            continue;
        }
        let step = gw / (pts.len() - 1) as f64;
        let x0 = gx;
        c.begin_path();
        for (i, v) in pts.iter().enumerate() {
            let x = x0 + step * i as f64;
            let y = gy + gh - gh * (v / max);
            if i == 0 {
                c.move_to(x, y);
            } else {
                c.line_to(x, y);
            }
        }
        c.set_stroke_style_str(color);
        c.set_line_width(1.6);
        c.stroke();
        // A soft fill under the first line.
        c.line_to(gx + gw, gy + gh);
        c.line_to(x0, gy + gh);
        c.close_path();
        c.set_fill_style_str(color);
        c.set_global_alpha(0.08);
        c.fill();
        c.set_global_alpha(1.0);
    }
}

fn draw_screens(app: &App, c: &Ctx, r: &Rect, now: f64) {
    let gap = 12.0;
    let sh = (r.h - gap * (SLOTS as f64 - 1.0)) / SLOTS as f64;
    for slot in 0..SLOTS {
        let rect = Rect {
            x: r.x,
            y: r.y + slot as f64 * (sh + gap),
            w: r.w,
            h: sh,
        };
        screen(app, c, &rect, slot, now);
    }
}

fn screen(app: &App, c: &Ctx, r: &Rect, slot: usize, now: f64) {
    panel(c, r);
    // Window chrome.
    for (i, col) in ["#ff5f57", "#febc2e", "#28c840"].iter().enumerate() {
        dot(c, r.x + 16.0 + i as f64 * 14.0, r.y + 17.0, 4.5, col);
    }
    let Some(s) = &app.screens[slot] else {
        text(
            c,
            "waiting for a team to start writing…",
            r.x + r.w / 2.0,
            r.y + r.h / 2.0,
            &format!("400 12px {SANS}"),
            FAINT,
            "center",
        );
        return;
    };
    let team = app.teams.get(&s.team);
    let team_name = team.map(|t| t.name.as_str()).unwrap_or("");
    text(
        c,
        &s.title,
        r.x + 64.0,
        r.y + 21.0,
        &format!("600 12px {SANS}"),
        TEXT,
        "left",
    );
    let tw = width(c, &s.title, &format!("600 12px {SANS}"));
    text(
        c,
        &format!("in {team_name}"),
        r.x + 70.0 + tw,
        r.y + 21.0,
        &format!("400 12px {SANS}"),
        DIM,
        "left",
    );
    // Who is in it.
    if let Some(t) = team {
        let mut x = r.x + r.w - 16.0;
        for &m in t.members.iter().rev() {
            let typing = app
                .people
                .get(m as usize)
                .is_some_and(|p| p.typing_until > now);
            dot(
                c,
                x,
                r.y + 17.0,
                7.0,
                &person_color(m, if typing { 62 } else { 34 }),
            );
            let initial: String = app
                .people
                .get(m as usize)
                .map(|p| p.name.chars().take(1).collect())
                .unwrap_or_default();
            text(
                c,
                &initial,
                x,
                r.y + 20.5,
                &format!("700 8px {SANS}"),
                BG,
                "center",
            );
            x -= 17.0;
        }
    }
    c.set_fill_style_str(LINE);
    c.fill_rect(r.x, r.y + 32.0, r.w, 1.0);

    // The note.
    let cipher_h = 64.0;
    let body = Rect {
        x: r.x + 16.0,
        y: r.y + 40.0,
        w: r.w - 32.0,
        h: r.h - 40.0 - cipher_h - 8.0,
    };
    let font = format!("400 12px {MONO}");
    let cw = width(c, "M", &font).max(1.0);
    let lh = 16.0;
    let measure = |t: &str| t.chars().count() as f64 * cw;
    // Visual rows, with their style and any cursors on them.
    struct VRow {
        text: String,
        style: u8,
        color: Option<u32>,
        cursors: Vec<(u32, f64)>,
    }
    let mut rows: Vec<VRow> = Vec::new();
    let mut heading_person = None;
    for (li, line) in s.lines.iter().enumerate() {
        let (style, shown) = if let Some(h) = line.strip_prefix("## ") {
            heading_person = app
                .people
                .iter()
                .position(|p| p.name == h)
                .map(|i| i as u32);
            (2u8, line.as_str())
        } else if line.starts_with("# ") {
            (1u8, line.as_str())
        } else {
            (0u8, line.as_str())
        };
        let wrapped = wrap::wrap(shown, body.w, measure);
        let start = rows.len();
        for w in &wrapped {
            rows.push(VRow {
                text: w.text.clone(),
                style,
                color: if style == 2 { heading_person } else { None },
                cursors: Vec::new(),
            });
        }
        for cur in s.cursors.iter().filter(|cu| cu.line as usize == li) {
            let (ri, off) = wrap::locate(&wrapped, cur.col as usize);
            if let Some(row) = rows.get_mut(start + ri) {
                row.cursors.push((cur.person, off as f64 * cw));
            }
        }
    }
    let fit = (body.h / lh).floor().max(1.0) as usize;
    let first = rows.len().saturating_sub(fit);
    c.save();
    rrect(c, body.x - 4.0, body.y, body.w + 8.0, body.h, 4.0);
    c.clip();
    let mut y = body.y + 13.0;
    for row in &rows[first..] {
        match row.style {
            1 => text(
                c,
                &row.text,
                body.x,
                y,
                &format!("700 13px {MONO}"),
                TEXT,
                "left",
            ),
            2 => {
                let col = row
                    .color
                    .map(|p| person_color(p, 68))
                    .unwrap_or_else(|| TEXT.to_string());
                text(
                    c,
                    &row.text,
                    body.x,
                    y,
                    &format!("700 12px {MONO}"),
                    &col,
                    "left",
                );
            }
            _ => segments(c, &row.text, body.x, y, &font, cw),
        }
        for (p, x) in &row.cursors {
            let col = person_color(*p, 64);
            let blink = ((now / 530.0) as i64 % 2 == 0) as u8 as f64 * 0.6 + 0.4;
            c.set_global_alpha(blink);
            c.set_fill_style_str(&col);
            c.fill_rect(body.x + x, y - 12.0, 2.0, 15.0);
            c.set_global_alpha(1.0);
            let name = app
                .people
                .get(*p as usize)
                .map(|v| v.name.as_str())
                .unwrap_or("");
            let nf = format!("700 9px {SANS}");
            let nw = width(c, name, &nf) + 8.0;
            rrect(c, body.x + x, y - 24.0, nw, 12.0, 3.0);
            c.set_fill_style_str(&col);
            c.fill();
            text(c, name, body.x + x + 4.0, y - 15.0, &nf, BG, "left");
        }
        y += lh;
    }
    c.restore();

    // What the server holds for it.
    let cy = r.y + r.h - cipher_h;
    c.set_fill_style_str(LINE);
    c.fill_rect(r.x, cy - 4.0, r.w, 1.0);
    match &app.ciphers[slot] {
        Some(ci) => {
            let fresh = ((now - ci.at) / 900.0).clamp(0.0, 1.0);
            text(
                c,
                &format!(
                    "ON THE SERVER · change #{} · {} bytes of ciphertext",
                    ci.seq, ci.size
                ),
                r.x + 16.0,
                cy + 12.0,
                &format!("700 9px {SANS}"),
                DIM,
                "left",
            );
            let hf = format!("400 11px {MONO}");
            let hw = width(c, "00 ", &hf).max(1.0);
            let per_row = ((r.w - 32.0) / hw).floor().max(4.0) as usize;
            let pairs: Vec<&str> = (0..ci.hex.len() / 2)
                .map(|i| &ci.hex[i * 2..i * 2 + 2])
                .collect();
            let green = format!("rgba(61,220,151,{:.2})", 0.95 - fresh * 0.5);
            for (row, chunk) in pairs.chunks(per_row).take(3).enumerate() {
                text(
                    c,
                    &chunk.join(" "),
                    r.x + 16.0,
                    cy + 28.0 + row as f64 * 14.0,
                    &hf,
                    &green,
                    "left",
                );
            }
        }
        None => text(
            c,
            "ON THE SERVER · waiting for the first change",
            r.x + 16.0,
            cy + 12.0,
            &format!("700 9px {SANS}"),
            FAINT,
            "left",
        ),
    }
}

/// A line of note text: [[links]] and ![[pictures]] in their colours (same
/// font, so positions match the cursors').
fn segments(c: &Ctx, s: &str, x: f64, y: f64, font: &str, cw: f64) {
    let mut col = 0usize;
    let mut rest = s;
    while !rest.is_empty() {
        let next = [rest.find("![["), rest.find("[[")]
            .into_iter()
            .flatten()
            .min();
        let Some((i, end)) = next.and_then(|i| rest[i..].find("]]").map(|e| (i, i + e + 2))) else {
            text(c, rest, x + col as f64 * cw, y, font, TEXT, "left");
            return;
        };
        let (plain, seg) = (&rest[..i], &rest[i..end]);
        text(c, plain, x + col as f64 * cw, y, font, TEXT, "left");
        col += plain.chars().count();
        let color = if seg.starts_with('!') { AMBER } else { CYAN };
        text(c, seg, x + col as f64 * cw, y, font, color, "left");
        col += seg.chars().count();
        rest = &rest[end..];
    }
}

fn report(c: &Ctx, world: &Rect, r: &Report, age: f64) {
    let a = (age / 600.0).clamp(0.0, 1.0);
    c.set_global_alpha(0.82 * a);
    c.set_fill_style_str(BG);
    rrect(c, world.x, world.y, world.w, world.h, 12.0);
    c.fill();
    c.set_global_alpha(a);
    if r.levels.len() > 1 {
        return report_levels(c, world, r);
    }
    let (w, h) = (world.w.min(620.0), 380.0_f64.min(world.h - 20.0));
    let (x, y) = (world.x + (world.w - w) / 2.0, world.y + (world.h - h) / 2.0);
    rrect(c, x, y, w, h, 16.0);
    c.set_fill_style_str(PANEL_2);
    c.fill();
    let ok = r.passed();
    c.set_stroke_style_str(if ok { GREEN } else { RED });
    c.set_line_width(2.0);
    c.stroke();
    let headline = if ok {
        "✓  Every copy is the same. Nothing was lost."
    } else {
        "✗  Some copies differ"
    };
    text(
        c,
        headline,
        x + w / 2.0,
        y + 44.0,
        &format!("700 19px {SANS}"),
        if ok { GREEN } else { RED },
        "center",
    );
    let lines = [
        format!(
            "{} people in {} teams · {:.0} s · {} actions · {} edits",
            r.users,
            r.teams,
            r.duration_s,
            thousands(r.actions),
            thousands(r.edits)
        ),
        if r.errors_while_down > 0 {
            format!(
                "{} new notes · {} pictures · {} errors ({} while the server was down)",
                r.notes, r.pictures, r.errors, r.errors_while_down
            )
        } else {
            format!(
                "{} new notes · {} pictures · {} errors",
                r.notes, r.pictures, r.errors
            )
        },
        format!(
            "Server: peak {:.0}% CPU · peak {:.0} MB memory",
            r.peak_cpu_pct,
            mb(r.peak_mem_bytes)
        ),
        format!(
            "{:.0} requests/s on average, {:.0} at the peak · p99 {:.1} ms",
            r.avg_http_rps, r.peak_http_rps, r.http_p99_ms
        ),
        format!(
            "Keystroke → others' screens: p50 {:.0} ms · p99 {:.0} ms",
            r.propagation_p50_ms, r.propagation_p99_ms
        ),
        match (r.recovery_s, r.levels.first().map(|l| l.downtime_s)) {
            (Some(s), _) => format!("After the crash, everyone was back and caught up in {s:.1} s"),
            (None, Some(d)) if d > 0 => {
                "After the crash, it didn't catch up within two minutes".to_string()
            }
            _ => "No crash this run".to_string(),
        },
        format!(
            "Compared {} notes ({} copies): {} differ",
            thousands(r.notes_checked as u64),
            thousands(r.copies_checked as u64),
            r.mismatches
        ),
        format!(
            "{} of {} finished sentences missing",
            r.sentences_missing,
            thousands(r.sentences as u64)
        ),
    ];
    let mut ly = y + 84.0;
    for (i, l) in lines.iter().enumerate() {
        let color = if i >= 6 { TEXT } else { DIM };
        text(
            c,
            l,
            x + w / 2.0,
            ly,
            &format!("500 13px {SANS}"),
            color,
            "center",
        );
        ly += 30.0;
    }
    c.set_global_alpha(1.0);
}

/// The end of a run with levels: how each level went, and the check.
fn report_levels(c: &Ctx, world: &Rect, r: &Report) {
    let rows = r.levels.len() as f64;
    let (w, h) = (
        (world.w - 40.0).min(900.0),
        (150.0 + rows * 21.0).min(world.h - 20.0),
    );
    let (x, y) = (world.x + (world.w - w) / 2.0, world.y + (world.h - h) / 2.0);
    rrect(c, x, y, w, h, 16.0);
    c.set_fill_style_str(PANEL_2);
    c.fill();
    let ok = r.passed();
    c.set_stroke_style_str(if ok { GREEN } else { RED });
    c.set_line_width(2.0);
    c.stroke();
    let headline = if ok {
        "✓  Every copy is the same. Nothing was lost."
    } else {
        "✗  Some copies differ"
    };
    text(
        c,
        headline,
        x + w / 2.0,
        y + 36.0,
        &format!("700 18px {SANS}"),
        if ok { GREEN } else { RED },
        "center",
    );
    text(
        c,
        &format!(
            "{} people in {} teams · {:.0} s · {} edits · compared {} notes ({} copies): {} differ · {} of {} sentences missing",
            r.users,
            r.teams,
            r.duration_s,
            thousands(r.edits),
            thousands(r.notes_checked as u64),
            thousands(r.copies_checked as u64),
            r.mismatches,
            r.sentences_missing,
            thousands(r.sentences as u64)
        ),
        x + w / 2.0,
        y + 60.0,
        &format!("500 12px {SANS}"),
        DIM,
        "center",
    );
    level_table(c, x + 24.0, y + 92.0, w - 48.0, &r.levels, None, 21.0, true);
}

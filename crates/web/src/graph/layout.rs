//! Force-directed layout for the graph view, like Obsidian's: notes push each
//! other apart, links pull them together like springs, and a weak pull
//! towards the middle keeps separate clusters in view. The push between all
//! pairs is approximated with a Barnes–Hut quadtree, so a step costs
//! O(n log n) and vaults with thousands of notes stay smooth.
//!
//! The model follows d3-force (velocity Verlet with an "alpha" that cools
//! down, so the layout settles and stops moving). Deterministic: the same
//! graph gives the same picture.

pub type Point = (f64, f64);

/// Below this, the layout has settled and stops.
pub const ALPHA_MIN: f64 = 0.001;
/// About 300 steps from 1 down to `ALPHA_MIN`.
const ALPHA_DECAY: f64 = 0.0228;
/// Share of velocity lost per step (friction).
const VELOCITY_DECAY: f64 = 0.4;
const LINK_DISTANCE: f64 = 45.0;
/// Push between notes (negative: they repel).
const CHARGE: f64 = -110.0;
/// Barnes–Hut accuracy: a cell further away than its size / `THETA` counts
/// as one point.
const THETA: f64 = 0.9;
/// Farther than this, notes don't push each other any more.
const CHARGE_RANGE: f64 = 900.0;
/// Pull towards the middle.
const GRAVITY: f64 = 0.05;
/// Quadtree depth at which points count as one place.
const MAX_DEPTH: usize = 28;

/// Size of a note's dot in layout units: grows with its links.
pub fn radius(degree: usize) -> f64 {
    3.5 + 1.7 * (degree as f64).sqrt()
}

/// Where the `i`-th new note starts: on a sunflower spiral, so starting
/// points are spread evenly and never coincide.
pub fn spiral(i: usize) -> Point {
    let r = 10.0 * (0.5 + i as f64).sqrt();
    let a = i as f64 * std::f64::consts::PI * (3.0 - 5f64.sqrt());
    (r * a.cos(), r * a.sin())
}

/// A tiny offset for points in exactly the same place (deterministic).
fn jiggle(i: usize) -> f64 {
    ((i % 7) as f64 - 3.0) * 1e-6 + 1e-6
}

pub struct Sim {
    pub pos: Vec<Point>,
    vel: Vec<Point>,
    /// Held in place (being dragged).
    pub fixed: Vec<Option<Point>>,
    edges: Vec<(usize, usize)>,
    /// Per edge: how far the pull moves its first end vs its second.
    bias: Vec<f64>,
    /// Per edge: spring strength (weaker for well-connected notes).
    strength: Vec<f64>,
    pub alpha: f64,
    /// What alpha cools towards: 0, or more while a note is dragged.
    pub alpha_target: f64,
}

impl Sim {
    /// `start(i)`: where note `i` starts (kept from an earlier layout), else
    /// on the spiral.
    pub fn new(
        n: usize,
        edges: Vec<(usize, usize)>,
        start: impl Fn(usize) -> Option<Point>,
    ) -> Sim {
        let mut count = vec![0usize; n];
        for &(a, b) in &edges {
            count[a] += 1;
            count[b] += 1;
        }
        let bias = edges
            .iter()
            .map(|&(a, b)| count[a] as f64 / (count[a] + count[b]) as f64)
            .collect();
        let strength = edges
            .iter()
            .map(|&(a, b)| 1.0 / count[a].min(count[b]).max(1) as f64)
            .collect();
        Sim {
            pos: (0..n)
                .map(|i| start(i).unwrap_or_else(|| spiral(i)))
                .collect(),
            vel: vec![(0.0, 0.0); n],
            fixed: vec![None; n],
            edges,
            bias,
            strength,
            alpha: 1.0,
            alpha_target: 0.0,
        }
    }

    pub fn settled(&self) -> bool {
        self.alpha < ALPHA_MIN && self.alpha_target == 0.0
    }

    /// Move again (after the graph changed), at least this much.
    pub fn reheat(&mut self, alpha: f64) {
        self.alpha = self.alpha.max(alpha);
    }

    pub fn tick(&mut self) {
        self.alpha += (self.alpha_target - self.alpha) * ALPHA_DECAY;
        let alpha = self.alpha;
        self.springs(alpha);
        self.repulsion(alpha);
        for (i, (p, v)) in self.pos.iter_mut().zip(self.vel.iter_mut()).enumerate() {
            v.0 -= p.0 * GRAVITY * alpha;
            v.1 -= p.1 * GRAVITY * alpha;
            match self.fixed[i] {
                Some(f) => {
                    *p = f;
                    *v = (0.0, 0.0);
                }
                None => {
                    v.0 *= 1.0 - VELOCITY_DECAY;
                    v.1 *= 1.0 - VELOCITY_DECAY;
                    p.0 += v.0;
                    p.1 += v.1;
                }
            }
        }
    }

    fn springs(&mut self, alpha: f64) {
        for (k, &(a, b)) in self.edges.iter().enumerate() {
            let mut x = self.pos[b].0 + self.vel[b].0 - self.pos[a].0 - self.vel[a].0;
            let mut y = self.pos[b].1 + self.vel[b].1 - self.pos[a].1 - self.vel[a].1;
            if x == 0.0 {
                x = jiggle(k);
            }
            if y == 0.0 {
                y = jiggle(k + 1);
            }
            let l = (x * x + y * y).sqrt();
            let f = (l - LINK_DISTANCE) / l * alpha * self.strength[k];
            let (x, y) = (x * f, y * f);
            let bias = self.bias[k];
            self.vel[b].0 -= x * bias;
            self.vel[b].1 -= y * bias;
            self.vel[a].0 += x * (1.0 - bias);
            self.vel[a].1 += y * (1.0 - bias);
        }
    }

    fn repulsion(&mut self, alpha: f64) {
        if self.pos.len() < 2 {
            return;
        }
        let tree = Tree::build(&self.pos);
        for i in 0..self.pos.len() {
            let (fx, fy) = tree.force_on(i, &self.pos);
            self.vel[i].0 += fx * alpha;
            self.vel[i].1 += fy * alpha;
        }
    }
}

/// The push one point gets from a mass `strength` at offset (`x`, `y`)
/// (pointing from the point to the mass), as d3's many-body force.
fn push(x: f64, y: f64, strength: f64, i: usize) -> Point {
    let (x, y) = if x == 0.0 && y == 0.0 {
        (jiggle(i), jiggle(i + 3))
    } else {
        (x, y)
    };
    let l = (x * x + y * y).max(1.0);
    (x * strength / l, y * strength / l)
}

struct Cell {
    x0: f64,
    y0: f64,
    size: f64,
    /// Total charge, and its centre.
    charge: f64,
    cx: f64,
    cy: f64,
    kids: Option<[usize; 4]>,
    points: Vec<usize>,
}

/// Barnes–Hut quadtree over the points.
struct Tree {
    cells: Vec<Cell>,
}

impl Tree {
    fn build(pos: &[Point]) -> Tree {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in pos {
            x0 = x0.min(p.0);
            y0 = y0.min(p.1);
            x1 = x1.max(p.0);
            y1 = y1.max(p.1);
        }
        let size = (x1 - x0).max(y1 - y0).max(1e-3) * 1.0001;
        let mut t = Tree {
            cells: vec![Cell::new(x0, y0, size)],
        };
        for i in 0..pos.len() {
            t.insert(0, i, pos, 0);
        }
        t.sum(0, pos);
        t
    }

    fn insert(&mut self, c: usize, i: usize, pos: &[Point], depth: usize) {
        if let Some(kids) = self.cells[c].kids {
            let k = self.cells[c].quadrant(pos[i]);
            return self.insert(kids[k], i, pos, depth + 1);
        }
        if self.cells[c].points.is_empty() || depth >= MAX_DEPTH {
            self.cells[c].points.push(i);
            return;
        }
        // Split: four children, then the points go down.
        let (x0, y0, h) = (self.cells[c].x0, self.cells[c].y0, self.cells[c].size / 2.0);
        let first = self.cells.len();
        for (dx, dy) in [(0.0, 0.0), (h, 0.0), (0.0, h), (h, h)] {
            self.cells.push(Cell::new(x0 + dx, y0 + dy, h));
        }
        self.cells[c].kids = Some([first, first + 1, first + 2, first + 3]);
        for p in std::mem::take(&mut self.cells[c].points) {
            self.insert(c, p, pos, depth);
        }
        self.insert(c, i, pos, depth);
    }

    fn sum(&mut self, c: usize, pos: &[Point]) {
        let (mut q, mut x, mut y) = (0.0, 0.0, 0.0);
        if let Some(kids) = self.cells[c].kids {
            for k in kids {
                self.sum(k, pos);
                let kid = &self.cells[k];
                q += kid.charge;
                x += kid.cx * kid.charge;
                y += kid.cy * kid.charge;
            }
        } else {
            for &p in &self.cells[c].points {
                q += CHARGE;
                x += pos[p].0 * CHARGE;
                y += pos[p].1 * CHARGE;
            }
        }
        let cell = &mut self.cells[c];
        cell.charge = q;
        if q != 0.0 {
            cell.cx = x / q;
            cell.cy = y / q;
        }
    }

    fn force_on(&self, i: usize, pos: &[Point]) -> Point {
        let (px, py) = pos[i];
        let (mut fx, mut fy) = (0.0, 0.0);
        let mut stack = vec![0];
        while let Some(c) = stack.pop() {
            let cell = &self.cells[c];
            if cell.charge == 0.0 {
                continue;
            }
            let (x, y) = (cell.cx - px, cell.cy - py);
            let l = x * x + y * y;
            if l > CHARGE_RANGE * CHARGE_RANGE {
                continue;
            }
            match cell.kids {
                // Far enough: the whole cell as one point.
                Some(_) if cell.size * cell.size / (THETA * THETA) < l => {
                    let (dx, dy) = push(x, y, cell.charge, i);
                    fx += dx;
                    fy += dy;
                }
                Some(kids) => stack.extend(kids),
                None => {
                    for &p in cell.points.iter().filter(|&&p| p != i) {
                        let (dx, dy) = push(pos[p].0 - px, pos[p].1 - py, CHARGE, i);
                        fx += dx;
                        fy += dy;
                    }
                }
            }
        }
        (fx, fy)
    }
}

impl Cell {
    fn new(x0: f64, y0: f64, size: f64) -> Cell {
        Cell {
            x0,
            y0,
            size,
            charge: 0.0,
            cx: 0.0,
            cy: 0.0,
            kids: None,
            points: Vec::new(),
        }
    }

    fn quadrant(&self, p: Point) -> usize {
        let h = self.size / 2.0;
        let right = p.0 >= self.x0 + h;
        let below = p.1 >= self.y0 + h;
        right as usize + 2 * below as usize
    }
}

/// How the layout is looked at: a screen point is `world × scale + (tx, ty)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub scale: f64,
    pub tx: f64,
    pub ty: f64,
}

impl View {
    pub const MIN_SCALE: f64 = 0.05;
    pub const MAX_SCALE: f64 = 6.0;

    pub fn to_screen(&self, p: Point) -> Point {
        (p.0 * self.scale + self.tx, p.1 * self.scale + self.ty)
    }

    pub fn to_world(&self, s: Point) -> Point {
        ((s.0 - self.tx) / self.scale, (s.1 - self.ty) / self.scale)
    }

    /// Zoom by `factor`, keeping what is under `at` (a screen point) there.
    pub fn zoom_at(&mut self, at: Point, factor: f64) {
        let w = self.to_world(at);
        self.scale = (self.scale * factor).clamp(Self::MIN_SCALE, Self::MAX_SCALE);
        self.tx = at.0 - w.0 * self.scale;
        self.ty = at.1 - w.1 * self.scale;
    }

    /// Everything in `points` within a `width` × `height` screen, `margin`
    /// pixels from its edges, centred; never closer than 1.6×.
    pub fn fit(points: &[Point], width: f64, height: f64, margin: f64) -> View {
        if points.is_empty() {
            return View {
                scale: 1.0,
                tx: width / 2.0,
                ty: height / 2.0,
            };
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in points {
            x0 = x0.min(p.0);
            y0 = y0.min(p.1);
            x1 = x1.max(p.0);
            y1 = y1.max(p.1);
        }
        let room = |s: f64| (s - 2.0 * margin).max(1.0);
        let scale = (room(width) / (x1 - x0).max(1.0))
            .min(room(height) / (y1 - y0).max(1.0))
            .clamp(Self::MIN_SCALE, 1.6);
        View {
            scale,
            tx: width / 2.0 - (x0 + x1) / 2.0 * scale,
            ty: height / 2.0 - (y0 + y1) / 2.0 * scale,
        }
    }
}

/// The note under screen point `at`: the nearest one whose dot (`radii`, in
/// layout units) is within `slop` screen pixels of it.
pub fn hit(pos: &[Point], radii: &[f64], view: &View, at: Point, slop: f64) -> Option<usize> {
    pos.iter()
        .zip(radii)
        .enumerate()
        .filter_map(|(i, (p, r))| {
            let s = view.to_screen(*p);
            let d = ((s.0 - at.0).powi(2) + (s.1 - at.1).powi(2)).sqrt();
            (d <= r * view.scale + slop).then_some((i, d))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: Point, b: Point) -> f64 {
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
    }

    fn settle(sim: &mut Sim) -> usize {
        let mut n = 0;
        while !sim.settled() {
            sim.tick();
            n += 1;
            assert!(n < 2000, "never settles");
        }
        n
    }

    #[test]
    fn linked_notes_end_up_closer_and_it_settles() {
        // Two triangles joined by one link, and a lone note.
        let edges = vec![(0, 1), (1, 2), (2, 0), (3, 4), (4, 5), (5, 3), (2, 3)];
        let mut sim = Sim::new(7, edges, |_| None);
        let steps = settle(&mut sim);
        assert!((250..400).contains(&steps), "{steps} steps");
        assert!(sim.pos.iter().all(|p| p.0.is_finite() && p.1.is_finite()));
        let linked = dist(sim.pos[0], sim.pos[1]);
        let apart = dist(sim.pos[0], sim.pos[5]);
        assert!(linked < apart, "{linked} vs {apart}");
        assert!(linked > 10.0, "not on top of each other: {linked}");
        // Notes repel: the lone one isn't left in the middle of a cluster.
        for i in 0..6 {
            assert!(dist(sim.pos[6], sim.pos[i]) > 15.0);
        }
        // The same graph gives the same picture.
        let mut again = Sim::new(
            7,
            vec![(0, 1), (1, 2), (2, 0), (3, 4), (4, 5), (5, 3), (2, 3)],
            |_| None,
        );
        settle(&mut again);
        assert_eq!(sim.pos, again.pos);
    }

    #[test]
    fn points_in_one_place_still_come_apart() {
        let mut sim = Sim::new(5, vec![], |_| Some((3.0, 3.0)));
        settle(&mut sim);
        for i in 0..5 {
            for j in i + 1..5 {
                assert!(dist(sim.pos[i], sim.pos[j]) > 1.0, "{:?}", sim.pos);
            }
        }
    }

    #[test]
    fn the_quadtree_approximates_the_exact_push() {
        let pos: Vec<Point> = (0..400).map(|i| spiral(i * 3)).collect();
        let tree = Tree::build(&pos);
        for i in [0, 57, 199, 399] {
            let approx = tree.force_on(i, &pos);
            let mut exact = (0.0, 0.0);
            for (j, p) in pos.iter().enumerate().filter(|(j, _)| *j != i) {
                let (x, y) = (p.0 - pos[i].0, p.1 - pos[i].1);
                if x * x + y * y <= CHARGE_RANGE * CHARGE_RANGE {
                    let f = push(x, y, CHARGE, j);
                    exact.0 += f.0;
                    exact.1 += f.1;
                }
            }
            let err = dist(approx, exact) / dist(exact, (0.0, 0.0)).max(1e-9);
            assert!(err < 0.15, "note {i}: {approx:?} vs {exact:?}");
        }
    }

    #[test]
    fn a_held_note_stays_where_it_is_held() {
        let mut sim = Sim::new(3, vec![(0, 1), (1, 2)], |_| None);
        sim.fixed[1] = Some((100.0, -40.0));
        sim.alpha_target = 0.3;
        for _ in 0..50 {
            sim.tick();
        }
        assert_eq!(sim.pos[1], (100.0, -40.0));
        assert!(!sim.settled(), "keeps moving while held");
        assert!(
            dist(sim.pos[0], sim.pos[1]) < 120.0,
            "its neighbours follow"
        );
    }

    #[test]
    fn views_fit_zoom_and_find_notes() {
        let pos = vec![(-50.0, 0.0), (50.0, 20.0), (0.0, -30.0)];
        let v = View::fit(&pos, 400.0, 300.0, 20.0);
        for p in &pos {
            let s = v.to_screen(*p);
            assert!(
                (20.0..=380.0).contains(&s.0) && (20.0..=280.0).contains(&s.1),
                "{s:?}"
            );
        }
        // What is under the cursor stays there.
        let mut z = v;
        let at = (123.0, 77.0);
        let before = z.to_world(at);
        z.zoom_at(at, 2.5);
        let after = z.to_world(at);
        assert!(dist(before, after) < 1e-9);
        assert!((z.scale / v.scale - 2.5).abs() < 1e-9);
        // Hits: the nearest dot within its radius and some slack.
        let radii = vec![5.0; 3];
        let s1 = v.to_screen(pos[1]);
        assert_eq!(hit(&pos, &radii, &v, (s1.0 + 2.0, s1.1), 4.0), Some(1));
        assert_eq!(hit(&pos, &radii, &v, (0.0, 0.0), 4.0), None);
        // A single note is centred, not zoomed in on.
        let one = View::fit(&[(7.0, 7.0)], 400.0, 300.0, 20.0);
        assert_eq!(one.to_screen((7.0, 7.0)), (200.0, 150.0));
        assert!(one.scale <= 1.6);
    }
}

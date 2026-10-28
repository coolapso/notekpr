//! The outline of SVG path data as polylines. Ported from points-on-path
//! 0.2.1 (MIT, see `crates/draw/PORTED.md`), which rough uses to fill paths.

use super::Point;
use super::path_data::{self, PathError, Segment};
use super::points_on_curve::{points_on_bezier_curves, simplify};

/// The subpaths of `path` as polylines (`pointsOnPath`): curves as points
/// within `tolerance`, each polyline then simplified with `distance` unless
/// that is 0 (or NaN).
pub fn points_on_path(
    path: &str,
    tolerance: f64,
    distance: f64,
) -> Result<Vec<Vec<Point>>, PathError> {
    let segments = path_data::normalize(&path_data::absolutize(&path_data::parse_path(path)?));
    Ok(points_on_segments(&segments, tolerance, distance))
}

/// [`points_on_path`] of path data already normalized.
pub(crate) fn points_on_segments(
    segments: &[Segment],
    tolerance: f64,
    distance: f64,
) -> Vec<Vec<Point>> {
    let mut sets = Vec::new();
    let mut current: Vec<Point> = Vec::new();
    let mut start = [0.0, 0.0];
    let mut pending: Vec<Point> = Vec::new();

    fn append_pending_curve(pending: &mut Vec<Point>, current: &mut Vec<Point>, tolerance: f64) {
        if pending.len() >= 4 {
            current.extend(points_on_bezier_curves(pending, tolerance, None));
        }
        pending.clear();
    }

    for Segment { key, data } in segments {
        match key {
            'M' => {
                append_pending_curve(&mut pending, &mut current, tolerance);
                if !current.is_empty() {
                    sets.push(std::mem::take(&mut current));
                }
                start = [data[0], data[1]];
                current.push(start);
            }
            'L' => {
                append_pending_curve(&mut pending, &mut current, tolerance);
                current.push([data[0], data[1]]);
            }
            'C' => {
                if pending.is_empty() {
                    let last = current.last().copied().unwrap_or(start);
                    pending.push(last);
                }
                pending.extend([[data[0], data[1]], [data[2], data[3]], [data[4], data[5]]]);
            }
            'Z' => {
                append_pending_curve(&mut pending, &mut current, tolerance);
                current.push(start);
            }
            _ => {}
        }
    }
    append_pending_curve(&mut pending, &mut current, tolerance);
    if !current.is_empty() {
        sets.push(current);
    }
    if distance == 0.0 || distance.is_nan() {
        return sets;
    }
    sets.iter()
        .map(|set| simplify(set, distance))
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_closes() {
        let sets = points_on_path("M0 0 L10 0 L10 10 Z M20 20 L30 30", 1.0, 0.0).unwrap();
        assert_eq!(
            sets,
            [
                vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 0.0]],
                vec![[20.0, 20.0], [30.0, 30.0]]
            ]
        );
    }

    #[test]
    fn curves_become_points() {
        let sets = points_on_path("M0 0 C0 100 100 100 100 0", 1.0, 0.0).unwrap();
        assert_eq!(sets.len(), 1);
        assert!(sets[0].len() > 4);
        assert_eq!(sets[0][0], [0.0, 0.0]);
        assert_eq!(*sets[0].last().unwrap(), [100.0, 0.0]);
        // Simplified: fewer points, same ends.
        let simple = points_on_path("M0 0 C0 100 100 100 100 0", 1.0, 1.0).unwrap();
        assert!(simple[0].len() < sets[0].len());
        assert_eq!(simple[0][0], [0.0, 0.0]);
    }

    #[test]
    fn errors_come_from_the_parser() {
        assert_eq!(
            points_on_path("M0 0 L", 1.0, 0.0),
            Err(PathError::EndedShort)
        );
        assert_eq!(points_on_path("", 1.0, 1.0), Ok(vec![]));
    }
}

//! SVG path data: parsing, absolute coordinates, and reduction to
//! move/line/curve/close. Ported from path-data-parser 0.1.0 (MIT, see
//! `crates/draw/PORTED.md`), which rough uses for `generator.path`.

use std::f64::consts::PI;
use std::fmt;

use super::Point;
use crate::js;

/// One path command and its numbers, e.g. `L` with `[x, y]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// The command letter (`M`, `l`, `C`, `z`, …).
    pub key: char,
    pub data: Vec<f64>,
}

impl Segment {
    fn new(key: char, data: Vec<f64>) -> Segment {
        Segment { key, data }
    }
}

/// Why path data could not be read. path-data-parser throws (or, for a
/// number after a close command, loops forever).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathError {
    /// A character that is neither a command, a number nor a separator.
    BadCharacter,
    /// A command where a number was expected.
    ParamNotANumber,
    /// The data ends in the middle of a command.
    EndedShort,
    /// A number right after `Z`/`z`, which takes none.
    NumberAfterClose,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PathError::BadCharacter => "path data has a character it can't contain",
            PathError::ParamNotANumber => "path data has a command where a number belongs",
            PathError::EndedShort => "path data ended short",
            PathError::NumberAfterClose => "path data has a number after a close command",
        })
    }
}

impl std::error::Error for PathError {}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Command(char),
    Number(f64),
    End,
}

/// How many numbers each command takes.
fn params(command: char) -> usize {
    match command.to_ascii_uppercase() {
        'A' => 7,
        'C' => 6,
        'H' | 'V' => 1,
        'L' | 'M' | 'T' => 2,
        'Q' | 'S' => 4,
        _ => 0,
    }
}

/// The length of the number at the start of `s`, as path-data-parser's
/// pattern `([-+]?[0-9]+(\.[0-9]*)?|[-+]?\.[0-9]+)([eE][-+]?[0-9]+)?` finds it.
fn number_len(s: &[u8]) -> Option<usize> {
    let digits = |from: usize| {
        s[from.min(s.len())..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let mut i = usize::from(matches!(s.first(), Some(b'-' | b'+')));
    let whole = digits(i);
    if whole > 0 {
        i += whole;
        if s.get(i) == Some(&b'.') {
            i += 1 + digits(i + 1);
        }
    } else if s.get(i) == Some(&b'.') && digits(i + 1) > 0 {
        i += 1 + digits(i + 1);
    } else {
        return None;
    }
    if matches!(s.get(i), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(s.get(i + 1), Some(b'-' | b'+')));
        let exp = digits(i + 1 + sign);
        if exp > 0 {
            i += 1 + sign + exp;
        }
    }
    Some(i)
}

/// The tokens of `d`, ending with [`Token::End`]; `None` at a character the
/// grammar doesn't know (path-data-parser then fails reading the result).
fn tokenize(d: &str) -> Option<Vec<Token>> {
    let s = d.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b',') {
            i += 1;
        } else if b"aAcChHlLmMqQsStTvVzZ".contains(&c) {
            tokens.push(Token::Command(char::from(c)));
            i += 1;
        } else {
            let n = number_len(&s[i..])?;
            // `${parseFloat(text)}` read back with `+`: the same number,
            // except that -0 prints as "0".
            let x: f64 = d[i..i + n].parse().ok()?;
            tokens.push(Token::Number(if x == 0.0 { 0.0 } else { x }));
            i += n;
        }
    }
    tokens.push(Token::End);
    Some(tokens)
}

/// The commands of SVG path data `d` (`parsePath`). Data that doesn't start
/// with a move is read as if it started with `M0,0`.
pub fn parse_path(d: &str) -> Result<Vec<Segment>, PathError> {
    let tokens = tokenize(d).ok_or(PathError::BadCharacter)?;
    let mut segments = Vec::new();
    // `None` is path-data-parser's "BOD" (beginning of data).
    let mut mode: Option<char> = None;
    let mut index = 0;
    while tokens[index] != Token::End {
        let count;
        match (mode, &tokens[index]) {
            (None, Token::Command(c @ ('M' | 'm'))) => {
                index += 1;
                count = params(*c);
                mode = Some(*c);
            }
            (None, _) => return parse_path(&format!("M0,0{d}")),
            (Some(m), Token::Number(_)) => {
                if params(m) == 0 {
                    return Err(PathError::NumberAfterClose);
                }
                count = params(m);
            }
            (Some(_), Token::Command(c)) => {
                index += 1;
                count = params(*c);
                mode = Some(*c);
            }
            (Some(_), Token::End) => unreachable!("the loop stops at the end"),
        }
        if index + count >= tokens.len() {
            return Err(PathError::EndedShort);
        }
        let mut data = Vec::with_capacity(count);
        for token in &tokens[index..index + count] {
            match token {
                Token::Number(x) => data.push(*x),
                _ => return Err(PathError::ParamNotANumber),
            }
        }
        let key = mode.expect("set above");
        segments.push(Segment::new(key, data));
        index += count;
        // Numbers after a move are lines.
        mode = match key {
            'M' => Some('L'),
            'm' => Some('l'),
            other => Some(other),
        };
    }
    Ok(segments)
}

/// Path data for `segments` (`serialize`), numbers printed as JavaScript
/// does.
pub fn serialize(segments: &[Segment]) -> String {
    let mut tokens: Vec<String> = Vec::new();
    let n = js::number_to_string;
    for Segment { key, data } in segments {
        tokens.push(key.to_string());
        match key {
            'C' | 'c' => tokens.extend([
                n(data[0]),
                format!("{},", n(data[1])),
                n(data[2]),
                format!("{},", n(data[3])),
                n(data[4]),
                n(data[5]),
            ]),
            'S' | 's' | 'Q' | 'q' => tokens.extend([
                n(data[0]),
                format!("{},", n(data[1])),
                n(data[2]),
                n(data[3]),
            ]),
            _ => tokens.extend(data.iter().map(|&x| n(x))),
        }
    }
    tokens.join(" ")
}

/// The same commands with absolute coordinates (`absolutize`): upper-case
/// keys only.
pub fn absolutize(segments: &[Segment]) -> Vec<Segment> {
    let (mut cx, mut cy) = (0.0, 0.0);
    let (mut subx, mut suby) = (0.0, 0.0);
    let mut out = Vec::with_capacity(segments.len());
    // Odd positions are y, even ones x.
    let relative = |data: &[f64], cx: f64, cy: f64| -> Vec<f64> {
        data.iter()
            .enumerate()
            .map(|(i, &d)| if i % 2 == 1 { d + cy } else { d + cx })
            .collect()
    };
    for Segment { key, data } in segments {
        match key {
            'M' => {
                out.push(Segment::new('M', data.clone()));
                (cx, cy) = (data[0], data[1]);
                (subx, suby) = (data[0], data[1]);
            }
            'm' => {
                cx += data[0];
                cy += data[1];
                out.push(Segment::new('M', vec![cx, cy]));
                subx = cx;
                suby = cy;
            }
            'L' => {
                out.push(Segment::new('L', data.clone()));
                (cx, cy) = (data[0], data[1]);
            }
            'l' => {
                cx += data[0];
                cy += data[1];
                out.push(Segment::new('L', vec![cx, cy]));
            }
            'C' => {
                out.push(Segment::new('C', data.clone()));
                cx = data[4];
                cy = data[5];
            }
            'c' => {
                let new = relative(data, cx, cy);
                cx = new[4];
                cy = new[5];
                out.push(Segment::new('C', new));
            }
            'Q' => {
                out.push(Segment::new('Q', data.clone()));
                cx = data[2];
                cy = data[3];
            }
            'q' => {
                let new = relative(data, cx, cy);
                cx = new[2];
                cy = new[3];
                out.push(Segment::new('Q', new));
            }
            'A' => {
                out.push(Segment::new('A', data.clone()));
                cx = data[5];
                cy = data[6];
            }
            'a' => {
                cx += data[5];
                cy += data[6];
                out.push(Segment::new(
                    'A',
                    vec![data[0], data[1], data[2], data[3], data[4], cx, cy],
                ));
            }
            'H' => {
                out.push(Segment::new('H', data.clone()));
                cx = data[0];
            }
            'h' => {
                cx += data[0];
                out.push(Segment::new('H', vec![cx]));
            }
            'V' => {
                out.push(Segment::new('V', data.clone()));
                cy = data[0];
            }
            'v' => {
                cy += data[0];
                out.push(Segment::new('V', vec![cy]));
            }
            'S' => {
                out.push(Segment::new('S', data.clone()));
                cx = data[2];
                cy = data[3];
            }
            's' => {
                let new = relative(data, cx, cy);
                cx = new[2];
                cy = new[3];
                out.push(Segment::new('S', new));
            }
            'T' => {
                out.push(Segment::new('T', data.clone()));
                cx = data[0];
                cy = data[1];
            }
            't' => {
                cx += data[0];
                cy += data[1];
                out.push(Segment::new('T', vec![cx, cy]));
            }
            'Z' | 'z' => {
                out.push(Segment::new('Z', vec![]));
                cx = subx;
                cy = suby;
            }
            _ => {}
        }
    }
    out
}

/// Absolute commands reduced to `M`, `L`, `C` and `Z` (`normalize`):
/// horizontal and vertical lines become lines, quadratic and smooth curves
/// cubic ones, and arcs a few cubic curves.
pub fn normalize(segments: &[Segment]) -> Vec<Segment> {
    let mut out = Vec::with_capacity(segments.len());
    let mut last_type = ' ';
    let (mut cx, mut cy) = (0.0, 0.0);
    let (mut subx, mut suby) = (0.0, 0.0);
    let (mut lcx, mut lcy) = (0.0, 0.0);
    for Segment { key, data } in segments {
        match key {
            'M' => {
                out.push(Segment::new('M', data.clone()));
                (cx, cy) = (data[0], data[1]);
                (subx, suby) = (data[0], data[1]);
            }
            'C' => {
                out.push(Segment::new('C', data.clone()));
                cx = data[4];
                cy = data[5];
                lcx = data[2];
                lcy = data[3];
            }
            'L' => {
                out.push(Segment::new('L', data.clone()));
                (cx, cy) = (data[0], data[1]);
            }
            'H' => {
                cx = data[0];
                out.push(Segment::new('L', vec![cx, cy]));
            }
            'V' => {
                cy = data[0];
                out.push(Segment::new('L', vec![cx, cy]));
            }
            'S' => {
                let (cx1, cy1) = if last_type == 'C' || last_type == 'S' {
                    (cx + (cx - lcx), cy + (cy - lcy))
                } else {
                    (cx, cy)
                };
                out.push(Segment::new(
                    'C',
                    vec![cx1, cy1, data[0], data[1], data[2], data[3]],
                ));
                lcx = data[0];
                lcy = data[1];
                cx = data[2];
                cy = data[3];
            }
            'T' => {
                let (x, y) = (data[0], data[1]);
                let (x1, y1) = if last_type == 'Q' || last_type == 'T' {
                    (cx + (cx - lcx), cy + (cy - lcy))
                } else {
                    (cx, cy)
                };
                let cx1 = cx + 2.0 * (x1 - cx) / 3.0;
                let cy1 = cy + 2.0 * (y1 - cy) / 3.0;
                let cx2 = x + 2.0 * (x1 - x) / 3.0;
                let cy2 = y + 2.0 * (y1 - y) / 3.0;
                out.push(Segment::new('C', vec![cx1, cy1, cx2, cy2, x, y]));
                lcx = x1;
                lcy = y1;
                cx = x;
                cy = y;
            }
            'Q' => {
                let (x1, y1, x, y) = (data[0], data[1], data[2], data[3]);
                let cx1 = cx + 2.0 * (x1 - cx) / 3.0;
                let cy1 = cy + 2.0 * (y1 - cy) / 3.0;
                let cx2 = x + 2.0 * (x1 - x) / 3.0;
                let cy2 = y + 2.0 * (y1 - y) / 3.0;
                out.push(Segment::new('C', vec![cx1, cy1, cx2, cy2, x, y]));
                lcx = x1;
                lcy = y1;
                cx = x;
                cy = y;
            }
            'A' => {
                let r1 = data[0].abs();
                let r2 = data[1].abs();
                let (angle, large_arc_flag, sweep_flag) = (data[2], data[3], data[4]);
                let (x, y) = (data[5], data[6]);
                if r1 == 0.0 || r2 == 0.0 {
                    out.push(Segment::new('C', vec![cx, cy, x, y, x, y]));
                    cx = x;
                    cy = y;
                } else if cx != x || cy != y {
                    let curves = arc_to_cubic_curves(
                        cx,
                        cy,
                        x,
                        y,
                        r1,
                        r2,
                        angle,
                        large_arc_flag,
                        sweep_flag,
                    );
                    out.extend(curves.into_iter().map(|c| Segment::new('C', c.to_vec())));
                    cx = x;
                    cy = y;
                }
            }
            'Z' => {
                out.push(Segment::new('Z', vec![]));
                cx = subx;
                cy = suby;
            }
            _ => {}
        }
        last_type = *key;
    }
    out
}

fn deg_to_rad(degrees: f64) -> f64 {
    (PI * degrees) / 180.0
}

fn rotate(x: f64, y: f64, angle_rad: f64) -> Point {
    let big_x = x * js::cos(angle_rad) - y * js::sin(angle_rad);
    let big_y = x * js::sin(angle_rad) + y * js::cos(angle_rad);
    [big_x, big_y]
}

/// An elliptical arc as cubic curves of at most 120° each, `[x1, y1, x2,
/// y2, x, y]` (`arcToCubicCurves`).
#[allow(clippy::too_many_arguments)]
fn arc_to_cubic_curves(
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    r1: f64,
    r2: f64,
    angle: f64,
    large_arc_flag: f64,
    sweep_flag: f64,
) -> Vec<[f64; 6]> {
    let angle_rad = deg_to_rad(angle);
    let params = arc_points(
        x1,
        y1,
        x2,
        y2,
        r1,
        r2,
        angle,
        large_arc_flag,
        sweep_flag,
        None,
    );
    params
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| {
            let r1 = rotate(p[0][0], p[0][1], angle_rad);
            let r2 = rotate(p[1][0], p[1][1], angle_rad);
            let r3 = rotate(p[2][0], p[2][1], angle_rad);
            [r1[0], r1[1], r2[0], r2[1], r3[0], r3[1]]
        })
        .collect()
}

/// The control points of the arc's curves in the arc's unrotated frame,
/// three per curve; `recursive` carries `[f1, f2, cx, cy]` into the
/// remaining part of an arc over 120°.
#[allow(clippy::too_many_arguments)]
fn arc_points(
    mut x1: f64,
    mut y1: f64,
    mut x2: f64,
    mut y2: f64,
    mut r1: f64,
    mut r2: f64,
    angle: f64,
    large_arc_flag: f64,
    sweep_flag: f64,
    recursive: Option<[f64; 4]>,
) -> Vec<Point> {
    let angle_rad = deg_to_rad(angle);
    let mut params = Vec::new();
    let (mut f1, mut f2, cx, cy);
    let sweep = js::truthy(sweep_flag);
    if let Some(r) = recursive {
        [f1, f2, cx, cy] = r;
    } else {
        [x1, y1] = rotate(x1, y1, -angle_rad);
        [x2, y2] = rotate(x2, y2, -angle_rad);
        let x = (x1 - x2) / 2.0;
        let y = (y1 - y2) / 2.0;
        let mut h = (x * x) / (r1 * r1) + (y * y) / (r2 * r2);
        if h > 1.0 {
            h = h.sqrt();
            r1 *= h;
            r2 *= h;
        }
        let sign = if large_arc_flag == sweep_flag {
            -1.0
        } else {
            1.0
        };
        let r1_pow = r1 * r1;
        let r2_pow = r2 * r2;
        let left = r1_pow * r2_pow - r1_pow * y * y - r2_pow * x * x;
        let right = r1_pow * y * y + r2_pow * x * x;
        let k = sign * (left / right).abs().sqrt();
        cx = k * r1 * y / r2 + (x1 + x2) / 2.0;
        cy = k * -r2 * x / r1 + (y1 + y2) / 2.0;
        f1 = js::fixed((y1 - cy) / r2, 9).asin();
        f2 = js::fixed((y2 - cy) / r2, 9).asin();
        if x1 < cx {
            f1 = PI - f1;
        }
        if x2 < cx {
            f2 = PI - f2;
        }
        if f1 < 0.0 {
            f1 += PI * 2.0;
        }
        if f2 < 0.0 {
            f2 += PI * 2.0;
        }
        if sweep && f1 > f2 {
            f1 -= PI * 2.0;
        }
        if !sweep && f2 > f1 {
            f2 -= PI * 2.0;
        }
    }
    // Curves of at most 120° each.
    let max_df = PI * 120.0 / 180.0;
    let mut df = f2 - f1;
    if df.abs() > max_df {
        let f2old = f2;
        let x2old = x2;
        let y2old = y2;
        if sweep && f2 > f1 {
            f2 = f1 + max_df;
        } else {
            f2 = f1 - max_df;
        }
        x2 = cx + r1 * js::cos(f2);
        y2 = cy + r2 * js::sin(f2);
        params = arc_points(
            x2,
            y2,
            x2old,
            y2old,
            r1,
            r2,
            angle,
            0.0,
            sweep_flag,
            Some([f2, f2old, cx, cy]),
        );
    }
    df = f2 - f1;
    let c1 = js::cos(f1);
    let s1 = js::sin(f1);
    let c2 = js::cos(f2);
    let s2 = js::sin(f2);
    let t = (df / 4.0).tan();
    let hx = 4.0 / 3.0 * r1 * t;
    let hy = 4.0 / 3.0 * r2 * t;
    let m1 = [x1, y1];
    let mut m2 = [x1 + hx * s1, y1 - hy * c1];
    let m3 = [x2 + hx * s2, y2 - hy * c2];
    let m4 = [x2, y2];
    m2[0] = 2.0 * m1[0] - m2[0];
    m2[1] = 2.0 * m1[1] - m2[1];
    let mut points = vec![m2, m3, m4];
    points.append(&mut params);
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(key: char, data: &[f64]) -> Segment {
        Segment::new(key, data.to_vec())
    }

    #[test]
    fn parses_commands_and_numbers() {
        let s = parse_path("M 10,20 L30 -4.5e1 h.5 v-.25 Z").unwrap();
        assert_eq!(
            s,
            [
                seg('M', &[10.0, 20.0]),
                seg('L', &[30.0, -45.0]),
                seg('h', &[0.5]),
                seg('v', &[-0.25]),
                seg('Z', &[]),
            ]
        );
    }

    #[test]
    fn numbers_without_separators() {
        // "-" starts a new number, and so does a second "." (as in browsers).
        let s = parse_path("M1-2l.5.5 1e2e3").unwrap_err();
        assert_eq!(s, PathError::BadCharacter);
        let s = parse_path("M1-2l.5.5").unwrap();
        assert_eq!(s, [seg('M', &[1.0, -2.0]), seg('l', &[0.5, 0.5])]);
        // 1.e5 and an exponent without digits.
        let s = parse_path("M1.e2 3").unwrap();
        assert_eq!(s, [seg('M', &[100.0, 3.0])]);
    }

    #[test]
    fn repeated_numbers_repeat_the_command() {
        let s = parse_path("m1 2 3 4 5 6").unwrap();
        assert_eq!(
            s,
            [
                seg('m', &[1.0, 2.0]),
                seg('l', &[3.0, 4.0]),
                seg('l', &[5.0, 6.0])
            ]
        );
        let s = parse_path("M0 0 C1 2 3 4 5 6 7 8 9 10 11 12").unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s[2], seg('C', &[7.0, 8.0, 9.0, 10.0, 11.0, 12.0]));
    }

    #[test]
    fn data_without_a_move_starts_at_the_origin() {
        let s = parse_path("L 5 5").unwrap();
        assert_eq!(s, [seg('M', &[0.0, 0.0]), seg('L', &[5.0, 5.0])]);
        assert_eq!(parse_path("").unwrap(), []);
        assert_eq!(parse_path("  ").unwrap(), []);
    }

    #[test]
    fn negative_zero_reads_as_zero() {
        let s = parse_path("M-0 -0.0").unwrap();
        assert!(s[0].data.iter().all(|x| *x == 0.0 && x.is_sign_positive()));
    }

    #[test]
    fn bad_data_is_an_error() {
        assert_eq!(parse_path("M 1 2 L 3"), Err(PathError::EndedShort));
        assert_eq!(parse_path("M 1 2 L 3 L"), Err(PathError::ParamNotANumber));
        assert_eq!(parse_path("M 1 2 # 3"), Err(PathError::BadCharacter));
        // path-data-parser loops forever here.
        assert_eq!(parse_path("M 1 2 Z 3"), Err(PathError::NumberAfterClose));
    }

    #[test]
    fn serializes_like_path_data_parser() {
        let s = parse_path("M1 2C3 4 5 6 7.5 8q1 2 3 4L.1 .2z").unwrap();
        assert_eq!(
            serialize(&s),
            "M 1 2 C 3 4, 5 6, 7.5 8 q 1 2, 3 4 L 0.1 0.2 z"
        );
    }

    #[test]
    fn absolutize_resolves_relative_commands() {
        let s = parse_path(
            "m10 10 l5 0 h5 v5 c1 1 2 2 3 3 s1 1 2 2 q1 1 2 2 t1 1 a5 5 0 0 1 10 0 z l1 1",
        )
        .unwrap();
        let a = absolutize(&s);
        assert_eq!(
            a,
            [
                seg('M', &[10.0, 10.0]),
                seg('L', &[15.0, 10.0]),
                seg('H', &[20.0]),
                seg('V', &[15.0]),
                seg('C', &[21.0, 16.0, 22.0, 17.0, 23.0, 18.0]),
                seg('S', &[24.0, 19.0, 25.0, 20.0]),
                seg('Q', &[26.0, 21.0, 27.0, 22.0]),
                seg('T', &[28.0, 23.0]),
                seg('A', &[5.0, 5.0, 0.0, 0.0, 1.0, 38.0, 23.0]),
                seg('Z', &[]),
                // After a close, relative commands start at the subpath's start.
                seg('L', &[11.0, 11.0]),
            ]
        );
    }

    #[test]
    fn normalize_keeps_only_move_line_curve_close() {
        let s = absolutize(&parse_path("M0 0 H10 V10 Q20 10 20 20 T30 30 S40 40 50 50 Z").unwrap());
        let n = normalize(&s);
        let keys: String = n.iter().map(|s| s.key).collect();
        assert_eq!(keys, "MLLCCCZ");
        assert_eq!(n[1], seg('L', &[10.0, 0.0]));
        assert_eq!(n[2], seg('L', &[10.0, 10.0]));
        // Q x1 y1 x y: control points two thirds of the way to (x1, y1).
        assert_eq!(
            n[3].data,
            [
                10.0 + 2.0 * (20.0 - 10.0) / 3.0,
                10.0,
                20.0,
                20.0 + 2.0 * (10.0 - 20.0) / 3.0,
                20.0,
                20.0
            ]
        );
        // T reflects the last control point.
        assert_eq!(n[4].data[4..], [30.0, 30.0]);
        // S after a non-curve starts at the current point; after T it isn't a C.
        assert_eq!(n[5].data[0..2], [30.0, 30.0]);
    }

    #[test]
    fn arcs_become_curves_of_at_most_120_degrees() {
        // A half circle of radius 10 from (0,0) to (20,0): two curves.
        let n = normalize(&absolutize(&parse_path("M0 0 A10 10 0 0 1 20 0").unwrap()));
        assert_eq!(n.len(), 3);
        let end = &n[2].data;
        assert!(
            (end[4] - 20.0).abs() < 1e-9 && end[5].abs() < 1e-9,
            "{end:?}"
        );
        // The first curve (120°) ends on the circle.
        let mid = &n[1].data;
        assert!((((mid[4] - 10.0).powi(2) + mid[5].powi(2)).sqrt() - 10.0).abs() < 1e-9);
        // A zero radius is a straight curve; a zero-length arc nothing.
        let n = normalize(&absolutize(
            &parse_path("M0 0 A0 5 0 0 1 20 0 A5 5 0 0 1 20 0").unwrap(),
        ));
        assert_eq!(
            n,
            [
                seg('M', &[0.0, 0.0]),
                seg('C', &[0.0, 0.0, 20.0, 0.0, 20.0, 0.0])
            ]
        );
    }
}

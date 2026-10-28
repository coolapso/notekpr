//! JavaScript's number semantics, where Excalidraw's output depends on them:
//! drawings must come out exactly as Excalidraw draws them (same random
//! numbers, same rounding, same path text), so the ported code computes and
//! prints numbers the way JavaScript does.

/// `Math.imul`: 32-bit integer multiplication, wrapping.
pub fn imul(a: i32, b: i32) -> i32 {
    a.wrapping_mul(b)
}

/// `Math.round`: halves round up (towards +∞), unlike Rust's `round`.
pub fn round(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    // `x + 0.5` would round first: 0.49999999999999994 + 0.5 is 1.
    let f = x.floor();
    if x - f >= 0.5 { f + 1.0 } else { f }
}

/// `Number.prototype.toString()`: the shortest digits that read back as the
/// same number, in JavaScript's layout (`3`, `0.1`, `1e-7`, `1.5e+21`).
pub fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if x == 0.0 {
        return "0".into();
    }
    let (digits, n) = shortest_digits(x.abs());
    let k = digits.len() as i32;
    let mut s = String::new();
    if x < 0.0 {
        s.push('-');
    }
    if k <= n && n <= 21 {
        s.push_str(&digits);
        s.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        s.push_str(&digits[..n as usize]);
        s.push('.');
        s.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        s.push_str("0.");
        s.extend(std::iter::repeat_n('0', (-n) as usize));
        s.push_str(&digits);
    } else {
        s.push_str(&digits[..1]);
        if k > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        s.push('e');
        s.push(if n - 1 < 0 { '-' } else { '+' });
        s.push_str(&(n - 1).abs().to_string());
    }
    s
}

/// The shortest digits that read back as `x` (positive), and JavaScript's
/// `n`: `x` is digits × 10^(n - digits). Rust's `{:e}` gives the same digits
/// except on an exact tie between two candidates, where JavaScript takes the
/// even one and Rust the larger.
fn shortest_digits(x: f64) -> (String, i32) {
    let parse = |s: &str| -> (String, i32) {
        let (mantissa, exp) = s.split_once('e').expect("exponent");
        let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
        (digits, exp.parse::<i32>().expect("exponent") + 1)
    };
    let (digits, n) = parse(&format!("{x:e}"));
    let k = digits.len();
    let last = digits.as_bytes()[k - 1] - b'0';
    if last.is_multiple_of(2) {
        return (digits, n);
    }
    // A tie: the exact value goes on with 5 and then only zeros. A short
    // look first (cheap), then far enough to be sure (an f64's exact value
    // has at most 767 significant digits).
    let is_tie = |more: usize| {
        let (exact, en) = parse(&format!("{:.*e}", k - 1 + more, x));
        let tail = &exact[k..];
        en == n && tail.as_bytes()[0] == b'5' && tail[1..].bytes().all(|b| b == b'0')
    };
    let tie = is_tie(24) && is_tie(800);
    let (exact, _) = parse(&format!("{:.*e}", k - 1 + 24, x));
    let below = &exact[..k] < digits.as_str();
    if !tie || !below || last == 0 {
        return (digits, n);
    }
    // Rust rounded the tie up to an odd digit: the even one is just below.
    let mut even = digits.clone().into_bytes();
    even[k - 1] -= 1;
    let even = String::from_utf8(even).expect("digits");
    let back: f64 = format!("0.{even}e{n}").parse().unwrap_or(f64::NAN);
    if back == x { (even, n) } else { (digits, n) }
}

/// `Number.prototype.toFixed(digits)`: rounds the exact value, a tie to the
/// larger magnitude (Rust's formatting rounds a tie to even).
pub fn to_fixed(x: f64, digits: u32) -> String {
    if !x.is_finite() || x.abs() >= 1e21 {
        return number_to_string(x);
    }
    // -0 prints without its sign (JavaScript checks `x < 0`).
    let x = if x == 0.0 { 0.0 } else { x };
    let d = digits as i32;
    // A tie needs the exact value to end at the digit after the last one
    // kept: x × 2^(digits+1) is then a whole number, and x × 10^digits is
    // computed exactly.
    if (x * 2f64.powi(d + 1)).fract() == 0.0 {
        let scaled = x.abs() * 10f64.powi(d);
        if scaled.fract() == 0.5 {
            let up = (scaled.floor() + 1.0) / 10f64.powi(d);
            return format!("{:.*}", digits as usize, if x < 0.0 { -up } else { up });
        }
    }
    format!("{:.*}", digits as usize, x)
}

/// `+x.toFixed(digits)`: the number rounded as `toFixed` does.
pub fn fixed(x: f64, digits: u32) -> f64 {
    to_fixed(x, digits).parse().unwrap_or(x)
}

/// Whether a number counts as true in a condition (`if (x)`): anything but
/// 0 and NaN.
pub fn truthy(x: f64) -> bool {
    x != 0.0 && !x.is_nan()
}

/// `x || fallback` for numbers: the fallback when `x` is 0 or NaN.
pub fn or(x: f64, fallback: f64) -> f64 {
    if truthy(x) { x } else { fallback }
}

/// `Math.max(a, b)`: NaN if either is (Rust's `max` drops a NaN), and +0
/// over -0.
pub fn max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_negative() { b } else { a }
    } else if a > b {
        a
    } else {
        b
    }
}

/// `Math.min(a, b)`: NaN if either is, and -0 under +0.
pub fn min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_negative() { a } else { b }
    } else if a < b {
        a
    } else {
        b
    }
}

/// ToInt32, what bitwise operators and `Math.imul` do to a number: whole
/// part, wrapped to 32 bits; NaN and infinities become 0.
pub fn to_int32(x: f64) -> i32 {
    if !x.is_finite() {
        return 0;
    }
    let m = x.trunc().rem_euclid(4_294_967_296.0);
    (m as u32) as i32
}

/// Whether `c` matches `\s` in a JavaScript regular expression (which is
/// not quite Rust's `char::is_whitespace`: U+FEFF is in, U+0085 is out).
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `Array.prototype.sort(compare)`: a stable sort whose comparison returns a
/// number (negative: `a` first, positive: `b` first, 0 or NaN: keep their
/// order). With a consistent comparison any stable sort gives JavaScript's
/// order; an inconsistent one (NaN coordinates) gives some order instead of
/// a panic, as Rust's own sorts may.
pub fn sort_by<T: Clone>(items: &mut [T], mut compare: impl FnMut(&T, &T) -> f64) {
    let n = items.len();
    let mut from = items.to_vec();
    let mut to = items.to_vec();
    let mut width = 1;
    while width < n {
        let mut start = 0;
        while start < n {
            let mid = (start + width).min(n);
            let end = (start + 2 * width).min(n);
            let (mut i, mut j, mut k) = (start, mid, start);
            while i < mid && j < end {
                // The right one goes first only when it must.
                if compare(&from[i], &from[j]) > 0.0 {
                    to[k] = from[j].clone();
                    j += 1;
                } else {
                    to[k] = from[i].clone();
                    i += 1;
                }
                k += 1;
            }
            while i < mid {
                to[k] = from[i].clone();
                i += 1;
                k += 1;
            }
            while j < end {
                to[k] = from[j].clone();
                j += 1;
                k += 1;
            }
            start = end;
        }
        std::mem::swap(&mut from, &mut to);
        width *= 2;
    }
    items.clone_from_slice(&from);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imul_wraps() {
        assert_eq!(imul(48271, 1_172_987_811), 740_761_613);
        assert_eq!(imul(-5, 3), -15);
        assert_eq!(imul(i32::MAX, 2), -2);
    }

    #[test]
    fn round_like_math_round() {
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -2.0);
        assert_eq!(round(-2.6), -3.0);
        assert_eq!(round(0.49999999999999994), 0.0);
        assert_eq!(round(7.0), 7.0);
    }

    #[test]
    fn numbers_print_like_javascript() {
        for (x, js) in [
            (0.0, "0"),
            (-0.0, "0"),
            (3.0, "3"),
            (-3.0, "-3"),
            (9.72, "9.72"),
            (-0.52, "-0.52"),
            (0.1 + 0.2, "0.30000000000000004"),
            (123456789.0, "123456789"),
            (1e21, "1e+21"),
            (1.5e21, "1.5e+21"),
            (1e20, "100000000000000000000"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (-1.25e-7, "-1.25e-7"),
            (2369.8125610351562, "2369.8125610351562"),
            (17.619999999999997, "17.619999999999997"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
        ] {
            assert_eq!(number_to_string(x), js, "{x:e}");
        }
    }

    #[test]
    fn to_fixed_like_javascript() {
        for (x, d, js) in [
            (0.125, 2, "0.13"),
            (-0.125, 2, "-0.13"),
            (0.375, 2, "0.38"),
            (2.5, 0, "3"),
            (1.005, 2, "1.00"), // 1.00499999999999989…
            (1.45, 1, "1.4"),   // 1.4499999999999999…
            (9.715, 2, "9.71"), // 9.7149999999999999…
            (247.4999, 2, "247.50"),
            (-0.0, 2, "0.00"),
            (-0.001, 2, "-0.00"),
            (12.0, 2, "12.00"),
        ] {
            assert_eq!(to_fixed(x, d), js, "{x} to {d}");
        }
        assert_eq!(number_to_string(fixed(231.2449, 2)), "231.24");
        assert_eq!(number_to_string(fixed(-0.001, 2)), "0");
    }

    #[test]
    fn truthiness_and_or() {
        assert!(truthy(-1.0) && truthy(0.5) && truthy(f64::INFINITY));
        assert!(!truthy(0.0) && !truthy(-0.0) && !truthy(f64::NAN));
        assert_eq!(or(0.0, 2.0), 2.0);
        assert_eq!(or(f64::NAN, 1.0), 1.0);
        assert_eq!(or(3.0, 1.0), 3.0);
    }

    #[test]
    fn max_and_min_like_math() {
        assert!(max(1.0, f64::NAN).is_nan());
        assert!(min(f64::NAN, 1.0).is_nan());
        assert_eq!(max(2.0, 3.0), 3.0);
        assert_eq!(min(2.0, 3.0), 2.0);
        assert!(max(-0.0, 0.0).is_sign_positive());
        assert!(max(0.0, -0.0).is_sign_positive());
        assert!(min(0.0, -0.0).is_sign_negative());
        assert!(min(-0.0, 0.0).is_sign_negative());
    }

    #[test]
    fn to_int32_wraps() {
        assert_eq!(to_int32(1.9), 1);
        assert_eq!(to_int32(-1.9), -1);
        assert_eq!(to_int32(2_147_483_648.0), i32::MIN);
        assert_eq!(to_int32(4_294_967_296.0), 0);
        assert_eq!(to_int32(1e10), 1_410_065_408);
        assert_eq!(to_int32(f64::NAN), 0);
        assert_eq!(to_int32(f64::NEG_INFINITY), 0);
    }

    #[test]
    fn spaces_like_regexp() {
        assert!(is_space(' ') && is_space('\u{feff}') && is_space('\u{2028}'));
        assert!(!is_space('\u{85}') && !is_space('a') && !is_space(','));
    }

    #[test]
    fn sort_is_stable_and_takes_number_comparisons() {
        let mut v: Vec<(f64, usize)> = [3.0, 1.0, 2.0, 1.0, 3.0, 0.0, 2.0, 1.0]
            .into_iter()
            .enumerate()
            .map(|(i, x)| (x, i))
            .collect();
        sort_by(&mut v, |a, b| a.0 - b.0);
        let order: Vec<usize> = v.iter().map(|x| x.1).collect();
        assert_eq!(order, [5, 1, 3, 7, 2, 6, 0, 4]);
        // NaN makes the comparison inconsistent: some order, no panic.
        let mut v = vec![2.0, f64::NAN, 1.0, f64::NAN, 0.0];
        sort_by(&mut v, |a, b| a - b);
        assert_eq!(v.iter().filter(|x| x.is_nan()).count(), 2);
        let mut empty: Vec<f64> = vec![];
        sort_by(&mut empty, |a, b| a - b);
        let mut one = vec![1.0];
        sort_by(&mut one, |a, b| a - b);
        assert_eq!(one, [1.0]);
    }
}

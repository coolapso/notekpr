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
}

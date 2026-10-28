//! `Math.sin`, `Math.cos` and `Math.atan2`, computed the same everywhere.
//!
//! The C library's functions (what Rust's `f64::sin` calls) differ from one
//! platform to the next in the last bit, and Rust on wasm32 brings its own;
//! JavaScript engines differ too. Drawings depend on these last bits now
//! and then (a loop that runs once more, a number cut after two decimals),
//! so these are fixed to what Excalidraw's own results show:
//!
//! * `sin` and `cos` are fdlibm 5.3's, as V8 has them
//!   (`src/base/ieee754.cc`). With them, perfect-freehand's test snapshots
//!   (computed in V8) come out exactly; with glibc's, 17 of their points
//!   end a few ulps off. Firefox uses fdlibm too; some Chrome builds use a
//!   port of glibc's.
//! * `atan2` is correctly rounded (see [`atan2`]).
//!
//! The fdlibm parts carry its notice (and V8's: see `crates/draw/PORTED.md`):
//!
//! > Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
//! >
//! > Developed at SunSoft, a Sun Microsystems, Inc. business.
//! > Permission to use, copy, modify, and distribute this
//! > software is freely granted, provided that this notice
//! > is preserved.

// fdlibm's constants, digit for digit as published (with their bits beside).
#![allow(clippy::excessive_precision)]

/// The high 32 bits of a double, as fdlibm's `__HI`.
fn high_word(x: f64) -> i32 {
    (x.to_bits() >> 32) as u32 as i32
}

/// The low 32 bits of a double, as fdlibm's `__LO`.
fn low_word(x: f64) -> u32 {
    x.to_bits() as u32
}

fn from_words(high: u32, low: u32) -> f64 {
    f64::from_bits((u64::from(high) << 32) | u64::from(low))
}

/// `x` × 2^`n`, exactly (musl's `scalbn`).
fn scalbn(x: f64, mut n: i32) -> f64 {
    let mut y = x;
    if n > 1023 {
        y *= f64::from_bits(0x7FE0_0000_0000_0000); // 2^1023
        n -= 1023;
        if n > 1023 {
            y *= f64::from_bits(0x7FE0_0000_0000_0000);
            n = (n - 1023).min(1023);
        }
    } else if n < -1022 {
        // 2^-1022 × 2^53, so that the last step rounds only once.
        let down = f64::from_bits(0x0010_0000_0000_0000) * f64::from_bits(0x4340_0000_0000_0000);
        y *= down;
        n += 1022 - 53;
        if n < -1022 {
            y *= down;
            n = (n + 1022 - 53).max(-1022);
        }
    }
    y * f64::from_bits(((0x3FF + n) as u64) << 52)
}

/// `__kernel_sin`: sin on [-π/4, π/4], `y` the tail of `x` (if `iy` is 1).
fn kernel_sin(x: f64, y: f64, iy: i32) -> f64 {
    const HALF: f64 = 5.00000000000000000000e-01;
    const S1: f64 = -1.66666666666666324348e-01; // 0xBFC55555, 0x55555549
    const S2: f64 = 8.33333333332248946124e-03; // 0x3F811111, 0x1110F8A6
    const S3: f64 = -1.98412698298579493134e-04; // 0xBF2A01A0, 0x19C161D5
    const S4: f64 = 2.75573137070700676789e-06; // 0x3EC71DE3, 0x57B1FE7D
    const S5: f64 = -2.50507602534068634195e-08; // 0xBE5AE5E6, 0x8A2B9CEB
    const S6: f64 = 1.58969099521155010221e-10; // 0x3DE5D93A, 0x5ACFD57C
    let ix = high_word(x) & 0x7FFF_FFFF;
    if ix < 0x3E40_0000 && x as i32 == 0 {
        // |x| < 2^-27
        return x;
    }
    let z = x * x;
    let v = z * x;
    let r = S2 + z * (S3 + z * (S4 + z * (S5 + z * S6)));
    if iy == 0 {
        x + v * (S1 + z * r)
    } else {
        x - ((z * (HALF * y - v * r) - y) - v * S1)
    }
}

/// `__kernel_cos`: cos on [-π/4, π/4], `y` the tail of `x`.
fn kernel_cos(x: f64, y: f64) -> f64 {
    const ONE: f64 = 1.00000000000000000000e+00;
    const C1: f64 = 4.16666666666666019037e-02; // 0x3FA55555, 0x5555554C
    const C2: f64 = -1.38888888888741095749e-03; // 0xBF56C16C, 0x16C15177
    const C3: f64 = 2.48015872894767294178e-05; // 0x3EFA01A0, 0x19CB1590
    const C4: f64 = -2.75573143513906633035e-07; // 0xBE927E4F, 0x809C52AD
    const C5: f64 = 2.08757232129817482790e-09; // 0x3E21EE9E, 0xBDB4B1C4
    const C6: f64 = -1.13596475577881948265e-11; // 0xBDA8FAE9, 0xBE8838D4
    let ix = high_word(x) & 0x7FFF_FFFF;
    if ix < 0x3E40_0000 && x as i32 == 0 {
        // |x| < 2^-27
        return ONE;
    }
    let z = x * x;
    let r = z * (C1 + z * (C2 + z * (C3 + z * (C4 + z * (C5 + z * C6)))));
    if ix < 0x3FD3_3333 {
        // |x| < 0.3
        return ONE - (0.5 * z - (z * r - x * y));
    }
    let qx = if ix > 0x3FE9_0000 {
        // |x| > 0.78125
        0.28125
    } else {
        // |x| / 4, its low word dropped
        from_words((ix - 0x0020_0000) as u32, 0)
    };
    let iz = 0.5 * z - qx;
    let a = ONE - qx;
    a - (iz - (z * r - x * y))
}

/// 2/π in 24-bit chunks.
const TWO_OVER_PI: [i32; 66] = [
    0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, 0x95993C, 0x439041, 0xFE5163,
    0xABDEBB, 0xC561B7, 0x246E3A, 0x424DD2, 0xE00649, 0x2EEA09, 0xD1921C, 0xFE1DEB, 0x1CB129,
    0xA73EE8, 0x8235F5, 0x2EBB44, 0x84E99C, 0x7026B4, 0x5F7E41, 0x3991D6, 0x398353, 0x39F49C,
    0x845F8B, 0xBDF928, 0x3B1FF8, 0x97FFDE, 0x05980F, 0xEF2F11, 0x8B5A0A, 0x6D1F6D, 0x367ECF,
    0x27CB09, 0xB74F46, 0x3F669E, 0x5FEA2D, 0x7527BA, 0xC7EBE5, 0xF17B3D, 0x0739F7, 0x8A5292,
    0xEA6BFB, 0x5FB11F, 0x8D5D08, 0x560330, 0x46FC7B, 0x6BABF0, 0xCFBC20, 0x9AF436, 0x1DA9E3,
    0x91615E, 0xE61B08, 0x659985, 0x5F14A0, 0x68408D, 0xFFD880, 0x4D7327, 0x310606, 0x1556CA,
    0x73A8C9, 0x60E27B, 0xC08C6B,
];

/// π/2 in pieces of 24 bits.
const PIO2: [f64; 8] = [
    1.57079625129699707031e+00, // 0x3FF921FB, 0x40000000
    7.54978941586159635335e-08, // 0x3E74442D, 0x00000000
    5.39030252995776476554e-15, // 0x3CF84698, 0x80000000
    3.28200341580791294123e-22, // 0x3B78CC51, 0x60000000
    1.27065575308067607349e-29, // 0x39F01B83, 0x80000000
    1.22933308981111328932e-36, // 0x387A2520, 0x40000000
    2.73370053816464559624e-44, // 0x36E38222, 0x80000000
    2.16741683877804819444e-51, // 0x3569F31D, 0x00000000
];

const TWO24: f64 = 1.67772160000000000000e+07; // 0x41700000, 0x00000000
const TWON24: f64 = 5.96046447753906250000e-08; // 0x3E700000, 0x00000000

/// `__kernel_rem_pio2` for `prec` 2 (53 bits): `x` (its 24-bit chunks
/// `tx`, scaled by 2^`e0`) minus the nearest multiple n of π/2, as `y[0] +
/// y[1]`; returns n mod 8. Only huge arguments (above 2^19·π/2) come here.
fn kernel_rem_pio2(tx: &[f64], y: &mut [f64; 2], e0: i32) -> i32 {
    // `init_jk[2]`
    let jk: usize = 4;
    let jp = jk;
    let nx = tx.len();
    let jx = nx - 1;
    let jv = ((e0 - 3) / 24).max(0) as usize;
    let mut q0 = e0 - 24 * (jv as i32 + 1);

    let mut f = [0.0f64; 20];
    let mut q = [0.0f64; 20];
    let mut fq = [0.0f64; 20];
    let mut iq = [0i32; 20];

    // f[0..=jx+jk] = TWO_OVER_PI[jv-jx..=jv+jk]
    let m = jx + jk;
    for (i, fi) in f.iter_mut().enumerate().take(m + 1) {
        let j = jv as i32 - jx as i32 + i as i32;
        *fi = if j < 0 {
            0.0
        } else {
            f64::from(TWO_OVER_PI[j as usize])
        };
    }
    // q[0..=jk]
    for i in 0..=jk {
        let mut fw = 0.0;
        for (j, &x) in tx.iter().enumerate() {
            fw += x * f[jx + i - j];
        }
        q[i] = fw;
    }

    let mut jz = jk;
    loop {
        // Distill q[] into iq[], reversed.
        let mut z = q[jz];
        let mut i = 0;
        let mut j = jz;
        while j > 0 {
            let fw = f64::from((TWON24 * z) as i32);
            iq[i] = (z - TWO24 * fw) as i32;
            z = q[j - 1] + fw;
            i += 1;
            j -= 1;
        }

        // n
        z = scalbn(z, q0);
        z -= 8.0 * (z * 0.125).floor();
        let mut n = z as i32;
        z -= f64::from(n);
        let mut ih = 0;
        if q0 > 0 {
            // iq[jz-1] is needed to determine n
            let i = iq[jz - 1] >> (24 - q0);
            n += i;
            iq[jz - 1] -= i << (24 - q0);
            ih = iq[jz - 1] >> (23 - q0);
        } else if q0 == 0 {
            ih = iq[jz - 1] >> 23;
        } else if z >= 0.5 {
            ih = 2;
        }

        if ih > 0 {
            // q > 0.5: take 1 - q
            n += 1;
            let mut carry = 0;
            for iqi in iq.iter_mut().take(jz) {
                let j = *iqi;
                if carry == 0 {
                    if j != 0 {
                        carry = 1;
                        *iqi = 0x100_0000 - j;
                    }
                } else {
                    *iqi = 0xFF_FFFF - j;
                }
            }
            if q0 > 0 {
                match q0 {
                    1 => iq[jz - 1] &= 0x7F_FFFF,
                    2 => iq[jz - 1] &= 0x3F_FFFF,
                    _ => {}
                }
            }
            if ih == 2 {
                z = 1.0 - z;
                if carry != 0 {
                    z -= scalbn(1.0, q0);
                }
            }
        }

        // Recompute with more terms if all that is left is zero.
        if z == 0.0 {
            let j = iq[jk..jz].iter().fold(0, |acc, &v| acc | v);
            if j == 0 {
                let mut k = 1;
                while iq[jk - k] == 0 {
                    k += 1;
                }
                for i in jz + 1..=jz + k {
                    f[jx + i] = f64::from(TWO_OVER_PI[jv + i]);
                    let mut fw = 0.0;
                    for (j, &x) in tx.iter().enumerate() {
                        fw += x * f[jx + i - j];
                    }
                    q[i] = fw;
                }
                jz += k;
                continue;
            }
        }

        // Chop off zero terms, or break z into 24 bits.
        if z == 0.0 {
            jz -= 1;
            q0 -= 24;
            while iq[jz] == 0 {
                jz -= 1;
                q0 -= 24;
            }
        } else {
            z = scalbn(z, -q0);
            if z >= TWO24 {
                let fw = f64::from((TWON24 * z) as i32);
                iq[jz] = (z - TWO24 * fw) as i32;
                jz += 1;
                q0 += 24;
                iq[jz] = fw as i32;
            } else {
                iq[jz] = z as i32;
            }
        }

        // The integer chunks as doubles.
        let mut fw = scalbn(1.0, q0);
        for i in (0..=jz).rev() {
            q[i] = fw * f64::from(iq[i]);
            fw *= TWON24;
        }

        // PIO2[0..=jp] × q[jz..=0]
        for i in (0..=jz).rev() {
            let mut fw = 0.0;
            let mut k = 0;
            while k <= jp && k <= jz - i {
                fw += PIO2[k] * q[i + k];
                k += 1;
            }
            fq[jz - i] = fw;
        }

        // Compress fq[] into y[].
        let mut fw = 0.0;
        for i in (0..=jz).rev() {
            fw += fq[i];
        }
        y[0] = if ih == 0 { fw } else { -fw };
        let mut fw = fq[0] - fw;
        for &v in &fq[1..=jz] {
            fw += v;
        }
        y[1] = if ih == 0 { fw } else { -fw };
        return n & 7;
    }
}

/// `__ieee754_rem_pio2`: `x` minus the nearest multiple n of π/2, as
/// `y[0] + y[1]`; returns n.
fn rem_pio2(x: f64, y: &mut [f64; 2]) -> i32 {
    /// High words of n·π/2 for n = 1…32: near them a first reduction may cancel.
    const NPIO2_HW: [i32; 32] = [
        0x3FF921FB, 0x400921FB, 0x4012D97C, 0x401921FB, 0x401F6A7A, 0x4022D97C, 0x4025FDBB,
        0x402921FB, 0x402C463A, 0x402F6A7A, 0x4031475C, 0x4032D97C, 0x40346B9C, 0x4035FDBB,
        0x40378FDB, 0x403921FB, 0x403AB41B, 0x403C463A, 0x403DD85A, 0x403F6A7A, 0x40407E4C,
        0x4041475C, 0x4042106C, 0x4042D97C, 0x4043A28C, 0x40446B9C, 0x404534AC, 0x4045FDBB,
        0x4046C6CB, 0x40478FDB, 0x404858EB, 0x404921FB,
    ];
    const HALF: f64 = 5.00000000000000000000e-01;
    // 6.36619772367581382433e-01, 0x3FE45F30, 0x6DC9C883
    const INVPIO2: f64 = std::f64::consts::FRAC_2_PI;
    const PIO2_1: f64 = 1.57079632673412561417e+00; // 0x3FF921FB, 0x54400000
    const PIO2_1T: f64 = 6.07710050650619224932e-11; // 0x3DD0B461, 0x1A626331
    const PIO2_2: f64 = 6.07710050630396597660e-11; // 0x3DD0B461, 0x1A600000
    const PIO2_2T: f64 = 2.02226624879595063154e-21; // 0x3BA3198A, 0x2E037073
    const PIO2_3: f64 = 2.02226624871116645580e-21; // 0x3BA3198A, 0x2E000000
    const PIO2_3T: f64 = 8.47842766036889956997e-32; // 0x397B839A, 0x252049C1

    let hx = high_word(x);
    let ix = hx & 0x7FFF_FFFF;
    if ix <= 0x3FE9_21FB {
        // |x| <= π/4: nothing to reduce
        *y = [x, 0.0];
        return 0;
    }
    if ix < 0x4002_D97C {
        // |x| < 3π/4: n is ±1
        if hx > 0 {
            let mut z = x - PIO2_1;
            if ix != 0x3FF9_21FB {
                y[0] = z - PIO2_1T;
                y[1] = (z - y[0]) - PIO2_1T;
            } else {
                // near π/2: 33+33+53 bits of π
                z -= PIO2_2;
                y[0] = z - PIO2_2T;
                y[1] = (z - y[0]) - PIO2_2T;
            }
            return 1;
        }
        let mut z = x + PIO2_1;
        if ix != 0x3FF9_21FB {
            y[0] = z + PIO2_1T;
            y[1] = (z - y[0]) + PIO2_1T;
        } else {
            z += PIO2_2;
            y[0] = z + PIO2_2T;
            y[1] = (z - y[0]) + PIO2_2T;
        }
        return -1;
    }
    if ix <= 0x4139_21FB {
        // |x| <= 2^19·π/2: medium size
        let t = x.abs();
        let n = (t * INVPIO2 + HALF) as i32;
        let fn_ = f64::from(n);
        let mut r = t - fn_ * PIO2_1;
        // first round, good to 85 bits
        let mut w = fn_ * PIO2_1T;
        if n < 32 && ix != NPIO2_HW[n as usize - 1] {
            y[0] = r - w;
        } else {
            let j = ix >> 20;
            y[0] = r - w;
            let i = j - ((high_word(y[0]) >> 20) & 0x7FF);
            if i > 16 {
                // second iteration, good to 118 bits
                let t = r;
                w = fn_ * PIO2_2;
                r = t - w;
                w = fn_ * PIO2_2T - ((t - r) - w);
                y[0] = r - w;
                let i = j - ((high_word(y[0]) >> 20) & 0x7FF);
                if i > 49 {
                    // third, 151 bits: covers all cases
                    let t = r;
                    w = fn_ * PIO2_3;
                    r = t - w;
                    w = fn_ * PIO2_3T - ((t - r) - w);
                    y[0] = r - w;
                }
            }
        }
        y[1] = (r - y[0]) - w;
        if hx < 0 {
            y[0] = -y[0];
            y[1] = -y[1];
            return -n;
        }
        return n;
    }
    if ix >= 0x7FF0_0000 {
        // inf or NaN
        *y = [f64::NAN, f64::NAN];
        return 0;
    }
    // Large: z = |x| scaled to 2^23…2^24, cut into three 24-bit chunks.
    let e0 = (ix >> 20) - 1046;
    let mut z = from_words((ix - (e0 << 20)) as u32, low_word(x));
    let mut tx = [0.0f64; 3];
    for t in tx.iter_mut().take(2) {
        *t = f64::from(z as i32);
        z = (z - *t) * TWO24;
    }
    tx[2] = z;
    let mut nx = 3;
    while tx[nx - 1] == 0.0 {
        nx -= 1;
    }
    let n = kernel_rem_pio2(&tx[..nx], y, e0);
    if hx < 0 {
        y[0] = -y[0];
        y[1] = -y[1];
        return -n;
    }
    n
}

/// `Math.sin`.
pub fn sin(x: f64) -> f64 {
    let ix = high_word(x) & 0x7FFF_FFFF;
    if ix <= 0x3FE9_21FB {
        return kernel_sin(x, 0.0, 0);
    }
    if ix >= 0x7FF0_0000 {
        // inf or NaN
        return f64::NAN;
    }
    let mut y = [0.0; 2];
    match rem_pio2(x, &mut y) & 3 {
        0 => kernel_sin(y[0], y[1], 1),
        1 => kernel_cos(y[0], y[1]),
        2 => -kernel_sin(y[0], y[1], 1),
        _ => -kernel_cos(y[0], y[1]),
    }
}

/// `Math.cos`.
pub fn cos(x: f64) -> f64 {
    let ix = high_word(x) & 0x7FFF_FFFF;
    if ix <= 0x3FE9_21FB {
        return kernel_cos(x, 0.0);
    }
    if ix >= 0x7FF0_0000 {
        // inf or NaN
        return f64::NAN;
    }
    let mut y = [0.0; 2];
    match rem_pio2(x, &mut y) & 3 {
        0 => kernel_cos(y[0], y[1]),
        1 => -kernel_sin(y[0], y[1], 1),
        2 => -kernel_cos(y[0], y[1]),
        _ => kernel_sin(y[0], y[1], 1),
    }
}

/// A double-double: the unevaluated sum `hi + lo`, `|lo|` at most half an
/// ulp of `hi`; about 106 bits of precision.
#[derive(Clone, Copy, Debug)]
struct Dd {
    hi: f64,
    lo: f64,
}

impl Dd {
    const fn new(hi: f64, lo: f64) -> Self {
        Dd { hi, lo }
    }

    const fn from(x: f64) -> Self {
        Dd { hi: x, lo: 0.0 }
    }
}

/// `a + b` exactly.
fn two_sum(a: f64, b: f64) -> Dd {
    let s = a + b;
    let bb = s - a;
    Dd::new(s, (a - (s - bb)) + (b - bb))
}

/// `a + b` exactly, given `|a| >= |b|`.
fn fast_two_sum(a: f64, b: f64) -> Dd {
    let s = a + b;
    Dd::new(s, b - (s - a))
}

/// `a × b` exactly (`mul_add` rounds once, on every platform).
fn two_prod(a: f64, b: f64) -> Dd {
    let p = a * b;
    Dd::new(p, a.mul_add(b, -p))
}

fn dd_add(a: Dd, b: Dd) -> Dd {
    let s = two_sum(a.hi, b.hi);
    let t = two_sum(a.lo, b.lo);
    let s = fast_two_sum(s.hi, s.lo + t.hi);
    fast_two_sum(s.hi, s.lo + t.lo)
}

fn dd_sub(a: Dd, b: Dd) -> Dd {
    dd_add(a, Dd::new(-b.hi, -b.lo))
}

fn dd_mul(a: Dd, b: Dd) -> Dd {
    let p = two_prod(a.hi, b.hi);
    fast_two_sum(p.hi, p.lo + (a.hi * b.lo + a.lo * b.hi))
}

fn dd_div(a: Dd, b: Dd) -> Dd {
    let q1 = a.hi / b.hi;
    let r = dd_sub(a, dd_mul(b, Dd::from(q1)));
    let q2 = r.hi / b.hi;
    let r = dd_sub(r, dd_mul(b, Dd::from(q2)));
    let q3 = r.hi / b.hi;
    dd_add(fast_two_sum(q1, q2), Dd::from(q3))
}

/// atan(k/64) for k = 0…64, as double-doubles.
const ATAN_K64: [(f64, f64); 65] = [
    (0.0, 0.0),
    (0.015623728620476831, -4.913600136566304e-19),
    (0.031239833430268277, -1.188442711587748e-18),
    (0.046840712915969654, -1.655677442254952e-19),
    (0.06241880999595735, -1.5490756308295046e-18),
    (0.0779666338315423, 5.804551873143357e-18),
    (0.09347678115858947, -6.2844725995420954e-18),
    (0.10894195698986579, 6.8267122072409585e-18),
    (0.12435499454676144, -3.1253241424539383e-18),
    (0.13970887428916365, -2.9579864247315813e-18),
    (0.15499674192394097, 9.585415594114324e-18),
    (0.1702119252854744, -3.541164079802125e-18),
    (0.18534794999569476, 4.180692268843079e-18),
    (0.2003985538258785, 3.1399542871844493e-18),
    (0.21535769969773805, 4.738160130078733e-19),
    (0.23021958727684372, 1.2313404529142703e-17),
    (0.24497866312686414, 1.0698755618734451e-17),
    (0.2596296294082575, 1.9238754924615304e-17),
    (0.2741674511196588, 8.261353575163773e-18),
    (0.2885873618940774, -1.428369957377257e-17),
    (0.3028848683749714, -1.1010827903001369e-17),
    (0.31705575320914703, -1.893928924292642e-17),
    (0.3310960767041321, -7.952610375793799e-18),
    (0.34500217720710513, -2.2938804755578304e-17),
    (0.35877067027057225, -2.4623815582638635e-17),
    (0.3723984466767542, 1.9612311504845653e-17),
    (0.38588266939807375, 2.378822732491941e-17),
    (0.39922076957525254, 2.246598105617042e-17),
    (0.4124104415973873, -1.587652227770689e-17),
    (0.42544963737004227, 2.3315530741892885e-17),
    (0.43833655985795783, -2.494277030626541e-17),
    (0.4510696559885235, -2.2703795229420475e-17),
    (0.4636476090008061, 2.2698777452961687e-17),
    (0.4760693303227612, 1.4654487332256713e-17),
    (0.48833395105640554, -1.1373236189329585e-17),
    (0.5004408131472942, -4.7181675085518756e-17),
    (0.5123894603107377, -2.5462781472855804e-17),
    (0.5241796287829132, 5.520094119641666e-18),
    (0.5358112379604637, -4.0637956834825575e-18),
    (0.5472843809874369, 4.923709671396255e-17),
    (0.5585993153435624, -5.4556305485916264e-18),
    (0.5697564534829784, 1.2255062085054184e-17),
    (0.5807563535676704, -1.441464378193067e-17),
    (0.5915997103351114, 4.920495453686772e-17),
    (0.6022873461349642, 2.950430737228402e-17),
    (0.6128202021652414, -3.1552061848586226e-17),
    (0.6231993299340659, 2.672403885140095e-17),
    (0.6334258829691446, -2.7290767436015276e-17),
    (0.6435011087932844, 1.5834785051444286e-17),
    (0.6534263411807619, 3.5800634857340095e-17),
    (0.6632029927060933, -3.076054864429649e-17),
    (0.6728325475937632, -1.899315009714705e-17),
    (0.6823165548747481, 6.943223671560008e-18),
    (0.6916566218531999, -8.117151192285796e-18),
    (0.7008544078844502, -1.987626234335816e-17),
    (0.7099116184635249, -4.597166450584887e-17),
    (0.7188299996216245, -2.1478388444456983e-17),
    (0.7276113326265107, 2.569325697391839e-18),
    (0.7362574289814281, 3.473937648299457e-17),
    (0.7447701257160751, 3.708315849135547e-17),
    (0.7531512809621944, -2.4256934659182068e-17),
    (0.7614027698055784, 9.850030332752822e-18),
    (0.7695264804056583, -3.704991905602721e-17),
    (0.7775243103733478, -2.6676490951944502e-17),
    (std::f64::consts::FRAC_PI_4, 3.061616997868383e-17),
];

/// (-1)^i / (2i + 1) for i = 0…7: atan's Taylor coefficients.
const ATAN_SERIES: [(f64, f64); 8] = [
    (1.0, 0.0),
    (-0.3333333333333333, -1.850371707708594e-17),
    (0.2, -1.1102230246251566e-17),
    (-0.14285714285714285, -7.93016446160826e-18),
    (0.1111111111111111, 6.1679056923619804e-18),
    (-0.09090909090909091, 2.523234146875356e-18),
    (0.07692307692307693, -4.270088556250602e-18),
    (-0.06666666666666667, -9.251858538542971e-19),
];

const PI_DD: Dd = Dd::new(std::f64::consts::PI, 1.2246467991473532e-16);
const PI_O_2_DD: Dd = Dd::new(std::f64::consts::FRAC_PI_2, 6.123233995736766e-17);

/// atan(t) for 0 <= t <= 1, as a double-double: atan(c) from the table for
/// the nearest c = k/64, plus the Taylor series of atan(d) for
/// d = (t - c) / (1 + t·c), which is below 2^-7.
fn atan_dd(t: Dd) -> Dd {
    let k = (t.hi * 64.0 + 0.5).floor() as usize;
    let c = Dd::from(k as f64 / 64.0);
    let d = dd_div(dd_sub(t, c), dd_add(Dd::from(1.0), dd_mul(t, c)));
    let u = dd_mul(d, d);
    let mut s = Dd::new(ATAN_SERIES[7].0, ATAN_SERIES[7].1);
    for &(hi, lo) in ATAN_SERIES[..7].iter().rev() {
        s = dd_add(Dd::new(hi, lo), dd_mul(u, s));
    }
    let (hi, lo) = ATAN_K64[k];
    dd_add(Dd::new(hi, lo), dd_mul(d, s))
}

/// `Math.atan2(y, x)`, correctly rounded: computed with about 100 bits,
/// then rounded once.
///
/// Not fdlibm's (V8's and Firefox's port), which is up to about an ulp off:
/// the browser that exported `tests/fixtures/example.svg` computed it this
/// accurately, and fdlibm's value turns one rounded corner of a stroke
/// there by a different number of steps. glibc's and Apple's `atan2` agree
/// with this one.
pub fn atan2(y: f64, x: f64) -> f64 {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

    // The special cases, as fdlibm has them.
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if y == 0.0 {
        return match (x.is_sign_negative(), y.is_sign_negative()) {
            (false, _) => y,
            (true, false) => PI,
            (true, true) => -PI,
        };
    }
    if x == 0.0 {
        return if y < 0.0 { -FRAC_PI_2 } else { FRAC_PI_2 };
    }
    if x.is_infinite() {
        let a = match (x > 0.0, y.is_infinite()) {
            (true, true) => FRAC_PI_4,
            (true, false) => 0.0,
            (false, true) => 3.0 * FRAC_PI_4,
            (false, false) => PI,
        };
        return if y < 0.0 { -a } else { a };
    }
    if y.is_infinite() {
        return if y < 0.0 { -FRAC_PI_2 } else { FRAC_PI_2 };
    }

    // atan(t) of the smaller over the larger, t in [0, 1], then the octant.
    let (ax, ay) = (x.abs(), y.abs());
    let swap = ay > ax;
    let (mut num, mut den) = if swap { (ax, ay) } else { (ay, ax) };
    if den < f64::from_bits(0x07B0_0000_0000_0000) {
        // Below 2^-900: scaled up (exactly) so the products below don't
        // lose bits to underflow.
        let up = f64::from_bits(0x6570_0000_0000_0000); // 2^600
        num *= up;
        den *= up;
    }
    let q = num / den;
    let p = two_prod(q, den);
    let t = fast_two_sum(q, ((num - p.hi) - p.lo) / den);
    let mut z = atan_dd(t);
    if swap {
        z = dd_sub(PI_O_2_DD, z);
    }
    if x < 0.0 {
        z = dd_sub(PI_DD, z);
    }
    let v = z.hi + z.lo;
    if y < 0.0 { -v } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    /// Units in the last place between two doubles of the same sign.
    fn ulps(a: f64, b: f64) -> u64 {
        a.to_bits().abs_diff(b.to_bits())
    }

    #[test]
    fn constants_are_the_documented_bits() {
        for (value, bits) in [
            (-1.66666666666666324348e-01, 0xBFC5_5555_5555_5549u64),
            (8.33333333332248946124e-03, 0x3F81_1111_1110_F8A6),
            (-1.98412698298579493134e-04, 0xBF2A_01A0_19C1_61D5),
            (2.75573137070700676789e-06, 0x3EC7_1DE3_57B1_FE7D),
            (-2.50507602534068634195e-08, 0xBE5A_E5E6_8A2B_9CEB),
            (1.58969099521155010221e-10, 0x3DE5_D93A_5ACF_D57C),
            (4.16666666666666019037e-02, 0x3FA5_5555_5555_554C),
            (-1.38888888888741095749e-03, 0xBF56_C16C_16C1_5177),
            (2.48015872894767294178e-05, 0x3EFA_01A0_19CB_1590),
            (-2.75573143513906633035e-07, 0xBE92_7E4F_809C_52AD),
            (2.08757232129817482790e-09, 0x3E21_EE9E_BDB4_B1C4),
            (-1.13596475577881948265e-11, 0xBDA8_FAE9_BE88_38D4),
            (std::f64::consts::FRAC_2_PI, 0x3FE4_5F30_6DC9_C883),
            (1.57079632673412561417e+00, 0x3FF9_21FB_5440_0000),
            (6.07710050650619224932e-11, 0x3DD0_B461_1A62_6331),
            (6.07710050630396597660e-11, 0x3DD0_B461_1A60_0000),
            (2.02226624879595063154e-21, 0x3BA3_198A_2E03_7073),
            (2.02226624871116645580e-21, 0x3BA3_198A_2E00_0000),
            (8.47842766036889956997e-32, 0x397B_839A_2520_49C1),
        ] {
            assert_eq!(f64::to_bits(value), bits, "{value:e}");
        }
        for (piece, bits) in PIO2.iter().zip([
            0x3FF921FB_40000000u64,
            0x3E74442D_00000000,
            0x3CF84698_80000000,
            0x3B78CC51_60000000,
            0x39F01B83_80000000,
            0x387A2520_40000000,
            0x36E38222_80000000,
            0x3569F31D_00000000,
        ]) {
            assert_eq!(piece.to_bits(), bits);
        }
    }

    #[test]
    fn known_values() {
        assert_eq!(sin(0.0), 0.0);
        assert!(sin(-0.0).is_sign_negative());
        assert_eq!(cos(0.0), 1.0);
        assert_eq!(sin(FRAC_PI_2), 1.0);
        assert_eq!(cos(FRAC_PI_2), 6.123233995736766e-17);
        assert_eq!(sin(PI), 1.2246467991473532e-16);
        assert_eq!(cos(PI), -1.0);
        assert!(sin(f64::INFINITY).is_nan() && cos(f64::NAN).is_nan());
        assert_eq!(atan2(0.0, -1.0), PI);
        assert_eq!(atan2(-0.0, -1.0), -PI);
        assert_eq!(atan2(-100.0, 0.0), -FRAC_PI_2);
        assert_eq!(atan2(1.0, 1.0), PI / 4.0);
        assert_eq!(atan2(f64::INFINITY, f64::NEG_INFINITY), 3.0 * PI / 4.0);
        assert!(atan2(f64::NAN, 1.0).is_nan());
    }

    #[test]
    fn atan2_is_correctly_rounded() {
        // Exact values worked out with 80-digit arithmetic; glibc's atan2
        // is an ulp off on the first four, fdlibm's on the last.
        for (y, x, exact) in [
            (0.2870264492269732, 0.6714116337052669, 0.4039837138610048),
            (0.5807738711410155, -0.054412278729259134, 1.664212904516187),
            (-0.535885372210624, 0.5330440111812271, -0.7880562944035837),
            (
                f64::from_bits(4596822084905820872),
                f64::from_bits(4606927151761674122),
                0.2152520329351953,
            ),
            // From the stroke the fdlibm value would draw differently.
            (0.9452345942148451, -0.32639173074619554, 1.9032800467589621),
        ] {
            assert_eq!(atan2(y, x), exact, "atan2({y:e}, {x:e})");
            assert_eq!(atan2(-y, x), -exact);
        }
        // Swapped octants, tiny and huge ratios, subnormals.
        assert_eq!(atan2(1.0, 1e-300), FRAC_PI_2);
        assert_eq!(atan2(1e-300, 1.0), 1e-300);
        assert_eq!(atan2(1e-300, -1.0), PI);
        assert_eq!(atan2(5e-324, 5e-324), PI / 4.0);
        assert_eq!(atan2(1e308, -1e308), 3.0 * PI / 4.0);
    }

    #[test]
    fn close_to_the_c_library() {
        // Not a reference (it rounds differently now and then), but a slip
        // in the port would be far more than an ulp off.
        let mut x = 1.0e-9;
        while x < 1.0e22 {
            for v in [x, -x, x * 1.37, x * 3.1] {
                assert!(ulps(sin(v), v.sin()) <= 1, "sin {v:e}");
                assert!(ulps(cos(v), v.cos()) <= 1, "cos {v:e}");
                assert!(ulps(atan2(v, 1.3), v.atan2(1.3)) <= 1, "atan2 {v:e}");
                assert!(ulps(atan2(-2.0, v), (-2.0f64).atan2(v)) <= 1, "atan2 {v:e}");
            }
            x *= 1.21;
        }
        for v in [
            1e300,
            -1e300,
            1.7976931348623157e308,
            823550.0,
            823551.0,
            1e6,
            2.0f64.powi(60),
        ] {
            assert!(ulps(sin(v), v.sin()) <= 1, "sin {v:e}");
            assert!(ulps(cos(v), v.cos()) <= 1, "cos {v:e}");
        }
    }
}

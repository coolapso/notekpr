//! `Math.sin`, `Math.cos`, `Math.tan`, `Math.acos`, `Math.asin`,
//! `Math.atan` and `Math.atan2`, computed the same everywhere.
//!
//! The C library's functions (what Rust's `f64::sin` calls) differ from one
//! platform to the next in the last bit, and Rust on wasm32 brings its own;
//! JavaScript engines differ too. Drawings depend on these last bits now
//! and then (a loop that runs once more, a number cut after two decimals),
//! so these are fixed to what Excalidraw's own results show:
//!
//! They are all fdlibm 5.3's, as V8 has them (`src/base/ieee754.cc`).
//! With them, perfect-freehand's test snapshots (computed in V8) come out
//! exactly; with glibc's, 17 of their points end a few ulps off. Firefox
//! uses fdlibm too. Browsers differ here — some Chrome builds use a port of
//! glibc's, and Chromium 156 rounds them correctly, an ulp from fdlibm's on
//! 3 to 8% of arguments — and notekpr draws the same in all of them, so one
//! rule is enough. (The browser that made Excalidraw's export of the test
//! drawing computed `atan2` exactly: fdlibm's draws one stroke's outline
//! with a point fewer at one corner, the same pixels: see
//! `svg::tests::same_as_excalidraws_export`.)
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

/// `__kernel_tan`: tan on [-π/4, π/4] (`iy` 1), or -1/tan (`iy` -1); `y`
/// the tail of `x`.
fn kernel_tan(mut x: f64, mut y: f64, iy: i32) -> f64 {
    const T: [f64; 13] = [
        3.33333333333334091986e-01,  // 0x3FD55555, 0x55555563
        1.33333333333201242699e-01,  // 0x3FC11111, 0x1110FE7A
        5.39682539762260521377e-02,  // 0x3FABA1BA, 0x1BB341FE
        2.18694882948595424599e-02,  // 0x3F9664F4, 0x8406D637
        8.86323982359930005737e-03,  // 0x3F8226E3, 0xE96E8493
        3.59207910759131235356e-03,  // 0x3F6D6D22, 0xC9560328
        1.45620945432529025516e-03,  // 0x3F57DBC8, 0xFEE08315
        5.88041240820264096874e-04,  // 0x3F4344D8, 0xF2F26501
        2.46463134818469906812e-04,  // 0x3F3026F7, 0x1A8D1068
        7.81794442939557092300e-05,  // 0x3F147E88, 0xA03792A6
        7.14072491382608190305e-05,  // 0x3F12B80F, 0x32F0A7E9
        -1.85586374855275456654e-05, // 0xBEF375CB, 0xDB605373
        2.59073051863633712884e-05,  // 0x3EFB2A70, 0x74BF7AD4
    ];
    const ONE: f64 = 1.00000000000000000000e+00;
    // 7.85398163397448278999e-01, 0x3FE921FB, 0x54442D18
    const PIO4: f64 = std::f64::consts::FRAC_PI_4;
    const PIO4LO: f64 = 3.06161699786838301793e-17; // 0x3C81A626, 0x33145C07
    let hx = high_word(x);
    let ix = hx & 0x7FFF_FFFF;
    if ix < 0x3E30_0000 && x as i32 == 0 {
        // |x| < 2^-28
        if ((ix as u32 | low_word(x)) | (iy + 1) as u32) == 0 {
            return ONE / x.abs();
        }
        if iy == 1 {
            return x;
        }
        return minus_inverse(x, y);
    }
    if ix >= 0x3FE5_9428 {
        // |x| >= 0.6744: from tan(π/4 - |x|)
        if hx < 0 {
            x = -x;
            y = -y;
        }
        let z = PIO4 - x;
        let w = PIO4LO - y;
        x = z + w;
        y = 0.0;
    }
    let z = x * x;
    let w = z * z;
    // x^5·(T[1] + x^4·T[3] + … + x^20·T[11])
    //   + x^5·(x^2·(T[2] + x^4·T[4] + … + x^22·T[12]))
    let r = T[1] + w * (T[3] + w * (T[5] + w * (T[7] + w * (T[9] + w * T[11]))));
    let v = z * (T[2] + w * (T[4] + w * (T[6] + w * (T[8] + w * (T[10] + w * T[12])))));
    let s = z * x;
    let mut r = y + z * (s * (r + v) + y);
    r += T[0] * s;
    let w = x + r;
    if ix >= 0x3FE5_9428 {
        let v = f64::from(iy);
        return f64::from(1 - ((hx >> 30) & 2)) * (v - 2.0 * (x - (w * w / (w + v) - r)));
    }
    if iy == 1 {
        return w;
    }
    minus_inverse(x, r)
}

/// -1 / (x + y), worked out accurately (`__kernel_tan`'s last steps).
fn minus_inverse(x: f64, y: f64) -> f64 {
    let w = x + y;
    // w and -1/w with their low words dropped
    let z = from_words(high_word(w) as u32, 0);
    let v = y - (z - x);
    let a = -1.0 / w;
    let t = from_words(high_word(a) as u32, 0);
    let s = 1.0 + t * z;
    t + a * (s + t * v)
}

/// `Math.tan`.
pub fn tan(x: f64) -> f64 {
    let ix = high_word(x) & 0x7FFF_FFFF;
    if ix <= 0x3FE9_21FB {
        return kernel_tan(x, 0.0, 1);
    }
    if ix >= 0x7FF0_0000 {
        // inf or NaN
        return f64::NAN;
    }
    let mut y = [0.0; 2];
    let n = rem_pio2(x, &mut y);
    // 1: n even, -1: n odd
    kernel_tan(y[0], y[1], 1 - ((n & 1) << 1))
}

const ONE: f64 = 1.00000000000000000000e+00;
// 1.57079632679489655800e+00, 0x3FF921FB, 0x54442D18
const PIO2_HI: f64 = std::f64::consts::FRAC_PI_2;
const PIO2_LO: f64 = 6.12323399573676603587e-17; // 0x3C91A626, 0x33145C07

/// asin(√z)/√z - 1 ≈ p(z)/q(z), z = x² or (1 - |x|)/2: `e_asin.c`'s and
/// `e_acos.c`'s rational approximation, as two parts.
fn asin_pq(z: f64) -> (f64, f64) {
    const PS0: f64 = 1.66666666666666657415e-01; // 0x3FC55555, 0x55555555
    const PS1: f64 = -3.25565818622400915405e-01; // 0xBFD4D612, 0x03EB6F7D
    const PS2: f64 = 2.01212532134862925881e-01; // 0x3FC9C155, 0x0E884455
    const PS3: f64 = -4.00555345006794114027e-02; // 0xBFA48228, 0xB5688F3B
    const PS4: f64 = 7.91534994289814532176e-04; // 0x3F49EFE0, 0x7501B288
    const PS5: f64 = 3.47933107596021167570e-05; // 0x3F023DE1, 0x0DFDF709
    const QS1: f64 = -2.40339491173441421878e+00; // 0xC0033A27, 0x1C8A2D4B
    const QS2: f64 = 2.02094576023350569471e+00; // 0x40002AE5, 0x9C598AC8
    const QS3: f64 = -6.88283971605453293030e-01; // 0xBFE6066C, 0x1B8D0159
    const QS4: f64 = 7.70381505559019352791e-02; // 0x3FB3B8C5, 0xB12E9282
    let p = z * (PS0 + z * (PS1 + z * (PS2 + z * (PS3 + z * (PS4 + z * PS5)))));
    let q = ONE + z * (QS1 + z * (QS2 + z * (QS3 + z * QS4)));
    (p, q)
}

/// `Math.acos` (fdlibm's `__ieee754_acos`).
pub fn acos(x: f64) -> f64 {
    // 3.14159265358979311600e+00, 0x400921FB, 0x54442D18
    const PI: f64 = std::f64::consts::PI;
    let ratio = |z: f64| {
        let (p, q) = asin_pq(z);
        p / q
    };
    let hx = high_word(x);
    let ix = hx & 0x7FFF_FFFF;
    if ix >= 0x3FF0_0000 {
        // |x| >= 1
        if ((ix - 0x3FF0_0000) as u32 | low_word(x)) == 0 {
            // |x| == 1
            return if hx > 0 { 0.0 } else { PI + 2.0 * PIO2_LO };
        }
        // |x| > 1, NaN
        return f64::NAN;
    }
    if ix < 0x3FE0_0000 {
        // |x| < 0.5
        if ix <= 0x3C60_0000 {
            // |x| < 2^-57
            return PIO2_HI + PIO2_LO;
        }
        let r = ratio(x * x);
        return PIO2_HI - (x - (PIO2_LO - x * r));
    }
    if hx < 0 {
        // x < -0.5
        let z = (ONE + x) * 0.5;
        let s = z.sqrt();
        let w = ratio(z) * s - PIO2_LO;
        return PI - 2.0 * (s + w);
    }
    // x > 0.5
    let z = (ONE - x) * 0.5;
    let s = z.sqrt();
    let df = from_words(high_word(s) as u32, 0);
    let c = (z - df * df) / (s + df);
    let w = ratio(z) * s + c;
    2.0 * (df + w)
}

/// `Math.atan2(y, x)` (fdlibm's `__ieee754_atan2`).
pub fn atan2(y: f64, x: f64) -> f64 {
    // 7.8539816339744827900E-01, 0x3FE921FB, 0x54442D18
    const PI_O_4: f64 = std::f64::consts::FRAC_PI_4;
    // 3.1415926535897931160E+00, 0x400921FB, 0x54442D18
    const PI: f64 = std::f64::consts::PI;
    const PI_LO: f64 = 1.2246467991473531772E-16; // 0x3CA1A626, 0x33145C07
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    let (hx, lx) = (high_word(x), low_word(x));
    let (hy, ly) = (high_word(y), low_word(y));
    let (ix, iy) = (hx & 0x7FFF_FFFF, hy & 0x7FFF_FFFF);
    if ((hx.wrapping_sub(0x3FF0_0000)) as u32 | lx) == 0 {
        // x = 1
        return atan(y);
    }
    // 2·sign(x) + sign(y)
    let m = ((hy >> 31) & 1) | ((hx >> 30) & 2);
    if (iy as u32 | ly) == 0 {
        // y = ±0
        return match m {
            0 | 1 => y,
            2 => PI,
            _ => -PI,
        };
    }
    if (ix as u32 | lx) == 0 {
        // x = ±0
        return if hy < 0 { -PIO2_HI } else { PIO2_HI };
    }
    if ix == 0x7FF0_0000 {
        // x = ±∞
        return if iy == 0x7FF0_0000 {
            match m {
                0 => PI_O_4,
                1 => -PI_O_4,
                2 => 3.0 * PI_O_4,
                _ => -3.0 * PI_O_4,
            }
        } else {
            match m {
                0 => 0.0,
                1 => -0.0,
                2 => PI,
                _ => -PI,
            }
        };
    }
    if iy == 0x7FF0_0000 {
        // y = ±∞
        return if hy < 0 { -PIO2_HI } else { PIO2_HI };
    }
    // y/x
    let k = (iy - ix) >> 20;
    let (z, m) = if k > 60 {
        // |y/x| > 2^60
        (PIO2_HI + 0.5 * PI_LO, m & 1)
    } else if hx < 0 && k < -60 {
        // 0 > |y|/x > -2^-60
        (0.0, m)
    } else {
        (atan((y / x).abs()), m)
    };
    match m {
        0 => z,
        1 => -z,
        2 => PI - (z - PI_LO),
        _ => (z - PI_LO) - PI,
    }
}

/// `Math.atan` (fdlibm's `s_atan.c`).
pub fn atan(x: f64) -> f64 {
    const ATANHI: [f64; 4] = [
        4.63647609000806093515e-01,  // atan(0.5)hi 0x3FDDAC67, 0x0561BB4F
        std::f64::consts::FRAC_PI_4, // atan(1.0)hi 0x3FE921FB, 0x54442D18
        9.82793723247329054082e-01,  // atan(1.5)hi 0x3FEF730B, 0xD281F69B
        PIO2_HI,                     // atan(inf)hi 0x3FF921FB, 0x54442D18
    ];
    const ATANLO: [f64; 4] = [
        2.26987774529616870924e-17, // atan(0.5)lo 0x3C7A2B7F, 0x222F65E2
        3.06161699786838301793e-17, // atan(1.0)lo 0x3C81A626, 0x33145C07
        1.39033110312309984516e-17, // atan(1.5)lo 0x3C700788, 0x7AF0CBBD
        6.12323399573676603587e-17, // atan(inf)lo 0x3C91A626, 0x33145C07
    ];
    const AT: [f64; 11] = [
        3.33333333333329318027e-01,  // 0x3FD55555, 0x5555550D
        -1.99999999998764832476e-01, // 0xBFC99999, 0x9998EBC4
        1.42857142725034663711e-01,  // 0x3FC24924, 0x920083FF
        -1.11111104054623557880e-01, // 0xBFBC71C6, 0xFE231671
        9.09088713343650656196e-02,  // 0x3FB745CD, 0xC54C206E
        -7.69187620504482999495e-02, // 0xBFB3B0F2, 0xAF749A6D
        6.66107313738753120669e-02,  // 0x3FB10D66, 0xA0D03D51
        -5.83357013379057348645e-02, // 0xBFADDE2D, 0x52DEFD9A
        4.97687799461593236017e-02,  // 0x3FA97B4B, 0x24760DEB
        -3.65315727442169155270e-02, // 0xBFA2B444, 0x2C6A6C2F
        1.62858201153657823623e-02,  // 0x3F90AD3A, 0xE322DA11
    ];
    let hx = high_word(x);
    let ix = hx & 0x7FFF_FFFF;
    if ix >= 0x4410_0000 {
        // |x| >= 2^66
        if x.is_nan() {
            return x + x;
        }
        return if hx > 0 {
            ATANHI[3] + ATANLO[3]
        } else {
            -ATANHI[3] - ATANLO[3]
        };
    }
    let (x, id) = if ix < 0x3FDC_0000 {
        // |x| < 0.4375
        if ix < 0x3E40_0000 {
            // |x| < 2^-27
            return x;
        }
        (x, None)
    } else {
        let x = x.abs();
        if ix < 0x3FF3_0000 {
            // |x| < 1.1875
            if ix < 0x3FE6_0000 {
                // 7/16 <= |x| < 11/16
                ((2.0 * x - ONE) / (2.0 + x), Some(0))
            } else {
                // 11/16 <= |x| < 19/16
                ((x - ONE) / (x + ONE), Some(1))
            }
        } else if ix < 0x4003_8000 {
            // |x| < 2.4375
            ((x - 1.5) / (ONE + 1.5 * x), Some(2))
        } else {
            // 2.4375 <= |x| < 2^66
            (-1.0 / x, Some(3))
        }
    };
    let z = x * x;
    let w = z * z;
    // The sum of AT[i]·z^(i+1), as odd and even polynomials.
    let s1 = z * (AT[0] + w * (AT[2] + w * (AT[4] + w * (AT[6] + w * (AT[8] + w * AT[10])))));
    let s2 = w * (AT[1] + w * (AT[3] + w * (AT[5] + w * (AT[7] + w * AT[9]))));
    match id {
        None => x - x * (s1 + s2),
        Some(id) => {
            let z = ATANHI[id] - ((x * (s1 + s2) - ATANLO[id]) - x);
            if hx < 0 { -z } else { z }
        }
    }
}

/// `Math.asin` (fdlibm's `__ieee754_asin`).
pub fn asin(x: f64) -> f64 {
    // 7.85398163397448278999e-01, 0x3FE921FB, 0x54442D18
    const PIO4_HI: f64 = std::f64::consts::FRAC_PI_4;
    let hx = high_word(x);
    let ix = hx & 0x7FFF_FFFF;
    if ix >= 0x3FF0_0000 {
        // |x| >= 1
        if ((ix - 0x3FF0_0000) as u32 | low_word(x)) == 0 {
            // asin(±1) = ±π/2
            return x * PIO2_HI + x * PIO2_LO;
        }
        // |x| > 1, NaN
        return f64::NAN;
    }
    if ix < 0x3FE0_0000 {
        // |x| < 0.5
        if ix < 0x3E40_0000 {
            // |x| < 2^-27
            return x;
        }
        let t = x * x;
        let (p, q) = asin_pq(t);
        return x + x * (p / q);
    }
    // 0.5 <= |x| < 1
    let t = (ONE - x.abs()) * 0.5;
    let (p, q) = asin_pq(t);
    let s = t.sqrt();
    let t = if ix >= 0x3FEF_3333 {
        // |x| > 0.975
        let w = p / q;
        PIO2_HI - (2.0 * (s + s * w) - PIO2_LO)
    } else {
        let w = from_words(high_word(s) as u32, 0);
        let c = (t - w * w) / (s + w);
        let r = p / q;
        let p = 2.0 * s * r - (PIO2_LO - 2.0 * c);
        let q = PIO4_HI - 2.0 * w;
        PIO4_HI - (p - q)
    };
    if hx > 0 { t } else { -t }
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
            // tan
            (3.33333333333334091986e-01, 0x3FD5_5555_5555_5563),
            (1.33333333333201242699e-01, 0x3FC1_1111_1110_FE7A),
            (5.39682539762260521377e-02, 0x3FAB_A1BA_1BB3_41FE),
            (2.18694882948595424599e-02, 0x3F96_64F4_8406_D637),
            (8.86323982359930005737e-03, 0x3F82_26E3_E96E_8493),
            (3.59207910759131235356e-03, 0x3F6D_6D22_C956_0328),
            (1.45620945432529025516e-03, 0x3F57_DBC8_FEE0_8315),
            (5.88041240820264096874e-04, 0x3F43_44D8_F2F2_6501),
            (2.46463134818469906812e-04, 0x3F30_26F7_1A8D_1068),
            (7.81794442939557092300e-05, 0x3F14_7E88_A037_92A6),
            (7.14072491382608190305e-05, 0x3F12_B80F_32F0_A7E9),
            (-1.85586374855275456654e-05, 0xBEF3_75CB_DB60_5373),
            (2.59073051863633712884e-05, 0x3EFB_2A70_74BF_7AD4),
            (std::f64::consts::FRAC_PI_4, 0x3FE9_21FB_5444_2D18),
            (3.06161699786838301793e-17, 0x3C81_A626_3314_5C07),
            // atan
            (4.63647609000806093515e-01, 0x3FDD_AC67_0561_BB4F),
            (9.82793723247329054082e-01, 0x3FEF_730B_D281_F69B),
            (2.26987774529616870924e-17, 0x3C7A_2B7F_222F_65E2),
            (1.39033110312309984516e-17, 0x3C70_0788_7AF0_CBBD),
            (3.33333333333329318027e-01, 0x3FD5_5555_5555_550D),
            (-1.99999999998764832476e-01, 0xBFC9_9999_9998_EBC4),
            (1.42857142725034663711e-01, 0x3FC2_4924_9200_83FF),
            (-1.11111104054623557880e-01, 0xBFBC_71C6_FE23_1671),
            (9.09088713343650656196e-02, 0x3FB7_45CD_C54C_206E),
            (-7.69187620504482999495e-02, 0xBFB3_B0F2_AF74_9A6D),
            (6.66107313738753120669e-02, 0x3FB1_0D66_A0D0_3D51),
            (-5.83357013379057348645e-02, 0xBFAD_DE2D_52DE_FD9A),
            (4.97687799461593236017e-02, 0x3FA9_7B4B_2476_0DEB),
            (-3.65315727442169155270e-02, 0xBFA2_B444_2C6A_6C2F),
            (1.62858201153657823623e-02, 0x3F90_AD3A_E322_DA11),
            // acos, asin
            (std::f64::consts::PI, 0x4009_21FB_5444_2D18),
            (std::f64::consts::FRAC_PI_2, 0x3FF9_21FB_5444_2D18),
            (6.12323399573676603587e-17, 0x3C91_A626_3314_5C07),
            (1.66666666666666657415e-01, 0x3FC5_5555_5555_5555),
            (-3.25565818622400915405e-01, 0xBFD4_D612_03EB_6F7D),
            (2.01212532134862925881e-01, 0x3FC9_C155_0E88_4455),
            (-4.00555345006794114027e-02, 0xBFA4_8228_B568_8F3B),
            (7.91534994289814532176e-04, 0x3F49_EFE0_7501_B288),
            (3.47933107596021167570e-05, 0x3F02_3DE1_0DFD_F709),
            (-2.40339491173441421878e+00, 0xC003_3A27_1C8A_2D4B),
            (2.02094576023350569471e+00, 0x4000_2AE5_9C59_8AC8),
            (-6.88283971605453293030e-01, 0xBFE6_066C_1B8D_0159),
            (7.70381505559019352791e-02, 0x3FB3_B8C5_B12E_9282),
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
        // As MDN shows V8's.
        assert_eq!(tan(0.0), 0.0);
        assert!(tan(-0.0).is_sign_negative());
        assert_eq!(tan(PI / 4.0), 0.9999999999999999);
        assert_eq!(tan(FRAC_PI_2), 16331239353195370.0);
        assert_eq!(tan(PI), -1.2246467991473532e-16);
        assert!(tan(f64::INFINITY).is_nan() && tan(f64::NAN).is_nan());
        assert_eq!(acos(1.0), 0.0);
        assert_eq!(acos(-1.0), PI);
        assert_eq!(acos(0.0), FRAC_PI_2);
        // 1.0471975511965979: π/3 rounded (`Math.PI / 3` is an ulp below).
        assert_eq!(acos(0.5), std::f64::consts::FRAC_PI_3);
        assert_eq!(acos(0.5), PI / 3.0 + 2.220446049250313e-16);
        assert!(acos(1.0000000000000002).is_nan() && acos(-2.0).is_nan());
        assert!(acos(f64::NAN).is_nan());
    }

    #[test]
    fn tan_and_acos_are_fdlibms() {
        // Worked out with fdlibm 5.3's C source (`k_tan.c`, `s_tan.c`,
        // `e_acos.c`); the C library rounds each of these the other way.
        for (x, fdlibm) in [
            (0.27908705466896894, 0.28656615039540156),
            (-0.9010550944498701, -1.262892437044472),
            (273.82671281966236, 0.5569349561221422),
            (53856.98205159059, 0.7748095950879677),
        ] {
            assert_eq!(tan(x), fdlibm, "tan({x:e})");
            assert_eq!(tan(-x), -fdlibm, "tan({:e})", -x);
        }
        for (x, fdlibm) in [
            (-0.7, 2.34619382340565),
            (0.203928610274005, 1.365427131967663),
            (-0.42176030261370157, 2.0061821953452608),
            (0.6477799084457398, 0.8661296792604285),
            (-0.7252314783187781, 2.382166865785904),
        ] {
            assert_eq!(acos(x), fdlibm, "acos({x:e})");
        }
        // Tiny arguments, and the edges of the kernels' ranges.
        assert_eq!(tan(1e-30), 1e-30);
        assert_eq!(acos(1e-20), FRAC_PI_2);
        assert_eq!(acos(0.9999999999999999), 1.4901161193847656e-08);
        assert_eq!(tan(1.0), 1.5574077246549023);
        assert_eq!(tan(-2.5), 0.7470222972386603);
        assert_eq!(tan(100.0), -0.5872139151569291);
    }

    #[test]
    fn atan2_is_fdlibms() {
        // fdlibm's values (musl's port agrees bit for bit on 6 million
        // pairs, but for |y/x| above 2^60 with x < 0: below); glibc's
        // rounds those marked the other way, and so does a correctly
        // rounded atan2 (Chromium 156's) on all but the third.
        for (y, x, fdlibm) in [
            (0.2870264492269732, 0.6714116337052669, 0.4039837138610048), // glibc
            (0.5807738711410155, -0.054412278729259134, 1.664212904516187), // glibc
            (-0.535885372210624, 0.5330440111812271, -0.7880562944035836),
            (0.21244296919646488, 0.9716596656603411, 0.21525203293519526),
            // The stroke corner of the test drawing.
            (0.9452345942148451, -0.32639173074619554, 1.9032800467589623), // glibc
        ] {
            assert_eq!(atan2(y, x), fdlibm, "atan2({y:e}, {x:e})");
            assert_eq!(atan2(-y, x), -fdlibm);
        }
        // Swapped octants, tiny and huge ratios, subnormals. Beyond 2^60,
        // fdlibm (and V8) take π/2 as it is; musl works it out an ulp above.
        assert_eq!(atan2(1.0, 1e-300), FRAC_PI_2);
        assert_eq!(
            atan2(4.51085626303698e170, -1.0969263086111712e152),
            FRAC_PI_2
        );
        assert_eq!(atan2(1e-300, 1.0), 1e-300);
        assert_eq!(atan2(1e-300, -1.0), PI);
        assert_eq!(atan2(5e-324, 5e-324), PI / 4.0);
        assert_eq!(atan2(1e308, -1e308), 3.0 * PI / 4.0);
    }

    #[test]
    fn atan_and_asin_are_fdlibms() {
        // fdlibm's values (musl's port, an independent one, agrees bit for
        // bit on 3 million arguments); Chromium 156 rounds each of these
        // the other way, and so does glibc on those marked. One per range
        // fdlibm's `atan` and `asin` treat apart.
        for (x, fdlibm) in [
            (-0.12140910334916244, -0.1208177939167233),
            (-0.3059792004128391, -0.2969332322963384), // glibc
            (0.6371744699820336, 0.5673061283351712),   // glibc
            (0.9557169693231335, 0.7627591692382667),
            (1.4084775994124437, 0.9533994488666657),
            (-1.3931691041386736, -0.9482316297959212), // glibc
            (6.143111076980652, 1.4094277718383728),
            (-3.480384192965394, -1.291008519726185), // glibc
        ] {
            assert_eq!(atan(x), fdlibm, "atan({x:e})");
            assert_eq!(atan(-x), -fdlibm, "atan({:e})", -x);
        }
        for (x, fdlibm) in [
            (0.09036477333108645, 0.09048821078304194),
            (0.3574306640214754, 0.3655153674378027), // glibc
            (0.5282203521910387, 0.556503291237209),
            (0.8050036351435284, 0.9356815954880828), // glibc
            (0.9892542055164536, 1.4240646316978456),
            (-0.9806503395168527, -1.373756300170009), // glibc
        ] {
            assert_eq!(asin(x), fdlibm, "asin({x:e})");
            assert_eq!(asin(-x), -fdlibm, "asin({:e})", -x);
        }
        // Ends and edges.
        assert_eq!(atan(1.0), PI / 4.0);
        assert_eq!(atan(f64::INFINITY), FRAC_PI_2);
        assert_eq!(atan(-1e300), -FRAC_PI_2);
        assert_eq!(atan(1e-300), 1e-300);
        assert!(atan(-0.0).is_sign_negative() && atan(f64::NAN).is_nan());
        assert_eq!(asin(1.0), FRAC_PI_2);
        assert_eq!(asin(-1.0), -FRAC_PI_2);
        // 0.5235987755982989: π/6 rounded (`Math.PI / 6` is an ulp below).
        assert_eq!(asin(0.5), std::f64::consts::FRAC_PI_6);
        assert_eq!(asin(0.9999999999999999), 1.5707963118937354);
        assert_eq!(asin(1e-300), 1e-300);
        assert!(asin(-0.0).is_sign_negative());
        assert!(asin(1.0000000000000002).is_nan() && asin(f64::NAN).is_nan());
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
                assert!(ulps(tan(v), v.tan()) <= 1, "tan {v:e}");
                assert!(ulps(atan(v), v.atan()) <= 1, "atan {v:e}");
            }
            x *= 1.21;
        }
        let mut x = -1.0;
        while x <= 1.0 {
            assert!(ulps(acos(x), x.acos()) <= 1, "acos {x:e}");
            assert!(ulps(asin(x), x.asin()) <= 1, "asin {x:e}");
            x += 0.00173;
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
            assert!(ulps(tan(v), v.tan()) <= 1, "tan {v:e}");
        }
    }
}

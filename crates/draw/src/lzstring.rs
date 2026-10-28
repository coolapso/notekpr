//! lz-string's Base64 variant, as Obsidian's Excalidraw plugin compresses
//! drawings (`compressed-json` blocks). Ported from lz-string 1.5 (MIT, see
//! `PORTED.md`). JavaScript strings are UTF-16, and lz-string works on their
//! code units, so this does too: the same text gives the same output.

use std::collections::{HashMap, HashSet};

const BASE64: &[u8; 65] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";

/// `LZString.compressToBase64`.
pub fn compress_to_base64(text: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = compress(&units, 6, |v| BASE64[v as usize] as char);
    match out.len() % 4 {
        1 => out.push_str("==="),
        2 => out.push_str("=="),
        3 => out.push('='),
        _ => {}
    }
    out
}

/// `LZString.decompressFromBase64`; `None` for data that isn't lz-string's.
/// Line breaks and spaces are skipped (Obsidian breaks the text into lines).
pub fn decompress_from_base64(data: &str) -> Option<String> {
    let values: Vec<u32> = data
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .map(|b| BASE64.iter().position(|&c| c == b).unwrap_or(0) as u32)
        .collect();
    if values.is_empty() {
        return None;
    }
    let units = decompress(&values, 32)?;
    Some(String::from_utf16_lossy(&units))
}

/// Writes bits into characters of `bits_per_char` bits each.
struct BitWriter<F: Fn(u32) -> char> {
    out: String,
    val: u32,
    position: u32,
    bits_per_char: u32,
    to_char: F,
}

impl<F: Fn(u32) -> char> BitWriter<F> {
    fn bit(&mut self, bit: u32) {
        self.val = (self.val << 1) | bit;
        if self.position == self.bits_per_char - 1 {
            self.position = 0;
            self.out.push((self.to_char)(self.val));
            self.val = 0;
        } else {
            self.position += 1;
        }
    }

    /// `count` bits of `value`, lowest first.
    fn value(&mut self, mut value: u32, count: u32) {
        for _ in 0..count {
            self.bit(value & 1);
            value >>= 1;
        }
    }
}

fn compress(input: &[u16], bits_per_char: u32, to_char: impl Fn(u32) -> char) -> String {
    let mut dictionary: HashMap<Vec<u16>, u32> = HashMap::new();
    let mut to_create: HashSet<Vec<u16>> = HashSet::new();
    let mut w: Vec<u16> = Vec::new();
    // The first entry doesn't count.
    let mut enlarge_in: u32 = 2;
    let mut dict_size: u32 = 3;
    let mut num_bits: u32 = 2;
    let mut out = BitWriter {
        out: String::new(),
        val: 0,
        position: 0,
        bits_per_char,
        to_char,
    };

    // The code for `w`: a new single character (its 8 or 16 bits), or its
    // place in the dictionary.
    let emit = |w: &Vec<u16>,
                out: &mut BitWriter<_>,
                to_create: &mut HashSet<Vec<u16>>,
                dictionary: &HashMap<Vec<u16>, u32>,
                enlarge_in: &mut u32,
                num_bits: &mut u32| {
        if to_create.contains(w) {
            if w[0] < 256 {
                out.value(0, *num_bits);
                out.value(u32::from(w[0]), 8);
            } else {
                out.value(1, *num_bits);
                out.value(u32::from(w[0]), 16);
            }
            *enlarge_in -= 1;
            if *enlarge_in == 0 {
                *enlarge_in = 1 << *num_bits;
                *num_bits += 1;
            }
            to_create.remove(w);
        } else {
            out.value(dictionary[w], *num_bits);
        }
        *enlarge_in -= 1;
        if *enlarge_in == 0 {
            *enlarge_in = 1 << *num_bits;
            *num_bits += 1;
        }
    };

    for &c in input {
        let c = vec![c];
        if !dictionary.contains_key(&c) {
            dictionary.insert(c.clone(), dict_size);
            dict_size += 1;
            to_create.insert(c.clone());
        }
        let mut wc = w.clone();
        wc.extend_from_slice(&c);
        if dictionary.contains_key(&wc) {
            w = wc;
        } else {
            emit(
                &w,
                &mut out,
                &mut to_create,
                &dictionary,
                &mut enlarge_in,
                &mut num_bits,
            );
            dictionary.insert(wc, dict_size);
            dict_size += 1;
            w = c;
        }
    }
    if !w.is_empty() {
        emit(
            &w,
            &mut out,
            &mut to_create,
            &dictionary,
            &mut enlarge_in,
            &mut num_bits,
        );
    }
    // The end of the stream, then the last character.
    out.value(2, num_bits);
    loop {
        out.val <<= 1;
        if out.position == bits_per_char - 1 {
            let v = out.val;
            out.out.push((out.to_char)(v));
            break;
        }
        out.position += 1;
    }
    out.out
}

/// Reads bits from characters of `reset` bits each (as values).
struct BitReader<'a> {
    values: &'a [u32],
    val: u32,
    position: u32,
    index: usize,
    reset: u32,
}

impl BitReader<'_> {
    fn bits(&mut self, count: u32) -> u32 {
        let mut bits = 0;
        for i in 0..count {
            let b = self.val & self.position;
            self.position >>= 1;
            if self.position == 0 {
                self.position = self.reset;
                self.val = self.values.get(self.index).copied().unwrap_or(0);
                self.index += 1;
            }
            if b > 0 {
                bits |= 1 << i;
            }
        }
        bits
    }
}

fn decompress(values: &[u32], reset: u32) -> Option<Vec<u16>> {
    let mut dictionary: Vec<Vec<u16>> = vec![Vec::new(), Vec::new(), Vec::new()];
    let mut enlarge_in: u32 = 4;
    let mut num_bits: u32 = 3;
    let mut input = BitReader {
        values,
        val: values[0],
        position: reset,
        index: 1,
        reset,
    };
    let first = match input.bits(2) {
        0 => input.bits(8) as u16,
        1 => input.bits(16) as u16,
        _ => return Some(Vec::new()),
    };
    dictionary.push(vec![first]);
    let mut w = vec![first];
    let mut result = w.clone();
    loop {
        if input.index > values.len() {
            return Some(Vec::new());
        }
        let mut c = input.bits(num_bits) as usize;
        match c {
            0 | 1 => {
                let unit = input.bits(if c == 0 { 8 } else { 16 }) as u16;
                dictionary.push(vec![unit]);
                c = dictionary.len() - 1;
                enlarge_in -= 1;
            }
            2 => return Some(result),
            _ => {}
        }
        if enlarge_in == 0 {
            enlarge_in = 1 << num_bits;
            num_bits += 1;
        }
        let entry = if c < dictionary.len() && c > 2 {
            dictionary[c].clone()
        } else if c == dictionary.len() {
            let mut e = w.clone();
            e.push(w[0]);
            e
        } else {
            return None;
        };
        result.extend_from_slice(&entry);
        let mut next = w;
        next.push(entry[0]);
        dictionary.push(next);
        enlarge_in -= 1;
        w = entry;
        if enlarge_in == 0 {
            enlarge_in = 1 << num_bits;
            num_bits += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_output() {
        // lz-string's own README example and a few made-up strings, with
        // the output lz-string 1.5 gives for them.
        assert_eq!(compress_to_base64("hello"), "BYUwNmD2Q===");
        assert_eq!(compress_to_base64(""), "Q===");
    }

    #[test]
    fn round_trips() {
        for text in [
            "a",
            "hello world, hello world, hello world",
            "{\"type\":\"excalidraw\",\"elements\":[]}",
            "ünïcödé — and an emoji 🎨 and 中文",
            &"abcabcabd".repeat(500),
        ] {
            let packed = compress_to_base64(text);
            assert_eq!(decompress_from_base64(&packed).as_deref(), Some(text));
            // Obsidian's line breaks don't matter.
            let broken: String = packed
                .as_bytes()
                .chunks(7)
                .map(|c| std::str::from_utf8(c).unwrap())
                .collect::<Vec<_>>()
                .join("\n\n");
            assert_eq!(decompress_from_base64(&broken).as_deref(), Some(text));
        }
    }

    #[test]
    fn garbage_is_refused_or_empty() {
        assert_eq!(decompress_from_base64(""), None);
        assert!(decompress_from_base64("////////").is_none_or(|s| s.len() < 8));
    }
}

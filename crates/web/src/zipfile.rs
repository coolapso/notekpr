//! Minimal ZIP reader/writer for vault export and import in the browser
//! (deflate via `miniz_oxide`; no JavaScript libraries).
//!
//! Reading unpacks at most [`MAX_ENTRY`] bytes per file and [`MAX_TOTAL`]
//! bytes in all, so a small "zip bomb" can't exhaust the page's memory.
//! More than 65 535 entries use the zip64 end-of-directory records; single
//! entries or archives of 4 GiB and more can't occur here (wasm32 memory).

use crate::util::human_bytes;

pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
}

/// Largest file unpacked from an archive.
pub const MAX_ENTRY: usize = 256 << 20;
/// Most bytes unpacked from one archive.
pub const MAX_TOTAL: usize = 512 << 20;

fn u16le(v: u16, o: &mut Vec<u8>) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn u32le(v: u32, o: &mut Vec<u8>) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn u64le(v: u64, o: &mut Vec<u8>) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn fit32(v: usize, what: &str) -> Result<u32, String> {
    u32::try_from(v).map_err(|_| format!("{what} is too large for a zip file"))
}

/// DOS date/time for "now" is not important for portability; use 2024-01-01.
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = ((2024 - 1980) << 9) | (1 << 5) | 1;

pub fn write(entries: &[Entry]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let crc = crc32fast::hash(&e.data);
        let deflated = miniz_oxide::deflate::compress_to_vec(&e.data, 6);
        let (method, body) = if deflated.len() < e.data.len() {
            (8u16, deflated)
        } else {
            (0u16, e.data.clone())
        };
        let offset = fit32(out.len(), "the export")?;
        let csize = fit32(body.len(), &e.name)?;
        let usize_ = fit32(e.data.len(), &e.name)?;
        let name = e.name.as_bytes();
        let nlen = u16::try_from(name.len()).map_err(|_| format!("{}: name too long", e.name))?;
        u32le(0x0403_4b50, &mut out);
        u16le(20, &mut out);
        u16le(0x0800, &mut out);
        u16le(method, &mut out);
        u16le(DOS_TIME, &mut out);
        u16le(DOS_DATE, &mut out);
        u32le(crc, &mut out);
        u32le(csize, &mut out);
        u32le(usize_, &mut out);
        u16le(nlen, &mut out);
        u16le(0, &mut out);
        out.extend_from_slice(name);
        out.extend_from_slice(&body);

        u32le(0x0201_4b50, &mut central);
        u16le(20, &mut central);
        u16le(20, &mut central);
        u16le(0x0800, &mut central);
        u16le(method, &mut central);
        u16le(DOS_TIME, &mut central);
        u16le(DOS_DATE, &mut central);
        u32le(crc, &mut central);
        u32le(csize, &mut central);
        u32le(usize_, &mut central);
        u16le(nlen, &mut central);
        u16le(0, &mut central);
        u16le(0, &mut central);
        u16le(0, &mut central);
        u16le(0, &mut central);
        u32le(0, &mut central);
        u32le(offset, &mut central);
        central.extend_from_slice(name);
    }
    let cd_offset = fit32(out.len(), "the export")?;
    let cd_size = fit32(central.len(), "the export")?;
    out.extend_from_slice(&central);
    let count = u16::try_from(entries.len()).unwrap_or(0xFFFF);
    if count == 0xFFFF {
        // zip64 end of central directory record and locator: 64-bit counts.
        let zip64_at = out.len() as u64;
        u32le(0x0606_4b50, &mut out);
        u64le(44, &mut out);
        u16le(45, &mut out);
        u16le(45, &mut out);
        u32le(0, &mut out);
        u32le(0, &mut out);
        u64le(entries.len() as u64, &mut out);
        u64le(entries.len() as u64, &mut out);
        u64le(cd_size as u64, &mut out);
        u64le(cd_offset as u64, &mut out);
        u32le(0x0706_4b50, &mut out);
        u32le(0, &mut out);
        u64le(zip64_at, &mut out);
        u32le(1, &mut out);
    }
    u32le(0x0605_4b50, &mut out);
    u16le(0, &mut out);
    u16le(0, &mut out);
    u16le(count, &mut out);
    u16le(count, &mut out);
    u32le(cd_size, &mut out);
    u32le(cd_offset, &mut out);
    u16le(0, &mut out);
    Ok(out)
}

fn r16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(o..o.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn r32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(o..o.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn r64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(o..o.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn big(v: u64) -> Result<usize, String> {
    usize::try_from(v).map_err(|_| "the archive is too large".to_string())
}

/// Entry count and central directory offset, from the zip64 records if the
/// classic end record says to look there.
fn directory(b: &[u8], eocd: usize) -> Result<(usize, usize), String> {
    let count = r16(b, eocd + 10).ok_or("truncated")?;
    let offset = r32(b, eocd + 16).ok_or("truncated")?;
    if count != 0xFFFF && offset != 0xFFFF_FFFF {
        return Ok((count as usize, offset as usize));
    }
    let loc = eocd.checked_sub(20).ok_or("truncated")?;
    if r32(b, loc) != Some(0x0706_4b50) {
        return Ok((count as usize, offset as usize));
    }
    let rec = big(r64(b, loc + 8).ok_or("truncated")?)?;
    if r32(b, rec) != Some(0x0606_4b50) {
        return Err("corrupt zip64 directory".into());
    }
    Ok((
        big(r64(b, rec + 32).ok_or("truncated")?)?,
        big(r64(b, rec + 48).ok_or("truncated")?)?,
    ))
}

/// Sizes and offset that don't fit 32 bits are in the zip64 extra field.
fn zip64_extra(extra: &[u8], fields: &mut [&mut u64]) {
    let mut p = 0;
    while let (Some(id), Some(len)) = (r16(extra, p), r16(extra, p + 2)) {
        let body = p + 4;
        if id == 0x0001 {
            let mut q = body;
            for f in fields.iter_mut().filter(|f| ***f == 0xFFFF_FFFF) {
                match r64(extra, q) {
                    Some(v) if q + 8 <= body + len as usize => **f = v,
                    _ => return,
                }
                q += 8;
            }
            return;
        }
        p = body + len as usize;
    }
}

pub fn read(b: &[u8]) -> Result<Vec<Entry>, String> {
    read_with_limits(b, MAX_ENTRY, MAX_TOTAL)
}

/// Unpack an archive, refusing entries larger than `max_entry` bytes and
/// more than `max_total` bytes in all.
pub fn read_with_limits(
    b: &[u8],
    max_entry: usize,
    max_total: usize,
) -> Result<Vec<Entry>, String> {
    let eocd = (0..b.len().saturating_sub(21))
        .rev()
        .find(|&i| r32(b, i) == Some(0x0605_4b50))
        .ok_or("not a zip file")?;
    let (count, mut p) = directory(b, eocd)?;
    let mut out = Vec::with_capacity(count.min(1 << 16));
    let mut total: usize = 0;
    for _ in 0..count {
        if r32(b, p) != Some(0x0201_4b50) {
            return Err("corrupt central directory".into());
        }
        let method = r16(b, p + 10).ok_or("truncated")?;
        let mut csize = r32(b, p + 20).ok_or("truncated")? as u64;
        let mut usize_ = r32(b, p + 24).ok_or("truncated")? as u64;
        let nlen = r16(b, p + 28).ok_or("truncated")? as usize;
        let xlen = r16(b, p + 30).ok_or("truncated")? as usize;
        let clen = r16(b, p + 32).ok_or("truncated")? as usize;
        let mut local = r32(b, p + 42).ok_or("truncated")? as u64;
        let name =
            String::from_utf8_lossy(b.get(p + 46..p + 46 + nlen).ok_or("truncated")?).to_string();
        let extra = b
            .get(p + 46 + nlen..p + 46 + nlen + xlen)
            .ok_or("truncated")?;
        zip64_extra(extra, &mut [&mut usize_, &mut csize, &mut local]);
        p += 46 + nlen + xlen + clen;
        if name.ends_with('/') {
            continue;
        }
        let (csize, usize_, local) = (big(csize)?, big(usize_)?, big(local)?);
        // Refuse before unpacking anything: the sizes are checked again below.
        if usize_ > max_entry {
            return Err(format!(
                "{name} is too large to import ({} unpacked; at most {} per file)",
                human_bytes(usize_ as u64),
                human_bytes(max_entry as u64)
            ));
        }
        total = total.saturating_add(usize_);
        if total > max_total {
            return Err(format!(
                "the archive is too large to import (more than {} unpacked)",
                human_bytes(max_total as u64)
            ));
        }
        if r32(b, local) != Some(0x0403_4b50) {
            return Err("corrupt local header".into());
        }
        let lnlen = r16(b, local + 26).ok_or("truncated")? as usize;
        let lxlen = r16(b, local + 28).ok_or("truncated")? as usize;
        let start = local + 30 + lnlen + lxlen;
        let raw = b
            .get(start..start.saturating_add(csize))
            .ok_or("truncated data")?;
        let data = match method {
            0 => raw.to_vec(),
            // Never inflate more than the entry claims to hold.
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(raw, usize_)
                .map_err(|_| format!("cannot inflate {name} (corrupt, or larger than it says)"))?,
            m => return Err(format!("{name}: unsupported compression method {m}")),
        };
        if data.len() != usize_ {
            return Err(format!("{name}: size mismatch"));
        }
        out.push(Entry { name, data });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let e = vec![
            Entry {
                name: "a.md".into(),
                data: b"hello hello hello hello".to_vec(),
            },
            Entry {
                name: "dir/ü.bin".into(),
                data: vec![1, 2, 3],
            },
        ];
        let z = write(&e).unwrap();
        let back = read(&z).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].data, e[0].data);
        assert_eq!(back[1].name, "dir/ü.bin");
    }

    #[test]
    fn many_entries_use_zip64() {
        let e: Vec<Entry> = (0..70_000)
            .map(|i| Entry {
                name: format!("n/{i}.md"),
                data: vec![b'x'; i % 3],
            })
            .collect();
        let z = write(&e).unwrap();
        let back = read(&z).unwrap();
        assert_eq!(back.len(), 70_000);
        assert_eq!(back[69_999].name, "n/69999.md");
        assert_eq!(back[69_999].data, vec![b'x'; 69_999 % 3]);
    }

    /// Offset of the first central directory entry.
    fn central(z: &[u8]) -> usize {
        let eocd = z.len() - 22;
        r32(z, eocd + 16).unwrap() as usize
    }

    #[test]
    fn unpacking_is_bounded() {
        let zeros = Entry {
            name: "bomb.bin".into(),
            data: vec![0; 4 << 20],
        };
        let z = write(&[zeros]).unwrap();
        assert!(z.len() < 64 << 10, "deflates well");
        assert_eq!(read(&z).unwrap()[0].data.len(), 4 << 20);
        // Over the per-file or total limit: refused before inflating.
        let err = read_with_limits(&z, 1 << 20, 64 << 20).err().unwrap();
        assert!(err.contains("too large"), "{err}");
        let err = read_with_limits(&z, 64 << 20, 1 << 20).err().unwrap();
        assert!(err.contains("too large"), "{err}");
        // An entry that lies about its size inflates no further than it says.
        let mut lying = z.clone();
        let c = central(&lying);
        lying[c + 24..c + 28].copy_from_slice(&1000u32.to_le_bytes());
        let err = read(&lying).err().unwrap();
        assert!(err.contains("cannot inflate"), "{err}");
        // Many entries pointing at the same data count towards the total.
        let mut e = Vec::new();
        for i in 0..3 {
            e.push(Entry {
                name: format!("{i}.bin"),
                data: vec![0; 1 << 20],
            });
        }
        let z = write(&e).unwrap();
        assert!(read_with_limits(&z, 2 << 20, (5 << 20) / 2).is_err());
        assert!(read_with_limits(&z, 2 << 20, 3 << 20).is_ok());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let z = write(&[Entry {
            name: "a.md".into(),
            data: b"hello".to_vec(),
        }])
        .unwrap();
        for cut in 0..z.len() {
            let _ = read(&z[..cut]);
            let mut bad = z.clone();
            bad[cut] ^= 0xFF;
            let _ = read(&bad);
        }
    }
}

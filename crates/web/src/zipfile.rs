//! Minimal ZIP reader/writer for vault export and import in the browser
//! (deflate via `miniz_oxide`; no JavaScript libraries).

pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
}

fn u16le(v: u16, o: &mut Vec<u8>) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn u32le(v: u32, o: &mut Vec<u8>) {
    o.extend_from_slice(&v.to_le_bytes());
}

/// DOS date/time for "now" is not important for portability; use 2024-01-01.
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = ((2024 - 1980) << 9) | (1 << 5) | 1;

pub fn write(entries: &[Entry]) -> Vec<u8> {
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
        let offset = out.len() as u32;
        let name = e.name.as_bytes();
        u32le(0x0403_4b50, &mut out);
        u16le(20, &mut out);
        u16le(0x0800, &mut out);
        u16le(method, &mut out);
        u16le(DOS_TIME, &mut out);
        u16le(DOS_DATE, &mut out);
        u32le(crc, &mut out);
        u32le(body.len() as u32, &mut out);
        u32le(e.data.len() as u32, &mut out);
        u16le(name.len() as u16, &mut out);
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
        u32le(body.len() as u32, &mut central);
        u32le(e.data.len() as u32, &mut central);
        u16le(name.len() as u16, &mut central);
        u16le(0, &mut central);
        u16le(0, &mut central);
        u16le(0, &mut central);
        u16le(0, &mut central);
        u32le(0, &mut central);
        u32le(offset, &mut central);
        central.extend_from_slice(name);
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    u32le(0x0605_4b50, &mut out);
    u16le(0, &mut out);
    u16le(0, &mut out);
    u16le(entries.len() as u16, &mut out);
    u16le(entries.len() as u16, &mut out);
    u32le(cd_size, &mut out);
    u32le(cd_offset, &mut out);
    u16le(0, &mut out);
    out
}

fn r16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn r32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

pub fn read(b: &[u8]) -> Result<Vec<Entry>, String> {
    let eocd = (0..b.len().saturating_sub(21))
        .rev()
        .find(|&i| r32(b, i) == Some(0x0605_4b50))
        .ok_or("not a zip file")?;
    let count = r16(b, eocd + 10).ok_or("truncated")? as usize;
    let mut p = r32(b, eocd + 16).ok_or("truncated")? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if r32(b, p) != Some(0x0201_4b50) {
            return Err("corrupt central directory".into());
        }
        let method = r16(b, p + 10).ok_or("truncated")?;
        let csize = r32(b, p + 20).ok_or("truncated")? as usize;
        let usize_ = r32(b, p + 24).ok_or("truncated")? as usize;
        let nlen = r16(b, p + 28).ok_or("truncated")? as usize;
        let xlen = r16(b, p + 30).ok_or("truncated")? as usize;
        let clen = r16(b, p + 32).ok_or("truncated")? as usize;
        let local = r32(b, p + 42).ok_or("truncated")? as usize;
        let name =
            String::from_utf8_lossy(b.get(p + 46..p + 46 + nlen).ok_or("truncated")?).to_string();
        p += 46 + nlen + xlen + clen;
        if r32(b, local) != Some(0x0403_4b50) {
            return Err("corrupt local header".into());
        }
        let lnlen = r16(b, local + 26).ok_or("truncated")? as usize;
        let lxlen = r16(b, local + 28).ok_or("truncated")? as usize;
        let start = local + 30 + lnlen + lxlen;
        let raw = b.get(start..start + csize).ok_or("truncated data")?;
        if name.ends_with('/') {
            continue;
        }
        let data = match method {
            0 => raw.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec(raw)
                .map_err(|_| format!("cannot inflate {name}"))?,
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
        let z = write(&e);
        let back = read(&z).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].data, e[0].data);
        assert_eq!(back[1].name, "dir/ü.bin");
    }
}

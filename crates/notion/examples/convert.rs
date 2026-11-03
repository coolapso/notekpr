//! What importing Notion exports would bring, as files in a folder:
//! `cargo run -p notekpr-notion --example convert -- OUT EXPORT.zip…`.
//! The icons go in `OUT/icons.txt`. Names in the zips are read as UTF-8,
//! as Notion writes them (without saying so), and zips inside are opened.

use std::error::Error;
use std::io::Read;
use std::path::PathBuf;

fn unzip(bytes: &[u8], out: &mut notekpr_notion::Files) -> Result<(), Box<dyn Error>> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        if f.is_dir() {
            continue;
        }
        let name = String::from_utf8_lossy(f.name_raw()).into_owned();
        let mut data = Vec::new();
        f.read_to_end(&mut data)?;
        if name.to_lowercase().ends_with(".zip") {
            unzip(&data, out)?;
        } else {
            out.push((name, data));
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(out), zips) = (args.next(), args.collect::<Vec<_>>()) else {
        return Err("usage: convert OUT EXPORT.zip…".into());
    };
    let mut exports = Vec::new();
    for z in &zips {
        let mut files = Vec::new();
        unzip(&std::fs::read(z)?, &mut files)?;
        exports.push(files);
    }
    let opts = notekpr_notion::Options {
        into: notekpr_notion::landing_folder(|_| false),
        ..Default::default()
    };
    let converted = notekpr_notion::convert(exports, &opts)?;
    let out = PathBuf::from(out);
    for (path, data) in &converted.files {
        let p = out.join(path);
        std::fs::create_dir_all(p.parent().expect("a file in a folder"))?;
        std::fs::write(p, data)?;
    }
    let icons: String = converted
        .icons
        .iter()
        .map(|(p, i)| format!("{i} {p}\n"))
        .collect();
    std::fs::write(out.join("icons.txt"), icons)?;
    let r = &converted.report;
    println!(
        "{} notes, {} files, {} icons, {} databases, {} pages to import, {} kinds of losses",
        r.notes,
        r.files,
        converted.icons.len(),
        r.databases.len(),
        r.waiting.len(),
        r.lost.len()
    );
    Ok(())
}

//! The simulated world: who the people are, which teams they form, and what
//! they write. Everything is derived from one seed, so a run can be repeated.

/// A small, fast pseudo-random generator (SplitMix64). Not for secrets —
/// the clients' keys come from the real crypto in `coolpsonotes-core`.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// In `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }

    pub fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }

    /// Weighted choice: the index of one of `weights`.
    pub fn weighted(&mut self, weights: &[u32]) -> usize {
        let total: u64 = weights.iter().map(|&w| w as u64).sum();
        let mut r = self.next() % total;
        for (i, &w) in weights.iter().enumerate() {
            if r < w as u64 {
                return i;
            }
            r -= w as u64;
        }
        weights.len() - 1
    }
}

pub const NAMES: &[&str] = &[
    "Maya", "Leo", "Ana", "Kai", "Zoe", "Omar", "Iris", "Noah", "Lena", "Ravi", "Sofia", "Jonas",
    "Amara", "Theo", "Mei", "Luca", "Nora", "Tariq", "Elena", "Finn", "Yara", "Mateo", "Ines",
    "Hugo", "Aisha", "Felix", "Lina", "Diego", "Freya", "Sami", "Clara", "Arjun", "Ida", "Marco",
    "Selin", "Oskar", "Leila", "Emil", "Priya", "Tomas", "Hana", "Rafael", "Mila", "Ken", "Sara",
    "Pablo", "Alma", "Nils", "Zara", "Ivan", "Rosa", "Ben", "Esra", "Jules", "Nadia", "Tim",
    "Lucia", "Kofi", "Greta", "Yusuf", "Vera", "Milo", "Chloe", "Bruno", "Asha", "Paul", "Lea",
    "Dario", "Anika", "Seb", "Luz", "Viktor", "Naomi", "Joao", "Elif", "Max", "Tara", "Andre",
    "Juno", "Rui", "Saga", "Adam", "Nina", "Kenji", "Olga", "Emma", "Ilya", "Beatriz", "Lars",
    "Fatima", "Sven", "Carmen", "Ali", "Ruth", "Hiro", "Eva", "Samir", "Ingrid", "Tiago", "Maja",
    "Arne", "Lucy", "Mehmet", "Alba", "Erik", "Dina", "Pedro",
];

pub const TEAMS: &[&str] = &[
    "Garden club",
    "Product team",
    "Book club",
    "Climbing crew",
    "Family",
    "Band practice",
    "Thesis group",
    "Food co-op",
    "Startup",
    "Choir",
    "Robotics club",
    "Trip to Japan",
    "Wedding planning",
    "Hackathon",
    "Lab notes",
    "Running club",
    "Board game night",
    "Neighbourhood",
    "Podcast",
    "Design studio",
    "Support team",
    "Sailing",
    "Chess club",
    "Volunteers",
    "Film club",
    "Research group",
    "Bakery",
    "Bike repair",
    "Language exchange",
    "School parents",
    "Photo walk",
    "Open source",
    "Ops on call",
    "Kitchen",
    "Cycling tour",
    "Writers' room",
    "Allotment",
    "Makerspace",
    "Study group",
    "House move",
];

pub const TITLES: &[&str] = &[
    "Ideas",
    "Meeting notes",
    "Shopping list",
    "Roadmap",
    "Reading list",
    "Recipes",
    "Budget",
    "Plan",
    "Checklist",
    "Questions",
    "Decisions",
    "Packing list",
    "Schedule",
    "Contacts",
    "Retro",
    "Wishlist",
    "Inventory",
    "Timeline",
    "Brainstorm",
    "Glossary",
    "FAQ",
    "Links",
    "Draft",
    "Summary",
    "To do",
    "Notes from the call",
    "Next steps",
    "Open issues",
    "Lessons learned",
    "Weekly update",
    "Goals",
    "Guide",
    "Playlist",
    "Map",
    "Menu",
    "Rules",
    "Setup",
    "Agenda",
    "Feedback",
    "Sketches",
];

/// What people type: ordinary note text.
pub const SENTENCES: &[&str] = &[
    "Let's meet on Thursday at six and bring the printed plan.",
    "The tomatoes need more sun, move them to the south bed.",
    "I booked the room for the whole afternoon.",
    "Can someone check the budget before Friday?",
    "We agreed to ship the first version without the export.",
    "Bring water, snacks and a warm jacket for the summit.",
    "The bakery on the corner opens at seven now.",
    "Chapter four was the best part so far.",
    "Remember to water the basil while I'm away.",
    "The train leaves at 8:14 from platform three.",
    "Added the new contacts to the shared list.",
    "Decision: we keep the old logo for one more year.",
    "Who has the key to the storage room?",
    "The demo went well, two follow-up calls next week.",
    "Buy flour, eggs, butter and a lemon.",
    "The sauce needs an hour on low heat.",
    "Draft the email to the landlord about the heating.",
    "Practice the bridge twice before the gig.",
    "Moved the deadline to the end of the month.",
    "Let's try the new route along the river.",
    "Upload the photos from Saturday to the album.",
    "The server room is too warm again.",
    "Next sprint: search, sharing and the mobile layout.",
    "Grandma's recipe uses brown sugar, not white.",
    "Rehearsal is cancelled, the hall is flooded.",
    "I'll pick up the tickets on my way home.",
    "Ask about the deposit before signing anything.",
    "The kids want pancakes on Sunday.",
    "We still need two more volunteers for the stand.",
    "Sketches for the poster are in the folder.",
    "Wind picks up after three, leave the harbour early.",
    "Read the paper before the reading group.",
    "The prototype runs for nine hours on one charge.",
    "Order more labels for the seed packets.",
    "The bug only happens when the list is empty.",
    "Put the chairs back after the meeting, please.",
    "Lunch is on me if we finish the slides today.",
    "Call the plumber about the dripping tap.",
    "The bikes need new chains before the tour.",
    "We should write down how the release works.",
    "Two pages a day and the draft is done by June.",
    "Swap the Tuesday and Wednesday shifts.",
    "The light is best just after sunrise.",
    "Use the blue folder for receipts.",
    "Cut the intro, start with the story.",
    "The new members start on Monday.",
    "Keep the hot sauce away from the kids.",
    "Measure the hallway before buying the shelf.",
    "Thanks everyone, that was a great evening.",
    "Try the quiet carriage, it's at the front.",
];

/// The text a person types next: one or two sentences.
pub fn sentence(rng: &mut Rng) -> String {
    let mut s = rng.pick(SENTENCES).to_string();
    if rng.chance(0.3) {
        s.push(' ');
        s.push_str(rng.pick(SENTENCES));
    }
    s
}

/// Split text the way people type it: a few characters at a time.
pub fn keystrokes(rng: &mut Rng, text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let n = (rng.range(1, 4) as usize).min(chars.len() - i);
        out.push(chars[i..i + n].iter().collect());
        i += n;
    }
    out
}

/// People into teams of 3 to 7 (each person in exactly one).
pub fn teams(rng: &mut Rng, people: u32) -> Vec<Vec<u32>> {
    let mut ids: Vec<u32> = (0..people).collect();
    rng.shuffle(&mut ids);
    let mut out: Vec<Vec<u32>> = Vec::new();
    let mut rest = &ids[..];
    while !rest.is_empty() {
        let n = (rng.range(3, 7) as usize).min(rest.len());
        out.push(rest[..n].to_vec());
        rest = &rest[n..];
    }
    // A last group of one or two joins the one before.
    if out.len() > 1 && out.last().is_some_and(|t| t.len() < 3) {
        let last = out.pop().unwrap();
        out.last_mut().unwrap().extend(last);
    }
    out
}

/// The person's display name and their (unique) sign-in name.
pub fn person_name(id: u32) -> (String, String) {
    let base = NAMES[id as usize % NAMES.len()];
    let round = id as usize / NAMES.len();
    let display = if round == 0 {
        base.to_string()
    } else {
        format!("{base} {}", round + 1)
    };
    let login = display.to_lowercase().replace(' ', "");
    (display, login)
}

pub fn team_name(id: u32) -> String {
    let base = TEAMS[id as usize % TEAMS.len()];
    match id as usize / TEAMS.len() {
        0 => base.to_string(),
        n => format!("{base} {}", n + 1),
    }
}

/// A picture as people upload them: a real PNG of `kib` KiB or so, with
/// noisy content, so it is about as big encrypted as it is here.
pub fn picture(rng: &mut Rng, kib: u32) -> Vec<u8> {
    let side = ((kib as f64 * 1024.0 / 3.0).sqrt() as u32).max(8);
    let (r0, g0, b0) = (rng.range(40, 200), rng.range(40, 200), rng.range(40, 200));
    let mut raw = Vec::with_capacity((side * (side * 3 + 1)) as usize);
    for y in 0..side {
        raw.push(0);
        for x in 0..side {
            let n = rng.next();
            raw.push(((r0 + (x * 255 / side) as u64) / 2 + (n & 31)) as u8);
            raw.push(((g0 + (y * 255 / side) as u64) / 2 + ((n >> 8) & 31)) as u8);
            raw.push((b0 / 2 + ((n >> 16) & 63)) as u8);
        }
    }
    png(side, side, &raw)
}

/// A PNG with stored (uncompressed) deflate blocks.
fn png(width: u32, height: u32, raw: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65_535).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend((c.len() as u16).to_le_bytes());
        z.extend((!(c.len() as u16)).to_le_bytes());
        z.extend(c);
    }
    z.extend(adler32(raw).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    header.extend([8, 2, 0, 0, 0]);
    for (kind, data) in [(&b"IHDR"[..], &header[..]), (b"IDAT", &z), (b"IEND", &[])] {
        out.extend((data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend(kind);
        out.extend(data);
        let crc = crc32(&out[start..]);
        out.extend(crc.to_be_bytes());
    }
    out
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &x in data {
        crc ^= x as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_world() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        assert_eq!(teams(&mut a, 50), teams(&mut b, 50));
        assert_eq!(sentence(&mut a), sentence(&mut b));
    }

    #[test]
    fn everyone_is_in_one_team_of_three_or_more() {
        let mut rng = Rng::new(1);
        for n in [3, 4, 10, 37, 200] {
            let t = teams(&mut rng, n);
            let mut all: Vec<u32> = t.iter().flatten().copied().collect();
            all.sort();
            assert_eq!(all, (0..n).collect::<Vec<_>>());
            assert!(t.iter().all(|m| m.len() >= 3), "{t:?}");
        }
    }

    #[test]
    fn names_are_unique() {
        let logins: std::collections::BTreeSet<String> =
            (0..500).map(|i| person_name(i).1).collect();
        assert_eq!(logins.len(), 500);
    }

    #[test]
    fn keystrokes_add_up_to_the_text() {
        let mut rng = Rng::new(3);
        let s = "Grandma's recipe uses brown sugar, not white — ünïcode too.";
        assert_eq!(keystrokes(&mut rng, s).concat(), s);
    }

    #[test]
    fn pictures_are_pngs_of_about_the_asked_size() {
        let mut rng = Rng::new(5);
        let p = picture(&mut rng, 40);
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert!((35_000..50_000).contains(&p.len()), "{}", p.len());
        // The IHDR chunk's checksum is right (decoders check it).
        let ihdr = &p[12..12 + 4 + 13];
        assert_eq!(crc32(ihdr).to_be_bytes(), p[29..33]);
    }
}

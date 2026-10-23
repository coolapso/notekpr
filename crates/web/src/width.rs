//! How wide notes are shown, from narrow (a comfortable line length) to
//! wide (all the room beside the sidebar and the side panel).
//!
//! Each browser has a default (`UiPrefs::note_width`), which can instead be
//! the same on all of someone's devices (`Prefs::note_width`), and anyone
//! can give a note a width of its own (`Prefs::note_widths`). All of it is
//! personal: nobody else sees how wide you made a note.

use serde::{Deserialize, Deserializer, Serialize};

/// A note's width, as the percentage of the way from narrow (0) to wide
/// (100). The style sheet turns it into a width (`.note-main.wider`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct NoteWidth(u8);

/// The slider's step, in percent.
pub const STEP: u8 = 5;

/// Most notes with a width of their own that are remembered: the oldest
/// choices are forgotten first, so the synced preferences stay small.
pub const MAX_OWN: usize = 500;

impl NoteWidth {
    pub const NARROW: NoteWidth = NoteWidth(0);
    pub const WIDE: NoteWidth = NoteWidth(100);

    /// Rounded to the slider's step, within 0–100.
    pub fn new(percent: f64) -> Self {
        if !percent.is_finite() {
            return Self::NARROW;
        }
        let step = f64::from(STEP);
        NoteWidth(((percent / step).round() * step).clamp(0.0, 100.0) as u8)
    }

    pub fn percent(self) -> u8 {
        self.0
    }

    /// How it reads next to the slider.
    pub fn label(self) -> String {
        match self.0 {
            0 => "Narrow".into(),
            100 => "Wide".into(),
            p => format!("{p}%"),
        }
    }

    /// The note column's inline style (none when narrow).
    pub fn style(self) -> String {
        match self.0 {
            0 => String::new(),
            p => format!("--wide: {}", f64::from(p) / 100.0),
        }
    }
}

impl<'de> Deserialize<'de> for NoteWidth {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(NoteWidth::new(f64::deserialize(d)?))
    }
}

/// The width given to `note` (`None`: the default).
pub fn own(list: &[(String, NoteWidth)], note: &str) -> Option<NoteWidth> {
    list.iter().find(|(n, _)| n == note).map(|(_, w)| *w)
}

/// Give `note` a width of its own, or `None` to use the default again. The
/// latest choice goes first; beyond [`MAX_OWN`] the oldest are dropped.
pub fn set_own(list: &mut Vec<(String, NoteWidth)>, note: &str, width: Option<NoteWidth>) {
    list.retain(|(n, _)| n != note);
    if let Some(w) = width {
        list.insert(0, (note.to_string(), w));
        list.truncate(MAX_OWN);
    }
}

/// A width saved in the synced preferences. Something unreadable (say,
/// written by a newer version) counts as none: one odd value must not make
/// the whole record unreadable, which would lose its key pins and favorites.
pub fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<Option<NoteWidth>, D::Error> {
    Ok(serde_json::Value::deserialize(d)?
        .as_f64()
        .map(NoteWidth::new))
}

/// The notes' own widths; entries that can't be read are skipped (see
/// [`lenient`]).
pub fn lenient_list<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(String, NoteWidth)>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    let entries = v.as_array().map(Vec::as_slice).unwrap_or_default();
    Ok(entries
        .iter()
        .filter_map(|e| {
            let [note, width] = e.as_array()?.as_slice() else {
                return None;
            };
            Some((note.as_str()?.to_string(), NoteWidth::new(width.as_f64()?)))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_snap_to_the_slider() {
        assert_eq!(NoteWidth::new(0.0), NoteWidth::NARROW);
        assert_eq!(NoteWidth::new(42.0).percent(), 40);
        assert_eq!(NoteWidth::new(43.0).percent(), 45);
        assert_eq!(NoteWidth::new(250.0), NoteWidth::WIDE);
        assert_eq!(NoteWidth::new(-5.0), NoteWidth::NARROW);
        assert_eq!(NoteWidth::new(f64::NAN), NoteWidth::NARROW);
        assert_eq!(
            [0.0, 60.0, 100.0].map(|p| NoteWidth::new(p).label()),
            ["Narrow", "60%", "Wide"]
        );
        assert_eq!(NoteWidth::NARROW.style(), "");
        assert_eq!(NoteWidth::new(60.0).style(), "--wide: 0.6");
        assert_eq!(NoteWidth::WIDE.style(), "--wide: 1");
    }

    #[test]
    fn the_latest_choice_comes_first_and_old_ones_go() {
        let mut list = vec![];
        set_own(&mut list, "a", Some(NoteWidth::WIDE));
        set_own(&mut list, "b", Some(NoteWidth::new(50.0)));
        set_own(&mut list, "a", Some(NoteWidth::new(70.0)));
        assert_eq!(list[0], ("a".into(), NoteWidth::new(70.0)));
        assert_eq!(own(&list, "b"), Some(NoteWidth::new(50.0)));
        set_own(&mut list, "b", None);
        assert_eq!((own(&list, "b"), list.len()), (None, 1));
        for i in 0..MAX_OWN + 10 {
            set_own(&mut list, &format!("n{i}"), Some(NoteWidth::WIDE));
        }
        assert_eq!(list.len(), MAX_OWN);
        assert_eq!(own(&list, "a"), None, "the oldest choice was dropped");
        assert!(own(&list, &format!("n{}", MAX_OWN + 9)).is_some());
    }

    #[derive(Deserialize, Serialize, Default)]
    struct Saved {
        #[serde(default, deserialize_with = "lenient")]
        width: Option<NoteWidth>,
        #[serde(default, deserialize_with = "lenient_list")]
        own: Vec<(String, NoteWidth)>,
        #[serde(default)]
        pins: Vec<String>,
    }

    #[test]
    fn odd_values_dont_make_the_record_unreadable() {
        let s = Saved {
            width: Some(NoteWidth::new(35.0)),
            own: vec![("a".into(), NoteWidth::WIDE)],
            pins: vec!["pin".into()],
        };
        let back: Saved = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!((back.width, back.own, back.pins), (s.width, s.own, s.pins));
        let odd = r#"{"width": "huge", "own": [["a", 100], ["b"], 7, ["c", {"x": 1}], ["d", 55]], "pins": ["pin"]}"#;
        let back: Saved = serde_json::from_str(odd).unwrap();
        assert_eq!(back.width, None);
        assert_eq!(
            back.own,
            [
                ("a".into(), NoteWidth::WIDE),
                ("d".into(), NoteWidth::new(55.0))
            ]
        );
        assert_eq!(back.pins, ["pin"]);
        let back: Saved = serde_json::from_str(r#"{"own": {"a": 1}, "pins": []}"#).unwrap();
        assert!(back.own.is_empty());
    }
}

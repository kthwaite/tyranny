//! Splits a radio station's now-playing text into artist and title.

/// Now-playing text from a radio station, split into its parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioText {
    pub artist: String,
    pub title: String,
    pub raw: String,
    /// True when the text carries no usable track name.
    pub junk: bool,
}

/// Parses `raw` for a station named `station`.
pub(crate) fn parse(raw: &str, station: &str) -> RadioText {
    let text = raw.trim();
    let junk = text.is_empty()
        || !text.chars().any(char::is_alphabetic)
        || text.eq_ignore_ascii_case(station.trim());
    if junk {
        return RadioText {
            artist: String::new(),
            title: String::new(),
            raw: raw.to_string(),
            junk,
        };
    }
    let (artist, title) = match text.split_once(" - ") {
        Some((artist, title)) => (artist.trim(), title.trim()),
        None => ("", text),
    };
    RadioText {
        artist: artist.to_string(),
        title: title.to_string(),
        raw: raw.to_string(),
        junk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_at_the_first_spaced_dash() {
        let text = parse("Burial - Archangel - Edit", "NTS");
        assert_eq!(text.artist, "Burial");
        assert_eq!(text.title, "Archangel - Edit");
        assert!(!text.junk);
    }

    #[test]
    fn without_a_separator_the_text_is_the_title() {
        let text = parse("Archangel", "NTS");
        assert_eq!(text.artist, "");
        assert_eq!(text.title, "Archangel");
        assert!(!text.junk);
    }

    #[test]
    fn blank_station_name_and_letterless_text_are_junk() {
        for raw in ["", "   ", "NTS", "- 03:00 -"] {
            let text = parse(raw, "NTS");
            assert!(text.junk, "{raw:?} should be junk");
            assert_eq!(text.artist, "", "{raw:?}");
            assert_eq!(text.title, "", "{raw:?}");
            assert_eq!(text.raw, raw, "{raw:?}");
        }
    }

    #[test]
    fn trims_both_parts() {
        let text = parse("  Burial  -  Archangel ", "NTS");
        assert_eq!(text.artist, "Burial");
        assert_eq!(text.title, "Archangel");
    }
}

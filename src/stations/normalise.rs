/// Normalise a station name for comparison.
/// Lowercases, strips punctuation, expands abbreviations.
pub fn normalise(name: &str) -> String {
    let lower = name.to_lowercase();
    // Strip punctuation except spaces.
    let stripped: String = lower
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();

    let expanded = expand_abbreviations(&stripped);

    // Collapse multiple spaces.
    expanded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn expand_abbreviations(s: &str) -> String {
    let mut result = s.to_string();
    // Word-boundary replacements (simple left-to-right).
    let replacements: &[(&str, &str)] = &[
        (" st ", " street "),
        (" & ", " and "),
        ("london waterloo", "waterloo"),
        ("london victoria", "victoria"),
        ("london bridge", "london bridge"), // keep as-is (common query)
        ("london kings cross", "kings cross"),
        ("london paddington", "paddington"),
        ("london euston", "euston"),
        ("london liverpool street", "liverpool street"),
        ("london cannon street", "cannon street"),
        ("london charing cross", "charing cross"),
        ("london blackfriars", "blackfriars"),
    ];
    for (from, to) in replacements {
        // Only replace if the whole string starts with or equals the pattern
        // (London prefix stripping) or it's a mid-string abbreviation.
        if s.starts_with(from.trim()) && from.contains("london") {
            result = result.replacen(from.trim(), to.trim(), 1);
        } else {
            // Pad with spaces for word-boundary matching.
            let padded_from = format!(" {from} ");
            let padded_to = format!(" {to} ");
            let padded_result = format!(" {result} ");
            if padded_result.contains(&padded_from) {
                result = padded_result
                    .replacen(&padded_from, &padded_to, 1)
                    .trim()
                    .to_string();
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_strips_punctuation() {
        assert_eq!(normalise("King's Cross"), "kings cross");
    }

    #[test]
    fn strips_london_prefix() {
        assert_eq!(normalise("London Kings Cross"), "kings cross");
    }

    #[test]
    fn collapses_spaces() {
        assert_eq!(normalise("  Waterloo  "), "waterloo");
    }
}

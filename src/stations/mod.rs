mod normalise;

use anyhow::{Context, Result};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use std::{collections::HashMap, path::Path};

/// A single station from the CRS list.
#[derive(Debug, Clone)]
pub struct Station {
    pub crs: String,
    pub name: String,
}

#[derive(Debug)]
pub enum FindResult {
    Exact(String),
    Ambiguous(Vec<(String, String)>), // (display_name, crs)
    NotFound,
}

/// In-memory index over the CRS station list.
pub struct StationIndex {
    by_crs: HashMap<String, Station>,
    by_normalised_name: HashMap<String, Vec<String>>, // normalised_name → [crs, ...]
    all: Vec<(String, String)>,                       // (normalised_name, crs)
}

impl StationIndex {
    /// Load the index from a CSV file with columns: `crs,station_name`.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read CRS CSV: {}", path.display()))?;

        let mut by_crs = HashMap::new();
        let mut by_normalised_name: HashMap<String, Vec<String>> = HashMap::new();
        let mut all = Vec::new();

        for line in raw.lines().skip(1) {
            // Skip header.
            let mut parts = line.splitn(2, ',');
            let crs = match parts.next() {
                Some(c) => c.trim().to_uppercase(),
                None => continue,
            };
            let name = match parts.next() {
                Some(n) => n.trim().to_string(),
                None => continue,
            };
            if crs.is_empty() || name.is_empty() {
                continue;
            }
            let norm = normalise::normalise(&name);
            all.push((norm.clone(), crs.clone()));
            by_normalised_name
                .entry(norm)
                .or_default()
                .push(crs.clone());
            by_crs.insert(crs.clone(), Station { crs, name });
        }

        Ok(Self {
            by_crs,
            by_normalised_name,
            all,
        })
    }

    /// Look up a station by its CRS code (case-insensitive).
    pub fn by_crs(&self, crs: &str) -> Option<&Station> {
        self.by_crs.get(&crs.to_uppercase())
    }

    /// Find a station by name using exact match → partial match → fuzzy fallback.
    ///
    /// Also handles `"Name (CRS)"` input (e.g. `"London Waterloo (WAT)"`) and
    /// bare 3-letter CRS codes so users can disambiguate by typing the code.
    pub fn find(&self, query: &str) -> FindResult {
        // Direct CRS lookup — bare code ("WAT") or name with code suffix ("London Waterloo (WAT)").
        let crs_candidate = if query.len() == 3 {
            Some(query.to_uppercase())
        } else if query.ends_with(')') {
            query
                .rfind('(')
                .map(|i| query[i + 1..query.len() - 1].trim().to_uppercase())
        } else {
            None
        };
        if let Some(crs) = crs_candidate
            && self.by_crs.contains_key(&crs)
        {
            return FindResult::Exact(crs);
        }

        let norm = normalise::normalise(query);

        // 1. Exact normalised match — also collect prefix extensions (e.g. "waterloo east"
        //    when the query is "waterloo") so they surface alongside the exact hit.
        if let Some(codes) = self.by_normalised_name.get(&norm) {
            let prefix = format!("{norm} ");
            let mut all_codes = codes.clone();
            for (n, crs) in &self.all {
                if n.starts_with(&prefix) && !all_codes.contains(crs) {
                    all_codes.push(crs.clone());
                }
            }
            if all_codes.len() == 1 {
                return FindResult::Exact(all_codes[0].clone());
            } else {
                return FindResult::Ambiguous(self.to_display(&all_codes));
            }
        }

        // 2. Token-based partial match.
        let tokens: Vec<&str> = norm.split_whitespace().collect();
        let partial: Vec<String> = self
            .all
            .iter()
            .filter(|(n, _)| tokens.iter().all(|t| n.contains(t)))
            .map(|(_, crs)| crs.clone())
            .collect();

        if partial.len() == 1 {
            return FindResult::Exact(partial[0].clone());
        }
        if !partial.is_empty() {
            return FindResult::Ambiguous(self.to_display(&partial));
        }

        // 3. Fuzzy fallback.
        let matcher = SkimMatcherV2::default();
        let threshold = 60_i64;
        let mut scored: Vec<(i64, &str, &str)> = self
            .all
            .iter()
            .filter_map(|(n, crs)| {
                matcher
                    .fuzzy_match(n, &norm)
                    .filter(|&s| s >= threshold)
                    .map(|s| (s, n.as_str(), crs.as_str()))
            })
            .collect();
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
        scored.truncate(5);

        if scored.is_empty() {
            return FindResult::NotFound;
        }
        if scored.len() == 1 {
            return FindResult::Exact(scored[0].2.to_string());
        }
        let codes: Vec<String> = scored.iter().map(|(_, _, crs)| crs.to_string()).collect();
        FindResult::Ambiguous(self.to_display(&codes))
    }

    fn to_display(&self, codes: &[String]) -> Vec<(String, String)> {
        codes
            .iter()
            .filter_map(|c| {
                self.by_crs
                    .get(c)
                    .map(|s| (format!("{} ({})", s.name, s.crs), s.crs.clone()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn make_index(csv: &str) -> StationIndex {
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        f.write_all(csv.as_bytes()).expect("write");
        StationIndex::load(f.path()).expect("load")
    }

    const SAMPLE_CSV: &str = "crs,station_name\nWAT,London Waterloo\nWAE,London Waterloo East\nSUR,Surbiton\nWIM,Wimbledon\nKGX,London Kings Cross\n";

    #[test]
    fn exact_match() {
        let idx = make_index(SAMPLE_CSV);
        match idx.find("Surbiton") {
            FindResult::Exact(crs) => assert_eq!(crs, "SUR"),
            other => panic!("Expected Exact, got {other:?}"),
        }
    }

    #[test]
    fn partial_match_kings_cross() {
        let idx = make_index(SAMPLE_CSV);
        match idx.find("kings cross") {
            FindResult::Exact(crs) => assert_eq!(crs, "KGX"),
            other => panic!("Expected Exact, got {other:?}"),
        }
    }

    #[test]
    fn by_crs_lookup() {
        let idx = make_index(SAMPLE_CSV);
        let s = idx.by_crs("WAT").expect("WAT should exist");
        assert_eq!(s.name, "London Waterloo");
    }

    #[test]
    fn waterloo_returns_both_stations() {
        let idx = make_index(SAMPLE_CSV);
        match idx.find("waterloo") {
            FindResult::Ambiguous(candidates) => {
                let crss: Vec<&str> = candidates.iter().map(|(_, crs)| crs.as_str()).collect();
                assert!(crss.contains(&"WAT"), "WAT missing from {crss:?}");
                assert!(crss.contains(&"WAE"), "WAE missing from {crss:?}");
            }
            other => panic!("Expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn not_found() {
        let idx = make_index(SAMPLE_CSV);
        assert!(matches!(
            idx.find("Nonexistent Station XYZ"),
            FindResult::NotFound
        ));
    }

    #[test]
    fn bare_crs_resolves_exactly() {
        let idx = make_index(SAMPLE_CSV);
        match idx.find("WAT") {
            FindResult::Exact(crs) => assert_eq!(crs, "WAT"),
            other => panic!("Expected Exact, got {other:?}"),
        }
    }

    #[test]
    fn name_with_crs_suffix_resolves_exactly() {
        let idx = make_index(SAMPLE_CSV);
        match idx.find("London Waterloo (WAT)") {
            FindResult::Exact(crs) => assert_eq!(crs, "WAT"),
            other => panic!("Expected Exact, got {other:?}"),
        }
    }

    #[test]
    fn name_with_lowercase_crs_suffix_resolves_exactly() {
        let idx = make_index(SAMPLE_CSV);
        match idx.find("London Waterloo (wat)") {
            FindResult::Exact(crs) => assert_eq!(crs, "WAT"),
            other => panic!("Expected Exact, got {other:?}"),
        }
    }
}

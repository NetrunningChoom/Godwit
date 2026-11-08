// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A network's privacy overview for Settings: short factual paragraphs, each
//! with sources. Networks write their own; core never writes the wording.

/// One claim and where it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyParagraph {
    pub text: String,
    /// (label, address) pairs, opened in the browser. Every claim has one.
    pub sources: Vec<(String, String)>,
}

/// Shown under every set of sources: they are snapshots in time.
pub const SOURCES_CAVEAT: &str = "These sources are from the dates shown and may be out of date; things may have changed or improved since. Read them and make your own judgement.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacySection {
    pub title: String,
    pub paragraphs: Vec<PrivacyParagraph>,
    /// Shown under the sources (normally [`SOURCES_CAVEAT`]).
    pub caveat: String,
}

impl PrivacySection {
    /// Whether every claim is sourced and every source is an https address.
    pub fn is_sourced(&self) -> bool {
        self.paragraphs
            .iter()
            .all(|p| !p.sources.is_empty() && p.sources.iter().all(|(_, u)| u.starts_with("https://")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsourced_claims_are_caught() {
        let mut s = PrivacySection {
            title: "T".into(),
            paragraphs: vec![PrivacyParagraph {
                text: "x".into(),
                sources: vec![("a".into(), "https://a.example".into())],
            }],
            caveat: String::new(),
        };
        assert!(s.is_sourced());
        s.paragraphs[0].sources[0].1 = "http://a.example".into();
        assert!(!s.is_sourced());
        s.paragraphs[0].sources.clear();
        assert!(!s.is_sourced());
    }
}

// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A network's privacy overview for Settings: short factual paragraphs, each
//! with sources. Networks write their own; core never writes the wording. Each network lists
//! its own official pages first, then third-party sources, each with why.

/// One source, or one claim and where it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyParagraph {
    /// The claim; empty for a plain link (the UI then shows only `why` and the sources).
    pub text: String,
    /// One neutral line on why the sources are listed: what a reader learns there.
    pub why: String,
    /// (label, address) pairs, opened in the browser. Every claim has one.
    pub sources: Vec<(String, String)>,
}

/// Shown under every set of sources: they are snapshots in time.
pub const SOURCES_CAVEAT: &str = "These sources are from the dates shown and may be out of date; things may have changed or improved since. Read them and make your own judgement.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacySection {
    pub title: String,
    /// What the sources below are about, shown under the title (may be empty).
    pub intro: String,
    pub paragraphs: Vec<PrivacyParagraph>,
    /// An optional closing line the network writes, shown above the caveat.
    pub takeaway: String,
    /// Shown under the sources (normally [`SOURCES_CAVEAT`]).
    pub caveat: String,
}

impl PrivacySection {
    /// The takeaway (if any) and the caveat, as one text under the sources.
    pub fn footer(&self) -> String {
        if self.takeaway.is_empty() {
            self.caveat.clone()
        } else {
            format!("{}\n\n{}", self.takeaway, self.caveat)
        }
    }

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
            intro: String::new(),
            paragraphs: vec![PrivacyParagraph {
                text: "x".into(),
                why: "y".into(),
                sources: vec![("a".into(), "https://a.example".into())],
            }],
            takeaway: String::new(),
            caveat: String::new(),
        };
        assert!(s.is_sourced());
        s.paragraphs[0].sources[0].1 = "http://a.example".into();
        assert!(!s.is_sourced());
        s.paragraphs[0].sources.clear();
        assert!(!s.is_sourced());
    }
}

// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The emoji of Matrix's verification table (64, spec.matrix.org "SAS
//! method: emoji"), bundled so verifying a device fetches nothing from any
//! server. Fluent Emoji, MIT (see `assets/emoji/sas/LICENSE`).

/// (codepoints in hex, joined by `-`; 48×48 PNG)
const TABLE: [(&str, &[u8]); 64] = [
    ("1f30f", include_bytes!("../../assets/emoji/sas/1f30f.png")),
    ("1f319", include_bytes!("../../assets/emoji/sas/1f319.png")),
    ("1f333", include_bytes!("../../assets/emoji/sas/1f333.png")),
    ("1f335", include_bytes!("../../assets/emoji/sas/1f335.png")),
    ("1f337", include_bytes!("../../assets/emoji/sas/1f337.png")),
    ("1f33d", include_bytes!("../../assets/emoji/sas/1f33d.png")),
    ("1f344", include_bytes!("../../assets/emoji/sas/1f344.png")),
    ("1f34c", include_bytes!("../../assets/emoji/sas/1f34c.png")),
    ("1f34e", include_bytes!("../../assets/emoji/sas/1f34e.png")),
    ("1f353", include_bytes!("../../assets/emoji/sas/1f353.png")),
    ("1f355", include_bytes!("../../assets/emoji/sas/1f355.png")),
    ("1f381", include_bytes!("../../assets/emoji/sas/1f381.png")),
    ("1f382", include_bytes!("../../assets/emoji/sas/1f382.png")),
    ("1f385", include_bytes!("../../assets/emoji/sas/1f385.png")),
    ("1f3a7", include_bytes!("../../assets/emoji/sas/1f3a7.png")),
    ("1f3a9", include_bytes!("../../assets/emoji/sas/1f3a9.png")),
    ("1f3b8", include_bytes!("../../assets/emoji/sas/1f3b8.png")),
    ("1f3ba", include_bytes!("../../assets/emoji/sas/1f3ba.png")),
    ("1f3c1", include_bytes!("../../assets/emoji/sas/1f3c1.png")),
    ("1f3c6", include_bytes!("../../assets/emoji/sas/1f3c6.png")),
    ("1f40e", include_bytes!("../../assets/emoji/sas/1f40e.png")),
    ("1f413", include_bytes!("../../assets/emoji/sas/1f413.png")),
    ("1f418", include_bytes!("../../assets/emoji/sas/1f418.png")),
    ("1f419", include_bytes!("../../assets/emoji/sas/1f419.png")),
    ("1f41f", include_bytes!("../../assets/emoji/sas/1f41f.png")),
    ("1f422", include_bytes!("../../assets/emoji/sas/1f422.png")),
    ("1f427", include_bytes!("../../assets/emoji/sas/1f427.png")),
    ("1f430", include_bytes!("../../assets/emoji/sas/1f430.png")),
    ("1f431", include_bytes!("../../assets/emoji/sas/1f431.png")),
    ("1f436", include_bytes!("../../assets/emoji/sas/1f436.png")),
    ("1f437", include_bytes!("../../assets/emoji/sas/1f437.png")),
    ("1f43c", include_bytes!("../../assets/emoji/sas/1f43c.png")),
    ("1f44d", include_bytes!("../../assets/emoji/sas/1f44d.png")),
    ("1f453", include_bytes!("../../assets/emoji/sas/1f453.png")),
    ("1f4a1", include_bytes!("../../assets/emoji/sas/1f4a1.png")),
    ("1f4c1", include_bytes!("../../assets/emoji/sas/1f4c1.png")),
    ("1f4cc", include_bytes!("../../assets/emoji/sas/1f4cc.png")),
    ("1f4ce", include_bytes!("../../assets/emoji/sas/1f4ce.png")),
    ("1f4d5", include_bytes!("../../assets/emoji/sas/1f4d5.png")),
    ("1f511", include_bytes!("../../assets/emoji/sas/1f511.png")),
    ("1f512", include_bytes!("../../assets/emoji/sas/1f512.png")),
    ("1f514", include_bytes!("../../assets/emoji/sas/1f514.png")),
    ("1f525", include_bytes!("../../assets/emoji/sas/1f525.png")),
    ("1f527", include_bytes!("../../assets/emoji/sas/1f527.png")),
    ("1f528", include_bytes!("../../assets/emoji/sas/1f528.png")),
    ("1f600", include_bytes!("../../assets/emoji/sas/1f600.png")),
    ("1f680", include_bytes!("../../assets/emoji/sas/1f680.png")),
    ("1f682", include_bytes!("../../assets/emoji/sas/1f682.png")),
    ("1f6b2", include_bytes!("../../assets/emoji/sas/1f6b2.png")),
    ("1f916", include_bytes!("../../assets/emoji/sas/1f916.png")),
    ("1f981", include_bytes!("../../assets/emoji/sas/1f981.png")),
    ("1f984", include_bytes!("../../assets/emoji/sas/1f984.png")),
    ("1f98b", include_bytes!("../../assets/emoji/sas/1f98b.png")),
    ("231b", include_bytes!("../../assets/emoji/sas/231b.png")),
    ("23f0", include_bytes!("../../assets/emoji/sas/23f0.png")),
    ("2601", include_bytes!("../../assets/emoji/sas/2601.png")),
    ("2602", include_bytes!("../../assets/emoji/sas/2602.png")),
    ("260e", include_bytes!("../../assets/emoji/sas/260e.png")),
    ("2693", include_bytes!("../../assets/emoji/sas/2693.png")),
    ("26bd", include_bytes!("../../assets/emoji/sas/26bd.png")),
    ("2702", include_bytes!("../../assets/emoji/sas/2702.png")),
    ("2708", include_bytes!("../../assets/emoji/sas/2708.png")),
    ("270f", include_bytes!("../../assets/emoji/sas/270f.png")),
    ("2764", include_bytes!("../../assets/emoji/sas/2764.png")),
];

/// The picture for an emoji of the table, if it's one.
pub fn png(symbol: &str) -> Option<&'static [u8]> {
    // Variation selectors (U+FE0F) aren't part of the names.
    let code: Vec<String> = symbol
        .chars()
        .filter(|c| *c != '\u{fe0f}')
        .map(|c| format!("{:x}", u32::from(c)))
        .collect();
    let code = code.join("-");
    TABLE.iter().find(|(c, _)| *c == code).map(|(_, png)| *png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_table_is_here() {
        assert_eq!(TABLE.len(), 64);
        assert!(png("🐶").is_some());
        assert!(png("❤️").is_some(), "with a variation selector");
        assert!(png("✈").is_some(), "without one");
        assert!(png("🦀").is_none(), "not in the table");
        for (code, bytes) in TABLE {
            assert!(image::load_from_memory(bytes).is_ok(), "{code} decodes");
        }
    }
}

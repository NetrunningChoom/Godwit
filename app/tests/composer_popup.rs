// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The composer's suggestion list is a PopupWindow, which Slint only shows
//! through `show()`; a bound `visible` never opens it.

#[test]
fn autocomplete_popup_is_opened_by_show() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../crates/godwit-ui/ui/timeline.slint");
    let source = std::fs::read_to_string(path).expect("timeline.slint is readable");
    assert!(
        source.contains("autocomplete-menu.show()"),
        "the suggestion popup is never shown"
    );
}

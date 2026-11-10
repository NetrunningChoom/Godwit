// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

//! Auto Moderation rules, mapped to the neutral [`ModerationRule`]. Shapes
//! from <https://docs.discord.food/resources/auto-moderation>.

use godwit_core::server_settings::ModerationRule;
use serde_json::Value;

/// The rule's trigger in words (`trigger_type`: 1 keyword, 3 spam, 4 keyword
/// preset, 5 mention spam, 6 user profile, 7 guild policy).
fn kind(trigger_type: i64) -> &'static str {
    match trigger_type {
        1 => "Keywords",
        3 => "Spam",
        4 => "Common word lists",
        5 => "Mention spam",
        6 => "Profile keywords",
        7 => "Server policy",
        _ => "Other",
    }
}

/// The rules of a `GET .../auto-moderation/rules` answer.
pub fn parse_rules(list: &Value) -> Vec<ModerationRule> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let id = r["id"].as_str()?;
            Some(ModerationRule {
                id: id.into(),
                name: r["name"].as_str().unwrap_or_default().into(),
                enabled: r["enabled"].as_bool().unwrap_or(false),
                kind: kind(r["trigger_type"].as_i64().unwrap_or(0)).into(),
                keywords: r["trigger_metadata"]["keyword_filter"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|k| k.as_str().map(Into::into))
                    .collect(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_rules_with_keywords_and_unknown_triggers() {
        let rules = parse_rules(&json!([
            {"id": "1", "name": "No links", "enabled": true, "trigger_type": 1,
             "trigger_metadata": {"keyword_filter": ["http", "www"]}},
            {"id": "2", "name": "Spam", "trigger_type": 3},
            {"id": "3", "name": "?", "enabled": false, "trigger_type": 99},
            {"name": "no id"}
        ]));
        assert_eq!(rules.len(), 3);
        assert_eq!(&*rules[0].kind, "Keywords");
        assert!(rules[0].enabled);
        assert_eq!(rules[0].keywords.len(), 2);
        assert!(!rules[1].enabled && rules[1].keywords.is_empty());
        assert_eq!(&*rules[2].kind, "Other");
    }

    #[test]
    fn a_non_list_answer_is_no_rules() {
        assert!(parse_rules(&json!({"message": "Missing Permissions"})).is_empty());
    }
}

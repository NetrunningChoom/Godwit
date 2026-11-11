// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Slack's message markup ("mrkdwn",
//! <https://api.slack.com/reference/surfaces/formatting>) to Godwit rich text
//! and plain text, and what the user typed back to Slack's text.
//!
//! `*bold*`, `_italic_`, `~strike~`, `` `code` ``, ```` ```blocks``` ````, `>`
//! quotes (sent as `&gt;`), `<@U…>` mentions, `<#C…|name>` channels,
//! `<!here>`-style broadcasts, `<url|label>` links and `:shortcode:` emoji
//! (as the emoji itself, so the shared emoji pictures show; custom ones stay
//! as their `:name:`). Never panics; anything odd stays text.

use godwit_core::rich;

/// Names for what mrkdwn refers to by id.
pub trait Names {
    /// A user's name, and the id to link (`T…:U…`).
    fn user(&self, id: &str) -> (String, String);
    /// A channel's name, and the room id to link, if known.
    fn channel(&self, id: &str) -> (String, Option<String>);
    /// A custom emoji's image URL from the workspace's `emoji.list` response.
    fn custom_emoji(&self, _name: &str) -> Option<(String, String)> {
        None
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Converted {
    pub rich: String,
    pub plain: String,
}

/// `&amp;`, `&lt;`, `&gt;`: the only entities Slack writes.
pub fn decode(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// `:name:` (and `:name::skin-tone-2:`) as the emoji, if it's a standard one.
pub fn shortcode(name: &str) -> Option<String> {
    let base = name.split("::").next().unwrap_or(name);
    let e = emojis::get_by_shortcode(base)?;
    let tone = name.split("::skin-tone-").nth(1).and_then(|t| t.parse::<u8>().ok());
    let toned = tone.and_then(|t| {
        let tone = match t {
            2 => emojis::SkinTone::Light,
            3 => emojis::SkinTone::MediumLight,
            4 => emojis::SkinTone::Medium,
            5 => emojis::SkinTone::MediumDark,
            6 => emojis::SkinTone::Dark,
            _ => return None,
        };
        e.with_skin_tone(tone)
    });
    Some(toned.unwrap_or(e).as_str().to_owned())
}

/// Resolve Slack's custom emoji aliases and reactions to their canonical image entry.
pub fn custom_emoji<'a>(
    name: &str,
    emoji: &'a serde_json::Map<String, serde_json::Value>,
) -> Option<(&'a str, &'a str)> {
    let mut key = name;
    // Bounded, so an alias cycle cannot loop.
    for _ in 0..8 {
        let (stored, value) = emoji.get_key_value(key)?;
        let value = value.as_str()?;
        if let Some(alias) = value.strip_prefix("alias:") {
            key = alias;
        } else {
            return Some((stored.as_str(), value));
        }
    }
    None
}

/// The shortcode Slack reacts with for an emoji (`👍` → `+1`).
pub fn reaction_name(emoji: &str) -> Option<String> {
    let e = emojis::get(emoji)
        .or_else(|| emojis::get(&format!("{emoji}\u{fe0f}")))
        .or_else(|| emojis::get(&emoji.replace('\u{fe0f}', "")))?;
    // Slack names the thumbs as `+1` / `-1`, as GitHub's shortcodes do.
    e.shortcode().map(str::to_owned)
}

/// Where an emphasis mark may open: after start, a space or punctuation,
/// before something that isn't a space.
fn opens(chars: &[char], i: usize) -> bool {
    let before =
        i == 0 || chars[i - 1].is_whitespace() || (chars[i - 1].is_ascii_punctuation() && chars[i - 1] != chars[i]);
    let after = chars.get(i + 1).is_some_and(|c| !c.is_whitespace() && *c != chars[i]);
    before && after
}

fn closes(chars: &[char], i: usize) -> bool {
    let before = i > 0 && !chars[i - 1].is_whitespace();
    let after = chars
        .get(i + 1)
        .is_none_or(|c| c.is_whitespace() || c.is_ascii_punctuation());
    before && after
}

#[derive(Default)]
struct Out {
    rich: String,
    plain: String,
}

impl Out {
    fn text(&mut self, s: &str) {
        self.rich.push_str(&rich::escape(s));
        self.plain.push_str(s);
    }
}

/// One `<…>` reference.
fn reference(inner: &str, names: &impl Names, out: &mut Out) {
    let (target, label) = match inner.split_once('|') {
        Some((t, l)) => (t, Some(decode(l))),
        None => (inner, None),
    };
    if let Some(user) = target.strip_prefix('@') {
        let (name, id) = names.user(user);
        let name = label.unwrap_or(name);
        out.rich
            .push_str(&format!("[{}](godwit://user/{id})", rich::escape(&format!("@{name}"))));
        out.plain.push_str(&format!("@{name}"));
    } else if let Some(channel) = target.strip_prefix('#') {
        let (name, room) = names.channel(channel);
        let name = label.unwrap_or(name);
        match room {
            Some(room) => out.rich.push_str(&format!(
                "[{}](godwit://room/{room})",
                rich::escape(&format!("#{name}"))
            )),
            None => out.rich.push_str(&rich::escape(&format!("#{name}"))),
        }
        out.plain.push_str(&format!("#{name}"));
    } else if let Some(special) = target.strip_prefix('!') {
        // `<!here>`, `<!channel>`, `<!everyone>`, `<!subteam^S…|@team>`, `<!date^…|text>`.
        let word = special.split('^').next().unwrap_or(special);
        let shown = label.unwrap_or_else(|| format!("@{word}"));
        if matches!(word, "here" | "channel" | "everyone" | "subteam") {
            // Broadcasts and user groups show as mentions (nothing to open).
            out.rich
                .push_str(&format!("[{}](godwit://mention/{word})", rich::escape(&shown)));
            out.plain.push_str(&shown);
        } else {
            out.text(&shown);
        }
    } else {
        let url = decode(target);
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:") {
            let shown = label.unwrap_or_else(|| url.trim_start_matches("mailto:").to_owned());
            let href: String = url
                .chars()
                .map(|c| match c {
                    '(' => "%28".to_owned(),
                    ')' => "%29".to_owned(),
                    ' ' => "%20".to_owned(),
                    c => c.to_string(),
                })
                .collect();
            out.rich.push_str(&format!("[{}]({href})", rich::escape(&shown)));
            out.plain.push_str(&shown);
        } else {
            out.text(&label.unwrap_or(url));
        }
    }
}

/// One line (no code blocks, no quote marker).
fn line(text: &str, names: &impl Names, out: &mut Out) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut plain = String::new();
    let flush = |plain: &mut String, out: &mut Out| {
        if !plain.is_empty() {
            out.text(&decode(plain));
            plain.clear();
        }
    };
    while i < chars.len() {
        let c = chars[i];
        let close_of = |mark: char, from: usize| (from..chars.len()).find(|&j| chars[j] == mark && closes(&chars, j));
        match c {
            '`' => {
                if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '`') {
                    flush(&mut plain, out);
                    let code: String = chars[i + 1..end].iter().collect();
                    let code = decode(&code);
                    out.rich.push_str(&rich::code_span(&code));
                    out.plain.push_str(&code);
                    i = end + 1;
                    continue;
                }
            }
            '<' => {
                if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '>') {
                    flush(&mut plain, out);
                    let inner: String = chars[i + 1..end].iter().collect();
                    reference(&inner, names, out);
                    i = end + 1;
                    continue;
                }
            }
            ':' => {
                // `:name:` or `:name::skin-tone-3:`.
                let mut end = (i + 1..chars.len()).find(|&j| chars[j] == ':');
                // Only a `::skin-tone-N:` suffix continues the name: `:a::b:` is two emoji.
                let tone: Vec<char> = "skin-tone-".chars().collect();
                if let Some(e) = end
                    && chars.get(e + 1) == Some(&':')
                    && chars.get(e + 2..e + 2 + tone.len()) == Some(tone.as_slice())
                {
                    end = (e + 2..chars.len()).find(|&j| chars[j] == ':');
                }
                if let Some(end) = end {
                    let name: String = chars[i + 1..end].iter().collect();
                    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || "_+-:".contains(c)) {
                        if let Some(emoji) = shortcode(&name) {
                            flush(&mut plain, out);
                            out.text(&emoji);
                            i = end + 1;
                            continue;
                        }
                        if let Some((canonical, url)) = names.custom_emoji(&name)
                            && url.starts_with("https://")
                        {
                            flush(&mut plain, out);
                            out.rich.push_str(&rich::emoji(&canonical, &url));
                            out.plain.push_str(&format!(":{canonical}:"));
                            i = end + 1;
                            continue;
                        }
                    }
                }
            }
            '*' | '_' | '~' if opens(&chars, i) => {
                if let Some(end) = close_of(c, i + 1).filter(|&e| e > i + 1) {
                    flush(&mut plain, out);
                    let mark = match c {
                        '*' => "**",
                        '_' => "*",
                        _ => "~~",
                    };
                    let inner: String = chars[i + 1..end].iter().collect();
                    let mut sub = Out::default();
                    line(&inner, names, &mut sub);
                    out.rich.push_str(mark);
                    out.rich.push_str(&sub.rich);
                    out.rich.push_str(mark);
                    out.plain.push_str(&sub.plain);
                    i = end + 1;
                    continue;
                }
            }
            _ => {}
        }
        plain.push(c);
        i += 1;
    }
    flush(&mut plain, out);
}

/// Slack's text to rich and plain text.
pub fn convert(text: &str, names: &impl Names) -> Converted {
    let mut out = Out::default();
    let mut first = true;
    let fences = text.matches("```").count();
    for (k, part) in text.split("```").enumerate() {
        // Odd parts are inside a code block if a fence closes them (an
        // unclosed one stays text).
        if k % 2 == 1 && k < fences {
            let code = decode(part.trim_matches('\n'));
            if !out.rich.is_empty() {
                out.rich.push_str("\n\n");
                out.plain.push('\n');
            }
            out.rich.push_str(&rich::code_block("", &code));
            out.plain.push_str(&code);
            out.rich.push_str("\n\n");
            first = true;
            continue;
        }
        let mut quote: Vec<Out> = Vec::new();
        for l in part.split('\n') {
            if let Some(q) = l.strip_prefix("&gt;").or_else(|| l.strip_prefix('>')) {
                let mut o = Out::default();
                line(q.strip_prefix(' ').unwrap_or(q), names, &mut o);
                quote.push(o);
                continue;
            }
            if !quote.is_empty() {
                end_quote(&mut out, &mut quote);
                first = true;
            }
            if !first {
                out.rich.push_str("\\\n");
                out.plain.push('\n');
            }
            first = false;
            line(l, names, &mut out);
        }
        if !quote.is_empty() {
            end_quote(&mut out, &mut quote);
            first = true;
        }
    }
    Converted {
        rich: out.rich.trim().to_owned(),
        plain: out.plain.trim().to_owned(),
    }
}

fn end_quote(out: &mut Out, quote: &mut Vec<Out>) {
    let inner: Vec<String> = quote.iter().map(|q| q.rich.clone()).collect();
    if !out.rich.is_empty() {
        out.rich.push_str("\n\n");
        out.plain.push('\n');
    }
    out.rich.push_str(&rich::quote(&inner.join("\\\n")));
    out.rich.push_str("\n\n");
    out.plain
        .push_str(&quote.iter().map(|q| q.plain.clone()).collect::<Vec<_>>().join("\n"));
    out.plain.push('\n');
    quote.clear();
}

/// What the user typed, as Slack's text: `&`, `<` and `>` escaped (Slack
/// reads its own markup, `*bold*` and the rest, from the rest).
pub fn from_text(text: &str) -> String {
    // Slack's Web API treats ID references as active mentions. Preserve the
    // same tokens the official client writes when an autocomplete selection
    // is inserted; escaping their angle brackets silently turns them into
    // ordinary text. Everything else remains escaped.
    let mut out = String::with_capacity(text.len());
    let mut plain = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        plain.push_str(&rest[..start]);
        let Some(end) = rest[start + 1..].find('>') else {
            plain.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let end = start + end + 2;
        let token = &rest[start..end];
        if slack_reference(&rest[start + 1..end - 1]) {
            out.push_str(&escape_slack_text(&plain));
            plain.clear();
            out.push_str(token);
        } else {
            plain.push_str(token);
        }
        rest = &rest[end..];
    }
    plain.push_str(rest);
    out.push_str(&escape_slack_text(&plain));
    out
}

fn escape_slack_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn slack_reference(reference: &str) -> bool {
    let (target, _) = reference.split_once('|').unwrap_or((reference, ""));
    let valid_id = |prefixes: &[u8], id: &str| {
        id.as_bytes().split_first().is_some_and(|(prefix, rest)| {
            prefixes.contains(prefix) && !rest.is_empty() && rest.iter().all(u8::is_ascii_alphanumeric)
        })
    };
    target.strip_prefix('@').is_some_and(|id| valid_id(b"UW", id))
        || target.strip_prefix('#').is_some_and(|id| valid_id(b"CGD", id))
        || matches!(target, "!here" | "!channel" | "!everyone")
        || target.strip_prefix("!subteam^").is_some_and(|id| valid_id(b"S", id))
}

/// The mrkdwn a message shows. Apps lay theirs out in Block Kit
/// (https://api.slack.com/reference/block-kit/blocks) and legacy attachments
/// (https://api.slack.com/reference/messaging/attachments), leaving `text`
/// as a fallback or empty; Slack's client shows the blocks then. A person's
/// own message carries only `rich_text` blocks, the same as its `text`.
pub fn message_source(m: &serde_json::Value) -> String {
    let text = m["text"].as_str().unwrap_or_default();
    let blocks = m["blocks"].as_array().map(Vec::as_slice).unwrap_or_default();
    let laid_out = blocks.iter().any(|b| b["type"] != "rich_text");
    let mut parts: Vec<String> = Vec::new();
    if laid_out || text.trim().is_empty() {
        parts.extend(blocks.iter().filter_map(block_text).filter(|t| !t.trim().is_empty()));
    }
    if parts.is_empty() && !text.trim().is_empty() {
        parts.push(text.to_owned());
    }
    for a in m["attachments"].as_array().into_iter().flatten() {
        // Link previews come as attachments with `from_url`; they aren't the
        // message's own words.
        if a.get("from_url").is_some() && a["is_app_unfurl"] != true {
            continue;
        }
        let mut lines: Vec<String> = Vec::new();
        let field = |k: &str| a[k].as_str().filter(|t| !t.trim().is_empty()).map(str::to_owned);
        lines.extend(field("pretext"));
        lines.extend(field("author_name"));
        if let Some(title) = field("title") {
            lines.push(match a["title_link"].as_str() {
                Some(link) if link.starts_with("https://") => format!("*<{link}|{title}>*"),
                _ => format!("*{title}*"),
            });
        }
        lines.extend(field("text"));
        for f in a["fields"].as_array().into_iter().flatten() {
            match (f["title"].as_str(), f["value"].as_str()) {
                (Some(t), Some(v)) if !t.is_empty() => lines.push(format!("*{t}*\n{v}")),
                (_, Some(v)) => lines.push(v.to_owned()),
                _ => {}
            }
        }
        if let Some(blocks) = a["blocks"].as_array() {
            lines.extend(blocks.iter().filter_map(block_text).filter(|t| !t.trim().is_empty()));
        }
        lines.extend(field("footer"));
        if lines.is_empty() {
            lines.extend(field("fallback"));
        }
        if !lines.is_empty() {
            parts.push(lines.join("\n"));
        }
    }
    parts.join("\n")
}

/// One block's words, as mrkdwn.
fn block_text(b: &serde_json::Value) -> Option<String> {
    let text_of = |t: &serde_json::Value| t["text"].as_str().map(str::to_owned);
    match b["type"].as_str()? {
        "section" => {
            let mut lines: Vec<String> = text_of(&b["text"]).into_iter().collect();
            lines.extend(b["fields"].as_array().into_iter().flatten().filter_map(text_of));
            Some(lines.join("\n"))
        }
        "header" => text_of(&b["text"]).map(|t| format!("*{t}*")),
        "context" => Some(
            b["elements"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|e| matches!(e["type"].as_str(), Some("mrkdwn" | "plain_text")))
                .filter_map(text_of)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        "rich_text" => Some(rich_text(&b["elements"])),
        "image" => b["title"]["text"]
            .as_str()
            .or(b["alt_text"].as_str())
            .map(str::to_owned),
        _ => None,
    }
}

/// A `rich_text` block's elements back as mrkdwn
/// (https://api.slack.com/reference/block-kit/blocks#rich_text).
fn rich_text(elements: &serde_json::Value) -> String {
    let mut out = String::new();
    for e in elements.as_array().into_iter().flatten() {
        match e["type"].as_str().unwrap_or_default() {
            "text" => out.push_str(e["text"].as_str().unwrap_or_default()),
            "link" => match (e["url"].as_str(), e["text"].as_str()) {
                (Some(url), Some(text)) => out.push_str(&format!("<{url}|{text}>")),
                (Some(url), None) => out.push_str(&format!("<{url}>")),
                _ => {}
            },
            "user" => out.push_str(&format!("<@{}>", e["user_id"].as_str().unwrap_or_default())),
            "channel" => out.push_str(&format!("<#{}>", e["channel_id"].as_str().unwrap_or_default())),
            "emoji" => out.push_str(&format!(":{}:", e["name"].as_str().unwrap_or_default())),
            "rich_text_section" => out.push_str(&rich_text(&e["elements"])),
            "rich_text_preformatted" => out.push_str(&format!("```{}```", rich_text(&e["elements"]))),
            "rich_text_quote" => out.push_str(&format!("> {}", rich_text(&e["elements"]))),
            "rich_text_list" => {
                for item in e["elements"].as_array().into_iter().flatten() {
                    out.push_str(&format!("• {}\n", rich_text(&item["elements"])));
                }
            }
            _ => {}
        }
        if matches!(e["type"].as_str(), Some("rich_text_preformatted" | "rich_text_quote")) {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct N;
    impl Names for N {
        fn user(&self, id: &str) -> (String, String) {
            ("Ada".into(), format!("T1:{id}"))
        }
        fn channel(&self, id: &str) -> (String, Option<String>) {
            ("general".into(), Some(format!("T1:{id}")))
        }
        fn custom_emoji(&self, name: &str) -> Option<(String, String)> {
            (name == "celebrate").then(|| ("party".into(), "https://emoji.slack-edge.com/T/party.png".into()))
        }
    }

    fn conv(s: &str) -> Converted {
        convert(s, &N)
    }

    #[test]
    fn emphasis_code_and_entities() {
        let c = conv("*bold* _it_ ~gone~ `a &lt; b` &amp; 2*3");
        assert_eq!(c.rich, "**bold** *it* ~~gone~~ `a < b` \\& 2\\*3");
        assert_eq!(c.plain, "bold it gone a < b & 2*3");
    }

    #[test]
    fn adjacent_emoji_are_each_converted() {
        assert_eq!(conv(":smile::smile:").plain, "😄😄");
        assert_eq!(conv(":smile::smile::smile:").plain, "😄😄😄");
        assert_eq!(conv(":smile: :smile:").plain, "😄 😄");
        assert_eq!(conv(":wave::skin-tone-3::smile:").plain, "👋🏼😄");
        let c = conv(":celebrate::celebrate:");
        assert_eq!(c.plain, ":party::party:");
        assert_eq!(c.rich.matches("godwit://emoji/").count(), 2);
        assert_eq!(conv("a::b").plain, "a::b");
    }

    #[test]
    fn references() {
        let c = conv("hi <@U2> in <#C3> see <https://a.b/c(d)|this> <!here>");
        assert_eq!(
            c.rich,
            "hi [\\@Ada](godwit://user/T1:U2) in [\\#general](godwit://room/T1:C3) see [this](https://a.b/c%28d%29) [\\@here](godwit://mention/here)"
        );
        assert_eq!(conv("<javascript:alert(1)|x>").rich, "x");
    }

    #[test]
    fn emoji_become_characters() {
        assert_eq!(
            conv(":+1: :wave::skin-tone-3: :partyparrot:").plain,
            "👍 👋🏼 :partyparrot:"
        );
        assert_eq!(reaction_name("👍").as_deref(), Some("+1"));
        assert_eq!(shortcode("+1").as_deref(), Some("👍"));
        assert_eq!(shortcode("heart").as_deref(), Some("❤️"));
        assert_eq!(shortcode("wave::skin-tone-2").as_deref(), Some("👋🏻"));
    }

    #[test]
    fn custom_workspace_emoji_aliases_resolve_to_the_image() {
        let emoji = serde_json::json!({"party":"https://emoji.slack-edge.com/T/party.png", "celebrate":"alias:party"});
        let map = emoji.as_object().expect("test object");
        assert_eq!(
            custom_emoji("celebrate", map),
            Some(("party", "https://emoji.slack-edge.com/T/party.png"))
        );
        assert_eq!(custom_emoji("missing", map), None);
        let rendered = conv(":celebrate:");
        assert_eq!(rendered.plain, ":party:");
        assert!(rendered.rich.contains("godwit://emoji/"));
        assert!(rendered.rich.contains("party.png"));
    }

    #[test]
    fn blocks_quotes_and_breaks() {
        let c = conv("one\ntwo\n&gt; quoted\n```let x = 1;```\nafter");
        assert!(c.rich.starts_with("one\\\ntwo\n\n> quoted"), "{}", c.rich);
        assert!(c.rich.contains("```\nlet x = 1;\n```"), "{}", c.rich);
        assert!(c.rich.ends_with("after"), "{}", c.rich);
    }

    #[test]
    fn odd_input_never_panics() {
        for s in [
            "<",
            "```",
            "*",
            ":",
            "<@",
            "_a",
            "~~~",
            "```a",
            "&gt;",
            ":a::skin-tone-x:",
        ] {
            let _ = conv(s);
        }
    }

    #[test]
    fn sent_text_is_escaped() {
        assert_eq!(from_text("a < b & *c*"), "a &lt; b &amp; *c*");
    }

    #[test]
    fn sent_id_mentions_keep_slacks_active_reference_syntax() {
        assert_eq!(
            from_text("Hi <@U123>, <!here> and <#C456|general> <tag>"),
            "Hi <@U123>, <!here> and <#C456|general> &lt;tag&gt;"
        );
        assert_eq!(from_text("<javascript:alert(1)>"), "&lt;javascript:alert(1)&gt;");
    }

    #[test]
    fn apps_show_their_blocks_and_attachments_and_people_their_text() {
        let person = serde_json::json!({"text": "hi", "blocks": [{"type": "rich_text", "elements": [
            {"type": "rich_text_section", "elements": [{"type": "text", "text": "hi"}]}]}]});
        assert_eq!(message_source(&person), "hi");
        let app = serde_json::json!({"text": "fallback", "blocks": [
            {"type": "header", "text": {"type": "plain_text", "text": "Build"}},
            {"type": "section", "text": {"type": "mrkdwn", "text": "passed"}},
            {"type": "divider"}]});
        assert_eq!(message_source(&app), "*Build*\npassed");
        let legacy = serde_json::json!({"text": "", "attachments": [
            {"title": "Alert", "text": "disk full", "fields": [{"title": "Host", "value": "db1"}]},
            {"from_url": "https://example.com", "title": "a preview"}]});
        assert_eq!(message_source(&legacy), "*Alert*\ndisk full\n*Host*\ndb1");
        let only_blocks = serde_json::json!({"text": "", "blocks": [{"type": "rich_text", "elements": [
            {"type": "rich_text_section", "elements": [{"type": "text", "text": "yo "}, {"type": "emoji", "name": "wave"}]}]}]});
        assert_eq!(message_source(&only_blocks), "yo :wave:");
    }
}

// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Steam chat's BBCode as Godwit rich text: emoticons as inline pictures,
//! stickers shown large, links, spoilers, bold/italic/strike, code and
//! quotes; game invites become cards. Tags Godwit doesn't know (random rolls, …) keep
//! their text and lose the tag, so nothing shows as raw markup.

use godwit_core::rich;

/// Steam's emoticon pictures: `/economy/emoticon/<name>`.
const EMOTICONS: &str = "https://community.fastly.steamstatic.com/economy/emoticon/";
/// Steam's sticker pictures: `/economy/sticker/<type>` (type percent-encoded).
const STICKERS: &str = "https://community.fastly.steamstatic.com/economy/sticker/";

/// An emoticon's picture by name (no colons).
pub fn emoticon_url(name: &str) -> String {
    format!("{EMOTICONS}{}", rich::percent_encode(name))
}

/// A sticker's picture by name.
pub fn sticker_url(name: &str) -> String {
    format!("{STICKERS}{}", rich::percent_encode(name))
}

/// A message converted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Converted {
    /// Godwit rich text (`Markup::Rich`).
    pub rich: String,
    /// The text as plainly as it reads (replies, notifications, copying).
    pub plain: String,
    /// Stickers: (picture, name), shown large in place of the text.
    pub jumbo: Vec<(String, String)>,
    /// Pictures posted into the chat (`[img src=…]`): (url, pixel size).
    pub images: Vec<(String, Option<(u32, u32)>)>,
    /// Group chat invites (`[invite chatgroupid=… chatid=…]`) as Godwit
    /// invite codes, `g<group>-<chat>` (the room ids of group rooms).
    pub invites: Vec<String>,
    /// Game invites (`[gameinvite appid=… connect=…]`,
    /// `[lobbyinvite appid=… lobbyid=…]`), shown as cards with Join.
    pub games: Vec<GameInvite>,
    /// Tags Godwit doesn't know: name and attribute names, never values
    /// (for the log, to learn new markup).
    pub unknown: Vec<String>,
}

/// An invitation to play a Steam game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameInvite {
    pub app: u32,
    pub join: GameJoin,
}

/// How a game invite is joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameJoin {
    /// Start the game with these arguments (a server, a party: the game's
    /// own `connect` string, possibly empty).
    Connect(String),
    /// Join this Steam lobby.
    Lobby(u64),
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Text(String),
    Tag {
        name: String,
        /// The `=value` after the name (`[url=…]`), if any.
        value: Option<String>,
        attrs: Vec<(String, String)>,
        children: Vec<Node>,
    },
}

/// Converts one Steam chat message.
pub fn convert(source: &str) -> Converted {
    let nodes = parse(source);
    let mut out = Converted::default();
    for n in &nodes {
        out.rich.push_str(&render(n, &mut out.jumbo, &mut out.images));
        out.plain.push_str(&plain(n));
        collect_tags(n, &mut out.invites, &mut out.games, &mut out.unknown);
    }
    out.plain = out.plain.trim().to_owned();
    if !out.invites.is_empty() && out.plain.is_empty() {
        out.plain = "Invited you to a group chat".to_owned();
        out.rich = format!("*{}*", out.plain);
    }
    if !out.games.is_empty() && out.plain.is_empty() {
        out.plain = "Invited you to play".to_owned();
        out.rich = format!("*{}*", out.plain);
    }
    // Markup Godwit doesn't know that leaves nothing to show: say so rather
    // than show a blank message.
    if out.plain.is_empty() && out.jumbo.is_empty() && out.images.is_empty() && !source.trim().is_empty() {
        out.plain = "Sent something Godwit can't show yet".to_owned();
        out.rich = format!("*{}*", out.plain);
    }
    if !out.images.is_empty() && out.plain.is_empty() && out.jumbo.is_empty() {
        out.plain = "[image]".to_owned();
    }
    if !out.jumbo.is_empty() && out.plain.is_empty() {
        out.plain = out
            .jumbo
            .iter()
            .map(|(_, n)| format!("[{n}]"))
            .collect::<Vec<_>>()
            .join(" ");
    }
    out
}

type Images = Vec<(String, Option<(u32, u32)>)>;

fn render(node: &Node, jumbo: &mut Vec<(String, String)>, images: &mut Images) -> String {
    let Node::Tag {
        name,
        value,
        attrs,
        children,
    } = node
    else {
        let Node::Text(t) = node else { return String::new() };
        return rich::escape(t);
    };
    let inner = || {
        children
            .iter()
            .map(|c| render(c, &mut Vec::new(), &mut Vec::new()))
            .collect::<String>()
    };
    let text = || children.iter().map(plain).collect::<String>();
    match name.as_str() {
        "emoticon" => {
            let name = text();
            let name = name.trim();
            if name.is_empty() {
                return String::new();
            }
            rich::emoji(name, &emoticon_url(name))
        }
        "sticker" => {
            let kind = attr(attrs, "type").or(value.clone()).unwrap_or_default();
            if !kind.is_empty() {
                jumbo.push((sticker_url(&kind), kind));
            }
            String::new()
        }
        "url" => {
            let target = value.clone().unwrap_or_else(text);
            let label = if children.is_empty() {
                rich::escape(&target)
            } else {
                inner()
            };
            if target.starts_with("https://") || target.starts_with("http://") {
                format!("[{label}]({})", target.replace(' ', "%20").replace(')', "%29"))
            } else {
                label
            }
        }
        // Steam's chat posts uploads as `[img src=… width=… height=…][/img]`
        // (the body empty); older markup puts the address in the body.
        "img" => {
            let target = attr(attrs, "src").unwrap_or_else(text);
            let target = target.trim();
            if target.starts_with("https://") || target.starts_with("http://") {
                let size = attr(attrs, "width")
                    .and_then(|w| w.parse().ok())
                    .zip(attr(attrs, "height").and_then(|h| h.parse().ok()));
                images.push((target.replace(' ', "%20"), size));
                String::new()
            } else {
                rich::escape(target)
            }
        }
        // Steam's room effects (confetti, fireworks, …) play over the chat in
        // its app; Godwit says one was played.
        "roomeffect" => wrap("*", &rich::escape(&room_effect(attrs, value))),
        "spoiler" => rich::spoiler(&text()),
        "b" => wrap("**", &inner()),
        "i" => wrap("*", &inner()),
        "s" | "strike" => wrap("~~", &inner()),
        "code" | "pre" => rich::code_span(&text()),
        "quote" => format!("\n{}\n", rich::quote(&inner())),
        // Unknown tags: their text, without the markup.
        _ => children.iter().map(|c| render(c, jumbo, images)).collect(),
    }
}

/// `inner` between `mark`s, unless it's empty (an empty `****` reads as text).
fn wrap(mark: &str, inner: &str) -> String {
    if inner.trim().is_empty() {
        inner.to_owned()
    } else {
        format!("{mark}{inner}{mark}")
    }
}

fn plain(node: &Node) -> String {
    match node {
        Node::Text(t) => t.clone(),
        Node::Tag {
            name,
            children,
            value,
            attrs,
        } => {
            let text: String = children.iter().map(plain).collect();
            match name.as_str() {
                "emoticon" => format!(":{}:", text.trim()),
                "sticker" | "img" => String::new(),
                "roomeffect" => room_effect(attrs, value),
                "url" if text.is_empty() => value.clone().unwrap_or_default(),
                _ => text,
            }
        }
    }
}

/// Tags Godwit knows how to show.
const KNOWN: &[&str] = &[
    "emoticon",
    "sticker",
    "url",
    "img",
    "roomeffect",
    "spoiler",
    "b",
    "i",
    "s",
    "strike",
    "code",
    "pre",
    "quote",
    "invite",
    "gameinvite",
    "lobbyinvite",
];

/// Invites, and the tags Godwit doesn't know (for the log).
fn collect_tags(node: &Node, invites: &mut Vec<String>, games: &mut Vec<GameInvite>, unknown: &mut Vec<String>) {
    let Node::Tag {
        name, attrs, children, ..
    } = node
    else {
        return;
    };
    if name == "invite" {
        let group = attr(attrs, "chatgroupid").and_then(|g| g.parse::<u64>().ok());
        let chat = attr(attrs, "chatid").and_then(|c| c.parse::<u64>().ok()).unwrap_or(0);
        if let Some(group) = group {
            invites.push(format!("g{group}-{chat}"));
        }
    }
    if name == "gameinvite" || name == "lobbyinvite" {
        // Only the digits: pasted invites sometimes carry typographic quotes.
        let number =
            |key: &str| attr(attrs, key).and_then(|v| v.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok());
        let join = if name == "lobbyinvite" {
            number("lobbyid").map(GameJoin::Lobby)
        } else {
            Some(GameJoin::Connect(attr(attrs, "connect").unwrap_or_default()))
        };
        if let (Some(app), Some(join)) = (number("appid").and_then(|a: u64| u32::try_from(a).ok()), join) {
            games.push(GameInvite { app, join });
        }
    }
    if !KNOWN.contains(&name.as_str()) {
        let keys: Vec<&str> = attrs.iter().map(|(k, _)| k.as_str()).collect();
        unknown.push(format!("[{name}] {keys:?}"));
    }
    for c in children {
        collect_tags(c, invites, games, unknown);
    }
}

/// "Played the Confetti room effect", or without a name if Steam gave none.
fn room_effect(attrs: &[(String, String)], value: &Option<String>) -> String {
    match attr(attrs, "type")
        .or_else(|| value.clone())
        .filter(|t| !t.trim().is_empty())
    {
        Some(kind) => format!("Played the {} room effect", kind.trim()),
        None => "Played a room effect".to_owned(),
    }
}

fn attr(attrs: &[(String, String)], key: &str) -> Option<String> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

/// What a `[…]` holds: an opening tag, a closing one, or neither (text).
enum Bracket {
    Open {
        name: String,
        value: Option<String>,
        attrs: Vec<(String, String)>,
    },
    Close(String),
}

fn bracket(inside: &str) -> Option<Bracket> {
    if let Some(name) = inside.strip_prefix('/') {
        let name = name.trim().to_ascii_lowercase();
        return valid_name(&name).then_some(Bracket::Close(name));
    }
    let name_end = inside.find([' ', '=']).unwrap_or(inside.len());
    let name = inside[..name_end].to_ascii_lowercase();
    if !valid_name(&name) {
        return None;
    }
    let rest = &inside[name_end..];
    let (value, rest) = match rest.strip_prefix('=') {
        Some(v) => {
            let (v, rest) = quoted_or_word(v);
            (Some(v), rest)
        }
        None => (None, rest),
    };
    let mut attrs = Vec::new();
    let mut rest = rest.trim_start();
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].trim().to_ascii_lowercase();
        let (v, after) = quoted_or_word(&rest[eq + 1..]);
        if !key.is_empty() {
            attrs.push((key, v));
        }
        rest = after.trim_start();
    }
    Some(Bracket::Open { name, value, attrs })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 24 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A `"quoted value"` or a bare word, and what follows it.
fn quoted_or_word(s: &str) -> (String, &str) {
    if let Some(q) = s.strip_prefix('"') {
        match q.find('"') {
            Some(end) => (q[..end].to_owned(), &q[end + 1..]),
            None => (q.to_owned(), ""),
        }
    } else {
        let end = s.find(' ').unwrap_or(s.len());
        (s[..end].to_owned(), &s[end..])
    }
}

/// Steam's BBCode as a tree. `\[` is a literal bracket; a closing tag
/// without its opening, or an opening never closed, stays text.
fn parse(source: &str) -> Vec<Node> {
    // (name, value, attrs, children, the raw opening tag)
    type Open = (String, Option<String>, Vec<(String, String)>, Vec<Node>, String);
    let mut stack: Vec<Open> = vec![(String::new(), None, Vec::new(), Vec::new(), String::new())];
    let mut text = String::new();
    let flush = |text: &mut String, stack: &mut Vec<Open>| {
        if !text.is_empty()
            && let Some(top) = stack.last_mut()
        {
            match top.3.last_mut() {
                Some(Node::Text(t)) => t.push_str(text),
                _ => top.3.push(Node::Text(std::mem::take(text))),
            }
            text.clear();
        }
    };
    let mut rest = source;
    while let Some(at) = rest.find(['[', '\\']) {
        text.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("\\[") {
            text.push('[');
            rest = after;
            continue;
        }
        if let Some(after) = tail.strip_prefix('\\') {
            text.push('\\');
            rest = after;
            continue;
        }
        let Some(end) = tail.find(']') else {
            text.push_str(tail);
            rest = "";
            break;
        };
        let raw = &tail[..=end];
        match bracket(&tail[1..end]) {
            Some(Bracket::Open { name, value, attrs }) => {
                flush(&mut text, &mut stack);
                stack.push((name, value, attrs, Vec::new(), raw.to_owned()));
            }
            Some(Bracket::Close(name)) if stack.iter().skip(1).any(|o| o.0 == name) => {
                flush(&mut text, &mut stack);
                // Close everything opened since (left open by mistake) too.
                while let Some((n, value, attrs, children, _)) = stack.pop() {
                    let node = Node::Tag {
                        name: n.clone(),
                        value,
                        attrs,
                        children,
                    };
                    if let Some(parent) = stack.last_mut() {
                        parent.3.push(node);
                    }
                    if n == name {
                        break;
                    }
                }
            }
            _ => text.push_str(raw),
        }
        rest = &tail[end + 1..];
    }
    text.push_str(rest);
    flush(&mut text, &mut stack);
    // Never closed: their opening tag as text, their contents kept.
    while stack.len() > 1 {
        let Some((_, _, _, children, raw)) = stack.pop() else {
            break;
        };
        if let Some(parent) = stack.last_mut() {
            parent.3.push(Node::Text(raw));
            parent.3.extend(children);
        }
    }
    stack.pop().map(|s| s.3).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoticons_become_inline_pictures() {
        let c = convert("hi [emoticon]tobdog[/emoticon]!");
        assert!(c.rich.contains(godwit_core::rich::EMOJI), "{}", c.rich);
        assert!(c.rich.contains("economy%2Femoticon%2Ftobdog") || c.rich.contains("emoticon"));
        assert_eq!(c.plain, "hi :tobdog:!");
    }

    #[test]
    fn stickers_show_large_and_read_as_their_name() {
        let c = convert(r#"[sticker type="Goose Chase" limit="0"][/sticker]"#);
        assert_eq!(c.jumbo.len(), 1);
        assert!(
            c.jumbo[0].0.ends_with("/economy/sticker/Goose%20Chase"),
            "{}",
            c.jumbo[0].0
        );
        assert_eq!(c.jumbo[0].1, "Goose Chase");
        assert_eq!(c.rich, "");
        assert_eq!(c.plain, "[Goose Chase]");
    }

    #[test]
    fn group_chat_invites_become_invite_codes() {
        let c = convert(r#"[invite chatgroupid="123" chatid="456" inviter="7"][/invite]"#);
        assert_eq!(c.invites, vec!["g123-456".to_owned()]);
        assert_eq!(c.plain, "Invited you to a group chat");
        assert!(c.unknown.is_empty());
        let odd = convert(r#"[newthing a="1" b="2"][/newthing]"#);
        assert_eq!(odd.unknown, vec![r#"[newthing] ["a", "b"]"#.to_owned()]);
    }

    #[test]
    fn room_effects_and_unknown_empty_markup_never_show_blank() {
        let c = convert(r#"[roomeffect type="confetti"][/roomeffect]"#);
        assert_eq!(c.plain, "Played the confetti room effect");
        assert_eq!(c.rich, "*Played the confetti room effect*");
        assert_eq!(convert("[roomeffect][/roomeffect]").plain, "Played a room effect");
        let unknown = convert("[somethingnew a=\"1\"][/somethingnew]");
        assert_eq!(unknown.plain, "Sent something Godwit can't show yet");
        assert_eq!(convert("").plain, "");
    }

    #[test]
    fn chat_images_become_attachments() {
        let c = convert(
            "[img src=https://images.steamusercontent.com/ugc/1/AB/ thumbnail_src=https://images.steamusercontent.com/ugc/1/AB/?imw=5&imh=5 width=640 height=480][/img]",
        );
        assert_eq!(
            c.images,
            vec![(
                "https://images.steamusercontent.com/ugc/1/AB/".to_owned(),
                Some((640, 480))
            )]
        );
        assert_eq!(c.rich, "");
        assert_eq!(c.plain, "[image]");
        let old = convert("look [img]https://example.com/a.png[/img]");
        assert_eq!(old.images, vec![("https://example.com/a.png".to_owned(), None)]);
        assert_eq!(old.plain, "look");
        assert!(convert("[img src=javascript:x][/img]").images.is_empty());
    }

    #[test]
    fn links_spoilers_and_styles_convert() {
        let c = convert("[url=\"https://example.com/a b\"]site[/url] [spoiler]secret[/spoiler] [b]bold[/b]");
        assert!(c.rich.contains("[site](https://example.com/a%20b)"), "{}", c.rich);
        assert!(c.rich.contains(godwit_core::rich::SPOILER));
        assert!(c.rich.contains("**bold**"));
        assert_eq!(c.plain, "site secret bold");
        assert!(
            convert("[url=javascript:alert(1)]x[/url]").rich == "x",
            "only web links"
        );
    }

    #[test]
    fn game_and_lobby_invites_become_cards() {
        let c = convert(r#"[gameinvite appid="253250" connect=" --multiplayer.steam.id=5"][/gameinvite]"#);
        assert_eq!(
            c.games,
            vec![GameInvite {
                app: 253250,
                join: GameJoin::Connect(" --multiplayer.steam.id=5".into()),
            }]
        );
        assert_eq!(c.plain, "Invited you to play");
        assert!(c.unknown.is_empty());
        let c = convert(r#"[lobbyinvite appid="594650" lobbyid="109775241"][/lobbyinvite]"#);
        assert_eq!(c.games[0].join, GameJoin::Lobby(109_775_241));
        // A forum-pasted invite's typographic quotes still read.
        assert_eq!(convert("[gameinvite appid=“730”][/gameinvite]").games[0].app, 730);
        // No app: nothing to show a card for.
        assert!(convert(r#"[gameinvite connect="x"][/gameinvite]"#).games.is_empty());
    }

    #[test]
    fn unknown_or_broken_markup_never_shows_raw_tags_or_loses_text() {
        let c = convert("[gameinvite appid=\"1\"]come play[/gameinvite]");
        assert_eq!(c.plain, "come play");
        assert!(!c.rich.contains("gameinvite"));
        // Literal brackets and stray tags stay text.
        assert_eq!(convert(r"a \[b] c").plain, "a [b] c");
        assert_eq!(convert("[b]never closed").plain, "[b]never closed");
        assert_eq!(convert("a [/i] b").plain, "a [/i] b");
        assert_eq!(convert("1 [ 2 ] 3").plain, "1 [ 2 ] 3");
    }
}

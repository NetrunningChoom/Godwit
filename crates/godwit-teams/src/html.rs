// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Teams' message HTML (`RichText/Html`) to Godwit rich text and back.
//!
//! Teams sends a small, regular subset: paragraphs, breaks, emphasis,
//! links, code, lists, quotes, mention spans (numbered, resolved through
//! the message's `properties.mentions`), emoji images (their `alt` is the
//! emoji) and reply quotes (`<blockquote itemtype=…/Reply itemid=…>`).
//! Anything else keeps its text. Never panics; unknown markup degrades to
//! text, never to markup.

use std::collections::HashMap;

use godwit_core::rich;

const MENTION: &str = "schema.skype.com/Mention";
const EMOJI: &str = "schema.skype.com/Emoji";
const REPLY: &str = "schema.skype.com/Reply";

/// A converted message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Converted {
    pub rich: String,
    pub plain: String,
    /// The message replied to, from a reply quote.
    pub reply_to: Option<String>,
}

/// Who a mention points at: `itemid` → (MRI, name).
pub type Mentions = HashMap<String, (String, String)>;

/// `properties.mentions`: an array, or a JSON string holding one, of
/// `{"itemid", "mri", "displayName"}`.
pub fn mentions(properties: &serde_json::Value) -> Mentions {
    let list = match &properties["mentions"] {
        serde_json::Value::String(s) => serde_json::from_str(s).unwrap_or_default(),
        v => v.clone(),
    };
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = match &m["itemid"] {
                serde_json::Value::String(s) if !s.is_empty() => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => return None,
            };
            let mri = m["mri"].as_str()?.to_owned();
            let name = m["displayName"].as_str().unwrap_or_default().to_owned();
            Some((id, (mri, name)))
        })
        .collect()
}

/// The MRIs named by `properties.mentions`, regardless of whether the web
/// client encoded that property as a JSON string or an already parsed array.
pub fn mention_mris(properties: &serde_json::Value) -> Vec<String> {
    let list = match &properties["mentions"] {
        serde_json::Value::String(s) => serde_json::from_str(s).unwrap_or_default(),
        v => v.clone(),
    };
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|mention| mention["mri"].as_str().map(str::to_owned))
        .collect()
}

/// A tag as read: its name (lower case), whether it closes, its attributes.
struct Tag {
    name: String,
    closing: bool,
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

enum Token {
    Text(String),
    Tag(Tag),
}

/// Decodes `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`/`&#39;`, `&nbsp;` and
/// numeric references; anything else stays as written.
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // References are short: look a few characters ahead only (by
        // character, so a multi-byte one can't be split).
        let Some(end) = rest.char_indices().take(12).find(|&(_, c)| c == ';').map(|(i, _)| i) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_tag(inner: &str) -> Option<Tag> {
    let inner = inner.trim().trim_end_matches('/').trim();
    let (closing, inner) = match inner.strip_prefix('/') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, inner),
    };
    let name_end = inner.find(|c: char| c.is_whitespace()).unwrap_or(inner.len());
    let name = inner[..name_end].to_ascii_lowercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let mut attrs = Vec::new();
    let mut rest = inner[name_end..].trim_start();
    while !rest.is_empty() {
        let key_end = rest.find(|c: char| c == '=' || c.is_whitespace()).unwrap_or(rest.len());
        let key = rest[..key_end].to_ascii_lowercase();
        rest = rest[key_end..].trim_start();
        let value = if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let (value, next) = match after.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let body = &after[1..];
                    let end = body.find(q).unwrap_or(body.len());
                    (&body[..end], body.get(end + 1..).unwrap_or(""))
                }
                _ => {
                    let end = after.find(char::is_whitespace).unwrap_or(after.len());
                    (&after[..end], &after[end..])
                }
            };
            rest = next.trim_start();
            decode_entities(value)
        } else {
            String::new()
        };
        if key.is_empty() {
            break;
        }
        attrs.push((key, value));
    }
    Some(Tag { name, closing, attrs })
}

fn tokens(html: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        match rest.find('<') {
            Some(0) => {
                // Comments are skipped whole.
                if let Some(after) = rest.strip_prefix("<!--") {
                    rest = after.find("-->").map_or("", |e| &after[e + 3..]);
                    continue;
                }
                match rest.find('>') {
                    Some(end) => {
                        match parse_tag(&rest[1..end]) {
                            Some(tag) => out.push(Token::Tag(tag)),
                            None => out.push(Token::Text(rest[..=end].to_owned())),
                        }
                        rest = &rest[end + 1..];
                    }
                    None => {
                        out.push(Token::Text(rest.to_owned()));
                        rest = "";
                    }
                }
            }
            Some(i) => {
                out.push(Token::Text(decode_entities(&rest[..i])));
                rest = &rest[i..];
            }
            None => {
                out.push(Token::Text(decode_entities(rest)));
                rest = "";
            }
        }
    }
    out
}

/// Inline images and Skype URIObject attachments. Emoji images stay text.
pub fn attachments(html: &str) -> Vec<godwit_core::message::Attachment> {
    use godwit_core::message::{Attachment, AttachmentKind};
    let mut out: Vec<Attachment> = Vec::new();
    let mut object = None;
    for token in tokens(html) {
        let Token::Tag(tag) = token else { continue };
        if tag.name == "uriobject" {
            if tag.closing {
                object = None;
                continue;
            }
            let Some(uri) = tag.attr("uri").filter(|u| godwit_core::media::https(u).is_some()) else {
                continue;
            };
            let image = tag
                .attr("type")
                .is_some_and(|t| t.starts_with("Picture") || t.starts_with("Image"));
            let original = format!(
                "{}/views/{}",
                uri.trim_end_matches('/'),
                if image { "imgpsh_fullsize" } else { "original" }
            );
            out.push(Attachment {
                name: if image { "image" } else { "file" }.into(),
                url: if image {
                    tag.attr("url_thumbnail").unwrap_or(&original)
                } else {
                    &original
                }
                .into(),
                original_url: Some(original.into()),
                spoiler: false,
                kind: if image {
                    AttachmentKind::Image
                } else {
                    AttachmentKind::File
                },
                size: 0,
                dimensions: None,
            });
            object = Some(out.len() - 1);
        } else if let Some(index) = object {
            if let Some(file) = out.get_mut(index) {
                match tag.name.as_str() {
                    "originalname" => {
                        if let Some(name) = tag.attr("v") {
                            file.name = name.into();
                            if godwit_core::media::mime(name).starts_with("image/") {
                                file.kind = AttachmentKind::Image;
                            }
                        }
                    }
                    "filesize" => file.size = tag.attr("v").and_then(|v| v.parse().ok()).unwrap_or(0),
                    _ => {}
                }
            }
        } else if tag.name == "img" && !tag.attr("itemtype").is_some_and(|t| t.contains(EMOJI)) {
            let Some(url) = tag.attr("src").filter(|u| godwit_core::media::https(u).is_some()) else {
                continue;
            };
            let dim = |key: &str| tag.attr(key).and_then(|s| s.parse::<u32>().ok()).filter(|n| *n > 0);
            out.push(Attachment {
                name: tag.attr("alt").filter(|v| !v.is_empty()).unwrap_or("image").into(),
                url: url.into(),
                original_url: None,
                spoiler: false,
                kind: AttachmentKind::Image,
                size: 0,
                dimensions: dim("width").zip(dim("height")),
            });
        }
    }
    out
}

/// Writes rich and plain text side by side.
#[derive(Default)]
struct Writer {
    rich: String,
    plain: String,
    /// A block ended: the next text starts a new one.
    pending_block: bool,
    /// Whitespace was seen and not yet written.
    pending_space: bool,
}

impl Writer {
    fn block(&mut self) {
        if !self.rich.is_empty() {
            self.pending_block = true;
        }
        self.pending_space = false;
    }

    fn start(&mut self) {
        if self.pending_block {
            self.rich.push_str("\n\n");
            self.plain.push('\n');
            self.pending_block = false;
            self.pending_space = false;
        } else if self.pending_space {
            if !self.rich.is_empty() && !self.rich.ends_with('\n') {
                self.rich.push(' ');
                self.plain.push(' ');
            }
            self.pending_space = false;
        }
    }

    /// Text, its whitespace collapsed as HTML does.
    fn text(&mut self, s: &str) {
        let mut first = true;
        for word in s.split(char::is_whitespace) {
            if !first {
                self.pending_space = true;
            }
            first = false;
            if word.is_empty() {
                continue;
            }
            self.start();
            self.rich.push_str(&rich::escape(word));
            self.plain.push_str(word);
        }
    }

    /// Markup that belongs to the next text (`**`, `[`).
    fn open(&mut self, mark: &str) {
        self.start();
        self.rich.push_str(mark);
    }

    fn close(&mut self, mark: &str) {
        self.rich.push_str(mark);
    }

    fn raw(&mut self, rich: &str, plain: &str) {
        self.start();
        self.rich.push_str(rich);
        self.plain.push_str(plain);
    }

    fn line_break(&mut self) {
        if !self.rich.is_empty() && !self.pending_block {
            self.rich.push_str("\\\n");
            self.plain.push('\n');
        }
        self.pending_space = false;
    }
}

/// Collects the text inside an element (code, a mention's name) without markup.
fn inner_text(tokens: &[Token], from: usize, name: &str) -> (String, usize) {
    let mut text = String::new();
    let mut depth = 0usize;
    let mut i = from;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Text(t) => text.push_str(t),
            Token::Tag(t) if t.name == "br" => text.push('\n'),
            Token::Tag(t) if t.name == name && !t.closing => depth += 1,
            Token::Tag(t) if t.name == name && t.closing => {
                if depth == 0 {
                    return (text, i);
                }
                depth -= 1;
            }
            Token::Tag(_) => {}
        }
        i += 1;
    }
    (text, i)
}

/// Only web and mail links become links.
fn safe_link(href: &str) -> Option<String> {
    let lower = href.trim().to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")) {
        return None;
    }
    Some(
        href.trim()
            .chars()
            .map(|c| match c {
                '(' => "%28".to_owned(),
                ')' => "%29".to_owned(),
                ' ' => "%20".to_owned(),
                '<' => "%3C".to_owned(),
                '>' => "%3E".to_owned(),
                c => c.to_string(),
            })
            .collect(),
    )
}

/// Teams HTML to rich and plain text; `mentions` resolves mention spans.
pub fn convert(html: &str, mentions: &Mentions) -> Converted {
    let tokens = tokens(html);
    let mut w = Writer::default();
    let mut reply_to = None;
    // Open marks to close, by tag name; links remember their address.
    let mut links: Vec<Option<String>> = Vec::new();
    let mut list: Vec<(bool, u32)> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Text(t) => w.text(t),
            Token::Tag(tag) => match (tag.name.as_str(), tag.closing) {
                ("p" | "div" | "table" | "tr", _) => w.block(),
                ("br", _) => w.line_break(),
                ("b" | "strong", false) => w.open("**"),
                ("b" | "strong", true) => w.close("**"),
                ("i" | "em", false) => w.open("*"),
                ("i" | "em", true) => w.close("*"),
                ("s" | "strike" | "del", false) => w.open("~~"),
                ("s" | "strike" | "del", true) => w.close("~~"),
                ("a", false) => {
                    let href = tag.attr("href").and_then(safe_link);
                    if href.is_some() {
                        w.open("[");
                    }
                    links.push(href);
                }
                ("a", true) => {
                    if let Some(Some(href)) = links.pop() {
                        w.close(&format!("]({href})"));
                    }
                }
                ("code", false) => {
                    let (text, end) = inner_text(&tokens, i + 1, "code");
                    w.raw(&rich::code_span(&text), &text);
                    i = end;
                }
                ("pre", false) => {
                    let (text, end) = inner_text(&tokens, i + 1, "pre");
                    w.block();
                    w.raw(&rich::code_block("", text.trim_end_matches('\n')), &text);
                    w.block();
                    i = end;
                }
                ("span", false) if tag.attr("itemtype").is_some_and(|t| t.contains(MENTION)) => {
                    let (text, end) = inner_text(&tokens, i + 1, "span");
                    let shown = text.trim();
                    match tag.attr("itemid").and_then(|id| mentions.get(id)) {
                        // Ids are `8:orgid:{guid}`: only URL-safe characters.
                        Some((mri, name)) if mri.bytes().all(|b| b.is_ascii_alphanumeric() || b"-:._".contains(&b)) => {
                            let name = if shown.is_empty() { name.as_str() } else { shown };
                            w.raw(
                                &format!("[{}](godwit://user/{mri})", rich::escape(&format!("@{name}"))),
                                &format!("@{name}"),
                            );
                        }
                        Some(_) => w.text(shown),
                        None => w.text(shown),
                    }
                    i = end;
                }
                ("img", _) => {
                    let alt = tag.attr("alt").unwrap_or_default();
                    if tag.attr("itemtype").is_some_and(|t| t.contains(EMOJI)) {
                        // `alt` is the emoji, or a text shortcut like `(smile)`:
                        // then the emoticon id (`itemid`) says which.
                        let emoji = Some(alt)
                            .filter(|a| !a.is_ascii())
                            .map(str::to_owned)
                            .or_else(|| tag.attr("itemid").and_then(crate::emoji::for_key))
                            .unwrap_or_else(|| alt.to_owned());
                        w.text(&emoji);
                    } else {
                        // Inline pictures live behind Teams' signed-in media service.
                        w.raw(&rich::escape("[picture]"), "[picture]");
                    }
                }
                ("blockquote", false) if tag.attr("itemtype").is_some_and(|t| t.contains(REPLY)) => {
                    reply_to = tag.attr("itemid").filter(|s| !s.is_empty()).map(str::to_owned);
                    // The quote repeats the earlier message: the timeline shows it.
                    let (_, end) = inner_text(&tokens, i + 1, "blockquote");
                    i = end;
                }
                ("blockquote", false) => {
                    let (text, end) = inner_text(&tokens, i + 1, "blockquote");
                    let inner = convert(&escape_text(&text), mentions);
                    w.block();
                    w.raw(&rich::quote(&inner.rich), &inner.plain);
                    w.block();
                    i = end;
                }
                ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                    let level = tag.name[1..].parse().unwrap_or(1);
                    let (text, end) = inner_text(&tokens, i + 1, &tag.name);
                    w.block();
                    w.raw(&rich::heading(level, &rich::escape(text.trim())), text.trim());
                    w.block();
                    i = end;
                }
                ("ul", false) => list.push((false, 0)),
                ("ol", false) => list.push((true, 0)),
                ("ul" | "ol", true) => {
                    list.pop();
                    w.block();
                }
                ("li", false) => {
                    w.block();
                    let marker = match list.last_mut() {
                        Some((true, n)) => {
                            *n += 1;
                            format!("{n}. ")
                        }
                        _ => "- ".to_owned(),
                    };
                    let indent = "  ".repeat(list.len().saturating_sub(1));
                    // A list item sits on its own line, not a paragraph.
                    if w.pending_block {
                        w.pending_block = false;
                        w.rich.push('\n');
                        w.plain.push('\n');
                    }
                    w.raw(&format!("{indent}{marker}"), &format!("{indent}{marker}"));
                }
                ("td" | "th", false) => w.pending_space = true,
                _ => {}
            },
        }
        i += 1;
    }
    // Marks left open by broken HTML would read as literal text: drop them.
    Converted {
        rich: w.rich.trim().to_owned(),
        plain: w.plain.trim().to_owned(),
        reply_to,
    }
}

/// Escapes text for HTML.
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// What the user typed, as the HTML Teams sends: escaped, one paragraph,
/// line breaks kept.
pub fn from_text(text: &str) -> String {
    let body = text.lines().map(escape_text).collect::<Vec<_>>().join("<br>");
    format!("<p>{body}</p>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(html: &str) -> Converted {
        convert(html, &Mentions::new())
    }

    #[test]
    fn images_and_file_objects_are_media_but_emoji_are_not() {
        let files = attachments(
            r#"<img src="https://example.org/image?a=1&amp;b=2" width="640" height="480"><img itemtype="http://schema.skype.com/Emoji" src="https://example.org/emoji"><URIObject type="File.1" uri="https://api.asm.skype.com/v1/objects/x"><OriginalName v="report.pdf"/><FileSize v="42"/></URIObject>"#,
        );
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].dimensions, Some((640, 480)));
        assert!(files[0].url.contains("a=1&b=2"));
        assert_eq!(files[1].name.as_ref(), "report.pdf");
        assert_eq!(files[1].size, 42);
        assert!(files[1].url.ends_with("/views/original"));
    }

    #[test]
    fn paragraphs_breaks_and_emphasis() {
        let c = conv("<p>Hello <b>you</b> &amp; <i>them</i></p><p>line<br>two</p>");
        assert_eq!(c.rich, "Hello **you** \\& *them*\n\nline\\\ntwo");
        assert_eq!(c.plain, "Hello you & them\nline\ntwo");
    }

    #[test]
    fn plain_text_is_escaped_not_read_as_markup() {
        let c = conv("<p>2*3 = [6](x)</p>");
        assert_eq!(c.rich, "2\\*3 \\= \\[6\\]\\(x\\)");
    }

    #[test]
    fn links_only_for_web_and_mail() {
        assert_eq!(
            conv(r#"<a href="https://a.b/c(d)">site</a>"#).rich,
            "[site](https://a.b/c%28d%29)"
        );
        assert_eq!(conv(r#"<a href="javascript:alert(1)">x</a>"#).rich, "x");
    }

    #[test]
    fn mentions_resolve_through_properties() {
        let props =
            serde_json::json!({"mentions": "[{\"itemid\":\"0\",\"mri\":\"8:orgid:abc\",\"displayName\":\"Ada\"}]"});
        let m = mentions(&props);
        let c = convert(
            r#"<p>Hi <span itemscope="" itemtype="http://schema.skype.com/Mention" itemid="0">Ada</span>!</p>"#,
            &m,
        );
        assert_eq!(c.rich, "Hi [\\@Ada](godwit://user/8:orgid:abc)\\!");
        assert_eq!(c.plain, "Hi @Ada!");
    }

    #[test]
    fn mention_mris_accept_string_and_array_properties() {
        for properties in [
            serde_json::json!({"mentions": "[{\"mri\":\"8:orgid:abc\"}]"}),
            serde_json::json!({"mentions": [{"mri": "8:orgid:abc"}]}),
        ] {
            assert_eq!(mention_mris(&properties), ["8:orgid:abc"]);
        }
    }

    #[test]
    fn replies_and_emoji() {
        let c = conv(
            r#"<blockquote itemscope="" itemtype="http://schema.skype.com/Reply" itemid="1700000000000"><strong>Bea</strong><p>earlier</p></blockquote><p>yes <img alt="😀" itemtype="http://schema.skype.com/Emoji" src="x"></p>"#,
        );
        assert_eq!(c.reply_to.as_deref(), Some("1700000000000"));
        assert_eq!(c.plain, "yes 😀");
        let c = conv(r#"<p><img itemtype="http://schema.skype.com/Emoji" itemid="smile" alt="(smile)" src="x"></p>"#);
        assert_eq!(c.plain, "🙂", "a text shortcut becomes its emoji");
    }

    #[test]
    fn code_lists_and_entities() {
        assert_eq!(conv("<code>a &lt; b</code>").rich, "`a < b`");
        let c = conv("<ul><li>one</li><li>two</li></ul>");
        assert_eq!(c.plain, "- one\n- two");
        assert_eq!(decode_entities("&#128512; &#x41; &bogus; &"), "😀 A &bogus; &");
    }

    #[test]
    fn broken_html_never_panics() {
        for html in [
            "<",
            "<p",
            "<a href=\"x",
            "</>",
            "<<>>",
            "&#xFFFFFFFF;",
            "<pre>",
            "<span itemtype=\"Mention\">",
        ] {
            let _ = conv(html);
        }
    }

    #[test]
    fn sent_text_is_escaped_html() {
        assert_eq!(from_text("a <b> & c\nnext"), "<p>a &lt;b&gt; &amp; c<br>next</p>");
    }
}

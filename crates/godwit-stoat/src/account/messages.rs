// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Messages: mapping, jumbo emoji, system text, embeds and files.

use super::*;

impl StoatAccount {
    pub(super) fn message(&self, m: &Message) -> CoreMessage {
        let account = self
            .account
            .clone()
            .unwrap_or_else(|| AccountId::new(Network::Stoat, ""));
        let server = self.store.channels.get(&m.channel).and_then(|c| c.server.as_deref());
        let masquerade = m.masquerade.as_ref();
        let author_name = masquerade
            .and_then(|q| q.name.clone())
            .unwrap_or_else(|| self.store.display_name(&m.author, server));
        let author_avatar = masquerade
            .and_then(|q| q.avatar.clone())
            .unwrap_or_else(|| self.avatar_url(&m.author, server));
        let (text, system) = match &m.system {
            Some(sys) => (self.system_text(sys, server), true),
            None => {
                let names = Names {
                    store: &self.store,
                    server,
                    autumn: self.config.as_ref().map(|c| c.features.autumn.url.as_str()),
                };
                (rich::to_rich(m.content.as_deref().unwrap_or_default(), &names), false)
            }
        };
        CoreMessage {
            id: MessageId(m.id.as_str().into()),
            author: UserId::new(account, m.author.as_str()),
            author_name: author_name.into(),
            author_avatar: Some(author_avatar.into()),
            author_colour: server
                .and_then(|s| self.store.role_colour(s, &m.author))
                .map(|(r, g, b)| [r, g, b]),
            sent_ms: ulid_timestamp_ms(&m.id).unwrap_or_default(),
            edited: m.edited.is_some(),
            system,
            text: text.into(),
            markup: if system { Markup::Plain } else { Markup::Rich },
            source: m.content.as_deref().unwrap_or_default().into(),
            reply_to: m.replies.first().map(|r| MessageId(r.as_str().into())),
            jumbo: if system {
                Vec::new()
            } else {
                self.jumbo(m.content.as_deref().unwrap_or_default())
            },
            attachments: m
                .attachments
                .iter()
                .filter_map(|f| {
                    Some(Attachment {
                        name: f.filename.as_str().into(),
                        url: self.file_url(f)?.into(),
                        kind: match f.metadata.kind.as_str() {
                            "Image" => AttachmentKind::Image,
                            "Video" => AttachmentKind::Video,
                            "Audio" => AttachmentKind::Audio,
                            _ => AttachmentKind::File,
                        },
                        size: f.size,
                        dimensions: f.metadata.width.zip(f.metadata.height),
                        original_url: self.file_url(f).map(|u| format!("{u}/original").into()),
                    })
                })
                .collect(),
            embeds: m.embeds.iter().filter_map(|e| self.embed(e, server)).collect(),
            invites: if system {
                Vec::new()
            } else {
                invite_codes(m.content.as_deref().unwrap_or_default())
                    .into_iter()
                    .map(Into::into)
                    .collect()
            },
            reactions: m
                .reactions
                .iter()
                .filter(|(_, users)| !users.is_empty())
                .map(|(emoji, users)| Reaction {
                    emoji: emoji.as_str().into(),
                    // Custom emoji are ids (ULIDs); Unicode emoji are themselves.
                    image_url: self.emoji_url(emoji).map(Into::into),
                    count: u32::try_from(users.len()).unwrap_or(u32::MAX),
                    me: self.store.me.as_ref().is_some_and(|me| users.contains(me)),
                })
                .collect(),
            pending: false,
            snippet: None,
            pinned: m.pinned.unwrap_or(false),
            forwardable: false,
        }
    }

    /// A message that's only emoji, as the pictures to show large (the
    /// official TextWithEmoji's "large emoji" case).
    pub(super) fn jumbo(&self, content: &str) -> Vec<(Arc<str>, Arc<str>)> {
        let blocks = crate::markdown::parse(content);
        let [crate::markdown::Block::Paragraph(inlines)] = blocks.as_slice() else {
            return Vec::new();
        };
        let Some(list) = crate::markdown::emoji_only(inlines) else {
            return Vec::new();
        };
        list.into_iter()
            .filter_map(|e| match e {
                Ok(id) => {
                    let name = self
                        .store
                        .emojis
                        .get(&id)
                        .map_or_else(|| id.clone(), |e| e.name.clone());
                    Some((self.emoji_url(&id)?.into(), format!(":{name}:").into()))
                }
                Err(text) => {
                    let name = emojis::get(&text).map_or(text.as_str(), |e| e.shortcode().unwrap_or(e.name()));
                    Some((self.emoji_url(&text)?.into(), format!(":{name}:").into()))
                }
            })
            .collect()
    }

    /// The official SystemMessage wording.
    pub(super) fn system_text(&self, sys: &Value, server: Option<&str>) -> String {
        let field = |k: &str| sys.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let user = |k: &str| self.store.display_name(&field(k), server);
        match field("type").as_str() {
            "text" => field("content"),
            "user_added" => format!("{} has been added by {}", user("id"), user("by")),
            "user_left" if server.is_some() => format!("{} left the server", user("id")),
            "user_left" => format!("{} left the group", user("id")),
            "user_remove" => format!("{} has been removed by {}", user("id"), user("by")),
            "user_kicked" => format!("{} has been kicked from the server", user("id")),
            "user_banned" => format!("{} has been banned from the server", user("id")),
            "user_joined" => format!("{} joined the server", user("id")),
            "channel_renamed" => format!("{} updated the group name to {}", user("by"), field("name")),
            "channel_description_changed" => format!("{} updated the group description", user("by")),
            "channel_icon_changed" => format!("{} updated the group icon", user("by")),
            "channel_ownership_changed" => format!("{} transferred group ownership to {}", user("from"), user("to")),
            "message_pinned" => format!("{} pinned a message", user("by")),
            "message_unpinned" => format!("{} unpinned a message", user("by")),
            "call_started" => format!("{} started a call", user("by")),
            other => other.to_owned(),
        }
    }

    /// A Stoat embed in the core's shape, laid out as the official `Embed`
    /// component chooses: GIFs bare, bare image and video links without a
    /// card, everything else as a card.
    pub(super) fn embed(&self, embed: &crate::api::Embed, server: Option<&str>) -> Option<godwit_core::message::Embed> {
        use crate::api::Embed as StoatEmbed;
        use godwit_core::message::{Embed as CoreEmbed, EmbedStyle};
        let proxy = |u: &str| -> Arc<str> { self.config.as_ref().map_or_else(|| u.to_owned(), |c| c.proxy(u)).into() };
        let sized = |w: u32, h: u32| (w > 0 && h > 0).then_some((w, h));
        let gif = is_gif(embed);
        Some(match embed {
            StoatEmbed::Website {
                title,
                description,
                url,
                original_url,
                site_name,
                image,
                video,
                special,
                colour,
                icon_url,
            } => {
                let page = url.as_deref().or(original_url.as_deref());
                let dimensions = image
                    .as_ref()
                    .and_then(|i| sized(i.width, i.height))
                    .or_else(|| video.as_ref().and_then(|v| sized(v.width?, v.height?)));
                // Official: GIF videos come straight from the provider.
                let video_url = video
                    .as_ref()
                    .map(|v| if gif { v.url.as_str().into() } else { proxy(&v.url) });
                let bare = title.is_none() && description.is_none() && site_name.is_none();
                // gifbox's own WebM plays natively, at full quality, where its
                // poster would only be a still (and its MP4 needs a browser).
                let webm = video.as_ref().filter(|_| gif).and_then(|v| gifbox_webm(&v.url));
                CoreEmbed {
                    title: title.as_deref().map(Into::into),
                    description: description.as_deref().map(Into::into),
                    url: page.map(Into::into),
                    site_name: site_name.as_deref().map(Into::into),
                    image_url: webm
                        .as_deref()
                        .map(proxy)
                        .or_else(|| image.as_ref().map(|i| proxy(&i.url))),
                    video_url,
                    player_url: special
                        .as_ref()
                        .and_then(|special| special_player(special, original_url.as_deref().or(url.as_deref())))
                        // January has no `special` for music.youtube.com (and may miss Spotify's): the page plays too.
                        .or_else(|| page.and_then(godwit_core::message::link_player))
                        .map(Into::into),
                    media_dimensions: dimensions,
                    image_thumbnail: !gif && image.as_ref().is_some_and(|i| i.size.as_deref() == Some("Preview")),
                    colour: colour.as_deref().and_then(parse_embed_colour),
                    icon_url: icon_url.as_deref().map(proxy),
                    style: if gif {
                        EmbedStyle::Gif
                    } else if bare && image.is_some() {
                        EmbedStyle::Media
                    } else {
                        EmbedStyle::Card
                    },
                    description_markup: Markup::Plain,
                }
            }
            StoatEmbed::Image(i) => CoreEmbed {
                url: Some(i.url.as_str().into()),
                image_url: Some(proxy(&i.url)),
                media_dimensions: sized(i.width, i.height),
                style: if gif { EmbedStyle::Gif } else { EmbedStyle::Media },
                ..CoreEmbed::default()
            },
            StoatEmbed::Video(v) => CoreEmbed {
                url: Some(v.url.as_str().into()),
                image_url: gifbox_webm(&v.url).filter(|_| gif).map(|u| proxy(&u)),
                // Official: GIF videos straight from the provider, except GifBox's.
                video_url: Some(if gif && !has_origin(&v.url, "https://gifbox.me") {
                    v.url.as_str().into()
                } else {
                    proxy(&v.url)
                }),
                media_dimensions: v.width.zip(v.height).and_then(|(w, h)| sized(w, h)),
                style: if gif { EmbedStyle::Gif } else { EmbedStyle::Media },
                ..CoreEmbed::default()
            },
            StoatEmbed::Text {
                title,
                description,
                url,
                media,
                colour,
                icon_url,
            } => {
                let media_url = media.as_ref().and_then(|f| self.file_url(f)).map(Arc::<str>::from);
                let kind = media.as_ref().map(|f| f.metadata.kind.as_str());
                let names = Names {
                    store: &self.store,
                    server,
                    autumn: self.config.as_ref().map(|c| c.features.autumn.url.as_str()),
                };
                CoreEmbed {
                    title: title.as_deref().map(Into::into),
                    // Bots write Markdown, as officially rendered.
                    description: description.as_deref().map(|d| rich::to_rich(d, &names).into()),
                    url: url.as_deref().map(Into::into),
                    image_url: media_url.clone().filter(|_| kind == Some("Image")),
                    video_url: media_url.filter(|_| kind == Some("Video")),
                    media_dimensions: media
                        .as_ref()
                        .and_then(|f| sized(f.metadata.width?, f.metadata.height?)),
                    colour: colour.as_deref().and_then(parse_embed_colour),
                    // Bot icons are links of their own: proxied like a site's.
                    icon_url: icon_url.as_deref().map(proxy),
                    description_markup: Markup::Rich,
                    ..CoreEmbed::default()
                }
            }
            StoatEmbed::None => return None,
        })
    }

    pub(super) fn file_url(&self, f: &File) -> Option<String> {
        let autumn = &self.config.as_ref()?.features.autumn.url;
        Some(format!("{autumn}/{}/{}", f.tag, f.id))
    }
}

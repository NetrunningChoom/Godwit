// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The networks Godwit speaks, and what each one is like to use.
//!
//! [`Security`] feeds the per-chat indicator and the "better network" ranking.
//! It's a plain-language summary, not an audit: review it when a network
//! changes its policies, and keep [`SECURITY_REVIEWED`] current.

use serde::{Deserialize, Serialize};

use crate::members::Presence;

/// When the [`Security`] summaries were last checked against the networks'
/// own documentation.
pub const SECURITY_REVIEWED: &str = "2026-11";

/// A chat network. The order is the order Godwit adds them in. Others
/// (Matrix, Signal, WhatsApp) come back once these are done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Network {
    Stoat,
    Discord,
    Steam,
    /// Open source and Discord-like (docs.fluxer.com); first version: text chat.
    Fluxer,
    /// Microsoft Teams, work and school accounts; first version: chats and channels.
    Teams,
    /// Slack: each workspace a server; first version: text chat.
    Slack,
    /// Google Chat: direct messages and spaces; first version: text chat.
    GoogleChat,
}

impl Network {
    /// It has friends (requests, a friends list) and messaging goes through
    /// them, as on Stoat and Discord.
    /// A friends list of its own, with requests (adding by name).
    pub fn has_friends(self) -> bool {
        matches!(self, Self::Stoat | Self::Discord | Self::Steam)
    }

    /// Rooms' member lists come from the network; on the others they're
    /// who has written lately.
    pub fn has_member_lists(self) -> bool {
        matches!(self, Self::Stoat | Self::Discord | Self::Steam)
    }

    /// The newest Emoji version this network's own clients have, when it's
    /// behind the full set: the picker offers nothing newer, so a reaction
    /// or message never carries an emoji the other side can't show.
    /// `None`: every emoji Godwit knows. Teams: Emoji 15.1, per Emojipedia's
    /// Teams page (2026-11-04). Slack and Google Chat: no source found yet,
    /// so not limited (see docs/gchat.md).
    pub fn emoji_version(self) -> Option<(u32, u32)> {
        // Teams' own set (its emoticon table) limits it in the app.
        match self {
            Self::Teams => Some((15, 1)),
            _ => None,
        }
    }

    /// Its account raises its own alerts (`Event::Alert`), following the
    /// network's notification settings; for the others the app alerts for
    /// direct and group chats itself.
    pub fn raises_alerts(self) -> bool {
        matches!(self, Self::Stoat | Self::Discord | Self::Steam | Self::Teams)
    }

    /// Presence we can see and set. On the others (workplace chats)
    /// Godwit shows us as online while signed in, as their web clients
    /// keep you active while they're open.
    pub fn has_presence(self) -> bool {
        matches!(
            self,
            Self::Stoat | Self::Discord | Self::Steam | Self::Slack | Self::Teams | Self::GoogleChat
        )
    }

    /// How the network lists a server's threads. Slack gathers them in a
    /// panel (its "Threads" view); elsewhere threads are rooms of their own.
    pub fn thread_style(self) -> ThreadStyle {
        match self {
            Self::Slack => ThreadStyle::Panel,
            _ => ThreadStyle::Rooms,
        }
    }

    /// Its calls can carry our camera or screen (the share button shows,
    /// greyed out where a call doesn't allow it).
    pub fn call_video(self) -> bool {
        matches!(self, Self::Stoat | Self::Discord)
    }

    /// A custom status line can be set (Steam has none).
    pub fn has_status_text(self) -> bool {
        self != Self::Steam && self.has_presence()
    }

    /// The presences we can pick, in the official client's order and words.
    pub fn presence_choices(self) -> &'static [(&'static str, Presence)] {
        use Presence::*;
        match self {
            Self::GoogleChat => &[("Automatic", Online), ("Do Not Disturb", Focus), ("Set as away", Idle)],
            Self::Teams => &[
                ("Available", Online),
                ("Busy", Busy),
                ("Do Not Disturb", Focus),
                ("Away", Idle),
                ("Appear offline", Invisible),
            ],
            Self::Slack => &[("Available", Online), ("Away", Idle)],
            // Steam's friends list: online, away, invisible.
            Self::Steam => &[("Online", Online), ("Away", Idle), ("Invisible", Invisible)],
            // Workplace chats: always online while signed in (see `has_presence`).
            n if !n.has_presence() => &[("Online", Online)],
            // Stoat's (and the default): the official five.
            _ => &[
                ("Online", Online),
                ("Idle", Idle),
                ("Focus", Focus),
                ("Do Not Disturb", Busy),
                ("Invisible", Invisible),
            ],
        }
    }

    /// Server settings (overview, roles, invites, bans) can be edited.
    pub fn has_server_settings(self) -> bool {
        matches!(self, Self::Stoat | Self::Discord)
    }

    /// The account settings pages the network offers besides the profile.
    pub fn account_pages(self) -> &'static [AccountPage] {
        match self {
            // Auto-delete is a Godwit extra on Stoat (from Godwit).
            Self::Stoat => &[AccountPage::AutoDelete],
            Self::Discord => &[AccountPage::PrivacyAndSessions],
            _ => &[],
        }
    }

    /// A profile banner can be set. Discord's is Nitro, so never offered.
    pub fn has_profile_banner(self) -> bool {
        self != Self::Discord
    }

    /// The size a picture is scaled to before uploading, and the most it may
    /// weigh. Avatars and icons are 512² (shown at most ~128 px); Stoat's
    /// Autumn takes icons up to 2.5 MB and banners up to 6 MB, Discord up to
    /// 8 MB.
    pub fn image_spec(self, kind: ImageKind) -> ImageSpec {
        const MB: u64 = 1024 * 1024;
        let avatar = ImageSpec {
            width: 512,
            height: 512,
            max_bytes: match self {
                Self::Stoat => MB * 5 / 2,
                Self::Discord => MB * 8,
                _ => MB * 4,
            },
        };
        let banner = |width, height| ImageSpec {
            width,
            height,
            ..avatar
        };
        match (kind, self) {
            (ImageKind::Avatar, _) => avatar,
            // Stoat's server banner sits above the channel list (2:1); its
            // profile background heads the profile card (3:1).
            (ImageKind::ServerBanner, Self::Stoat) => ImageSpec {
                max_bytes: MB * 6,
                ..banner(1200, 600)
            },
            (ImageKind::ProfileBanner, Self::Stoat) => ImageSpec {
                max_bytes: MB * 6,
                ..banner(1500, 500)
            },
            // Discord's profile banner is 17:6 (680×240 shown), a server's 16:9.
            (ImageKind::ProfileBanner, _) => banner(1360, 480),
            (ImageKind::ServerBanner, _) => banner(1920, 1080),
        }
    }

    /// Who besides the people in a chat can read unencrypted messages, when
    /// it's not just the network itself.
    pub fn readers(self) -> Option<&'static str> {
        match self {
            Self::GoogleChat => Some("Google and Workspace admins"),
            _ => None,
        }
    }

    /// Only partly built so far (text chat only).
    pub fn work_in_progress(self) -> bool {
        matches!(self, Self::Fluxer | Self::Teams | Self::Slack | Self::GoogleChat)
    }

    /// A person's name is the same in every chat (Steam's persona), unlike
    /// per-server nicknames: when it changes, lines already shown take it.
    pub fn names_network_wide(self) -> bool {
        self == Self::Steam
    }

    /// No push stream yet (Google Chat): reopening a loaded chat must fetch
    /// what came meanwhile.
    pub fn refetch_on_open(self) -> bool {
        self == Self::GoogleChat
    }

    /// What identifies the network's own desktop app among running
    /// processes, so alerts can stay quiet while it runs. Only Steam's can be
    /// told so far.
    pub fn official_app(self) -> Option<OfficialApp> {
        match self {
            // The client is "steam" (also under Flatpak); its UI runs as
            // "steamwebhelper".
            Self::Steam => Some(OfficialApp {
                linux: &["steam", "steamwebhelper"],
                windows: &["steam.exe"],
                macos: &["steam_osx"],
            }),
            _ => None,
        }
    }

    pub const ALL: [Self; 7] = [
        Self::Stoat,
        Self::Discord,
        Self::Steam,
        Self::Fluxer,
        Self::Teams,
        Self::Slack,
        Self::GoogleChat,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Stoat => "Stoat",
            Self::Discord => "Discord",
            Self::Steam => "Steam",
            Self::Fluxer => "Fluxer",
            Self::Teams => "Microsoft Teams",
            Self::Slack => "Slack",
            Self::GoogleChat => "Google Chat",
        }
    }

    /// The name in a word, for tight spots (the rail's label above an account).
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Teams => "Teams",
            other => other.name(),
        }
    }

    /// A short, stable key for ids and config files.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Stoat => "stoat",
            Self::Discord => "discord",
            Self::Steam => "steam",
            Self::Fluxer => "fluxer",
            Self::Teams => "teams",
            Self::Slack => "slack",
            Self::GoogleChat => "gchat",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|n| n.key() == key)
    }

    /// What the network is like by default. A room can be better than its
    /// network's default; see [`crate::room::Room::encryption`].
    pub const fn security(self) -> Security {
        use Encryption::*;
        use MetadataExposure::*;
        use ThirdPartyClients::*;
        match self {
            // No end-to-end encryption; open source, self-hostable.
            Self::Stoat => Security {
                messages: None,
                calls: None,
                metadata: Full,
                open_source: true,
                needs_phone_number: false,
                third_party_clients: Allowed,
            },
            // Messages aren't encrypted; calls are (the DAVE protocol).
            Self::Discord => Security {
                messages: None,
                calls: Always,
                metadata: Full,
                open_source: false,
                needs_phone_number: false,
                third_party_clients: Forbidden,
            },
            Self::Steam => Security {
                messages: None,
                calls: None,
                metadata: Full,
                open_source: false,
                needs_phone_number: false,
                third_party_clients: Unsupported,
            },
            // Open source and self-hostable; its docs are written for
            // third-party clients and bots.
            Self::Fluxer => Security {
                messages: None,
                calls: None,
                metadata: Full,
                open_source: true,
                needs_phone_number: false,
                third_party_clients: Allowed,
            },
            // Encrypted in transit and at rest, not end to end: Microsoft and
            // the organisation's admins can read chats (end-to-end encryption
            // exists only for some 1:1 calls, off by default). Other clients
            // aren't supported, and an organisation can block them.
            Self::Teams => Security {
                messages: None,
                calls: None,
                metadata: Full,
                open_source: false,
                needs_phone_number: false,
                third_party_clients: Unsupported,
            },
            // Not end to end: Slack and the workspace's owners can read
            // messages (exports). Its web session works from other clients,
            // as wee-slack's, but Slack doesn't support that.
            Self::Slack => Security {
                messages: None,
                calls: None,
                metadata: Full,
                open_source: false,
                needs_phone_number: false,
                third_party_clients: Unsupported,
            },
            // Google Chat isn't end-to-end encrypted: Google and Workspace
            // admins can read messages. Signing in as its web client is
            // unsupported, just as Slack's web session is.
            Self::GoogleChat => Security {
                messages: None,
                calls: None,
                metadata: Full,
                open_source: false,
                needs_phone_number: false,
                third_party_clients: Unsupported,
            },
        }
    }
}

/// Whether content is end-to-end encrypted, i.e. unreadable to the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Encryption {
    None,
    /// Encrypted unless someone turns it off (or makes a room public).
    ByDefault,
    Always,
}

/// How much the network can see about who talks to whom, when.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MetadataExposure {
    /// Contents and connections.
    Full,
    /// Connections and timing, not contents.
    Partial,
    /// As little as the design allows.
    Minimal,
}

/// The network's stance on clients like Godwit; shown so users know the risk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThirdPartyClients {
    /// Open protocol; other clients are welcome.
    Allowed,
    /// Works, but the network doesn't support it.
    Unsupported,
    /// Against the terms: accounts can be banned.
    Forbidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Security {
    pub messages: Encryption,
    pub calls: Encryption,
    pub metadata: MetadataExposure,
    pub open_source: bool,
    pub needs_phone_number: bool,
    pub third_party_clients: ThirdPartyClients,
}

impl Security {
    /// A ranking for "the better network" for talking to one person:
    /// message encryption first, then metadata, then openness. Higher is
    /// better.
    ///
    /// `ByDefault` counts as encrypted: a new DM starts encrypted.
    pub fn score(&self) -> u32 {
        let encryption = match self.messages {
            Encryption::None => 0,
            Encryption::ByDefault | Encryption::Always => 1,
        };
        let metadata = match self.metadata {
            MetadataExposure::Full => 0,
            MetadataExposure::Partial => 1,
            MetadataExposure::Minimal => 2,
        };
        encryption * 100 + metadata * 10 + u32::from(self.open_source)
    }
}

/// How a network lists threads ([`Network::thread_style`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadStyle {
    /// Each thread is a room of its own.
    Rooms,
    /// A server's threads are gathered in one panel.
    Panel,
}

/// An account settings page beyond the profile ([`Network::account_pages`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountPage {
    AutoDelete,
    PrivacyAndSessions,
}

/// A picture someone uploads ([`Network::image_spec`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    /// Avatars and server icons (round or square).
    Avatar,
    ProfileBanner,
    ServerBanner,
}

/// The size a picture is scaled to and the most it may weigh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageSpec {
    pub width: u32,
    pub height: u32,
    pub max_bytes: u64,
}

/// Process names of a network's own desktop app, per operating system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfficialApp {
    pub linux: &'static [&'static str],
    pub windows: &'static [&'static str],
    pub macos: &'static [&'static str],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_per_network() {
        use Network::*;
        let panel: Vec<_> = Network::ALL
            .into_iter()
            .filter(|n| n.thread_style() == ThreadStyle::Panel)
            .collect();
        assert_eq!(panel, [Slack]);
        let video: Vec<_> = Network::ALL.into_iter().filter(|n| n.call_video()).collect();
        assert_eq!(video, [Stoat, Discord]);
        let settings: Vec<_> = Network::ALL.into_iter().filter(|n| n.has_server_settings()).collect();
        assert_eq!(settings, [Stoat, Discord]);
        assert!(!Steam.has_status_text() && Stoat.has_status_text() && !Fluxer.has_status_text());
        assert!(!Discord.has_profile_banner() && Stoat.has_profile_banner());
        assert_eq!(Stoat.account_pages(), [AccountPage::AutoDelete]);
        assert_eq!(Discord.account_pages(), [AccountPage::PrivacyAndSessions]);
        assert!(Steam.account_pages().is_empty());
        assert_eq!(GoogleChat.readers(), Some("Google and Workspace admins"));
        assert!(Stoat.readers().is_none());
        let wip: Vec<_> = Network::ALL.into_iter().filter(|n| n.work_in_progress()).collect();
        assert_eq!(wip, [Fluxer, Teams, Slack, GoogleChat]);
        assert!(Steam.names_network_wide() && !Discord.names_network_wide());
        assert!(GoogleChat.refetch_on_open() && !Slack.refetch_on_open());
        let apps: Vec<_> = Network::ALL
            .into_iter()
            .filter(|n| n.official_app().is_some())
            .collect();
        assert_eq!(apps, [Steam]);
    }

    #[test]
    fn presence_choices_per_network() {
        let labels = |n: Network| n.presence_choices().iter().map(|c| c.0).collect::<Vec<_>>();
        assert_eq!(
            labels(Network::Stoat),
            ["Online", "Idle", "Focus", "Do Not Disturb", "Invisible"]
        );
        assert_eq!(labels(Network::Steam), ["Online", "Away", "Invisible"]);
        assert_eq!(labels(Network::Slack), ["Available", "Away"]);
        assert_eq!(labels(Network::Teams).len(), 5);
        assert_eq!(
            labels(Network::GoogleChat),
            ["Automatic", "Do Not Disturb", "Set as away"]
        );
        assert_eq!(labels(Network::Fluxer), ["Online"]);
    }

    #[test]
    fn image_specs_match_the_networks_limits() {
        const MB: u64 = 1024 * 1024;
        let s = Network::Stoat;
        assert_eq!(
            s.image_spec(ImageKind::Avatar),
            ImageSpec {
                width: 512,
                height: 512,
                max_bytes: MB * 5 / 2
            }
        );
        assert_eq!(
            s.image_spec(ImageKind::ServerBanner),
            ImageSpec {
                width: 1200,
                height: 600,
                max_bytes: MB * 6
            }
        );
        assert_eq!(
            s.image_spec(ImageKind::ProfileBanner),
            ImageSpec {
                width: 1500,
                height: 500,
                max_bytes: MB * 6
            }
        );
        let d = Network::Discord;
        assert_eq!(d.image_spec(ImageKind::Avatar).max_bytes, MB * 8);
        assert_eq!(
            d.image_spec(ImageKind::ProfileBanner),
            ImageSpec {
                width: 1360,
                height: 480,
                max_bytes: MB * 8
            }
        );
        assert_eq!(
            d.image_spec(ImageKind::ServerBanner),
            ImageSpec {
                width: 1920,
                height: 1080,
                max_bytes: MB * 8
            }
        );
        assert_eq!(Network::Steam.image_spec(ImageKind::Avatar).max_bytes, MB * 4);
    }

    #[test]
    fn keys_round_trip() {
        for n in Network::ALL {
            assert_eq!(Network::from_key(n.key()), Some(n));
        }
        assert_eq!(Network::from_key("irc"), None);
    }

    #[test]
    fn google_chat_has_a_stable_key_and_no_private_content_guarantee() {
        let network = Network::GoogleChat;
        assert_eq!(network.name(), "Google Chat");
        assert_eq!(network.key(), "gchat");
        assert_eq!(Network::from_key("gchat"), Some(network));
        assert!(!network.has_friends());
        assert_eq!(network.security().messages, Encryption::None);
        assert_eq!(network.security().calls, Encryption::None);
        assert_eq!(network.security().metadata, MetadataExposure::Full);
        assert_eq!(network.security().third_party_clients, ThirdPartyClients::Unsupported);
    }

    #[test]
    fn ranking_puts_encrypted_networks_first() {
        let score = |n: Network| n.security().score();
        assert!(score(Network::Stoat) > score(Network::Discord), "openness counts");
    }
}

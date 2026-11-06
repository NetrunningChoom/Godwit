// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Account picker, verification and room-security presentation.

use super::*;

/// The networks on the "Add an account" page, in its order.
pub const ACCOUNT_NETWORKS: [Network; 7] = crate::registry::PICKER;

pub fn network(n: Network, available: bool) -> NetworkItem {
    let sec = n.security();
    let name = n.name();
    let messages = match sec.messages {
        Encryption::Always => "Messages are always end-to-end encrypted".to_owned(),
        Encryption::ByDefault => "Messages are end-to-end encrypted by default".to_owned(),
        Encryption::None => "Messages aren't end-to-end encrypted".to_owned(),
    };
    let metadata = match sec.metadata {
        // Described as what the protocol protects, not as anything a network does.
        MetadataExposure::Full => "Who you talk to and when isn't end-to-end protected".to_owned(),
        MetadataExposure::Partial => "Message contents are protected; who you talk to and when isn't".to_owned(),
        MetadataExposure::Minimal => "Designed to keep who you talk to private".to_owned(),
    };
    let facts = format!(
        "{} · {}",
        if sec.open_source {
            "Open source"
        } else {
            "Closed source"
        },
        if sec.needs_phone_number {
            "Needs a phone number"
        } else {
            "No phone number needed"
        },
    );
    let warning = match sec.third_party_clients {
        ThirdPartyClients::Allowed => String::new(),
        ThirdPartyClients::Unsupported => {
            format!("{name} doesn't officially support other clients. Use at your own risk.")
        }
        ThirdPartyClients::Forbidden => {
            format!("{name}'s terms don't allow other clients; your account could be limited. Use at your own risk.")
        }
    };
    let lines: Vec<SharedString> = [messages, metadata, facts].into_iter().map(Into::into).collect();
    NetworkItem {
        key: n.key().into(),
        name: name.into(),
        available,
        encrypted: sec.messages != Encryption::None,
        lines: ModelRc::new(VecModel::from(lines)),
        warning: warning.into(),
        // These work in part: text chat only so far.
        wip: n.work_in_progress(),
    }
}

thread_local! {
    static NO_EMOJI: ModelRc<VerifyEmojiItem> = ModelRc::from(Rc::new(VecModel::<VerifyEmojiItem>::default()));
}
/// The verification part of an account row (all empty when there's none).
pub fn verification(step: Option<&Verify>, images: &mut Images) -> AccountItem {
    let (n, text, emoji) = match step {
        None => (0, String::new(), Vec::new()),
        Some(Verify::Waiting) => (1, String::new(), Vec::new()),
        Some(Verify::Compare(pairs)) => {
            let emoji = pairs
                .iter()
                .map(|(symbol, name)| {
                    // Bundled, so verifying fetches nothing; the name is what
                    // the spec defines, and always shows.
                    let image = crate::sas::png(symbol)
                        .and_then(|png| images.bundled(&format!("godwit-sas:{symbol}"), EMOJI, png));
                    VerifyEmojiItem {
                        name: name.as_ref().into(),
                        has_image: image.is_some(),
                        image: image.unwrap_or_else(no_image),
                    }
                })
                .collect();
            (2, String::new(), emoji)
        }
        Some(Verify::Confirmed) => (3, String::new(), Vec::new()),
        Some(Verify::Done) => (4, String::new(), Vec::new()),
        Some(Verify::Failed(why)) => (5, why.to_string(), Vec::new()),
        Some(Verify::Incoming) => (6, String::new(), Vec::new()),
    };
    AccountItem {
        verify_step: n,
        verify_text: text.into(),
        avatar: no_image(),
        // Models compare by identity, so a new one never equals the last:
        // with no emoji, one shared empty model keeps unchanged rows equal,
        // so they aren't rebuilt and a half-typed recovery key survives.
        verify_emoji: if emoji.is_empty() {
            NO_EMOJI.with(Clone::clone)
        } else {
            ModelRc::from(Rc::new(VecModel::from(emoji)))
        },
        ..AccountItem::default()
    }
}

/// The header's security line for a room.
pub fn security(r: &Room) -> (bool, String) {
    let network = r.network();
    match r.encryption {
        RoomEncryption::Encrypted => (true, "End-to-end encrypted".into()),
        RoomEncryption::Unknown => (false, "Checking encryption…".into()),
        RoomEncryption::Unencrypted => {
            // Only the fact, nothing about who could read what: claims about
            // a network's own conduct are its to make, not ours.
            let line = if network.readers().is_some() || network.security().messages == Encryption::None {
                "Not end-to-end encrypted".to_owned()
            } else {
                "Not end-to-end encrypted in this room".to_owned()
            };
            (false, line)
        }
    }
}

#[cfg(test)]
mod verification_tests {
    use super::*;

    #[test]
    fn rows_without_a_comparison_stay_equal_between_refreshes() {
        let mut images = Images::offline();
        let (x, y) = (verification(None, &mut images), verification(None, &mut images));
        assert!(x.verify_emoji == y.verify_emoji, "models");
        assert!(x.avatar == y.avatar, "images");
        assert!(no_image() == no_image(), "the stand-in compares equal");
        assert_eq!(x, y);
        assert_eq!(
            verification(Some(&Verify::Waiting), &mut images),
            verification(Some(&Verify::Waiting), &mut images)
        );
        let compare = Verify::Compare(vec![("🐶".into(), "Dog".into())]);
        assert_eq!(verification(Some(&compare), &mut images).verify_step, 2);
    }
}

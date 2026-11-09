// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! DAVE, Discord's end-to-end encryption for calls (<https://daveprotocol.com>),
//! through the `davey` crate. The MLS handshake follows Yapfast's `dave.rs`
//! (GPL-3.0-or-later), itself after songbird's (ISC): it answers with the
//! messages to send rather than sending them, so it can be tested offline.
//! Calls only: a call's MLS group is its channel.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU16;

use davey::{DaveSession, MediaType, ProposalsOperationType, SessionStatus};
use serenity_voice_model::Event;
use serenity_voice_model::payload::{
    DaveMlsCommitWelcome, DaveMlsInvalidCommitWelcome, DaveMlsKeyPackage, DaveMlsProposalsOperationType,
    DaveTransitionReady,
};

use super::note;

/// A message for the voice gateway.
#[derive(Debug)]
pub enum Out {
    Json(Event),
    Binary(Event),
}

pub struct Dave {
    user_id: u64,
    /// The MLS group's id: the call's channel.
    channel_id: u64,
    session: Option<DaveSession>,
    /// The protocol version in force; 0 is transport encryption only.
    version: u16,
    /// Transitions announced and not yet executed: id → protocol version.
    pending: HashMap<u16, u16>,
    /// Who the voice server says is connected, which proposals must match.
    users: HashSet<u64>,
    /// Why the last encrypted frame couldn't be read, for the log.
    pub unreadable: Option<String>,
}

impl Dave {
    pub fn new(user_id: u64, channel_id: u64) -> Self {
        Self {
            user_id,
            channel_id,
            session: None,
            version: 0,
            pending: HashMap::new(),
            users: HashSet::from([user_id]),
            unreadable: None,
        }
    }

    /// Someone a voice gateway message says is in the call.
    pub fn recognise(&mut self, user: u64) {
        self.users.insert(user);
    }

    /// Starts with the protocol version from the session description.
    pub fn start(&mut self, version: u16) -> Vec<Out> {
        self.version = version;
        self.reinit()
    }

    /// End-to-end encrypts an Opus frame. In a call without DAVE (version
    /// 0) frames go out as they are, under transport encryption only. In a
    /// DAVE call nothing goes out in the clear: until the group is ready,
    /// and when it can't encrypt, the frame is dropped (None). Discord's
    /// clients send in the clear while the group forms; Godwit prefers a
    /// moment of silence to sending unencrypted audio.
    pub fn encrypt_opus(&mut self, frame: &[u8]) -> Option<Vec<u8>> {
        if self.version == 0 {
            return Some(frame.to_vec());
        }
        if let Some(session) = self.session.as_mut()
            && session.is_ready()
        {
            return match session.encrypt_opus(frame) {
                Ok(encrypted) => Some(encrypted.into_owned()),
                Err(error) => {
                    note(&format!("could not end-to-end encrypt sound: {error:?}"));
                    None
                }
            };
        }
        None
    }

    /// End-to-end encrypts a whole video frame we send (VP8 as encoded,
    /// H264 in Annex B form), under the same rules as sound: as it is
    /// without DAVE, otherwise dropped (None) until the group can encrypt.
    pub fn encrypt_video(&mut self, codec: super::depacketize::Codec, frame: &[u8]) -> Option<Vec<u8>> {
        let codec = match codec {
            super::depacketize::Codec::Vp8 => davey::Codec::VP8,
            super::depacketize::Codec::H264 => davey::Codec::H264,
            super::depacketize::Codec::Av1 => davey::Codec::AV1,
        };
        if self.version == 0 {
            return Some(frame.to_vec());
        }
        if let Some(session) = self.session.as_mut()
            && session.is_ready()
        {
            return match session.encrypt(MediaType::VIDEO, codec, frame) {
                Ok(encrypted) => Some(encrypted.into_owned()),
                Err(error) => {
                    note(&format!("could not end-to-end encrypt video: {error:?}"));
                    None
                }
            };
        }
        None
    }

    /// Decrypts a received Opus frame from `sender`. Before the group is
    /// ready, frames without DAVE's marker pass through, as Discord's clients
    /// allow; once it is, only during a transition, which the session times,
    /// so the voice server can't slip unencrypted audio into an encrypted
    /// call. Frames that can't be decrypted are dropped.
    pub fn decrypt_opus(&mut self, sender: u64, frame: Vec<u8>) -> Option<Vec<u8>> {
        self.decrypt(sender, MediaType::AUDIO, frame)
    }

    /// Decrypts a whole received video frame from `sender` (H264 in Annex B
    /// form, VP8 as sent), under the same rules as sound.
    pub fn decrypt_video(&mut self, sender: u64, frame: Vec<u8>) -> Option<Vec<u8>> {
        self.decrypt(sender, MediaType::VIDEO, frame)
    }

    fn decrypt(&mut self, sender: u64, media: MediaType, frame: Vec<u8>) -> Option<Vec<u8>> {
        if self.version == 0 {
            return Some(frame);
        }
        let Some(session) = self.session.as_mut().filter(|s| s.is_ready()) else {
            // https://daveprotocol.com/#protocol-frame-check: a tag, a size
            // byte and the magic marker at least.
            if frame.len() < 11 || !frame.ends_with(&[0xfa, 0xfa]) {
                return Some(frame);
            }
            self.unreadable = Some("not in the group yet".to_owned());
            return None;
        };
        match session.decrypt(sender, media, &frame) {
            Ok(decrypted) => Some(decrypted),
            Err(error) => {
                self.unreadable = Some(format!("{error:?}"));
                None
            }
        }
    }

    /// Whether frames leave end-to-end encrypted.
    pub fn encrypting(&self) -> bool {
        self.version != 0 && self.session.as_ref().is_some_and(DaveSession::is_ready)
    }

    /// The group's voice privacy code, which Discord's clients show in a
    /// call's encryption details so the people in it can compare.
    pub fn privacy_code(&self) -> Option<&str> {
        self.session
            .as_ref()
            .filter(|_| self.encrypting())
            .and_then(DaveSession::voice_privacy_code)
    }

    /// Where the handshake is, for the log.
    pub fn status(&self) -> String {
        match &self.session {
            _ if self.version == 0 => "off".to_owned(),
            Some(session) => format!(
                "{:?}, ready {}, epoch {}",
                session.status(),
                session.is_ready(),
                session.epoch().map_or(0, |epoch| epoch.as_u64()),
            ),
            None => "no session".to_owned(),
        }
    }

    pub fn handle(&mut self, event: Event) -> Vec<Out> {
        match event {
            Event::ClientsConnect(event) => {
                self.users.extend(event.user_ids.iter().map(|user| user.0));
                vec![]
            }
            Event::ClientDisconnect(event) => {
                self.users.remove(&event.user_id.0);
                vec![]
            }
            Event::DavePrepareTransition(event) => {
                self.pending.insert(event.transition_id, event.protocol_version);
                if event.transition_id == 0 {
                    self.execute(event.transition_id);
                    vec![]
                } else {
                    // Downgrading: keep reading encrypted frames for a while.
                    if event.protocol_version == 0
                        && let Some(session) = self.session.as_mut()
                    {
                        session.set_passthrough_mode(true, Some(120));
                    }
                    vec![ready(event.transition_id, event.protocol_version)]
                }
            }
            Event::DaveExecuteTransition(event) => {
                self.execute(event.transition_id);
                note(&format!(
                    "DAVE transition {} done: {}",
                    event.transition_id,
                    self.status()
                ));
                vec![]
            }
            // A new group: send a new key package.
            Event::DavePrepareEpoch(event) if event.epoch == 1 => {
                self.version = event.protocol_version;
                self.reinit()
            }
            Event::DaveMlsExternalSender(event) => {
                if let Some(session) = self.session.as_mut()
                    && let Err(error) = session.set_external_sender(&event.external_sender)
                {
                    note(&format!("could not set DAVE's external sender: {error:?}"));
                }
                vec![]
            }
            Event::DaveMlsProposals(event) => {
                let operation = match event.operation_type {
                    DaveMlsProposalsOperationType::Append => ProposalsOperationType::APPEND,
                    DaveMlsProposalsOperationType::Revoke => ProposalsOperationType::REVOKE,
                };
                if operation == ProposalsOperationType::APPEND {
                    self.follow(proposals_group(&event.proposals));
                }
                let users: Vec<u64> = self.users.iter().copied().collect();
                let result = self
                    .session
                    .as_mut()
                    .map(|session| session.process_proposals(operation, &event.proposals, Some(&users)));
                match result {
                    Some(Ok(Some(commit))) => vec![Out::Binary(Event::from(DaveMlsCommitWelcome {
                        commit: commit.commit,
                        welcome: commit.welcome,
                    }))],
                    // Until our welcome arrives, proposals for the group the
                    // others are in don't match our empty one.
                    Some(Err(_))
                        if self
                            .session
                            .as_ref()
                            .is_some_and(|s| s.status() != SessionStatus::ACTIVE) =>
                    {
                        vec![]
                    }
                    Some(Err(error)) => {
                        note(&format!("could not process DAVE proposals: {error:?}"));
                        vec![]
                    }
                    _ => vec![],
                }
            }
            Event::DaveMlsAnnounceCommitTransition(event) => {
                self.follow(message_group(&event.commit_message));
                // Before we have a group, or have committed to our pending
                // one, a commit is for a group a welcome will bring us into.
                // libdave ignores it; refusing it would start over and lose
                // the key package that welcome is sealed to.
                if !self.session.as_ref().is_some_and(|session| {
                    matches!(
                        session.status(),
                        SessionStatus::AWAITING_RESPONSE | SessionStatus::ACTIVE
                    )
                }) {
                    return vec![];
                }
                let result = self
                    .session
                    .as_mut()
                    .map(|session| session.process_commit(&event.commit_message));
                self.joined(event.transition_id, result.map(|r| r.map_err(|e| format!("{e:?}"))))
            }
            Event::DaveMlsWelcome(event) => {
                let result = self
                    .session
                    .as_mut()
                    .map(|session| session.process_welcome(&event.welcome));
                self.joined(event.transition_id, result.map(|r| r.map_err(|e| format!("{e:?}"))))
            }
            _ => vec![],
        }
    }

    /// Moves to the group the voice server's messages name when it isn't
    /// the one guessed: a Go Live stream's group isn't its voice channel's,
    /// and Discord doesn't say it (Yapfast's finding). `davey` 0.1 can't move
    /// a session to another group (Yapfast patches it in), so the session
    /// keeps the guess until the next commit it can't take for that reason:
    /// that refusal starts over (`joined`), and starting over uses the group
    /// learned here. A welcome opens whichever group it's for, so frames
    /// decrypt meanwhile.
    fn follow(&mut self, group: Option<u64>) {
        let Some(group) = group.filter(|&group| group != self.channel_id) else {
            return;
        };
        note(&format!("DAVE's group is {group}, not the {} guessed", self.channel_id));
        self.channel_id = group;
    }

    /// After a commit or welcome: ready for the transition, or start over.
    fn joined(&mut self, transition_id: u16, result: Option<Result<(), String>>) -> Vec<Out> {
        match result {
            Some(Ok(())) if transition_id != 0 => {
                self.pending.insert(transition_id, self.version);
                vec![ready(transition_id, self.version)]
            }
            Some(Err(error)) => {
                note(&format!("a DAVE commit or welcome failed: {error}"));
                let mut out = vec![Out::Json(Event::from(DaveMlsInvalidCommitWelcome { transition_id }))];
                out.extend(self.reinit());
                out
            }
            _ => vec![],
        }
    }

    fn reinit(&mut self) -> Vec<Out> {
        let Some(version) = NonZeroU16::new(self.version) else {
            if let Some(session) = self.session.as_mut() {
                if let Err(error) = session.reset() {
                    note(&format!("could not reset the DAVE session: {error:?}"));
                }
                session.set_passthrough_mode(true, Some(10));
            }
            return vec![];
        };
        let key_package = match self.session.as_mut() {
            Some(session) => session
                .reinit(version, self.user_id, self.channel_id, None)
                .map_err(|e| format!("{e:?}"))
                .and_then(|()| session.create_key_package().map_err(|e| format!("{e:?}"))),
            None => DaveSession::new(version, self.user_id, self.channel_id, None)
                .map_err(|e| format!("{e:?}"))
                .and_then(|mut session| {
                    let key_package = session.create_key_package().map_err(|e| format!("{e:?}"));
                    self.session = Some(session);
                    key_package
                }),
        };
        match key_package {
            Ok(key_package) => vec![Out::Binary(Event::DaveMlsKeyPackage(DaveMlsKeyPackage { key_package }))],
            Err(error) => {
                note(&format!("could not start the DAVE session: {error}"));
                vec![]
            }
        }
    }

    fn execute(&mut self, transition_id: u16) {
        let Some(version) = self.pending.remove(&transition_id) else {
            note(&format!("DAVE transition to an unknown id {transition_id}"));
            return;
        };
        let old = std::mem::replace(&mut self.version, version);
        // Upgraded from transport-only encryption: frames still in flight
        // from before may be plain for a moment.
        if transition_id > 0
            && old == 0
            && version != 0
            && let Some(session) = self.session.as_mut()
        {
            session.set_passthrough_mode(true, Some(10));
        }
    }
}

/// The group of the first of a proposals message's MLS messages.
fn proposals_group(proposals: &[u8]) -> Option<u64> {
    message_group(vector(proposals)?.0)
}

/// The group of an MLS message: a version, a wire format of 1 (a public
/// message, the only kind DAVE sends in the clear) and then the group id
/// (RFC 9420, 6 and 6.2), which DAVE makes a big-endian u64.
fn message_group(message: &[u8]) -> Option<u64> {
    let (version, rest) = message.split_first_chunk::<2>()?;
    let (format, rest) = rest.split_first_chunk::<2>()?;
    if u16::from_be_bytes(*version) != 1 || u16::from_be_bytes(*format) != 1 {
        return None;
    }
    let (group, _) = vector(rest)?;
    Some(u64::from_be_bytes(group.try_into().ok()?))
}

/// An MLS variable-length vector (RFC 9420, 2.1.2): its contents and what follows.
fn vector(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let first = *bytes.first()?;
    let prefix = 1usize << (first >> 6);
    if prefix > 4 || bytes.len() < prefix {
        return None;
    }
    let length = bytes[1..prefix]
        .iter()
        .fold(usize::from(first & 0x3f), |length, &byte| {
            length << 8 | usize::from(byte)
        });
    let rest = &bytes[prefix..];
    (rest.len() >= length).then(|| rest.split_at(length))
}

fn ready(transition_id: u16, protocol_version: u16) -> Out {
    Out::Json(Event::from(DaveTransitionReady {
        transition_id,
        protocol_version,
    }))
}

#[cfg(test)]
mod tests {
    use serenity_voice_model::payload::{
        DaveExecuteTransition, DaveMlsAnnounceCommitTransition, DaveMlsExternalSender, DaveMlsWelcome,
        DavePrepareTransition,
    };

    use super::*;

    #[test]
    fn starts_with_a_key_package_and_passes_through_without_dave() {
        let mut dave = Dave::new(1, 2);
        assert!(dave.start(0).is_empty());
        assert_eq!(
            dave.encrypt_opus(b"opus").unwrap(),
            b"opus",
            "transport encryption only"
        );
        let marked = [&[0u8; 9][..], &[0xfa, 0xfa]].concat();
        assert_eq!(dave.decrypt_opus(2, marked.clone()), Some(marked.clone()));
        let out = dave.start(1);
        assert!(matches!(out.as_slice(), [Out::Binary(Event::DaveMlsKeyPackage(_))]));
        // No group yet: nothing goes out in the clear, and encrypted ones wait.
        assert_eq!(dave.encrypt_opus(b"opus"), None);
        assert_eq!(dave.decrypt_opus(2, b"plain".to_vec()), Some(b"plain".to_vec()));
        assert_eq!(dave.decrypt_opus(2, marked), None);
        assert!(!dave.encrypting());
    }

    #[test]
    fn answers_transitions() {
        let mut dave = Dave::new(1, 2);
        dave.start(1);
        let out = dave.handle(Event::from(DavePrepareTransition {
            protocol_version: 0,
            transition_id: 5,
        }));
        assert!(matches!(out.as_slice(), [Out::Json(Event::DaveTransitionReady(r))] if r.transition_id == 5));
        dave.handle(Event::from(DaveExecuteTransition { transition_id: 5 }));
        assert_eq!(dave.version, 0);
    }

    /// The voice server's side of DAVE: the external sender that proposes
    /// people into groups.
    struct Server {
        key: openmls_basic_credential::SignatureKeyPair,
    }

    impl Server {
        fn new() -> Self {
            let key = openmls_basic_credential::SignatureKeyPair::new(
                openmls::prelude::SignatureScheme::ECDSA_SECP256R1_SHA256,
            )
            .unwrap();
            Self { key }
        }

        /// What op 25 carries.
        fn external_sender(&self) -> Vec<u8> {
            use openmls::prelude::{BasicCredential, ExternalSender, tls_codec::Serialize};
            ExternalSender::new(
                self.key.public().to_vec().into(),
                BasicCredential::new(vec![0; 8]).into(),
            )
            .tls_serialize_detached()
            .unwrap()
        }

        /// What op 27 carries to add the owner of a key package to `group`.
        fn propose_add(&self, key_package: &[u8], group: u64) -> Vec<u8> {
            use openmls::prelude::{
                ExternalProposal, GroupEpoch, GroupId, KeyPackageIn, ProtocolVersion, SenderExtensionIndex,
                tls_codec::{Deserialize, Serialize, VLBytes},
            };
            use openmls_rust_crypto::OpenMlsRustCrypto;
            let provider = OpenMlsRustCrypto::default();
            let key_package = KeyPackageIn::tls_deserialize_exact(key_package)
                .unwrap()
                .validate(
                    openmls::prelude::OpenMlsProvider::crypto(&provider),
                    ProtocolVersion::Mls10,
                )
                .unwrap();
            let message = ExternalProposal::new_add::<OpenMlsRustCrypto>(
                key_package,
                GroupId::from_slice(&group.to_be_bytes()),
                GroupEpoch::from(0),
                &self.key,
                SenderExtensionIndex::new(0),
            )
            .unwrap()
            .tls_serialize_detached()
            .unwrap();
            VLBytes::new(message).tls_serialize_detached().unwrap()
        }
    }

    fn key_package(out: &[Out]) -> Vec<u8> {
        out.iter()
            .find_map(|out| match out {
                Out::Binary(Event::DaveMlsKeyPackage(package)) => Some(package.key_package.clone()),
                _ => None,
            })
            .expect("a key package")
    }

    /// Someone already in a call commits our addition, and the voice server
    /// announces that commit before our welcome arrives. We must wait for
    /// the welcome rather than refuse the commit and start over, then hear
    /// and be heard, end-to-end encrypted.
    #[test]
    fn joins_a_call_past_an_early_commit_and_encrypts_both_ways() {
        let (friend, me, channel) = (10, 20, 77);
        let server = Server::new();
        let sender = DaveMlsExternalSender {
            external_sender: server.external_sender(),
        };
        let mut other = DaveSession::new(1.try_into().unwrap(), friend, channel, None).unwrap();
        other.set_external_sender(&sender.external_sender).unwrap();
        other.create_key_package().unwrap();

        let mut dave = Dave::new(me, channel);
        dave.recognise(friend);
        let package = key_package(&dave.start(1));
        dave.handle(Event::from(sender));
        let commit = other
            .process_proposals(
                ProposalsOperationType::APPEND,
                &server.propose_add(&package, channel),
                None,
            )
            .unwrap()
            .expect("the friend commits our addition");

        let out = dave.handle(Event::from(DaveMlsAnnounceCommitTransition {
            transition_id: 3,
            commit_message: commit.commit.clone(),
        }));
        assert!(out.is_empty(), "no refusal, no new key package: {out:?}");
        let out = dave.handle(Event::from(DaveMlsWelcome {
            transition_id: 3,
            welcome: commit.welcome.expect("a welcome for us"),
        }));
        assert!(
            matches!(out.as_slice(), [Out::Json(Event::DaveTransitionReady(r))] if r.transition_id == 3),
            "{out:?}"
        );
        dave.handle(Event::from(DaveExecuteTransition { transition_id: 3 }));
        other.process_commit(&commit.commit).unwrap();
        assert!(dave.encrypting());
        assert_eq!(
            dave.privacy_code(),
            other.voice_privacy_code(),
            "both ends show the same code"
        );

        let sealed = other.encrypt_opus(b"friend's voice").unwrap().into_owned();
        assert_eq!(dave.decrypt_opus(friend, sealed), Some(b"friend's voice".to_vec()));
        assert_eq!(
            dave.decrypt_opus(friend, b"the server's voice".to_vec()),
            None,
            "an unencrypted frame in an encrypted call"
        );
        let sealed = dave.encrypt_opus(b"my voice").unwrap();
        assert!(sealed.ends_with(&[0xfa, 0xfa]), "end-to-end encrypted");
        assert_eq!(other.decrypt(me, MediaType::AUDIO, &sealed).unwrap(), b"my voice");

        // A camera's H264 keyframe, as the assembler puts it together: the
        // NAL headers stay in the clear, the rest is sealed.
        let frame = [
            0, 0, 0, 1, 0x67, 0x42, 0, 0x1f, 0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0xa0,
        ];
        let sealed = other
            .encrypt(MediaType::VIDEO, davey::Codec::H264, &frame)
            .unwrap()
            .into_owned();
        assert_ne!(sealed, frame);
        assert_eq!(dave.decrypt_video(friend, sealed), Some(frame.to_vec()));
        assert_eq!(
            dave.decrypt_video(friend, frame.to_vec()),
            None,
            "unencrypted video in an encrypted call"
        );

        // Our camera's VP8 keyframe goes out sealed, and the friend reads it:
        // its first ten bytes (tag, start code, size) stay clear, as DAVE's
        // VP8 rule has it.
        let mut keyframe = vec![0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x00, 0x24, 0x00];
        keyframe.extend_from_slice(&[0x55; 40]);
        let sealed = dave
            .encrypt_video(super::super::depacketize::Codec::Vp8, &keyframe)
            .unwrap();
        assert!(sealed.ends_with(&[0xfa, 0xfa]), "end-to-end encrypted");
        assert_eq!(sealed[..10], keyframe[..10]);
        assert_eq!(other.decrypt(me, MediaType::VIDEO, &sealed).unwrap(), keyframe);
    }

    #[test]
    fn reads_the_group_an_mls_message_names() {
        let group = 1_234_567u64.to_be_bytes();
        // Version 1, public message, an 8-byte group id, then the rest.
        let mut message = vec![0, 1, 0, 1, 8];
        message.extend_from_slice(&group);
        message.extend_from_slice(&[0, 0, 0, 3]);
        assert_eq!(message_group(&message), Some(1_234_567));
        // A proposals message wraps them in a vector of its own (here with
        // a two-byte length).
        let mut proposals = vec![0x40, message.len() as u8];
        proposals.extend_from_slice(&message);
        assert_eq!(proposals_group(&proposals), Some(1_234_567));
        assert_eq!(message_group(&[0, 1, 0, 2, 8]), None, "not a public message");
        assert_eq!(proposals_group(&[0x40, 200, 1]), None, "too short");
        let mut dave = Dave::new(1, 50);
        dave.follow(Some(51));
        assert_eq!(dave.channel_id, 51);
    }

    #[test]
    fn video_passes_through_without_dave() {
        let mut dave = Dave::new(1, 50);
        dave.start(0);
        assert_eq!(
            dave.decrypt_video(2, vec![0, 0, 0, 1, 0x65]),
            Some(vec![0, 0, 0, 1, 0x65])
        );
    }

    #[test]
    fn a_failed_welcome_is_refused_and_starts_over() {
        let mut dave = Dave::new(1, 2);
        dave.start(1);
        let out = dave.handle(Event::from(DaveMlsWelcome {
            transition_id: 4,
            welcome: vec![1, 2, 3],
        }));
        assert!(matches!(
            out.as_slice(),
            [Out::Json(Event::DaveMlsInvalidCommitWelcome(w)), Out::Binary(Event::DaveMlsKeyPackage(_))]
                if w.transition_id == 4
        ));
    }
}

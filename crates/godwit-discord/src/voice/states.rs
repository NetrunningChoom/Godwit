// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Who is in which voice channel or DM call, from the main gateway's voice
//! states: READY's and GUILD_CREATE's lists, VOICE_STATE_UPDATE, the calls
//! in DMs (CALL_CREATE/UPDATE/DELETE), and the passive updates large servers
//! send (PASSIVE_UPDATE_V1/V2), as Yapfast's tracker reads them.

use std::collections::{BTreeSet, HashMap};

use serde_json::Value;

/// One person's voice state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceState {
    pub user: String,
    pub channel: String,
    /// Which of their gateway sessions is in voice.
    pub session: Option<String>,
    pub self_mute: bool,
    pub self_deaf: bool,
    /// Muted or deafened by the server.
    pub mute: bool,
    pub deaf: bool,
    /// Sharing their screen (Go Live), or their camera.
    pub self_stream: bool,
    pub self_video: bool,
    /// In a stage: not allowed to speak (the audience).
    pub suppress: bool,
    /// When they asked to speak (an ISO 8601 time), if their hand is up.
    pub request_to_speak: Option<String>,
}

impl VoiceState {
    fn from(state: &Value) -> Option<Self> {
        let flag = |name: &str| state[name].as_bool().unwrap_or(false);
        Some(Self {
            user: state["user_id"].as_str()?.to_owned(),
            channel: state["channel_id"].as_str()?.to_owned(),
            session: state["session_id"].as_str().map(str::to_owned),
            self_mute: flag("self_mute"),
            self_deaf: flag("self_deaf"),
            mute: flag("mute"),
            deaf: flag("deaf"),
            self_stream: flag("self_stream"),
            self_video: flag("self_video"),
            suppress: flag("suppress"),
            request_to_speak: state["request_to_speak_timestamp"].as_str().map(str::to_owned),
        })
    }

    /// Their part on a stage: a speaker isn't suppressed; the audience is,
    /// with a hand up once they asked (docs.discord.food, Stage Instance:
    /// a speaker has no request_to_speak_timestamp, suppress false).
    pub fn stage_role(&self) -> godwit_core::call::StageRole {
        use godwit_core::call::StageRole;
        if !self.suppress {
            StageRole::Speaker
        } else if self.request_to_speak.is_some() {
            StageRole::HandRaised
        } else {
            StageRole::Audience
        }
    }

    pub fn muted(&self) -> bool {
        self.self_mute || self.mute
    }

    pub fn deafened(&self) -> bool {
        self.self_deaf || self.deaf
    }
}

/// Voice states by server ("" for DM calls) and user: one each, as Discord
/// keeps them.
#[derive(Default)]
pub struct VoiceStates {
    states: HashMap<(String, String), VoiceState>,
}

/// Channels whose occupants changed.
pub type Changed = BTreeSet<String>;

fn scope(guild: Option<&str>) -> String {
    guild.unwrap_or_default().to_owned()
}

impl VoiceStates {
    /// Everything from READY: each server's voice states.
    pub fn ready(&mut self, d: &Value) -> Changed {
        let mut changed: Changed = self.states.values().map(|s| s.channel.clone()).collect();
        self.states.clear();
        for guild in d["guilds"].as_array().into_iter().flatten() {
            if let Some(id) = guild["id"].as_str() {
                changed.extend(self.replace_guild(id, &guild["voice_states"]));
            }
        }
        changed
    }

    /// A server's whole list (GUILD_CREATE, PASSIVE_UPDATE_V1).
    pub fn replace_guild(&mut self, guild: &str, states: &Value) -> Changed {
        let mut changed = Changed::new();
        self.states.retain(|(g, _), s| {
            let keep = g != guild;
            if !keep {
                changed.insert(s.channel.clone());
            }
            keep
        });
        for state in states.as_array().into_iter().flatten() {
            changed.extend(self.apply(Some(guild), state));
        }
        changed
    }

    /// READY_SUPPLEMENTAL has the initial voice snapshot with prioritized
    /// READY. An omitted list is not an empty snapshot.
    pub fn supplemental(&mut self, data: &Value) -> Changed {
        let mut changed = Changed::new();
        for guild in data["guilds"].as_array().into_iter().flatten() {
            if let Some(id) = guild["id"].as_str()
                && guild["voice_states"].is_array()
            {
                changed.extend(self.replace_guild(id, &guild["voice_states"]));
            }
        }
        changed
    }

    /// One voice state (VOICE_STATE_UPDATE and the lists): a channel moves
    /// them there, none takes them out of voice.
    pub fn apply(&mut self, guild: Option<&str>, state: &Value) -> Changed {
        let mut changed = Changed::new();
        let Some(user) = state["user_id"].as_str() else {
            return changed;
        };
        let key = (scope(guild), user.to_owned());
        let old = self.states.remove(&key);
        if let Some(old) = &old {
            changed.insert(old.channel.clone());
        }
        if let Some(new) = VoiceState::from(state) {
            // Only a change of who's where, how they're muted, or what
            // they share, counts.
            if old.as_ref() == Some(&new) {
                changed.clear();
            } else {
                changed.insert(new.channel.clone());
            }
            self.states.insert(key, new);
        }
        changed
    }

    /// Takes someone out (PASSIVE_UPDATE_V2's `removed_voice_states`).
    pub fn remove(&mut self, guild: Option<&str>, user: &str) -> Changed {
        self.states
            .remove(&(scope(guild), user.to_owned()))
            .map(|s| s.channel)
            .into_iter()
            .collect()
    }

    /// A DM call ended (CALL_DELETE): nobody's in it any more.
    pub fn end_call(&mut self, channel: &str) -> Changed {
        let before = self.states.len();
        self.states.retain(|(g, _), s| !(g.is_empty() && s.channel == channel));
        if before == self.states.len() {
            Changed::new()
        } else {
            Changed::from([channel.to_owned()])
        }
    }

    /// Who's in `channel`, in a steady order.
    pub fn in_channel(&self, channel: &str) -> Vec<&VoiceState> {
        let mut states: Vec<&VoiceState> = self.states.values().filter(|s| s.channel == channel).collect();
        states.sort_by(|a, b| a.user.cmp(&b.user));
        states
    }

    /// `user`'s voice state in `guild` (None: DM calls).
    pub fn of(&self, guild: Option<&str>, user: &str) -> Option<&VoiceState> {
        self.states.get(&(scope(guild), user.to_owned()))
    }

    /// Every channel someone is in.
    pub fn channels(&self) -> Changed {
        self.states.values().map(|s| s.channel.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn stage_roles_follow_suppress_and_the_raised_hand() {
        use godwit_core::call::StageRole;
        let role = |v: serde_json::Value| VoiceState::from(&v).unwrap().stage_role();
        let base = json!({"user_id": "1", "channel_id": "2"});
        assert_eq!(role(base.clone()), StageRole::Speaker);
        let mut audience = base.clone();
        audience["suppress"] = json!(true);
        assert_eq!(role(audience.clone()), StageRole::Audience);
        audience["request_to_speak_timestamp"] = json!("2026-01-01T00:00:00+00:00");
        assert_eq!(role(audience), StageRole::HandRaised);
    }

    #[test]
    fn follows_people_between_channels_and_out() {
        let mut states = VoiceStates::default();
        let changed = states.ready(&json!({"guilds": [{"id": "10", "voice_states": [
            {"user_id": "1", "channel_id": "11", "session_id": "a", "self_mute": true},
            {"user_id": "2", "channel_id": "11"},
        ]}]}));
        assert_eq!(changed, Changed::from(["11".to_owned()]));
        assert_eq!(states.in_channel("11").len(), 2);
        assert!(states.of(Some("10"), "1").unwrap().muted());

        // Moving: both channels change.
        let changed = states.apply(
            Some("10"),
            &json!({"user_id": "1", "channel_id": "12", "self_mute": true, "session_id": "a"}),
        );
        assert_eq!(changed, Changed::from(["11".to_owned(), "12".to_owned()]));
        // The same state again changes nothing.
        let same = json!({"user_id": "1", "channel_id": "12", "self_mute": true, "session_id": "a"});
        assert!(states.apply(Some("10"), &same).is_empty());
        // Leaving: a null channel.
        let changed = states.apply(Some("10"), &json!({"user_id": "1", "channel_id": null}));
        assert_eq!(changed, Changed::from(["12".to_owned()]));
        assert!(states.of(Some("10"), "1").is_none());
        assert_eq!(states.remove(Some("10"), "2"), Changed::from(["11".to_owned()]));
        assert!(states.channels().is_empty());
    }

    #[test]
    fn dm_calls_and_server_lists() {
        let mut states = VoiceStates::default();
        states.apply(None, &json!({"user_id": "1", "channel_id": "20", "self_deaf": true}));
        states.apply(Some("10"), &json!({"user_id": "1", "channel_id": "11"}));
        assert!(states.of(None, "1").unwrap().deafened());
        assert_eq!(states.end_call("20"), Changed::from(["20".to_owned()]));
        assert!(states.end_call("20").is_empty());
        // A server's list replaces what was known of it.
        let changed = states.replace_guild("10", &json!([{"user_id": "3", "channel_id": "13"}]));
        assert_eq!(changed, Changed::from(["11".to_owned(), "13".to_owned()]));
        assert_eq!(states.in_channel("13")[0].user, "3");
    }

    #[test]
    fn supplemental_omitted_snapshots_preserve_voice_and_empty_ones_clear_it() {
        let mut states = VoiceStates::default();
        states.apply(Some("10"), &json!({"user_id":"1","channel_id":"11"}));
        assert!(states.supplemental(&json!({"guilds":[{"id":"10"}]})).is_empty());
        assert_eq!(states.in_channel("11").len(), 1);
        assert_eq!(
            states.supplemental(&json!({"guilds":[{"id":"10","voice_states":[]}]})),
            Changed::from(["11".into()])
        );
        assert!(states.in_channel("11").is_empty());
    }
}

// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Managing a channel's webhooks from its settings page: list, create,
//! rename, delete and copy the URL (docs.discord.food/resources/webhook).
//! Changes go out as `Request::Admin`, so a server that asks for a 2FA code
//! gets it asked like the other admin requests.

use super::*;
use crate::backend::{Admin, RequestError};
use crate::permissions as p;
use crate::webhooks::{Hook, HookToken};
use godwit_core::webhooks::{Webhook, WebhookChange, WebhookUrl};

/// Webhook names are at most 80 characters (Discord's limit); the core's own
/// gate is tighter.
const NAME_MAX: usize = 80;

fn is_snowflake(id: &str) -> bool {
    !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())
}

fn core_list(hooks: &[Hook]) -> Vec<Webhook> {
    hooks
        .iter()
        .map(|h| Webhook {
            id: h.id.as_str().into(),
            name: h.name.as_str().into(),
            avatar_url: h.avatar_url.as_deref().map(Into::into),
        })
        .collect()
}

impl DiscordAccount {
    /// The server channel whose webhooks we may manage, and its server.
    fn webhook_channel(&self, room: &RoomId) -> Result<(String, String), &'static str> {
        if self.account.as_ref() != Some(&room.account) {
            return Err("This channel belongs to another account.");
        }
        let Some(c) = self.store.channels.get(room.raw.as_ref()) else {
            return Err("This channel is no longer available.");
        };
        let Some(guild) = c.guild.clone() else {
            return Err("Only server channels have webhooks.");
        };
        if c.thread() || c.kind == store::GUILD_CATEGORY || !self.store.can(&c.id, p::MANAGE_WEBHOOKS) {
            return Err("You need the Manage Webhooks permission for this.");
        }
        Ok((c.id.clone(), guild))
    }

    pub(super) fn webhook_command(&mut self, command: Command) -> Vec<Event> {
        let (room, change) = match command {
            Command::LoadWebhooks { room } => (room, None),
            Command::ChangeWebhook { room, change } => (room, Some(change)),
            _ => return Vec::new(),
        };
        let failed = |room: RoomId, e: &str| {
            vec![Event::Webhooks {
                room,
                result: Err(e.into()),
            }]
        };
        let (channel, guild) = match self.webhook_channel(&room) {
            Ok(target) => target,
            Err(e) => return failed(room, e),
        };
        let op = match change {
            None => Admin::Webhooks {
                channel: channel.clone(),
            },
            Some(WebhookChange::Create { name }) => {
                let name = name.trim();
                if name.is_empty() || name.chars().count() > NAME_MAX {
                    return failed(room, "A webhook's name is 1 to 80 characters.");
                }
                Admin::CreateWebhook {
                    channel: channel.clone(),
                    name: name.to_owned(),
                }
            }
            Some(WebhookChange::Rename { id, name }) => {
                let name = name.trim();
                if name.is_empty() || name.chars().count() > NAME_MAX || !is_snowflake(&id) {
                    return failed(room, "A webhook's name is 1 to 80 characters.");
                }
                Admin::RenameWebhook {
                    channel: channel.clone(),
                    id: id.to_string(),
                    name: name.to_owned(),
                }
            }
            Some(WebhookChange::Delete { id }) => {
                if !is_snowflake(&id) {
                    return failed(room, "That webhook is no longer available.");
                }
                Admin::DeleteWebhook {
                    channel: channel.clone(),
                    id: id.to_string(),
                }
            }
            Some(WebhookChange::CopyUrl { id }) => {
                let url = self
                    .webhooks
                    .as_ref()
                    .filter(|(open, _)| *open == channel)
                    .and_then(|(_, list)| list.iter().find(|h| *h.id == *id))
                    .and_then(Hook::url);
                return vec![Event::WebhookUrl {
                    room,
                    result: url
                        .map(WebhookUrl::new)
                        .ok_or_else(|| "This webhook has no URL to copy.".into()),
                }];
            }
        };
        if self.demo.is_some() {
            return self.demo_webhooks(room, channel, op);
        }
        self.net.send(Cmd::Request(Request::Admin { guild, op }));
        Vec::new()
    }

    /// The list (also after every change) for the open page; the tokens stay
    /// here for copying and the core gets none.
    pub(super) fn webhook_list(&mut self, channel: String, list: Vec<Hook>, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        let room = RoomId::new(account, channel.as_str());
        // A list for a channel we may no longer manage is dropped unread.
        if self.webhook_channel(&room).is_err() {
            return;
        }
        let shown = core_list(&list);
        self.webhooks = Some((channel, list));
        out.push(Event::Webhooks {
            room,
            result: Ok(shown),
        });
    }

    pub(super) fn webhook_failed(&mut self, channel: &str, error: RequestError, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        out.push(Event::Webhooks {
            room: RoomId::new(account, channel),
            result: Err(error.into()),
        });
    }

    /// Demo mode keeps the list here and answers as Discord would.
    fn demo_webhooks(&mut self, room: RoomId, channel: String, op: Admin) -> Vec<Event> {
        let mut list = match self.webhooks.take() {
            Some((open, list)) if open == channel => list,
            _ => vec![demo_hook("9001", "Announcements"), demo_hook("9002", "Builds")],
        };
        match op {
            Admin::CreateWebhook { name, .. } => {
                let id = format!("{}", 9100 + list.len());
                list.push(demo_hook(&id, &name));
            }
            Admin::RenameWebhook { id, name, .. } => {
                if let Some(h) = list.iter_mut().find(|h| h.id == id) {
                    h.name = name;
                }
            }
            Admin::DeleteWebhook { id, .. } => list.retain(|h| h.id != id),
            _ => {}
        }
        let shown = core_list(&list);
        self.webhooks = Some((channel, list));
        vec![Event::Webhooks {
            room,
            result: Ok(shown),
        }]
    }
}

fn demo_hook(id: &str, name: &str) -> Hook {
    Hook {
        id: id.to_owned(),
        name: name.to_owned(),
        avatar_url: None,
        token: Some(HookToken::new("demo-token")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::AdminAnswer;

    fn staffed(perms: u64) -> (DiscordAccount, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (net, mut cmds) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut a = DiscordAccount::with(u32::MAX, net, events, Status::Connecting);
        let mut ready = crate::store::test_ready();
        ready["guilds"][0]["roles"][1]["permissions"] = json!(perms.to_string());
        a.handle(Ev::Ready(Box::new(ready)), &mut Vec::new());
        while cmds.try_recv().is_ok() {}
        (a, cmds)
    }

    fn room(a: &DiscordAccount, id: &str) -> RoomId {
        RoomId::new(a.account.clone().unwrap(), id)
    }

    fn sent(cmds: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>) -> Option<(String, Admin)> {
        match cmds.try_recv() {
            Ok(Cmd::Request(Request::Admin { guild, op })) => Some((guild, op)),
            _ => None,
        }
    }

    fn listed(a: &mut DiscordAccount, channel: &str) -> Vec<Event> {
        let list = crate::webhooks::parse_list(&json!([
            {"id": "501", "type": 1, "name": "Deploys", "token": "tok"},
            {"id": "502", "type": 2, "name": "Follow"},
        ]));
        let mut out = Vec::new();
        a.admin_answer(
            "10",
            Ok(AdminAnswer::Webhooks {
                channel: channel.into(),
                list,
            }),
            &mut out,
        );
        out
    }

    #[test]
    fn the_capability_needs_manage_webhooks_on_a_server_channel() {
        let (a, _) = staffed(p::VIEW_CHANNEL | p::MANAGE_WEBHOOKS);
        let with = |id: &str| a.room(id).is_some_and(|r| r.capabilities.manage_webhooks);
        assert!(with("13"));
        assert!(!with("12"), "a category has none");
        assert!(!with("20"), "a DM has none");
        let (a, _) = staffed(p::VIEW_CHANNEL);
        assert!(!a.room("13").is_some_and(|r| r.capabilities.manage_webhooks));
    }

    #[test]
    fn requests_are_gated_and_go_out_as_admin_requests() {
        let (mut a, mut cmds) = staffed(p::VIEW_CHANNEL | p::MANAGE_WEBHOOKS);
        let r = room(&a, "13");
        assert!(a.run(Command::LoadWebhooks { room: r.clone() }).is_empty());
        assert!(matches!(sent(&mut cmds), Some((g, Admin::Webhooks { channel })) if g == "10" && channel == "13"));
        let change = |change| Command::ChangeWebhook {
            room: r.clone(),
            change,
        };
        a.run(change(WebhookChange::Create { name: " Hook ".into() }));
        assert!(matches!(sent(&mut cmds), Some((_, Admin::CreateWebhook { name, .. })) if name == "Hook"));
        a.run(change(WebhookChange::Rename {
            id: "501".into(),
            name: "New".into(),
        }));
        assert!(
            matches!(sent(&mut cmds), Some((_, Admin::RenameWebhook { id, name, .. })) if id == "501" && name == "New")
        );
        a.run(change(WebhookChange::Delete { id: "501".into() }));
        assert!(matches!(sent(&mut cmds), Some((_, Admin::DeleteWebhook { id, .. })) if id == "501"));
        // An id that is not a snowflake never reaches a URL.
        let out = a.run(change(WebhookChange::Delete {
            id: "../guilds/1".into(),
        }));
        assert!(matches!(out.as_slice(), [Event::Webhooks { result: Err(_), .. }]) && sent(&mut cmds).is_none());
    }

    #[test]
    fn without_the_permission_nothing_is_sent() {
        let (mut a, mut cmds) = staffed(p::VIEW_CHANNEL | p::MANAGE_GUILD);
        let out = a.run(Command::LoadWebhooks { room: room(&a, "13") });
        assert!(matches!(out.as_slice(), [Event::Webhooks { result: Err(_), .. }]));
        assert!(sent(&mut cmds).is_none());
    }

    #[test]
    fn a_list_answer_reaches_the_core_without_tokens_and_the_url_is_built_on_copy() {
        let (mut a, _cmds) = staffed(p::VIEW_CHANNEL | p::MANAGE_WEBHOOKS);
        let out = listed(&mut a, "13");
        let [Event::Webhooks { result: Ok(list), .. }] = out.as_slice() else {
            panic!("expected the list, got {out:?}");
        };
        assert_eq!(list.len(), 2);
        assert!(!format!("{out:?}").contains("tok"));
        let r = room(&a, "13");
        let copy = |a: &mut DiscordAccount, id: &str| {
            a.run(Command::ChangeWebhook {
                room: r.clone(),
                change: WebhookChange::CopyUrl { id: id.into() },
            })
        };
        let out = copy(&mut a, "501");
        assert!(matches!(
            out.as_slice(),
            [Event::WebhookUrl { result: Ok(u), .. }] if u.as_str() == "https://discord.com/api/webhooks/501/tok"
        ));
        assert!(!format!("{out:?}").contains("tok"));
        // A follower has no token, so no URL.
        assert!(matches!(
            copy(&mut a, "502").as_slice(),
            [Event::WebhookUrl { result: Err(_), .. }]
        ));
    }

    #[test]
    fn a_failed_request_ends_the_wait_with_its_reason() {
        let (mut a, _cmds) = staffed(p::VIEW_CHANNEL | p::MANAGE_WEBHOOKS);
        let mut out = Vec::new();
        a.admin_answer(
            "10",
            Ok(AdminAnswer::WebhooksFailed {
                channel: "13".into(),
                error: RequestError::from(anyhow::anyhow!("Missing Permissions")),
            }),
            &mut out,
        );
        assert!(matches!(out.as_slice(), [Event::Webhooks { result: Err(e), .. }] if e.contains("Missing")));
    }
}

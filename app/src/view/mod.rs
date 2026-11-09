// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Turns the core's state into the rows the Slint UI shows. Pure functions
//! plus [`sync`], which updates a model in place so lists keep their scroll
//! position.

mod calls;
mod common;
mod people;
mod settings;
mod sidebar;
mod timeline;
pub use calls::*;
use common::NO_OCCUPANTS;
pub(crate) use common::picture;
pub use common::*;
pub use people::*;
use people::{presence_label, status_rank};
pub use settings::*;
pub use sidebar::*;
use timeline::block_item;
pub use timeline::*;

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Local, TimeZone};
use godwit_core::application::{AppState, Person, Verify};
use godwit_core::call::{CallMember, CallStatus};
use godwit_core::contacts::{ContactId, Metacontact};
use godwit_core::ids::{AccountId, MessageId, RoomId, ServerId, UserId};
use godwit_core::members::{MemberGroup, Presence, Profile, Relationship};
use godwit_core::message::{Attachment, AttachmentKind, Embed, EmbedStyle, Invite, Markup, Message};
use godwit_core::network::{Encryption, MetadataExposure, Network, ThirdPartyClients};
use godwit_core::rich;
use godwit_core::room::{Room, RoomEncryption, RoomKind};
use godwit_core::server::{DirectoryRoom, Server};
use slint::{Image, Model, ModelRc, SharedString, StyledText, VecModel};

use crate::code;
use crate::images::{AVATAR, BANNER, CAPSULE, EMBED_ICON, EMOJI, Fit, Images, JUMBO, PREVIEW, REPLY_AVATAR, THUMBNAIL};
use crate::inline_player::{InlinePlayers, Kind as PlayerKind, Spec as PlayerSpec};
use crate::ui::{
    AccountItem, AppWindow, AttachmentItem, BlockItem, CallMemberItem, ComponentItem, ComponentRowItem, ContactItem,
    DirectoryItem, EmbedItem, EmojiCellItem, IdentityItem, InviteItem, MemberItem, MemberRoleItem, MergeItem,
    MessageItem, MutualItem, NetworkItem, PersonItem, PieceItem, ReactionItem, RequestItem, RoleItem, RoomItem,
    ServerItem, SwitchItem, VerifyEmojiItem,
};

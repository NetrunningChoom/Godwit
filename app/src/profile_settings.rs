// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Profile form drafts belong to the UI; loaded values and save results
//! belong to the core. A refresh must never replace an unsaved draft.

use godwit_core::application::{Action, AppState};
use godwit_core::ids::AccountId;
use godwit_core::own_profile::{OwnProfile, ProfileEdit, ProfileImage};
use slint::{ModelRc, SharedString, VecModel};

use crate::images::{Fit, Images};
use crate::ui::AppWindow;

#[derive(Default)]
pub struct ProfileEditor {
    accounts: Vec<AccountId>,
    selected: Option<AccountId>,
    original: Option<OwnProfile>,
    avatar: ProfileImage,
    banner: ProfileImage,
    dirty: bool,
    saving: bool,
    error: Option<String>,
}

impl ProfileEditor {
    pub fn populate(&mut self, ui: &AppWindow, state: &AppState, accounts: Vec<AccountId>) {
        if self.selected.as_ref().is_none_or(|id| !accounts.contains(id)) {
            self.selected = accounts.first().cloned();
            self.clear();
        }
        let labels: Vec<SharedString> = accounts
            .iter()
            .filter_map(|id| {
                state
                    .accounts
                    .iter()
                    .find(|a| &a.id == id)
                    .map(|a| format!("{} — {}", a.display_name, id.network.name()).into())
            })
            .collect();
        ui.set_edit_profile_accounts(ModelRc::new(VecModel::from(labels)));
        ui.set_edit_profile_index(
            self.selected
                .as_ref()
                .and_then(|id| accounts.iter().position(|a| a == id))
                .unwrap_or(0) as i32,
        );
        self.accounts = accounts;
    }

    /// Which row of the editor's account list `id` is (None: not editable here).
    pub fn row_of(&self, id: &AccountId) -> Option<usize> {
        self.accounts.iter().position(|a| a == id)
    }

    pub fn account(&self) -> Option<AccountId> {
        self.selected.clone()
    }

    fn clear(&mut self) {
        self.original = None;
        self.avatar = ProfileImage::Keep;
        self.banner = ProfileImage::Keep;
        self.dirty = false;
        self.saving = false;
        self.error = None;
    }

    pub fn select(&mut self, ui: &AppWindow, row: i32) -> Option<Action> {
        let account = usize::try_from(row).ok().and_then(|i| self.accounts.get(i))?.clone();
        if self.selected.as_ref() == Some(&account) && self.original.is_some() {
            return None;
        }
        self.selected = Some(account.clone());
        self.clear();
        ui.set_edit_profile_index(row);
        ui.set_edit_profile_banner_allowed(account.network.has_profile_banner());
        Some(Action::LoadOwnProfile(account))
    }

    pub fn reload(&mut self) -> Option<Action> {
        let account = self.selected.clone()?;
        self.clear();
        Some(Action::LoadOwnProfile(account))
    }

    fn copy_profile(&mut self, ui: &AppWindow, profile: &OwnProfile) {
        ui.set_edit_profile_name(profile.display_name.as_ref().into());
        ui.set_edit_profile_pronouns(profile.pronouns.as_ref().into());
        ui.set_edit_profile_bio(profile.bio.as_ref().into());
        self.clear();
        self.original = Some(profile.clone());
    }

    pub fn reset(&mut self, ui: &AppWindow, state: &AppState) {
        if let Some(profile) = self
            .selected
            .as_ref()
            .and_then(|id| state.own_profiles.get(id))
            .and_then(|s| s.profile.as_ref())
        {
            self.copy_profile(ui, profile);
        }
    }

    pub fn edited(&mut self, ui: &AppWindow) {
        self.dirty = true;
        self.error = None;
        ui.set_edit_profile_status("".into());
        ui.set_edit_profile_failed(false);
    }

    pub fn remove_image(&mut self, ui: &AppWindow, avatar: bool) {
        if ui.get_edit_profile_busy() {
            return;
        }
        if avatar {
            self.avatar = ProfileImage::Remove;
        } else {
            self.banner = ProfileImage::Remove;
        }
        self.edited(ui);
    }

    pub fn picked(&mut self, ui: &AppWindow, account: &AccountId, avatar: bool, result: Result<ProfileImage, String>) {
        // A delayed picker must not attach an image to a different account.
        if self.selected.as_ref() != Some(account) || ui.get_edit_profile_busy() {
            return;
        }
        match result {
            Ok(image) => {
                if avatar {
                    self.avatar = image;
                } else {
                    self.banner = image;
                }
                self.edited(ui);
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub fn save(&mut self, ui: &AppWindow) -> Option<Action> {
        if ui.get_edit_profile_busy() {
            return None;
        }
        let original = self.original.as_ref()?;
        let name = ui.get_edit_profile_name();
        let pronouns = ui.get_edit_profile_pronouns();
        let bio = ui.get_edit_profile_bio();
        let edit = ProfileEdit {
            display_name: (name.trim() != original.display_name.trim()).then(|| name.trim().into()),
            pronouns: (pronouns.trim() != original.pronouns.trim()).then(|| pronouns.trim().into()),
            bio: (bio.as_str() != original.bio.as_ref()).then(|| bio.as_str().into()),
            avatar: self.avatar.clone(),
            banner: self.banner.clone(),
        };
        self.saving = true;
        self.error = None;
        Some(Action::SaveOwnProfile {
            account: self.selected.clone()?,
            edit,
        })
    }

    pub fn show(&mut self, ui: &AppWindow, state: &AppState, images: &mut Images) {
        let current = self.selected.as_ref().and_then(|id| state.own_profiles.get(id));
        let profile = current.and_then(|state| state.profile.as_ref());
        if let Some(profile) = profile
            && current.is_some_and(|s| !s.loading)
            && (self.original.is_none() || (self.saving && current.is_some_and(|s| s.saved)))
        {
            self.copy_profile(ui, profile);
        }
        if current.is_some_and(|s| !s.saving) {
            self.saving = false;
        }
        let busy = current.is_some_and(|s| s.loading || s.saving);
        let error = self
            .error
            .as_deref()
            .or_else(|| current.and_then(|s| s.error.as_deref()));
        let status = if let Some(error) = error {
            error.to_owned()
        } else if current.is_some_and(|s| s.saving) {
            "Saving profile…".to_owned()
        } else if current.is_some_and(|s| s.loading) {
            "Loading profile…".to_owned()
        } else if current.is_some_and(|s| s.saved) && !self.dirty {
            "Profile saved.".to_owned()
        } else {
            String::new()
        };
        ui.set_edit_profile_loaded(profile.is_some() && !current.is_some_and(|s| s.loading));
        ui.set_edit_profile_busy(busy);
        ui.set_edit_profile_status(status.into());
        ui.set_edit_profile_failed(error.is_some());
        // Only a pending change needs saying; the picture itself shows what's there.
        let label = |choice: &ProfileImage| match choice {
            ProfileImage::Keep => String::new(),
            ProfileImage::Remove => "Will be removed when you save.".to_owned(),
            ProfileImage::File { .. } => "New: saved when you save.".to_owned(),
        };
        ui.set_edit_profile_avatar_label(label(&self.avatar).into());
        ui.set_edit_profile_banner_label(label(&self.banner).into());
        // A picked (cropped) picture shows at once, before it's uploaded.
        let picked = |choice: &ProfileImage| match choice {
            ProfileImage::File { path, .. } => slint::Image::load_from_path(path).ok(),
            _ => None,
        };
        let avatar = picked(&self.avatar).or_else(|| {
            profile
                .and_then(|p| p.avatar_url.as_deref())
                .filter(|_| self.avatar != ProfileImage::Remove)
                .and_then(|url| images.get(url, Fit::Circle(160)))
        });
        ui.set_edit_profile_has_avatar(avatar.is_some());
        if let Some(image) = avatar {
            ui.set_edit_profile_avatar(image);
        }
        let banner = picked(&self.banner).or_else(|| {
            profile
                .and_then(|p| p.banner_url.as_deref())
                .filter(|_| self.banner != ProfileImage::Remove)
                .and_then(|url| images.get(url, crate::images::EDIT_BANNER))
        });
        ui.set_edit_profile_has_banner(banner.is_some());
        if let Some(image) = banner {
            ui.set_edit_profile_banner(image);
        }
    }
}

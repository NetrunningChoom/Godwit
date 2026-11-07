// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Godwit's entry point: runs the window over the core and the network
//! backends (see the `godwit` library for the parts).

// Opened from Explorer, no console window opens beside Godwit's own; from a
// terminal it attaches to that one (`godwit::win32::attach_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use godwit_core::application::{Action, Secret};
use godwit_core::backend::Waker;
use godwit_core::members::Presence;
use godwit_core::server_settings::ServerAdmin;
use slint::ComponentHandle;

use godwit::shell::Shell;
use godwit::ui::AppWindow;

thread_local! {
    /// The shell lives on the UI thread; wake-ups from other threads reach
    /// it through `invoke_from_event_loop`.
    static SHELL: RefCell<Option<Rc<RefCell<Shell>>>> = const { RefCell::new(None) };
    /// Callbacks that came while the shell was busy, oldest first.
    static DEFERRED: RefCell<std::collections::VecDeque<ShellCall>> = const {
        RefCell::new(std::collections::VecDeque::new())
    };
}

type ShellCall = Box<dyn FnOnce(&mut Shell, &AppWindow)>;

/// Runs `f` on the shell. If the shell is busy (a property change in the
/// middle of a refresh fired a callback), `f` runs right after instead,
/// and so does every callback after it until the queue is empty: order is
/// kept, or a deferred draft edit could land after the Send that followed
/// it and bring the sent text back (or leave the send with a stale draft).
fn with_shell(ui: &slint::Weak<AppWindow>, f: impl FnOnce(&mut Shell, &AppWindow) + 'static) {
    let Some(window) = ui.upgrade() else { return };
    let Some(shell) = SHELL.with(|s| s.borrow().clone()) else {
        return;
    };
    if DEFERRED.with(|d| d.borrow().is_empty())
        && let Ok(mut sh) = shell.try_borrow_mut()
    {
        f(&mut sh, &window);
        return;
    }
    let first = DEFERRED.with(|d| {
        let mut d = d.borrow_mut();
        d.push_back(Box::new(f));
        d.len() == 1
    });
    if first {
        schedule_deferred(ui.clone());
    }
}

/// Runs the deferred callbacks in order once the shell is free.
fn schedule_deferred(ui: slint::Weak<AppWindow>) {
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        let Some(window) = ui.upgrade() else { return };
        let Some(shell) = SHELL.with(|s| s.borrow().clone()) else {
            return;
        };
        loop {
            let Ok(mut sh) = shell.try_borrow_mut() else {
                schedule_deferred(ui);
                return;
            };
            // Popped before running, so a callback queued from inside it
            // goes behind the rest.
            let Some(next) = DEFERRED.with(|d| d.borrow_mut().pop_front()) else {
                return;
            };
            next(&mut sh, &window);
        }
    });
}

/// A waker for the network and image threads: schedules one pump on the
/// UI thread, however many times it's called before that runs.
fn waker(ui: slint::Weak<AppWindow>) -> Waker {
    let scheduled = Arc::new(AtomicBool::new(false));
    Arc::new(move || {
        if scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let (flag, ui) = (scheduled.clone(), ui.clone());
        let queued = slint::invoke_from_event_loop(move || {
            flag.store(false, Ordering::Release);
            with_shell(&ui, |sh, w| sh.pump(w));
        });
        if queued.is_err() {
            // The event loop is gone: the app is closing.
            scheduled.store(false, Ordering::Release);
        }
    })
}

/// A release build run from elsewhere offers once to add itself to the
/// system's apps (install.rs); Settings › About adds or removes it later.
fn offer_install(ui: &AppWindow) {
    use godwit::install;
    ui.set_install_supported(install::target().is_some());
    ui.set_app_installed(install::installed());
    ui.set_install_offer(install::offer());
    let weak = ui.as_weak();
    ui.on_install_later(move || {
        install::decline();
        if let Some(ui) = weak.upgrade() {
            ui.set_install_offer(false);
            ui.set_install_note(true);
        }
    });
    let weak = ui.as_weak();
    ui.on_install_app(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        match install::install() {
            Ok(_) if install::running_installed() => {
                ui.set_app_installed(true);
                ui.set_install_offer(false);
                ui.set_install_status("Added to your apps.".into());
            }
            // Runs from its new place from now on (and updates there).
            Ok(path) => match std::process::Command::new(&path)
                .args(std::env::args_os().skip(1))
                .spawn()
            {
                Ok(_) => {
                    let _ = slint::quit_event_loop();
                }
                Err(e) => ui.set_install_status(format!("Added, but couldn't start it: {e}").into()),
            },
            Err(e) => ui.set_install_status(format!("Couldn't add Godwit: {e}").into()),
        }
    });
    let weak = ui.as_weak();
    ui.on_uninstall_app(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let running = install::running_installed();
        match install::uninstall() {
            // Windows can't remove a running .exe: it goes once Godwit quits.
            Ok(()) if cfg!(windows) && running => {
                let _ = slint::quit_event_loop();
            }
            Ok(()) => {
                ui.set_app_installed(false);
                ui.set_install_status("Removed from your apps. Your settings and sign-ins stay.".into());
            }
            Err(e) => ui.set_install_status(format!("Couldn't remove it: {e}").into()),
        }
    });
}

/// Release builds look for a newer release in the background and ask
/// before installing it (update.rs); yes downloads it and restarts.
fn offer_updates(ui: &AppWindow) {
    let weak = ui.as_weak();
    godwit::update::start(move |version| {
        let _ = weak.upgrade_in_event_loop(move |ui| ui.set_update_version(version.into()));
    });
    let weak = ui.as_weak();
    ui.on_update_now(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if !ui.get_update_status().is_empty() {
            return; // Already downloading.
        }
        ui.set_update_status("Downloading…".into());
        ui.set_update_progress(0.0);
        let weak = weak.clone();
        let progress_ui = weak.clone();
        godwit::update::install(
            move |progress| {
                let _ = progress_ui.upgrade_in_event_loop(move |ui| ui.set_update_progress(progress));
            },
            move |result| {
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    match result.and_then(|()| godwit::update::restart().map_err(|e| format!("restarting: {e}"))) {
                        Ok(()) => {
                            let _ = slint::quit_event_loop();
                        }
                        Err(e) => {
                            eprintln!("update: {e}");
                            ui.set_update_progress(-1.0);
                            ui.set_update_status(format!("Couldn't update: {e}").into());
                        }
                    }
                });
            },
        );
    });
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn connect_callbacks(ui: &AppWindow) {
    let w = ui.as_weak();

    // Each callback owns a weak window handle; with_shell keeps the borrow and
    // UI upgrade rules in one place.
    macro_rules! callback {
        ($target:ident.$name:ident($($arg:ident),*) => |$shell:pat_param, $window:pat_param| $body:expr) => {
            $target.$name({
                let weak = w.clone();
                move |$($arg),*| with_shell(&weak, move |$shell, $window| $body)
            });
        };
    }
    callback!(ui.on_log_in(server, user, password) => |sh, ui| {
        sh.log_in(ui, server.into(), user.into(), password.into())
    });
    callback!(ui.on_submit_mfa(code) => |sh, ui| sh.submit_mfa(ui, code.into()));
    callback!(ui.on_pick_network(row) => |sh, ui| sh.pick_network(ui, row));
    callback!(ui.on_login_open_link(url) => |sh, _| sh.open_login_link(&url));
    callback!(ui.on_login_copy(text) => |sh, _| sh.copy_login_text(text.to_string()));
    callback!(ui.on_login_back() => |sh, ui| sh.login_back(ui));
    callback!(ui.on_add_account() => |sh, ui| sh.add_account(ui));
    callback!(ui.on_set_alert_mode(row, index) => |sh, ui| sh.set_alert_mode(ui, row, index));
    callback!(ui.on_set_offline(row, offline) => |sh, ui| sh.set_offline_row(ui, row, offline));
    callback!(ui.on_log_out_account(row) => |sh, _| sh.log_out_account(row));
    callback!(ui.on_start_verify(row) => |sh, ui| {
        if let Some(account) = sh.account_at(row) {
            sh.act(ui, Action::StartVerification(account));
        }
    });
    callback!(ui.on_answer_verify(matches) => |sh, ui| sh.act(ui, Action::AnswerVerification { matches }));
    callback!(ui.on_join_room(row) => |sh, ui| {
        if let Some(room) = sh.directory_at(row) {
            sh.act(ui, Action::JoinRoom(room));
        }
    });
    callback!(ui.on_answer_invite(accept) => |sh, ui| {
        if let Some(room) = sh.open_room() {
            sh.act(ui, Action::AnswerInvite { room, accept });
        }
    });
    callback!(ui.on_accept_verify() => |sh, ui| sh.act(ui, Action::AcceptVerification));
    callback!(ui.on_close_verify() => |sh, ui| sh.act(ui, Action::CloseVerification));
    callback!(ui.on_unlock_account(row, key) => |sh, ui| {
        // Straight into a `Secret`: it's never logged.
        let key = Secret(key.to_string());
        if let Some(account) = sh.account_at(row) {
            sh.act(ui, Action::UnlockWithKey { account, key });
        }
    });
    callback!(ui.on_settings_opened() => |sh, ui| sh.settings_opened(ui));
    callback!(ui.on_settings_changed() => |sh, ui| sh.settings_changed(ui));
    callback!(ui.on_open_settings_account(index) => |sh, ui| sh.open_settings_account(ui, index));
    callback!(ui.on_settings_open_privacy() => |sh, ui| sh.open_privacy(ui));
    callback!(ui.on_settings_open_sessions() => |sh, ui| sh.open_sessions(ui));
    {
        let pages = ui.global::<godwit::ui::AccountPages>();
        callback!(pages.on_toggle_privacy(i) => |sh, ui| sh.toggle_privacy(ui, i));
        callback!(pages.on_log_out_session(i) => |sh, ui| sh.log_out_session(ui, i));
    }
    callback!(ui.on_settings_open_autodelete() => |sh, ui| sh.open_autodelete(ui));
    {
        use godwit_core::autodelete::AutoDeleteCommand as C;
        let ad = ui.global::<godwit::ui::AdState>();
        callback!(ad.on_new_rule() => |sh, ui| sh.autodelete_edit(ui, -1));
        callback!(ad.on_edit_rule(i) => |sh, ui| sh.autodelete_edit(ui, i));
        callback!(ad.on_toggle_rule(i, on) => |sh, ui| sh.autodelete_rule(ui, i, |id| C::SetEnabled(id, on)));
        callback!(ad.on_recheck_rule(i) => |sh, ui| sh.autodelete_rule(ui, i, C::Recheck));
        callback!(ad.on_remove_rule(i) => |sh, ui| sh.autodelete_rule(ui, i, C::Remove));
        callback!(ad.on_toggle_place(i) => |sh, ui| sh.autodelete_toggle_place(ui, i));
        callback!(ad.on_scope_changed() => |sh, ui| sh.autodelete_scope_changed(ui));
        callback!(ad.on_search_places(q) => |sh, ui| sh.autodelete_search(ui, q.to_string()));
        callback!(ad.on_set_gentle(gentle) => |sh, ui| {
            sh.autodelete_command(ui, C::SetGap(if gentle { 3000 } else { 1500 }));
        });
        callback!(ad.on_set_recheck(days) => |sh, ui| {
            sh.autodelete_command(ui, C::SetRecheckDays(u32::try_from(days).unwrap_or(0)));
        });
        callback!(ad.on_stop() => |sh, ui| sh.autodelete_command(ui, C::Stop));
        callback!(ad.on_set_on(on) => |sh, ui| sh.autodelete_command(ui, C::SetOn(on)));
        callback!(ad.on_ed_next() => |sh, ui| sh.autodelete_next(ui));
        callback!(ad.on_ed_save() => |sh, ui| sh.autodelete_save(ui));
    }
    callback!(ui.on_select_profile_account(row) => |sh, ui| sh.select_profile_account(ui, row));
    callback!(ui.on_reload_own_profile() => |sh, ui| sh.reload_own_profile(ui));
    callback!(ui.on_reset_own_profile() => |sh, ui| sh.reset_own_profile(ui));
    callback!(ui.on_save_own_profile() => |sh, ui| sh.save_own_profile(ui));
    callback!(ui.on_own_profile_edited() => |sh, ui| sh.own_profile_edited(ui));
    callback!(ui.on_remove_profile_image(avatar) => |sh, ui| sh.remove_profile_image(ui, avatar));
    ui.on_pick_profile_image({
        let w = w.clone();
        move |avatar| {
            let w = w.clone();
            with_shell(&w.clone(), move |sh, ui| {
                let Some(account) = sh.profile_edit_account() else {
                    return;
                };
                let chosen_for = account.clone();
                let spawned = std::thread::Builder::new()
                    .name("profile-image-picker".into())
                    .spawn(move || {
                        let Some(path) = rfd::FileDialog::new()
                            .set_title(if avatar { "Choose an avatar" } else { "Choose a banner" })
                            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
                            .pick_file()
                        else {
                            return;
                        };
                        // Decoded here (big files take a while), then cropped.
                        let target = godwit::cropper::CropFor::Profile {
                            account: chosen_for.clone(),
                            avatar,
                        };
                        let loaded = godwit::cropper::load(target, &path);
                        let queued = slint::invoke_from_event_loop(move || {
                            with_shell(&w, move |sh, ui| match loaded {
                                Ok(crop) => sh.open_crop(ui, crop),
                                Err(e) => sh.profile_image_picked(ui, &chosen_for, avatar, Err(e)),
                            })
                        });
                        if let Err(error) = queued {
                            eprintln!("error: returning the profile image selection: {error}");
                        }
                    });
                if let Err(error) = spawned {
                    eprintln!("error: opening the profile image picker: {error}");
                    sh.profile_image_picked(
                        ui,
                        &account,
                        avatar,
                        Err("The image picker could not be opened.".into()),
                    );
                }
            });
        }
    });
    callback!(ui.on_server_settings_page(page) => |sh, ui| sh.server_settings_page(ui, page));
    callback!(ui.on_delete_server_invite(row) => |sh, ui| sh.delete_server_invite(ui, row));
    callback!(ui.on_copy_server_invite(row) => |sh, _| sh.copy_server_invite(row));
    callback!(ui.on_unban_server_user(row) => |sh, ui| sh.unban_server_user(ui, row));
    callback!(ui.on_select_server_role(row) => |sh, ui| sh.select_server_role(ui, row));
    callback!(ui.on_create_server_role(name) => |sh, ui| sh.create_server_role(ui, &name));
    callback!(ui.on_server_role_edited() => |sh, ui| sh.server_role_edited(ui));
    callback!(ui.on_toggle_server_permission(row) => |sh, ui| sh.toggle_server_permission(ui, row));
    callback!(ui.on_save_server_role() => |sh, ui| sh.save_server_role(ui));
    callback!(ui.on_delete_server_role() => |sh, ui| sh.delete_server_role(ui));
    callback!(ui.on_moderate_member(ban, reason) => |sh, ui| sh.moderate_member(ui, ban, &reason));
    callback!(ui.on_server_menu_invite() => |sh, ui| sh.server_menu(ui, ServerAdmin::CreateInvite));
    ui.on_server_menu_notify({
        let w = w.clone();
        move |level| {
            let level = u8::try_from(level).unwrap_or(1);
            with_shell(&w, move |sh, ui| sh.server_menu(ui, ServerAdmin::SetNotify(level)))
        }
    });
    callback!(ui.on_server_menu_mark_read() => |sh, ui| sh.server_menu(ui, ServerAdmin::MarkRead));
    callback!(ui.on_server_menu_create_channel(name, voice) => |sh, ui| {
        sh.server_menu(
            ui,
            ServerAdmin::CreateChannel {
                name: name.as_str().into(),
                voice,
            },
        )
    });
    callback!(ui.on_server_menu_leave() => |sh, ui| sh.server_menu(ui, ServerAdmin::Leave));
    callback!(ui.on_copy_invite_link() => |sh, _| sh.copy_invite_link());
    callback!(ui.on_close_invite_link() => |sh, ui| sh.act(ui, Action::CloseInviteLink));
    callback!(ui.on_user_sound(i, volume, muted, done) => |sh, ui| sh.user_sound(ui, i, volume, muted, done));
    callback!(ui.on_stream_sound(i, volume, muted, done) => |sh, ui| sh.stream_sound(ui, i, volume, muted, done));
    callback!(ui.on_video_fullscreen(i) => |sh, ui| sh.video_fullscreen(ui, i));
    callback!(ui.on_watch_stream(i, watch) => |sh, ui| {
        let user = usize::try_from(i)
            .ok()
            .and_then(|i| sh.app_state().call.as_ref()?.streams.get(i).map(|s| s.user.clone()));
        if let Some(user) = user {
            sh.act(ui, Action::WatchStream { user, watch });
        }
    });
    callback!(ui.on_create_thread(row) => |sh, ui| sh.create_thread_from(ui, row));
    callback!(ui.on_new_post() => |sh, ui| sh.new_post(ui));
    callback!(ui.on_thread_confirm() => |sh, ui| sh.thread_confirm(ui));
    callback!(ui.on_open_search() => |sh, ui| sh.act(ui, Action::Search(String::new())));
    callback!(ui.on_search(query) => |sh, ui| sh.act(ui, Action::Search(query.to_string())));
    callback!(ui.on_close_search() => |sh, ui| sh.act(ui, Action::CloseSearch));
    callback!(ui.on_search_jump(i) => |sh, ui| sh.search_jump(ui, i));
    callback!(ui.on_open_pins() => |sh, ui| sh.act(ui, Action::OpenPins));
    callback!(ui.on_close_pins() => |sh, ui| sh.act(ui, Action::ClosePins));
    callback!(ui.on_pin(row) => |sh, ui| sh.pin_row(ui, row));
    callback!(ui.on_pin_jump(i) => |sh, ui| sh.pin_jump(ui, i));
    callback!(ui.on_pin_unpin(i) => |sh, ui| sh.pin_unpin(ui, i));
    callback!(ui.on_crop_done(original) => |sh, ui| sh.crop_done(ui, original));
    callback!(ui.on_crop_cancel() => |sh, ui| sh.crop_cancel(ui));
    callback!(ui.on_close_image() => |sh, ui| sh.close_viewer(ui));
    callback!(ui.on_step_image(step) => |sh, ui| sh.step_image(ui, step));
    callback!(ui.on_save_image(external) => |sh, _| sh.save_viewer_image(external));
    callback!(ui.on_incoming_call(answer) => |sh, ui| sh.incoming_call(ui, answer));
    callback!(ui.on_quiet_notice(notify) => |sh, ui| sh.quiet_notice(ui, notify));
    callback!(ui.on_rejoin_call(join) => |sh, ui| sh.rejoin_call(ui, join));
    callback!(ui.on_open_confirmed_link() => |sh, ui| sh.open_confirmed_link(ui));
    callback!(ui.on_discord_privacy(tighten) => |sh, ui| sh.discord_privacy(ui, tighten));
    callback!(ui.on_delete_server() => |sh, ui| sh.delete_server(ui));
    callback!(ui.on_open_server_settings() => |sh, ui| sh.open_server_settings(ui));
    callback!(ui.on_close_server_settings() => |sh, ui| sh.act(ui, Action::CloseServerSettings));
    callback!(ui.on_save_server_overview() => |sh, ui| sh.save_server_overview(ui));
    callback!(ui.on_reload_server_overview() => |sh, ui| sh.reload_server_overview(ui));
    callback!(ui.on_reset_server_overview() => |sh, ui| sh.reset_server_overview(ui));
    callback!(ui.on_server_overview_edited() => |sh, ui| sh.server_overview_edited(ui));
    callback!(ui.on_remove_server_image(icon) => |sh, ui| sh.remove_server_image(ui, icon));
    ui.on_pick_server_image({
        let w = w.clone();
        move |avatar| {
            let w = w.clone();
            with_shell(&w.clone(), move |sh, ui| {
                let Some(account) = sh.server_edit_token() else {
                    return;
                };
                let chosen_for = account.clone();
                let spawned = std::thread::Builder::new()
                    .name("server-image-picker".into())
                    .spawn(move || {
                        let Some(path) = rfd::FileDialog::new()
                            .set_title(if avatar {
                                "Choose a server icon"
                            } else {
                                "Choose a server banner"
                            })
                            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
                            .pick_file()
                        else {
                            return;
                        };
                        let target = godwit::cropper::CropFor::Server {
                            token: chosen_for.clone(),
                            icon: avatar,
                        };
                        let loaded = godwit::cropper::load(target, &path);
                        let queued = slint::invoke_from_event_loop(move || {
                            with_shell(&w, move |sh, ui| match loaded {
                                Ok(crop) => sh.open_crop(ui, crop),
                                Err(e) => sh.server_image_picked(ui, &chosen_for, avatar, Err(e)),
                            })
                        });
                        if let Err(error) = queued {
                            eprintln!("error: returning the server image selection: {error}");
                        }
                    });
                if let Err(error) = spawned {
                    eprintln!("error: opening the server image picker: {error}");
                    sh.server_image_picked(
                        ui,
                        &account,
                        avatar,
                        Err("The image picker could not be opened.".into()),
                    );
                }
            });
        }
    });
    callback!(ui.on_clear_cache() => |sh, ui| sh.clear_cache(ui));
    callback!(ui.on_mic_test(on) => |sh, ui| sh.mic_test(ui, on));
    callback!(ui.on_list_devices() => |sh, ui| sh.settings_opened(ui));
    callback!(ui.on_log_out() => |sh, _| sh.log_out());
    callback!(ui.on_select_home() => |sh, ui| sh.act(ui, Action::SelectServer(None)));
    callback!(ui.on_select_server(row) => |sh, ui| {
        if let Some(id) = usize::try_from(row).ok().and_then(|r| sh.server_at(r)) {
            sh.act(ui, Action::SelectServer(Some(id)));
        }
    });
    callback!(ui.on_talk_on(row) => |sh, ui| {
        if let Some(user) = sh.talk_on_at(row) {
            sh.act(ui, Action::TalkOn(user));
        }
    });
    callback!(ui.on_rename_person(name) => |sh, ui| {
        if let Some(contact) = sh.open_contact() {
            let nickname = Some(name.to_string()).filter(|n| !n.trim().is_empty());
            sh.act(ui, Action::SetNickname { contact, nickname });
        }
    });
    callback!(ui.on_reset_nickname(row) => |sh, ui| {
        if let Some(contact) = sh.person_at(row).map(|(c, _)| *c) {
            sh.act(
                ui,
                Action::SetNickname {
                    contact,
                    nickname: None,
                },
            );
        }
    });
    callback!(ui.on_toggle_account(row) => |sh, ui| {
        if let Ok(row) = usize::try_from(row) {
            sh.toggle_account(ui, row);
        }
    });
    callback!(ui.on_open_account(row) => |sh, ui| {
        if let Ok(row) = usize::try_from(row) {
            sh.open_account(ui, row);
        }
    });
    callback!(ui.on_select_room(row) => |sh, ui| {
        if let Some(id) = usize::try_from(row).ok().and_then(|r| sh.room_at(r)) {
            sh.act(ui, Action::SelectRoom(id));
        }
    });
    callback!(ui.on_load_earlier() => |sh, ui| sh.act(ui, Action::LoadEarlier));
    callback!(ui.on_draft_edited(text) => |sh, ui| sh.act(ui, Action::SetDraft(text.into())));
    // The timeline's text selection (selection.rs), through its global.
    {
        let select = ui.global::<godwit::ui::Select>();
        callback!(select.on_down(r, b, x, y, shift) => |sh, ui| sh.select_down(ui, r, b, x, y, shift));
        callback!(select.on_drag(r, b, x, y) => |sh, ui| sh.select_drag(ui, r, b, x, y));
        callback!(select.on_up(r, b, x, y) => |sh, ui| sh.select_up(ui, r, b, x, y));
        callback!(select.on_word(r, b, x, y) => |sh, ui| sh.select_word(ui, r, b, x, y));
        callback!(select.on_hover(r, b, x, y) => |sh, ui| sh.select_hover(ui, r, b, x, y));
        callback!(select.on_copy() => |sh, _| sh.select_copy());
        callback!(select.on_clear() => |sh, ui| sh.select_clear(ui));
        // While the window is being resized the width changes on every
        // frame: lay text out again once it settles.
        select.on_width_changed({
            let w = w.clone();
            let timer = Rc::new(slint::Timer::default());
            move |width| {
                let w = w.clone();
                timer.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_millis(80),
                    move || with_shell(&w, move |sh, ui| sh.text_width(ui, width)),
                );
            }
        });
    }
    // The GIF picker (gifs.rs), through its global.
    {
        let gifs = ui.global::<godwit::ui::Gifs>();
        callback!(gifs.on_opened() => |sh, ui| sh.open_gifs(ui));
        // As the official picker, a search waits for typing to pause (250 ms):
        // gifbox allows only a few searches per window.
        gifs.on_search({
            let w = w.clone();
            let latest = Rc::new(std::cell::RefCell::new(String::new()));
            let timer = Rc::new(slint::Timer::default());
            move |text| {
                *latest.borrow_mut() = text.to_string();
                let (w, latest) = (w.clone(), latest.clone());
                timer.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_millis(250),
                    move || {
                        let query = latest.borrow().clone();
                        with_shell(&w, move |sh, ui| sh.gif_search(ui, &query));
                    },
                );
            }
        });
        callback!(gifs.on_picked(i) => |sh, ui| sh.gif_picked(ui, i, now_ms()));
        callback!(gifs.on_scrolled(first, near_end) => |sh, ui| sh.gif_scrolled(ui, first, near_end));
        callback!(gifs.on_toggle_favourite(url, picture) => |sh, ui| sh.toggle_gif_favourite(ui, &url, &picture));
        // Read without the shell: drawing asks it, and the shell may be busy.
        gifs.on_is_favourite(|url, _version| godwit::gifs::is_starred(&url));
    }
    // What's in the composer is what goes: core's draft could lag the input.
    callback!(ui.on_send() => |sh, ui| {
        sh.act(ui, Action::SetDraft(ui.get_draft().into()));
        sh.act(ui, Action::Send { now_ms: now_ms() });
    });
    callback!(ui.on_dismiss_nudge(never) => |sh, ui| sh.act(ui, Action::DismissNudge { never }));
    callback!(ui.on_problems_opened() => |sh, ui| sh.problems_opened(ui));
    callback!(ui.on_copy_problems() => |sh, _| sh.copy_problems());
    callback!(ui.on_open_problem_log() => |sh, _| sh.open_problem_log());
    callback!(ui.on_clear_problems() => |sh, ui| sh.clear_problems(ui));
    callback!(ui.on_reply(row) => |sh, ui| {
        if let Some(id) = sh.message_at(row) {
            sh.act(ui, Action::ReplyTo(id));
        }
    });
    callback!(ui.on_edit(row) => |sh, ui| {
        if let Some(id) = sh.message_at(row) {
            sh.act(ui, Action::StartEdit(id));
        }
    });
    callback!(ui.on_delete(row) => |sh, ui| {
        if let Some(id) = sh.message_at(row) {
            sh.act(ui, Action::Delete(id));
        }
    });
    callback!(ui.on_react(row, reaction) => |sh, ui| sh.react_row(ui, row, reaction));
    callback!(ui.on_open_picker(row) => |sh, ui| sh.open_picker(ui, row));
    callback!(ui.on_picker_search(q) => |sh, _| sh.picker_search(&q));
    callback!(ui.on_picker_scrolled(first) => |sh, _| sh.picker_scrolled(first));
    callback!(ui.on_picker_picked(row, col) => |sh, ui| sh.picker_picked(ui, row, col));
    callback!(ui.on_close_picker() => |sh, ui| sh.close_picker(ui));
    callback!(ui.on_open_attachment(row, index) => |sh, ui| sh.open_attachment(ui, row, index));
    callback!(ui.on_link_clicked(row, url) => |sh, ui| sh.link(ui, row, &url));
    callback!(ui.on_play_inline(url, kind) => |sh, ui| sh.play_inline(ui, &url, &kind));
    callback!(ui.on_join_invite(code) => |sh, ui| sh.join_invite(ui, &code));
    callback!(ui.on_open_join_server() => |sh, ui| sh.open_join_server(ui));
    callback!(ui.on_close_join_server() => |sh, ui| sh.close_join_server(ui));
    callback!(ui.on_reset_join_server() => |sh, ui| sh.reset_join_server(ui));
    callback!(ui.on_inspect_invite(row, input) => |sh, ui| sh.inspect_invite(ui, row, &input));
    callback!(ui.on_accept_server_invite() => |sh, ui| sh.accept_server_invite(ui));
    callback!(ui.on_player_fullscreen(url) => |sh, ui| sh.player_fullscreen(ui, &url));
    callback!(ui.on_player_pointer(url, x, y, down) => |sh, _| sh.player_pointer(&url, x, y, down));
    callback!(ui.on_player_move(url, x, y) => |sh, _| sh.player_move(&url, x, y));
    callback!(ui.on_cancel_compose() => |sh, ui| sh.act(ui, Action::CancelCompose));
    callback!(ui.on_adjacent_room(delta) => |sh, ui| sh.adjacent_room(ui, delta));
    callback!(ui.on_switcher_search(q) => |sh, ui| sh.switcher_search(ui, &q));
    callback!(ui.on_switcher_picked(row) => |sh, ui| sh.switcher_picked(ui, row));
    callback!(ui.on_jump_to_reply(row) => |sh, ui| sh.jump_to_reply(ui, row));
    callback!(ui.on_jump_to_present() => |sh, ui| sh.act(ui, Action::JumpToPresent));
    callback!(ui.on_edit_last() => |sh, ui| sh.act(ui, Action::EditLast));
    callback!(ui.on_people_search(q) => |sh, ui| sh.people_search(ui, q.to_string(), None));
    callback!(ui.on_people_filter(n) => |sh, ui| {
        if n == "favourites-only" {
            sh.toggle_people_favourites(ui);
        } else {
            sh.people_search(ui, String::new(), Some(n.to_string()));
        }
    });
    // Files dropped on the window attach to the open chat. winit delivers
    // drops on Windows, macOS and X11; on Wayland (winit 0.30) it doesn't yet.
    {
        use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
        let w = w.clone();
        // Where the pointer is (logical), for re-sending accelerated wheel turns.
        let mut pointer = slint::LogicalPosition::default();
        let mut wheel = godwit::wheel::Wheel::default();
        ui.window().on_winit_window_event(move |window, event| {
            match event {
                winit::event::WindowEvent::Focused(focused) => {
                    let focused = *focused;
                    with_shell(&w, move |sh, _| sh.window_focus_changed(focused));
                }
                winit::event::WindowEvent::CursorMoved { position, .. } => {
                    let scale = f64::from(window.scale_factor());
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        pointer = slint::LogicalPosition::new((position.x / scale) as f32, (position.y / scale) as f32);
                    }
                }
                // A notched wheel: scroll further the longer it keeps turning.
                winit::event::WindowEvent::MouseWheel {
                    delta: winit::event::MouseScrollDelta::LineDelta(x, y),
                    ..
                } => {
                    let (delta_x, delta_y) = wheel.scroll(std::time::Instant::now(), *x, *y);
                    window.dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: pointer,
                        delta_x,
                        delta_y,
                    });
                    return EventResult::PreventDefault;
                }
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Back,
                    ..
                } => {
                    with_shell(&w, |sh, ui| sh.navigate_back(ui));
                    return EventResult::PreventDefault;
                }
                _ => {}
            }
            if let winit::event::WindowEvent::DroppedFile(path) = event {
                let path = path.clone();
                with_shell(&w, move |sh, ui| {
                    if ui.get_can_attach() {
                        sh.add_files(ui, vec![path]);
                    }
                });
            }
            EventResult::Propagate
        });
    }
    callback!(ui.on_panels_resized(sidebar, members) => |sh, _| sh.panels_resized(sidebar, members));
    callback!(ui.on_move_server_by(row, steps) => |sh, ui| sh.move_server_by(ui, row, steps));
    callback!(ui.on_move_account_by(row, steps) => |sh, ui| sh.move_account_by(ui, row, steps));
    callback!(ui.on_move_server(row, delta) => |sh, ui| sh.move_server(ui, row, delta));
    callback!(ui.on_search_home(q) => |sh, ui| sh.search_home(ui, q.to_string()));
    callback!(ui.on_member_rows(first, last) => |sh, ui| sh.act(
        ui,
        Action::MemberRows {
            first: u32::try_from(first).unwrap_or(0),
            last: u32::try_from(last).unwrap_or(0),
        }
    ));
    callback!(ui.on_filter_home(n) => |sh, ui| sh.filter_home(ui, n.to_string()));
    callback!(ui.on_room_menu(row, choice) => |sh, ui| sh.room_menu(ui, row, choice));
    callback!(ui.on_toggle_category(row) => |sh, ui| sh.toggle_category(ui, row));
    callback!(ui.on_toggle_hidden_rooms() => |sh, ui| sh.toggle_hidden_rooms(ui));
    ui.on_paste_files({
        let w = w.clone();
        // Answered now (the key handler waits for it); a busy shell lets the
        // text paste as usual.
        move || {
            let Some(window) = w.upgrade() else { return false };
            let Some(shell) = SHELL.with(|s| s.borrow().clone()) else {
                return false;
            };
            shell
                .try_borrow_mut()
                .map(|mut sh| sh.paste_files(&window))
                .unwrap_or(false)
        }
    });
    ui.on_attach({
        let w = w.clone();
        move || {
            // The dialog blocks, so it gets its own thread; the window stays live.
            let w = w.clone();
            let spawned = std::thread::Builder::new().name("file-dialog".into()).spawn(move || {
                let Some(paths) = rfd::FileDialog::new().set_title("Attach files").pick_files() else {
                    return;
                };
                let queued =
                    slint::invoke_from_event_loop(move || with_shell(&w, move |sh, ui| sh.add_files(ui, paths)));
                if let Err(e) = queued {
                    eprintln!("error: attaching files: {e}");
                }
            });
            if let Err(e) = spawned {
                eprintln!("error: opening the file dialog: {e}");
            }
        }
    });
    callback!(ui.on_remove_file(i) => |sh, ui| {
        if let Ok(i) = usize::try_from(i) {
            sh.act(ui, Action::RemoveFile(i));
        }
    });
    callback!(ui.on_join_call() => |sh, ui| {
        if let Some(room) = sh.selected_room() {
            sh.act(ui, Action::JoinCall(room));
        }
    });
    callback!(ui.on_toggle_members() => |sh, ui| sh.act(ui, Action::ToggleMembers));
    callback!(ui.on_open_message_profile(row) => |sh, ui| {
        if let Some(user) = sh.author_at(row) {
            sh.act(ui, Action::OpenProfile(user));
        }
    });
    callback!(ui.on_open_member_profile(row) => |sh, ui| {
        if let Some(user) = sh.member_at(row) {
            sh.act(ui, Action::OpenProfile(user));
        }
    });
    callback!(ui.on_mark_unread(row) => |sh, ui| {
        if let Some(id) = sh.message_at(row) {
            sh.act(ui, Action::MarkUnread(id));
        }
    });
    callback!(ui.on_copy_text(row) => |sh, _| sh.copy(row, false));
    callback!(ui.on_copy_string(text) => |_sh, _| godwit::shell::copy_string(text.to_string()));
    callback!(ui.on_copy_link(row) => |sh, _| sh.copy(row, true));
    callback!(ui.on_close_profile() => |sh, ui| sh.act(ui, Action::CloseProfile));
    callback!(ui.on_send_friend_request(key, name) => |sh, ui| {
        sh.send_friend_request(ui, key.as_str(), name.to_string());
    });
    callback!(ui.on_answer_friend_request(row, accept) => |sh, ui| {
        if let Some(user) = sh.request_at(row) {
            sh.act(
                ui,
                if accept {
                    Action::AddFriend(user)
                } else {
                    Action::RemoveFriend(user)
                },
            );
        }
    });
    callback!(ui.on_add_friend_profile() => |sh, ui| sh.befriend_profile(ui, true));
    callback!(ui.on_remove_friend_profile() => |sh, ui| sh.befriend_profile(ui, false));
    callback!(ui.on_message_profile() => |sh, ui| sh.message_profile(ui));
    callback!(ui.on_leave_call() => |sh, ui| sh.act(ui, Action::LeaveCall));
    callback!(ui.on_toggle_mute() => |sh, ui| sh.act(ui, Action::ToggleMute));
    callback!(ui.on_set_camera(active) => |sh, ui| sh.act(ui, Action::SetCamera(active)));
    callback!(ui.on_set_screen_share(active) => |sh, ui| sh.set_screen_share(ui, active));
    callback!(ui.on_confirm_screen_share() => |sh, ui| sh.confirm_screen_share(ui));
    callback!(ui.on_cancel_screen_share() => |sh, ui| sh.cancel_screen_share(ui));
    callback!(ui.on_toggle_deafen() => |sh, ui| sh.act(ui, Action::ToggleDeafen));
    callback!(ui.on_open_call_room() => |sh, ui| {
        if let Some(room) = sh.call_room() {
            sh.act(ui, Action::SelectRoom(room));
        }
    });
    callback!(ui.on_show_friends() => |sh, ui| sh.act(ui, Action::ShowFriends));
    callback!(ui.on_set_presence(i) => |sh, ui| {
        let presence = [
            Presence::Online,
            Presence::Idle,
            Presence::Focus,
            Presence::Busy,
            Presence::Invisible,
        ];
        if let (Some(account), Some(p)) = (
            sh.focus_account(),
            usize::try_from(i).ok().and_then(|i| presence.get(i)),
        ) {
            sh.act(ui, Action::SetPresence { account, presence: *p });
        }
    });
    callback!(ui.on_set_status_text(text) => |sh, ui| {
        if let Some(account) = sh.focus_account() {
            let text = Some(text.to_string()).filter(|t| !t.trim().is_empty());
            sh.act(ui, Action::SetStatusText { account, text });
        }
    });
    // The visible range changes on every row crossing while scrolling: it's
    // applied at most every 50 ms (as Yapfast's), after the latest report,
    // so fast scrolling doesn't rebuild the window on every frame.
    ui.on_visible_rows({
        let w = w.clone();
        let latest = Rc::new(std::cell::Cell::new((-1, -1)));
        let timer = Rc::new(slint::Timer::default());
        move |top, bottom| {
            latest.set((top, bottom));
            if !timer.running() {
                let (w, latest) = (w.clone(), latest.clone());
                timer.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_millis(50),
                    move || {
                        let (top, bottom) = latest.get();
                        with_shell(&w, move |sh, ui| sh.visible_rows(ui, top, bottom));
                    },
                );
            }
        }
    });
    callback!(ui.on_show_people() => |sh, ui| sh.act(ui, Action::ShowPeople));
    callback!(ui.on_favourite_person(row) => |sh, ui| sh.toggle_person_favourite(ui, row));
    callback!(ui.on_show_threads() => |sh, ui| sh.act(ui, Action::ShowThreads));
    callback!(ui.on_threads_load_more() => |sh, ui| sh.act(ui, Action::LoadMoreThreads));
    callback!(ui.on_overview_thread_open(row) => |sh, ui| {
        if let Some(room) = sh.thread_at(row) { sh.act(ui, Action::SelectRoom(room)); }
    });
    callback!(ui.on_show_active() => |sh, ui| sh.act(ui, Action::ShowActive));
    // Active now: a call opens its room, a person a chat with them.
    callback!(ui.on_active_open(row) => |sh, ui| {
        match sh.active_at(row) {
            Some(godwit::view::ActiveTarget::Call(room)) => sh.act(ui, Action::SelectRoom(room)),
            Some(godwit::view::ActiveTarget::Person(user)) => sh.act(ui, Action::OpenDirect(user)),
            None => {}
        }
    });
    callback!(ui.on_active_join(row) => |sh, ui| {
        if let Some(godwit::view::ActiveTarget::Call(room)) = sh.active_at(row) {
            sh.act(ui, Action::SelectRoom(room.clone()));
            sh.act(ui, Action::JoinCall(room));
        }
    });
    callback!(ui.on_link_people(into, from) => |sh, ui| {
        if let (Some((into, _)), Some((from, _))) = (sh.person_at(into), sh.person_at(from)) {
            let (into, from) = (*into, *from);
            sh.act(ui, Action::MergeContacts { into, from });
        }
    });
    callback!(ui.on_link_profile() => |sh, ui| {
        if let Some(user) = sh.profile_user() {
            sh.act(ui, Action::LinkFrom(user));
        }
    });
    callback!(ui.on_set_nudges(row, on) => |sh, ui| {
        if let Some((contact, _)) = sh.person_at(row) {
            let contact = *contact;
            sh.act(ui, Action::SetNudges { contact, on });
        }
    });
    callback!(ui.on_not_same(a, b) => |sh, ui| {
        if let (Some((a, _)), Some((b, _))) = (sh.person_at(a), sh.person_at(b)) {
            let (a, b) = (*a, *b);
            sh.act(ui, Action::NotSame { a, b });
        }
    });
    callback!(ui.on_unlink_person(row, k) => |sh, ui| {
        let user = sh
            .person_at(row)
            .and_then(|(_, ids)| ids.get(usize::try_from(k).ok()?).cloned());
        if let Some(user) = user {
            sh.act(ui, Action::SplitIdentity(user));
        }
    });
    callback!(ui.on_choose_network(row, k) => |sh, ui| {
        // Keep the exact identity: two of our accounts on one network
        // can both reach this person.
        let choice = sh
            .person_at(row)
            .and_then(|(c, ids)| Some((*c, ids.get(usize::try_from(k).ok()?)?.clone())));
        if let Some((contact, user)) = choice {
            sh.act(ui, Action::PreferIdentity { contact, user });
        }
    });
    callback!(ui.on_message_person(row) => |sh, ui| {
        // On the network Godwit would talk to them on, if we're
        // signed in there; else on any network we are.
        let user = sh.person_at(row).and_then(|(c, ids)| match sh.contact(*c) {
            Some(contact) => contact.best_identity(|u| sh.signed_in(&u.account)).cloned(),
            None => ids.iter().find(|u| sh.signed_in(&u.account)).cloned(),
        });
        if let Some(user) = user {
            sh.act(ui, Action::OpenDirect(user));
        }
    });
    callback!(ui.on_open_direct(row) => |sh, ui| {
        if let Some(user) = sh.friend_at(row) {
            sh.act(ui, Action::OpenDirect(user));
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    godwit::win32::attach_console();
    // Before any other thread starts (see memory.rs).
    godwit::memory::limit_pools();
    // reqwest and ureq use rustls; it needs its crypto provider chosen once.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Draw on the CPU unless the user picked a renderer: it's the lightest
    // (see docs/design.md, "Rendering").
    if std::env::var_os("SLINT_BACKEND").is_none() {
        // SAFETY: still single-threaded; nothing else reads the environment yet.
        unsafe { std::env::set_var("SLINT_BACKEND", "winit-software") };
    }

    let ui = AppWindow::new()?;
    offer_install(&ui);
    offer_updates(&ui);
    // The Wayland app id (the X11 class too): desktops match it to
    // godwit.desktop for the window's icon and name. Only after the platform
    // exists (AppWindow::new starts it); the real window is made at run().
    if let Err(e) = slint::set_xdg_app_id("godwit") {
        eprintln!("error: setting the app id: {e}");
    }
    // Windows names toasts and groups the taskbar button by this id.
    #[cfg(windows)]
    godwit::win32::register_app_id();
    // `--demo`: made-up data and no accounts, for trying the views.
    let demo = std::env::args().any(|a| a == "--demo");
    // `--verbose`: routine diagnostics too (always on in debug builds).
    if std::env::args().any(|a| a == "--verbose") {
        godwit_core::log::set_verbose(true);
    }
    let shell = Shell::new(&ui, waker(ui.as_weak()), demo)?;
    SHELL.with(|s| *s.borrow_mut() = Some(Rc::new(RefCell::new(shell))));
    connect_callbacks(&ui);
    // Draw the first screen whatever `pump` finds: with no accounts nothing
    // has happened yet, and it skips refreshing when nothing did (the window
    // then stayed on its default "Connecting…" screen).
    with_shell(&ui.as_weak(), |sh, w| {
        sh.pump(w);
        sh.refresh(w);
    });
    ui.run()?;
    // Closed normally: whether the call we were in is offered next start
    // follows Settings › Voice (`closing`).
    SHELL.with(|s| {
        if let Some(shell) = s.borrow().as_ref() {
            shell.borrow_mut().closing();
        }
    });
    Ok(())
}

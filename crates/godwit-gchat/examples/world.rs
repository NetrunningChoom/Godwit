// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Checks the kept sign-in (slot 0) against Google Chat: fetches who's
//! signed in and the conversation list, printing counts only.
//! `cargo run -p godwit-gchat --example world`.

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let saved = godwit_gchat::session::load(0).ok_or_else(|| anyhow::anyhow!("no sign-in in slot 0"))?;
    let mut api = godwit_gchat::api::Api::new(&saved)?;
    api.refresh().await?;
    println!("xsrf token: ok");
    let me = api.self_status().await?;
    println!("signed in: {}", me.user_status.user_id.is_some());
    let world = api.world(50).await?;
    let dms = world.iter().filter(|i| i.dm_members.is_some()).count();
    let unread = world.iter().filter(|i| i.read_state.unread_message_count() > 0).count();
    println!(
        "conversations: {} ({dms} DMs, {} spaces), {unread} unread",
        world.len(),
        world.len() - dms
    );
    Ok(())
}

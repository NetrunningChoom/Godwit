// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
// Portions adapted from Yapfast (https://github.com/yapfast/yapfast,
// crates/yapfast-discord/src/mfa.rs), Copyright (C) the Yapfast authors,
// GPL-3.0-or-later.
//! Two-factor authentication for a request Discord holds back until it is
//! confirmed (docs.discord.food, "Multi-Factor Authentication"): Discord
//! answers 401 with code 60003 and an `mfa` object (a `ticket` and the
//! `methods` it takes); the person confirms with one of them, which
//! `POST /mfa/finish` turns into a token; the request goes again, once, with
//! the token in `X-Discord-MFA-Authorization`. Discord asks this of
//! moderation in a server that requires 2FA for it, and of an owner's own
//! actions such as deleting the server.
//!
//! The ticket, the token and what the person typed live only in memory for
//! as long as one prompt: none is logged, put in an error or kept for later
//! requests, since each held-back request asks again.

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use tokio::sync::oneshot;

/// Discord's code for a request that needs two-factor authentication.
pub const MFA_REQUIRED: u64 = 60003;
/// Discord's code for a code, password or backup code it didn't take.
pub const INVALID_MFA: u64 = 60008;
/// How long a prompt waits for the person: a ticket doesn't outlive this
/// by much, and a forgotten prompt must not hold its request forever.
const PROMPT_LIFE: Duration = Duration::from_secs(10 * 60);

/// A way to confirm, as the ticket offers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// A passkey or security key, which needs a browser; not supported.
    Webauthn,
    /// A code from an authenticator app.
    Totp,
    /// A code Discord texts, once asked to; not supported.
    Sms,
    Password,
    Backup,
}

impl Method {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "webauthn" => Self::Webauthn,
            "totp" => Self::Totp,
            "sms" => Self::Sms,
            "password" => Self::Password,
            "backup" => Self::Backup,
            _ => return None,
        })
    }

    /// The method's name in `POST /mfa/finish`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Webauthn => "webauthn",
            Self::Totp => "totp",
            Self::Sms => "sms",
            Self::Password => "password",
            Self::Backup => "backup",
        }
    }

    /// Whether a code typed in can confirm with it.
    fn typed(self) -> bool {
        matches!(self, Self::Totp | Self::Password | Self::Backup)
    }
}

/// Discord held a request back until it is confirmed: the error a refused
/// request carries, for [`stepping_up`].
#[derive(Clone)]
pub struct Required {
    ticket: String,
    pub methods: Vec<Method>,
}

// The ticket stays out of logs and error chains.
impl std::fmt::Debug for Required {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Required")
            .field("methods", &self.methods)
            .finish_non_exhaustive()
    }
}

impl Required {
    /// The ticket in a 401's body, `{"code": 60003, "mfa": {"ticket", "methods": [{"type", ..}]}}`.
    /// Methods Godwit doesn't know are dropped. A TOTP entry that allows
    /// backup codes (`backup_codes_allowed`) also offers the backup method.
    pub(crate) fn from_body(body: &Value) -> Option<Self> {
        let mfa = &body["mfa"];
        let ticket = mfa["ticket"].as_str()?.to_owned();
        let mut methods = Vec::new();
        for entry in mfa["methods"].as_array().into_iter().flatten() {
            let Some(method) = entry["type"].as_str().and_then(Method::from_name) else {
                continue;
            };
            if !methods.contains(&method) {
                methods.push(method);
            }
            if method == Method::Totp && entry["backup_codes_allowed"] == true && !methods.contains(&Method::Backup) {
                methods.push(Method::Backup);
            }
        }
        Some(Self { ticket, methods })
    }
}

impl std::fmt::Display for Required {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Discord asks to confirm with two-factor authentication.")
    }
}

impl std::error::Error for Required {}

tokio::task_local! {
    /// The token `POST /mfa/finish` gave, for the one request that goes
    /// again after confirming.
    static TOKEN: String;
}

/// Runs `work` with its requests carrying `token`; the token is gone when it ends.
async fn with_token<F: Future>(token: String, work: F) -> F::Output {
    TOKEN.scope(token, work).await
}

#[cfg(test)]
pub(crate) async fn with_token_for_tests<F: Future>(token: String, work: F) -> F::Output {
    with_token(token, work).await
}

/// The token the running request carries, if it goes again after confirming.
pub(crate) fn token() -> Option<String> {
    TOKEN.try_with(Clone::clone).ok()
}

/// What the person did with a prompt.
pub(crate) enum Answer {
    /// Confirmed: the token to send the request again with.
    Token(String),
    Cancelled,
}

/// The prompts waiting for the person, by id: the ticket each confirms and
/// the request waiting on it.
#[derive(Default)]
pub(crate) struct Prompts {
    next: AtomicU64,
    waiting: Mutex<HashMap<u64, Waiting>>,
}

struct Waiting {
    ticket: String,
    answer: oneshot::Sender<Answer>,
}

impl Prompts {
    /// Waits on a new prompt for `ticket`, by its id.
    fn ask(&self, ticket: String) -> (u64, oneshot::Receiver<Answer>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        self.lock().insert(id, Waiting { ticket, answer });
        (id, answered)
    }

    /// The ticket of a prompt still waiting.
    pub(crate) fn ticket(&self, id: u64) -> Option<String> {
        self.lock().get(&id).map(|waiting| waiting.ticket.clone())
    }

    /// Ends a prompt with the person's answer.
    pub(crate) fn answer(&self, id: u64, answer: Answer) {
        if let Some(waiting) = self.lock().remove(&id) {
            let _ = waiting.answer.send(answer);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Waiting>> {
        self.waiting.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Runs `work`; if Discord holds it back for two-factor authentication,
/// raises a prompt with `ask` (its id and the methods that can be used),
/// waits for the person, and runs `work` once more with the token. A refusal
/// of that second run is final: nothing asks twice.
pub(crate) async fn stepping_up<T, Fut>(
    prompts: &Prompts,
    ask: impl Fn(u64, Vec<Method>),
    mut work: impl FnMut() -> Fut,
) -> Result<T>
where
    Fut: Future<Output = Result<T>>,
{
    stepping_up_for(prompts, ask, PROMPT_LIFE, &mut work).await
}

async fn stepping_up_for<T, Fut>(
    prompts: &Prompts,
    ask: impl Fn(u64, Vec<Method>),
    life: Duration,
    work: &mut impl FnMut() -> Fut,
) -> Result<T>
where
    Fut: Future<Output = Result<T>>,
{
    let error = match work().await {
        Err(error) => error,
        done => return done,
    };
    let Some(required) = error.downcast_ref::<Required>() else {
        return Err(error);
    };
    let usable: Vec<Method> = required.methods.iter().copied().filter(|m| m.typed()).collect();
    if usable.is_empty() {
        bail!(
            "Discord asks for a passkey or a text message to confirm this, which Godwit can't do yet. Confirm it in Discord's own app."
        );
    }
    let (id, answered) = prompts.ask(required.ticket.clone());
    ask(id, usable);
    let token = match tokio::time::timeout(life, answered).await {
        Ok(Ok(Answer::Token(token))) => token,
        Ok(_) => bail!("You didn't confirm, so nothing was changed."),
        Err(_) => {
            prompts.answer(id, Answer::Cancelled);
            bail!("The confirmation took too long, so nothing was changed. Try again.");
        }
    };
    with_token(token, work()).await.map_err(|error| {
        if error.is::<Required>() {
            anyhow!("Discord still asks for a confirmation. Try again.")
        } else {
            error
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, atomic::AtomicUsize};

    fn held(methods: Value) -> Value {
        json!({"code": 60003, "message": "Multi-factor authentication required",
               "mfa": {"ticket": "secret-ticket", "methods": methods}})
    }

    #[test]
    fn a_ticket_keeps_the_methods_godwit_knows_in_discords_order() {
        let body = held(json!([
            {"type": "webauthn", "challenge": "{}"},
            {"type": "totp"},
            {"type": "carrier-pigeon"},
            {"type": "backup"},
            {"type": "password"},
        ]));
        let required = Required::from_body(&body).unwrap();
        assert_eq!(
            required.methods,
            [Method::Webauthn, Method::Totp, Method::Backup, Method::Password]
        );
        assert!(Required::from_body(&json!({"code": 60003})).is_none());
    }

    #[test]
    fn totp_that_allows_backup_codes_offers_them_once() {
        let body = held(json!([{"type": "totp", "backup_codes_allowed": true}]));
        let required = Required::from_body(&body).unwrap();
        assert_eq!(required.methods, [Method::Totp, Method::Backup]);
        let both = held(json!([{"type": "totp", "backup_codes_allowed": true}, {"type": "backup"}]));
        assert_eq!(
            Required::from_body(&both).unwrap().methods,
            [Method::Totp, Method::Backup]
        );
    }

    #[test]
    fn the_ticket_never_shows_in_debug_or_display() {
        let required = Required::from_body(&held(json!([{"type": "totp"}]))).unwrap();
        assert!(!format!("{required:?}{required}").contains("secret-ticket"));
        assert_eq!(required.ticket, "secret-ticket");
    }

    #[test]
    fn a_refusal_with_the_code_becomes_the_typed_error() {
        let error = crate::api::refusal(reqwest::StatusCode::UNAUTHORIZED, &held(json!([{"type": "totp"}])));
        assert!(error.downcast_ref::<Required>().is_some());
        // The same code on another status is an ordinary refusal.
        let other = crate::api::refusal(reqwest::StatusCode::BAD_REQUEST, &held(json!([{"type": "totp"}])));
        assert!(other.downcast_ref::<Required>().is_none());
    }

    #[test]
    fn requests_carry_the_token_only_inside_its_scope() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            assert_eq!(token(), None);
            let inside = with_token("token".into(), async { token() }).await;
            assert_eq!(inside.as_deref(), Some("token"));
            assert_eq!(token(), None);
        });
    }

    type Asked = Arc<Mutex<Vec<(u64, Vec<Method>)>>>;
    type Attempt = std::pin::Pin<Box<dyn Future<Output = Result<u8>> + Send>>;

    /// A request that Discord holds back until the token is on it.
    fn guarded(calls: Arc<AtomicUsize>, accepted: &'static str) -> impl FnMut() -> Attempt {
        move || {
            let calls = calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::Relaxed);
                if token().as_deref() == Some(accepted) {
                    Ok(1)
                } else {
                    Err(anyhow!(
                        Required::from_body(&held(json!([{"type": "totp"}, {"type": "sms"}]))).unwrap()
                    ))
                }
            })
        }
    }

    #[tokio::test]
    async fn the_request_goes_again_once_with_the_token() {
        let prompts = Arc::new(Prompts::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let asked: Asked = Arc::default();
        let answerer = prompts.clone();
        let seen = asked.clone();
        let ask = move |id, methods: Vec<Method>| {
            seen.lock().unwrap().push((id, methods));
            // The ticket is what the finish call would use.
            assert_eq!(answerer.ticket(id).as_deref(), Some("secret-ticket"));
            answerer.answer(id, Answer::Token("mfa-token".into()));
        };
        let result = stepping_up(&prompts, ask, guarded(calls.clone(), "mfa-token")).await;
        assert_eq!(result.unwrap(), 1);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        // Text messages can't be typed in here; only totp is offered.
        assert_eq!(asked.lock().unwrap()[0].1, [Method::Totp]);
        assert_eq!(token(), None);
    }

    #[tokio::test]
    async fn a_refused_second_run_is_final_and_a_cancel_runs_nothing_more() {
        let prompts = Arc::new(Prompts::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let answerer = prompts.clone();
        // A token Discord doesn't take: the request is held back again.
        let ask = move |id, _| answerer.answer(id, Answer::Token("wrong".into()));
        let result = stepping_up(&prompts, ask, guarded(calls.clone(), "mfa-token")).await;
        assert!(result.unwrap_err().to_string().contains("still asks"));
        assert_eq!(calls.load(Ordering::Relaxed), 2, "one retry, never more");

        let calls = Arc::new(AtomicUsize::new(0));
        let answerer = prompts.clone();
        let ask = move |id, _| answerer.answer(id, Answer::Cancelled);
        let result = stepping_up(&prompts, ask, guarded(calls.clone(), "mfa-token")).await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn an_unanswered_prompt_expires() {
        let prompts = Prompts::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut work = guarded(calls.clone(), "mfa-token");
        let result = stepping_up_for(&prompts, |_, _| {}, Duration::from_millis(10), &mut work).await;
        assert!(result.unwrap_err().to_string().contains("too long"));
        assert!(prompts.ticket(0).is_none());
    }

    #[tokio::test]
    async fn other_errors_pass_through_untouched() {
        let prompts = Prompts::default();
        let result: Result<u8> =
            stepping_up(&prompts, |_, _| panic!("no prompt"), || async { Err(anyhow!("boom")) }).await;
        assert_eq!(result.unwrap_err().to_string(), "boom");
    }
}

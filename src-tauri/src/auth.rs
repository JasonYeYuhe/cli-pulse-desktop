//! Supabase email OTP sign-in for the Tauri desktop (v0.3.0).
//!
//! Three REST calls against Supabase GoTrue (auth.<project>.supabase.co):
//!   POST /auth/v1/otp     {email, create_user: true}        → 200, silent send
//!   POST /auth/v1/verify  {type: "email", email, token}      → {access_token, refresh_token, expires_in, user}
//!   POST /auth/v1/token?grant_type=refresh_token  {refresh_token} → {access_token, refresh_token, expires_in}
//!
//! This module is intentionally narrow: just enough to mint a session
//! that the caller will exchange for a desktop helper credential via
//! `register_desktop_helper` (defined in supabase.rs). Token persistence
//! is the caller's concern (lib.rs writes the refresh token to the OS
//! keychain via `keychain::store_refresh_token`).

use chrono::{DateTime, Duration, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::creds;

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    /// Supabase rate-limited the OTP send. The UI surfaces a "wait Ns"
    /// message to the user.
    #[error("rate limited (try again later)")]
    RateLimited,

    /// The 6-digit code didn't match. UI prompts user to re-enter.
    #[error("invalid OTP code")]
    InvalidCode,

    /// Refresh token is expired / revoked. UI shows session-expired
    /// banner and re-routes through OTP.
    #[error("refresh token rejected")]
    RefreshFailed,

    /// Anything else from GoTrue we don't have a typed branch for.
    /// Includes the HTTP status + raw body trim so the user (or
    /// support) can see what happened.
    #[error("auth error (HTTP {status}): {body}")]
    Other { status: u16, body: String },

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type AuthResult<T> = Result<T, AuthError>;

#[derive(Debug, Clone)]
pub struct AuthSession {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub user_id: Uuid,
    pub email: String,
}

fn client() -> AuthResult<Client> {
    let c = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("cli-pulse-desktop/", env!("CARGO_PKG_VERSION")))
        .build()?;
    Ok(c)
}

fn auth_url(path: &str) -> String {
    format!("{}/auth/v1/{}", creds::supabase_url(), path)
}

#[derive(Serialize)]
struct OtpRequest<'a> {
    email: &'a str,
    /// `true` so first-time-on-this-email signs the user up automatically;
    /// no separate "register" path needed. Same behavior as the iOS
    /// onboarding flow (`iOSLoginView.swift:280`).
    create_user: bool,
}

/// POST /auth/v1/otp — fire-and-forget OTP email send.
///
/// 200 = email queued (might still bounce; user has a "Resend" button).
/// 429 = rate-limited; UI shows specific copy.
/// Other 4xx with `error_code: invalid_email` etc → bubbles up as
/// `Other` with the body trim.
pub async fn send_otp(email: &str) -> AuthResult<()> {
    let body = OtpRequest {
        email,
        create_user: true,
    };
    let anon = creds::supabase_anon_key();
    let resp = client()?
        .post(auth_url("otp"))
        .header("apikey", &anon)
        .header("Authorization", format!("Bearer {anon}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = resp.status().as_u16();
    if status == 429 {
        return Err(AuthError::RateLimited);
    }
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::Other {
            status,
            body: body.chars().take(300).collect(),
        });
    }
    // We don't care about the success body — Supabase returns `{}`.
    let _ = resp.text().await;
    Ok(())
}

#[derive(Serialize)]
struct VerifyRequest<'a> {
    /// Always "email" for our flow. Supabase also accepts "sms" / "magiclink"
    /// but we don't expose those.
    #[serde(rename = "type")]
    otp_type: &'a str,
    email: &'a str,
    token: &'a str,
}

#[derive(Deserialize)]
struct VerifyResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    user: VerifyUser,
}

#[derive(Deserialize)]
struct VerifyUser {
    id: Uuid,
    email: Option<String>,
}

/// POST /auth/v1/verify — exchange the 6-digit code for a session.
///
/// 200 → session
/// 400 with `error_code: otp_expired` or `invalid_otp` → `InvalidCode`
/// 429 → `RateLimited` (spam-clicking verify with bad codes)
pub async fn verify_otp(email: &str, token: &str) -> AuthResult<AuthSession> {
    let body = VerifyRequest {
        otp_type: "email",
        email,
        token,
    };
    let anon = creds::supabase_anon_key();
    let resp = client()?
        .post(auth_url("verify"))
        .header("apikey", &anon)
        .header("Authorization", format!("Bearer {anon}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = resp.status().as_u16();
    if status == 429 {
        return Err(AuthError::RateLimited);
    }
    if status == 400 || status == 401 {
        // Treat client errors as InvalidCode unless the body suggests
        // something else specifically. Verifying with a wrong code is
        // by far the most common 4xx here.
        return Err(AuthError::InvalidCode);
    }
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::Other {
            status,
            body: body.chars().take(300).collect(),
        });
    }
    let parsed: VerifyResponse = resp.json().await?;
    Ok(AuthSession {
        expires_at: Utc::now() + Duration::seconds(parsed.expires_in),
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        user_id: parsed.user.id,
        email: parsed.user.email.unwrap_or_else(|| email.to_string()),
    })
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    user: VerifyUser,
}

/// POST /auth/v1/token?grant_type=refresh_token — rotate the session.
///
/// The refresh token is one-time-use; Supabase rotates and the response
/// carries the new one. Caller is responsible for persisting.
///
/// On 4xx → `RefreshFailed` (caller clears keychain + shows
/// session-expired banner).
pub async fn refresh(refresh_token: &str) -> AuthResult<AuthSession> {
    let body = RefreshRequest { refresh_token };
    let anon = creds::supabase_anon_key();
    let resp = client()?
        .post(format!("{}?grant_type=refresh_token", auth_url("token")))
        .header("apikey", &anon)
        .header("Authorization", format!("Bearer {anon}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = resp.status().as_u16();
    if (400..500).contains(&status) {
        return Err(AuthError::RefreshFailed);
    }
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::Other {
            status,
            body: body.chars().take(300).collect(),
        });
    }
    let parsed: RefreshResponse = resp.json().await?;
    Ok(AuthSession {
        expires_at: Utc::now() + Duration::seconds(parsed.expires_in),
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        user_id: parsed.user.id,
        email: parsed.user.email.unwrap_or_default(),
    })
}

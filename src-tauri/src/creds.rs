//! Supabase project credentials for the CLI Pulse backend.
//!
//! Baked-in defaults match the production project (Tokyo region,
//! project ref `gkjwsxotmwrgqsvfijzs`). Overridable via env vars at
//! runtime for staging / dev / local Supabase:
//!
//!   CLI_PULSE_SUPABASE_URL       — full https URL, no trailing slash
//!   CLI_PULSE_SUPABASE_ANON_KEY  — anon JWT
//!
//! The anon key is public by design (it's on iOS/macOS/Android too)
//! — all mutating RPCs authenticate via `(device_id, helper_secret)`
//! pairs, validated inside SECURITY DEFINER functions. See
//! `backend/supabase/helper_rpc.sql` in the main repo.

use std::env;

const DEFAULT_SUPABASE_URL: &str = "https://gkjwsxotmwrgqsvfijzs.supabase.co";
const DEFAULT_SUPABASE_ANON_KEY: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6ImdrandzeG90bXdyZ3FzdmZpanpzIiwicm9sZSI6ImFub24iLCJpYXQiOjE3NzQ2OTAzNzAsImV4cCI6MjA5MDI2NjM3MH0.uPHYnh0psr2-KQynBw2NiQZOhz5eZiEaWpfCwdXrNQM";

pub fn supabase_url() -> String {
    env::var("CLI_PULSE_SUPABASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SUPABASE_URL.to_string())
}

pub fn supabase_anon_key() -> String {
    env::var("CLI_PULSE_SUPABASE_ANON_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SUPABASE_ANON_KEY.to_string())
}

//! OS keyring integration for API secrets.
//!
//! Wraps `keyring` (Windows Credential Manager / macOS Keychain / libsecret)
//! with a Forge-scoped service name so all secrets live under one entry.
//! Callers reference secrets by short names (`GROQ_API_KEY`, `OPENAI_API_KEY`)
//! that mirror the environment variable names — the composition root falls
//! back to env vars when a keyring entry is missing, so existing setups
//! keep working.

use keyring::Entry;

const SERVICE: &str = "com.sarthak.forgeos";

pub fn set(name: &str, value: &str) -> Result<(), String> {
    let entry = Entry::new(SERVICE, name).map_err(|e| format!("entry: {e}"))?;
    entry.set_password(value).map_err(|e| format!("set: {e}"))
}

pub fn get(name: &str) -> Option<String> {
    let entry = Entry::new(SERVICE, name).ok()?;
    entry.get_password().ok()
}

pub fn has(name: &str) -> bool {
    get(name).is_some()
}

pub fn delete(name: &str) -> Result<(), String> {
    let entry = Entry::new(SERVICE, name).map_err(|e| format!("entry: {e}"))?;
    entry.delete_credential().map_err(|e| format!("delete: {e}"))
}

/// Resolve a secret: prefer env var (so a shell-configured key wins), fall
/// back to the OS keyring. Returns None if neither is set / non-empty.
pub fn resolve(name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name) {
        if !v.trim().is_empty() { return Some(v); }
    }
    get(name).filter(|v| !v.trim().is_empty())
}

/// List well-known secret names that Forge OS uses. Handy for a settings
/// panel that wants to show "set / not set" badges without probing each
/// backend individually.
pub const KNOWN_SECRETS: &[&str] = &[
    "GROQ_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "ANTHROPIC_API_KEY",
];

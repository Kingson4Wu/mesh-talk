//! TOFU (trust-on-first-use) contact verification, layered on top of the existing
//! fingerprints — it adds NO new crypto and changes no key material. We persist, per
//! contact account, the fingerprint we first saw and whether the user has verified it
//! out-of-band (compared the safety number in person / over a trusted channel).
//!
//! Why key by `account_id`: the UI groups a multi-device contact under one account, so
//! that is the stable contact identity. The fingerprint we pin is the contact's current
//! device `user_id`. If a KNOWN account later presents a DIFFERENT device fingerprint
//! (a new/replaced device — or, in the worst case, an attacker who has taken over the
//! account label), we surface a `fingerprint_changed` flag so the UI can warn loudly
//! and the user can re-verify. The pinned value is the FIRST-seen fingerprint; we never
//! silently overwrite it.

use crate::commands::CommandError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Manager;

/// What we persist about one contact account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustRecord {
    /// The device fingerprint (`user_id`) we first saw presenting this account.
    pub first_seen_fingerprint: String,
    /// True once the user has confirmed the safety number out-of-band.
    #[serde(default)]
    pub verified: bool,
}

/// The on-disk shape: `account_id` -> record.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct TrustFile {
    #[serde(default)]
    contacts: HashMap<String, TrustRecord>,
}

/// Managed Tauri state: the in-memory trust table behind a `Mutex`, seeded from disk
/// on setup and written through on every change.
#[derive(Clone, Default)]
pub struct TrustState(Arc<Mutex<TrustFile>>);

/// Trust state returned to the UI for one contact, including the live change signal.
#[derive(Clone, Debug, Serialize)]
pub struct TrustInfo {
    pub account_id: String,
    /// The fingerprint we pinned on first contact (the one the user should verify).
    pub first_seen_fingerprint: String,
    pub verified: bool,
    /// True if `current_fingerprint` differs from the pinned `first_seen_fingerprint`
    /// — a known contact's fingerprint changed; the UI must warn.
    pub fingerprint_changed: bool,
    /// Whether we have any record for this contact yet (false = brand-new contact).
    pub known: bool,
}

fn trust_path<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<std::path::PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("trust.json"))
}

/// Load the persisted trust table (empty if missing/unreadable) into managed state.
pub fn load_into_state<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &TrustState) {
    let loaded = trust_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<TrustFile>(&s).ok())
        .unwrap_or_default();
    *state.0.lock().unwrap() = loaded;
}

/// Persist the trust table to disk (best-effort; logged on failure).
fn save<R: tauri::Runtime>(app: &tauri::AppHandle<R>, file: &TrustFile) {
    let Some(path) = trust_path(app) else {
        log::warn!("No app config dir; trust table not persisted");
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(file) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("Failed to persist trust table: {e}");
            }
        }
        Err(e) => log::warn!("Failed to serialize trust table: {e}"),
    }
}

/// Fetch trust state for a contact account, given its CURRENT device fingerprint.
///
/// TOFU: if we have never seen this account, we pin `current_fingerprint` as the
/// first-seen value (and persist) so subsequent calls can detect a change. A brand-new
/// contact comes back `known: false, verified: false, fingerprint_changed: false`.
#[tauri::command]
pub fn get_trust(
    app: tauri::AppHandle,
    state: tauri::State<'_, TrustState>,
    account_id: String,
    current_fingerprint: String,
) -> Result<TrustInfo, CommandError> {
    let mut file = state.0.lock().unwrap();
    match file.contacts.get(&account_id).cloned() {
        Some(rec) => Ok(TrustInfo {
            account_id,
            fingerprint_changed: rec.first_seen_fingerprint != current_fingerprint,
            first_seen_fingerprint: rec.first_seen_fingerprint,
            verified: rec.verified,
            known: true,
        }),
        None => {
            // First use: pin the current fingerprint.
            file.contacts.insert(
                account_id.clone(),
                TrustRecord {
                    first_seen_fingerprint: current_fingerprint.clone(),
                    verified: false,
                },
            );
            let snapshot = file.clone();
            drop(file);
            save(&app, &snapshot);
            Ok(TrustInfo {
                account_id,
                first_seen_fingerprint: current_fingerprint,
                verified: false,
                fingerprint_changed: false,
                known: true,
            })
        }
    }
}

/// Mark a contact verified. The user is asserting they compared the safety number
/// out-of-band; we also (re-)pin `fingerprint` as the trusted first-seen value, so
/// verifying after a legitimate fingerprint change clears the warning.
#[tauri::command]
pub fn mark_verified(
    app: tauri::AppHandle,
    state: tauri::State<'_, TrustState>,
    account_id: String,
    fingerprint: String,
) -> Result<(), CommandError> {
    let snapshot = {
        let mut file = state.0.lock().unwrap();
        file.contacts.insert(
            account_id,
            TrustRecord {
                first_seen_fingerprint: fingerprint,
                verified: true,
            },
        );
        file.clone()
    };
    save(&app, &snapshot);
    Ok(())
}

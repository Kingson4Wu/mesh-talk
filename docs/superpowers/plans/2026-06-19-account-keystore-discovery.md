# Account Keystore + Discovery/Roster Grouping Implementation Plan (Multi-Device Plan 2)

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. SECURITY-CRITICAL (account↔device binding travels on the wire). Checkbox steps; full code inline.

**Goal:** Persist the account keypair to disk (alongside the device keystore), advertise each device's `account_id` + device certificate in its discovery announce, verify the binding on receipt, and let the roster group devices by account (`devices_of_account`).

**Architecture:** Builds on Plan 1's pure `identity::account` (`Account`, `AccountPublic`, `DeviceCertificate`). A new `identity::account_keystore` persists the `Account` secret using the same PBKDF2+AES-GCM primitives as the device keystore. `Announce` gains an optional `DeviceCertificate`; the device's announce signature is extended to **cover the account public key**, so an attacker cannot swap in a different (validly-self-signed) cert to re-home a victim's device under another account. `Roster` records each peer's `account_id` (from the verified cert) and exposes `devices_of_account`.

**Tech Stack:** Rust; `ed25519-dalek`, `sha2`, `aes-gcm`, `pbkdf2`, `bincode`, `serde`, `hex`. All already dependencies.

## Global Constraints

- **CPU/test discipline:** prefix cargo with `nice -n 10`; always `-- --test-threads=2`. Never run the full suite; scope to the touched module.
- **Branch:** `feat/redesign-phase0` (confirm with `git branch --show-current` before committing).
- **No secret in logs:** `Account` has no `Debug`; never log the account secret. Match existing module style exactly.
- **Fail closed:** all decoders reject trailing bytes / short input (mirror existing `decode`).

---

### Task 1: `identity::account_keystore` — persist the account keypair

**Files:**
- Create: `src-tauri/src/identity/account_keystore.rs`
- Modify: `src-tauri/src/identity/mod.rs` (register `pub mod account_keystore;`)

**Interfaces:**
- Consumes: `identity::account::Account` (`Account::generate`, `from_secret_bytes`, `secret_bytes`, `account_id`); `storage::encryption::{decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE}`; `storage::errors::StorageError`.
- Produces: `pub fn save(path: &Path, password: &str, account: &Account) -> Result<(), StorageError>`; `pub fn load(path: &Path, password: &str) -> Result<Account, StorageError>`; `pub fn load_or_create(path: &Path, password: &str) -> Result<Account, StorageError>`.

- [ ] **Step 1: write `identity/account_keystore.rs`**

```rust
//! Password-encrypted, on-disk keystore for an `Account` secret (the cross-device
//! Ed25519 account key). Separate file from the device keystore: the account key
//! is shared across a user's devices and is transferred at link time, so it has
//! its own lifecycle.
//!
//! Format: salt(16) || nonce(12) || AES-256-GCM ciphertext of the serialized
//! secret. Key derived from the password via PBKDF2 (see `storage::encryption`).

use crate::identity::account::Account;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use crate::storage::errors::StorageError;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct StoredAccount {
    ed25519_secret: [u8; 32],
}

/// Encrypt `account` with `password` and write it to `path` (creating parent
/// directories as needed).
pub fn save(path: &Path, password: &str, account: &Account) -> Result<(), StorageError> {
    let stored = StoredAccount {
        ed25519_secret: account.secret_bytes(),
    };
    let plaintext =
        bincode::serialize(&stored).map_err(|e| StorageError::Serialization(e.to_string()))?;

    let salt = generate_salt();
    let key = EncryptionKey::from_password(password, &salt)?;
    let (ciphertext, nonce) = encrypt_data(&plaintext, &key)?;

    let mut out = Vec::with_capacity(SALT_SIZE + NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| StorageError::DirectoryCreationFailed(parent.to_path_buf()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &out)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Read and decrypt the account keystore at `path` with `password`.
pub fn load(path: &Path, password: &str) -> Result<Account, StorageError> {
    let content = std::fs::read(path)?;
    if content.len() < SALT_SIZE + NONCE_SIZE {
        return Err(StorageError::Decryption(
            "account keystore file too short".to_string(),
        ));
    }
    let salt: [u8; SALT_SIZE] = content[0..SALT_SIZE]
        .try_into()
        .expect("length checked above");
    let nonce: [u8; NONCE_SIZE] = content[SALT_SIZE..SALT_SIZE + NONCE_SIZE]
        .try_into()
        .expect("length checked above");
    let ciphertext = &content[SALT_SIZE + NONCE_SIZE..];

    let key = EncryptionKey::from_password(password, &salt)?;
    let plaintext = decrypt_data(ciphertext, &nonce, &key)?;
    let stored: StoredAccount = bincode::deserialize(&plaintext)
        .map_err(|e| StorageError::Deserialization(e.to_string()))?;

    Ok(Account::from_secret_bytes(stored.ed25519_secret))
}

/// Load the account if present, otherwise generate a new one and save it.
pub fn load_or_create(path: &Path, password: &str) -> Result<Account, StorageError> {
    if path.exists() {
        load(path, password)
    } else {
        let account = Account::generate();
        save(path, password, &account)?;
        Ok(account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips_the_account() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");

        let original = Account::generate();
        save(&path, "correct horse battery staple", &original).expect("save");

        let loaded = load(&path, "correct horse battery staple").expect("load");
        assert_eq!(original.account_id(), loaded.account_id());
    }

    #[test]
    fn wrong_password_fails_to_decrypt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        save(&path, "right", &Account::generate()).expect("save");
        assert!(load(&path, "wrong").is_err());
    }

    #[test]
    fn load_or_create_is_stable_across_calls() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");

        let first = load_or_create(&path, "pw").expect("first");
        let second = load_or_create(&path, "pw").expect("second");
        assert_eq!(first.account_id(), second.account_id());
    }

    #[test]
    fn tampered_ciphertext_fails_to_load() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        save(&path, "pw", &Account::generate()).expect("save");

        let mut bytes = std::fs::read(&path).expect("read");
        bytes[28] ^= 0xFF; // flip a byte in the ciphertext region (after salt+nonce = 28)
        std::fs::write(&path, &bytes).expect("write tampered");
        assert!(load(&path, "pw").is_err());
    }

    #[test]
    fn truncated_file_fails_to_load() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        std::fs::write(&path, &[0u8; 10]).expect("write short");
        assert!(load(&path, "pw").is_err());
        std::fs::write(&path, &[0u8; 28]).expect("write 28-byte");
        assert!(load(&path, "pw").is_err());
    }

    #[test]
    fn loaded_account_can_certify() {
        use crate::identity::device::DeviceIdentity;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        save(&path, "pw", &Account::generate()).expect("save");

        let loaded = load(&path, "pw").expect("load");
        let device = DeviceIdentity::generate();
        assert!(loaded.certify(&device.public().ed25519_pub).verify());
    }
}
```

- [ ] **Step 2: register the module (`identity/mod.rs`)**

Add `pub mod account_keystore;` next to the other `pub mod`s (it already has `pub mod account;` and `pub mod keystore;`).

- [ ] **Step 3: test, clippy, fmt, commit**

```bash
cd src-tauri && nice -n 10 cargo test --lib identity::account_keystore -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/identity/account_keystore.rs src-tauri/src/identity/mod.rs
git commit -m "feat(identity): password-encrypted account keystore (multi-device plan 2)"
git status | head -1
```

Expected: all account_keystore tests PASS; clippy clean.

---

### Task 2: `Announce` carries the account↔device binding

**Files:**
- Modify: `src-tauri/src/discovery/announce.rs`

**Interfaces:**
- Consumes: `identity::account::{Account, DeviceCertificate}`; existing `DeviceIdentity`.
- Produces: new field `pub account_cert: Option<DeviceCertificate>` on `Announce`; `Announce::new_with_account(identity, account, name, tcp_port)`; `Announce::new_post_office_with_account(identity, account, name, tcp_port)`; `Announce::account_id(&self) -> Option<String>`. `Announce::new` / `new_post_office` keep their signatures (produce `account_cert: None`).

**Background:** today `signing_input` covers `user_id, ed25519_pub, x25519_pub, name, tcp_port, post_office`. We append the account binding. Without this, an attacker could take a victim's valid announce, attach `attacker_account.certify(victim_device_pub)` (a cert that *does* verify, since anyone can sign any public key), and the device signature would still pass — re-homing the victim's device under the attacker's account. Covering the account public key with the device signature closes this: the device must itself assert which account it belongs to.

- [ ] **Step 1: add the field and bump the wire version**

In `src-tauri/src/discovery/announce.rs`, update the imports, version, struct, and signing input.

Change the imports at the top:
```rust
use crate::identity::account::DeviceCertificate;
use crate::identity::account::Account;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use bincode::Options;
use serde::{Deserialize, Serialize};
```

Bump the version constant (the wire format changed):
```rust
const VERSION: u8 = 2;
```

Add the field to the struct (after `post_office`, before `sig`):
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announce {
    pub user_id: String,
    pub ed25519_pub: [u8; 32],
    pub x25519_pub: [u8; 32],
    pub name: String,
    pub tcp_port: u16,
    pub post_office: bool,
    /// The account this device claims membership in, proven by the account key's
    /// signature over this device's key. `None` for a device not yet linked to an
    /// account. The device's own `sig` covers the bound account key (below), so
    /// this cert cannot be swapped for one minting the device under another account.
    pub account_cert: Option<DeviceCertificate>,
    pub sig: Vec<u8>,
}
```

- [ ] **Step 2: extend `signing_input` to cover the account public key**

Replace the `signing_input` function so it takes the optional account public key and length-prefixes it (presence is unambiguous: a 0-length prefix means "no account"):

```rust
/// Length-prefixed, domain-separated bytes the announce signs over (everything
/// except `sig`). Length prefixes make the concatenation unambiguous. The account
/// public key is covered so a device's signature commits to which account it
/// claims — preventing an attacker from swapping in a different (validly-signed)
/// certificate to re-home this device under another account.
fn signing_input(
    user_id: &str,
    ed25519_pub: &[u8; 32],
    x25519_pub: &[u8; 32],
    name: &str,
    tcp_port: u16,
    post_office: bool,
    account_pub: Option<&[u8; 32]>,
) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(ANNOUNCE_DOMAIN);
    v.extend_from_slice(&(user_id.len() as u32).to_be_bytes());
    v.extend_from_slice(user_id.as_bytes());
    v.extend_from_slice(ed25519_pub);
    v.extend_from_slice(x25519_pub);
    v.extend_from_slice(&(name.len() as u32).to_be_bytes());
    v.extend_from_slice(name.as_bytes());
    v.extend_from_slice(&tcp_port.to_be_bytes());
    v.push(post_office as u8);
    match account_pub {
        Some(pk) => {
            v.extend_from_slice(&(pk.len() as u32).to_be_bytes()); // 32
            v.extend_from_slice(pk);
        }
        None => v.extend_from_slice(&0u32.to_be_bytes()),
    }
    v
}
```

- [ ] **Step 3: update constructors and `verify`**

Replace the `impl Announce` block's constructors (`new`, `new_post_office`, `new_with_role`) and `verify` with the version below. `new`/`new_post_office` delegate with `account: None`; new `_with_account` variants pass the account. `new_with_role` gains an `account: Option<&Account>` parameter.

```rust
impl Announce {
    /// Build and sign a normal (non-post-office) announce for `identity`, with no
    /// account binding.
    pub fn new(identity: &DeviceIdentity, name: impl Into<String>, tcp_port: u16) -> Self {
        Self::new_with_role(identity, name, tcp_port, false, None)
    }

    /// Build and sign a post-office announce for `identity`, with no account binding.
    pub fn new_post_office(
        identity: &DeviceIdentity,
        name: impl Into<String>,
        tcp_port: u16,
    ) -> Self {
        Self::new_with_role(identity, name, tcp_port, true, None)
    }

    /// Build and sign a normal announce that advertises `account` (the device is
    /// certified by the account and the device signature commits to the account key).
    pub fn new_with_account(
        identity: &DeviceIdentity,
        account: &Account,
        name: impl Into<String>,
        tcp_port: u16,
    ) -> Self {
        Self::new_with_role(identity, name, tcp_port, false, Some(account))
    }

    /// Build and sign a post-office announce that advertises `account`.
    pub fn new_post_office_with_account(
        identity: &DeviceIdentity,
        account: &Account,
        name: impl Into<String>,
        tcp_port: u16,
    ) -> Self {
        Self::new_with_role(identity, name, tcp_port, true, Some(account))
    }

    fn new_with_role(
        identity: &DeviceIdentity,
        name: impl Into<String>,
        tcp_port: u16,
        post_office: bool,
        account: Option<&Account>,
    ) -> Self {
        let public = identity.public();
        let user_id = public.user_id();
        let name = name.into();
        let account_cert = account.map(|a| a.certify(&public.ed25519_pub));
        let account_pub = account_cert.as_ref().map(|c| &c.account_ed25519_pub);
        let sig = identity
            .sign(&signing_input(
                &user_id,
                &public.ed25519_pub,
                &public.x25519_pub,
                &name,
                tcp_port,
                post_office,
                account_pub,
            ))
            .to_vec();
        Announce {
            user_id,
            ed25519_pub: public.ed25519_pub,
            x25519_pub: public.x25519_pub,
            name,
            tcp_port,
            post_office,
            account_cert,
            sig,
        }
    }

    /// True if the announce is internally consistent and authentically signed:
    /// `user_id` is the fingerprint of `ed25519_pub`; `sig` verifies (over all
    /// fields including `post_office` and the bound account key); and, if an account
    /// cert is present, it certifies THIS device under the bound account.
    pub fn verify(&self) -> bool {
        if self.user_id != PublicIdentity::user_id_from(&self.ed25519_pub) {
            return false;
        }
        // If a cert is present it must be for THIS device and validly account-signed.
        if let Some(cert) = &self.account_cert {
            if cert.device_ed25519_pub != self.ed25519_pub || !cert.verify() {
                return false;
            }
        }
        let account_pub = self.account_cert.as_ref().map(|c| &c.account_ed25519_pub);
        let Ok(sig): Result<[u8; 64], _> = self.sig.as_slice().try_into() else {
            return false;
        };
        DeviceIdentity::verify(
            &self.ed25519_pub,
            &signing_input(
                &self.user_id,
                &self.ed25519_pub,
                &self.x25519_pub,
                &self.name,
                self.tcp_port,
                self.post_office,
                account_pub,
            ),
            &sig,
        )
    }

    /// The advertised public identity (Ed25519 + X25519 keys).
    pub fn public(&self) -> PublicIdentity {
        PublicIdentity {
            ed25519_pub: self.ed25519_pub,
            x25519_pub: self.x25519_pub,
        }
    }

    /// The account id this device is bound to, if it advertises a (valid-shaped)
    /// cert. Only meaningful once `verify()` has returned true.
    pub fn account_id(&self) -> Option<String> {
        self.account_cert.as_ref().map(|c| c.account_id())
    }
}
```

- [ ] **Step 4: add tests for the account binding (append inside the existing `mod tests`)**

```rust
    #[test]
    fn account_announce_round_trips_and_verifies() {
        use crate::identity::account::Account;
        let id = DeviceIdentity::generate();
        let acct = Account::generate();
        let a = Announce::new_with_account(&id, &acct, "Alice", 4000);
        assert!(a.verify());
        assert_eq!(a.account_id(), Some(acct.account_id()));
        let back = decode(&encode(&a)).expect("decodes");
        assert_eq!(back, a);
        assert!(back.verify());
        assert_eq!(back.account_id(), Some(acct.account_id()));
    }

    #[test]
    fn no_account_announce_has_no_account_id() {
        let id = DeviceIdentity::generate();
        let a = Announce::new(&id, "Alice", 4000);
        assert!(a.verify());
        assert_eq!(a.account_id(), None);
    }

    #[test]
    fn swapping_in_a_foreign_account_cert_fails_verify() {
        // The attack: take a victim's account announce, replace the cert with one
        // an attacker validly minted over the victim's device key. cert.verify()
        // passes, but the device sig committed to the original account key → fail.
        use crate::identity::account::Account;
        let victim_device = DeviceIdentity::generate();
        let victim_account = Account::generate();
        let attacker_account = Account::generate();

        let mut a = Announce::new_with_account(&victim_device, &victim_account, "Victim", 4000);
        // Attacker can sign the victim's device key under their own account:
        let forged = attacker_account.certify(&victim_device.public().ed25519_pub);
        assert!(forged.verify()); // the cert itself is valid…
        a.account_cert = Some(forged); // …but the device never signed THIS account
        assert!(!a.verify());
    }

    #[test]
    fn a_cert_for_a_different_device_fails_verify() {
        use crate::identity::account::Account;
        let id = DeviceIdentity::generate();
        let other_device = DeviceIdentity::generate();
        let acct = Account::generate();
        let mut a = Announce::new_with_account(&id, &acct, "Alice", 4000);
        // Cert certifies a different device than the one announcing.
        a.account_cert = Some(acct.certify(&other_device.public().ed25519_pub));
        assert!(!a.verify());
    }

    #[test]
    fn stripping_the_account_cert_fails_verify() {
        // Removing the cert from an account-bound announce changes the signed
        // account key (Some → None) → device sig no longer matches.
        use crate::identity::account::Account;
        let id = DeviceIdentity::generate();
        let acct = Account::generate();
        let mut a = Announce::new_with_account(&id, &acct, "Alice", 4000);
        a.account_cert = None;
        assert!(!a.verify());
    }
```

- [ ] **Step 5: test, clippy, fmt, commit**

```bash
cd src-tauri && nice -n 10 cargo test --lib discovery::announce -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/discovery/announce.rs
git commit -m "feat(discovery): announce advertises account cert; device sig covers the account key (multi-device plan 2)"
git status | head -1
```

Expected: all announce tests PASS (the four new ones plus the existing 13); clippy clean.

---

### Task 3: `Roster` groups devices by account

**Files:**
- Modify: `src-tauri/src/discovery/roster.rs`

**Interfaces:**
- Consumes: `Announce::account_id() -> Option<String>` (Task 2); existing `Announce::{verify, public}`.
- Produces: new field `pub account_id: Option<String>` on `PeerRecord`; `Roster::devices_of_account(&self, account_id: &str) -> Vec<PublicIdentity>`.

- [ ] **Step 1: add the field to `PeerRecord` and populate it in `update`**

In `src-tauri/src/discovery/roster.rs`, add the field to the struct:
```rust
#[derive(Debug, Clone)]
pub struct PeerRecord {
    pub public: PublicIdentity,
    pub addr: SocketAddr,
    pub name: String,
    pub post_office: bool,
    /// The account this device belongs to, from its verified cert (`None` if the
    /// device advertised no account). Devices sharing an `account_id` are the same
    /// user's devices.
    pub account_id: Option<String>,
    pub last_seen: Instant,
}
```

In `update`, set it from the (already-verified) announce — insert `account_id: announce.account_id(),` in the `PeerRecord { .. }` literal (after `post_office: announce.post_office,`):
```rust
        self.peers.insert(
            announce.user_id.clone(),
            PeerRecord {
                public: announce.public(),
                addr: SocketAddr::new(source_ip, announce.tcp_port),
                name: announce.name.clone(),
                post_office: announce.post_office,
                account_id: announce.account_id(),
                last_seen: Instant::now(),
            },
        );
```

- [ ] **Step 2: add `devices_of_account`**

Add this method to `impl Roster` (e.g. after `post_offices`):
```rust
    /// The public identities of all known devices belonging to `account_id`.
    /// Empty if none are currently known. This is how an account resolves to the
    /// set of endpoints a message must fan out to.
    pub fn devices_of_account(&self, account_id: &str) -> Vec<PublicIdentity> {
        self.peers
            .values()
            .filter(|r| r.account_id.as_deref() == Some(account_id))
            .map(|r| r.public.clone())
            .collect()
    }
```

- [ ] **Step 3: add tests (append inside the existing `mod tests`)**

```rust
    #[test]
    fn groups_two_devices_under_one_account() {
        use crate::identity::account::Account;
        let account = Account::generate();
        let dev_a = DeviceIdentity::generate();
        let dev_b = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(
            &Announce::new_with_account(&dev_a, &account, "A", 4000),
            ip(),
            "self",
        );
        roster.update(
            &Announce::new_with_account(&dev_b, &account, "B", 4001),
            ip(),
            "self",
        );

        let devices = roster.devices_of_account(&account.account_id());
        assert_eq!(devices.len(), 2);
        assert!(devices.contains(&dev_a.public()));
        assert!(devices.contains(&dev_b.public()));
    }

    #[test]
    fn does_not_group_devices_of_a_different_account() {
        use crate::identity::account::Account;
        let mine = Account::generate();
        let theirs = Account::generate();
        let my_dev = DeviceIdentity::generate();
        let their_dev = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(
            &Announce::new_with_account(&my_dev, &mine, "Mine", 4000),
            ip(),
            "self",
        );
        roster.update(
            &Announce::new_with_account(&their_dev, &theirs, "Theirs", 4001),
            ip(),
            "self",
        );

        assert_eq!(roster.devices_of_account(&mine.account_id()).len(), 1);
        assert_eq!(roster.devices_of_account(&theirs.account_id()).len(), 1);
        assert!(roster.devices_of_account("unknown-account").is_empty());
    }

    #[test]
    fn peer_without_account_is_not_grouped() {
        let dev = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(&Announce::new(&dev, "Solo", 4000), ip(), "self");
        let rec = roster.get(&dev.public().user_id()).expect("recorded");
        assert_eq!(rec.account_id, None);
    }
```

- [ ] **Step 4: test, clippy, fmt, commit**

```bash
cd src-tauri && nice -n 10 cargo test --lib discovery::roster -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/discovery/roster.rs
git commit -m "feat(discovery): roster groups devices by account; devices_of_account (multi-device plan 2)"
git status | head -1
```

Expected: roster tests PASS (3 new + existing); clippy clean.

---

### Task 4: Wire the account into the node binary and the desktop runtime

**Files:**
- Modify: `src-tauri/src/bin/mesh-talk-node.rs` (both the normal-node `main` and `run_post_office`)
- Modify: `src-tauri/src/node/runtime.rs` (`RedesignRuntime::start` + add `account_id()` accessor)

**Interfaces:**
- Consumes: `identity::account_keystore::load_or_create` (Task 1); `Announce::{new_with_account, new_post_office_with_account}` (Task 2).
- Produces: announces emitted by the real app/CLI now carry the account binding; `RedesignRuntime::account_id(&self) -> String`.

**Background:** the account secret lives in a sibling keystore file next to the device `identity.keystore`. In the CLI that's `data_dir/account.keystore` (data_dir = parent of `--keystore`). In the desktop runtime it's `dir.join("account.keystore")` (same per-user dir as `identity.keystore`). Note: `RedesignRuntime::start`'s existing `account_id` *parameter* is the host-app namespace string for the data dir and is unrelated to this cryptographic account — keep it; add a separate accessor for the crypto account id.

- [ ] **Step 1: CLI normal-node path (`bin/mesh-talk-node.rs` `main`)**

After the device identity is loaded (the `let user_id = identity.public().user_id();` line, ~line 97) and BEFORE the announce is built, load-or-create the account. Derive the account path from the keystore path's parent:

```rust
    // Persistent cross-device account key (sibling of the device keystore).
    let account_path = std::path::Path::new(&args.keystore)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("account.keystore");
    let account =
        mesh_talk::identity::account_keystore::load_or_create(&account_path, &args.password)?;
```

Then change the announce construction (currently `Announce::new(&identity, args.name.clone(), tcp_port)`):
```rust
    let announce = Announce::new_with_account(&identity, &account, args.name.clone(), tcp_port);
```

- [ ] **Step 2: CLI post-office path (`bin/mesh-talk-node.rs` `run_post_office`)**

Locate the post-office announce (`let announce = Announce::new_post_office(&identity, args.name.clone(), tcp_port);`, ~line 224). Mirror Step 1: load-or-create the account from the sibling path just before it, then:
```rust
    let announce =
        Announce::new_post_office_with_account(&identity, &account, args.name.clone(), tcp_port);
```
(Reuse the same `account_path` derivation snippet from Step 1 — `run_post_office` has its own `identity`/`args` in scope.)

- [ ] **Step 3: desktop runtime (`node/runtime.rs`)**

Add the import near the other identity import:
```rust
use crate::identity::account_keystore;
```

In `start`, after `let self_uid = identity.public().user_id();` (~line 89), load-or-create the account in the per-user dir and capture its id:
```rust
        let account = account_keystore::load_or_create(&dir.join("account.keystore"), password)
            .map_err(|e| RuntimeError::Open(e.into()))?;
        let account_id = account.account_id();
```

Change the announce (currently `Announce::new(&identity, display_name.to_string(), tcp_port)`):
```rust
        let announce =
            Announce::new_with_account(&identity, &account, display_name.to_string(), tcp_port);
```

Add `account_id` to the `RedesignRuntime` struct and the returned literal:
```rust
pub struct RedesignRuntime {
    node: Arc<Node>,
    roster: Arc<Mutex<Roster>>,
    user_id: String,
    account_id: String,
    tasks: Vec<JoinHandle<()>>,
}
```
```rust
        Ok(RedesignRuntime {
            node,
            roster,
            user_id: self_uid,
            account_id,
            tasks,
        })
```

Add the accessor near `user_id()`:
```rust
    /// This node's cryptographic account id (stable across devices once linking
    /// is wired; today each device generates its own account on first run).
    pub fn account_id(&self) -> &str {
        &self.account_id
    }
```

- [ ] **Step 4: extend the runtime test to assert account persistence**

In `node/runtime.rs` `mod tests`, the existing `start_opens_a_node_and_exposes_its_identity` reopens the runtime as `runtime2`. Add, after the existing `assert_eq!(runtime.user_id(), runtime2.user_id());`:
```rust
        // The cryptographic account is created on disk and stable across reopen.
        assert_eq!(runtime.account_id().len(), 32); // account_id is 32 hex chars
        assert!(node_dir.join("account.keystore").exists());
        assert_eq!(runtime.account_id(), runtime2.account_id());
```

- [ ] **Step 5: build, test, clippy, fmt, commit**

```bash
cd src-tauri && nice -n 10 cargo build --bins --lib 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo test --lib node::runtime -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/bin/mesh-talk-node.rs src-tauri/src/node/runtime.rs
git commit -m "feat(node): CLI + desktop runtime advertise the account binding (multi-device plan 2)"
git status | head -1
```

Expected: build OK; runtime test PASS; clippy clean.

---

## Notes for the reviewer

- **Delivered:** the account keypair is now persisted (its own encrypted keystore), advertised in discovery announces via a device certificate, verified on receipt with the device signature **committing to the bound account key**, and the roster groups devices by `account_id` (`devices_of_account`). This is the discovery/identity substrate Plan 3 (account-addressed fan-out + self-sync) builds on.
- **Reviewer checks:**
  - The account-binding attack is closed: `signing_input` covers the account public key, so swapping in a foreign-but-valid cert, stripping the cert, or pointing it at another device all fail `verify` (the four new announce tests). Confirm `verify` rejects a cert whose `device_ed25519_pub` ≠ the announcing device.
  - Account keystore reuses the vetted `storage::encryption` primitives; `Account` has no `Debug`; the secret is never logged.
  - `Announce::new` / `new_post_office` are unchanged in signature (≈20 existing call sites + rigs keep compiling, account `None`).
  - Wire `VERSION` bumped 1→2 (the struct layout changed); decode still fail-closed on trailing bytes.
  - `account_id()` on the announce/roster is only meaningful after `verify()`.
- **Deferred to later plans:** account-addressed `send_to_account` fan-out over per-device ratchets + self-sync to own devices (Plan 3); the device-linking pairing flow that transfers the account secret + the "link a device" UI (Plan 4). Today each device generates its *own* account on first run — they are not yet the *same* account across devices; that unification arrives with linking (Plan 4).

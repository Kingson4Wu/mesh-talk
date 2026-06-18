# Multi-Device Design (Phase 3)

**Status:** Final Phase 3 item. Architecture chosen by engineering judgment per the standing directive (professional/reliable/clean). Builds on the per-identity Double Ratchet (DM + channel), which is complete + verified.

## Goal

Let one user run Mesh-Talk on several devices under one account: messages reach all of the user's devices, and the user's own devices stay in sync. Preserve forward secrecy (each device pair is its own ratchet endpoint).

## The architecture: account keypair + per-device identities + fan-out

The ratchet is per-device-pair (one ratchet endpoint per device), so two devices CANNOT share one identity (their ratchet state would diverge). Resolution (Signal-style):

- **Account keypair** (Ed25519), shared across the user's devices. `account_id` = hash of the account public key — this is the user's stable, cross-device handle. Conversations are addressed to an `account_id`, not a device.
- **Per-device identity** = the existing `DeviceIdentity` (its own Ed25519+X25519). Each device is an independent ratchet endpoint.
- **Device certificate** = the account key's signature over a device's public key. A device advertises `(device PublicIdentity, account_id, device certificate)`; any peer verifies the cert against the account key → knows which account a device belongs to. (A device proves it can act for the account by holding the account secret, shared at link time.)
- **Roster groups devices by account.** "Account X" resolves to the set of its currently-known device `PublicIdentity`s.
- **Fan-out send:** sending to an account seals + delivers to EACH known device of that account via that device's own ratchet session. **Self-sync:** the sender also fans out to its OWN other devices (a copy of what it sent), so all of a user's devices show the same history.
- **Dedup:** the same logical message arriving at a device once per session is normal (each device-pair is distinct); a device dedups by the message's content/event id as today (`emitted`).

## Device linking (provisioning a second device)

The new device generates its own `DeviceIdentity`. An existing (authorized) device transfers the **account secret** to the new device over a secure channel (v1: a one-time pairing code / the existing Noise transport between the two devices on the LAN), and issues it a device certificate. The new device then advertises under the account. (v1 keeps it simple: account secret shared to linked devices so any can link more + sign; a primary-only model is a later refinement.)

## Decomposition

1. **Account identity + device certificates** (`identity::account`, pure crypto): `Account` (Ed25519 keypair), `DeviceCertificate` (account-signed device key) + verify. Exhaustively tested. No I/O. **(This plan.)**
2. **Account keystore + discovery/roster grouping:** persist the account keypair (extend the keystore); advertise `account_id` + cert in announces; verify on receipt; roster exposes `devices_of_account(account_id) -> Vec<PublicIdentity>`.
3. **Account-addressed messaging + fan-out + self-sync:** `send_to_account` fans out over per-device ratchets + self-syncs to own devices; receive surfaces by account; history keyed by account.
4. **Device linking flow + UI:** pairing handshake (account-secret transfer over the LAN Noise channel) + a "link a device" screen.

Plan 1 (the crypto foundation) lands first — most verifiable, zero integration risk. Each later plan is its own reviewed slice.

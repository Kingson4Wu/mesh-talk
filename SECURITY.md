# Security Policy

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Use [GitHub Private Vulnerability Reporting](https://github.com/Kingson4Wu/mesh-talk/security/advisories/new)
to submit a report privately. We will acknowledge the report within 7 days and
keep you informed as we work on a fix.

## Scope

Mesh-Talk is a peer-to-peer LAN chat client that runs on each user's own machine;
there is no central server. Key security properties:

- **No central trust** — peers are discovered over UDP broadcast and connect
  directly over TCP on the local network.
- **Encryption at rest** — per-user secrets (identity key, contacts store) are
  encrypted with a key derived from the user's password (PBKDF2 + AES-256-GCM).
  The RSA key protecting the contacts store is decrypted only after login.
- **Local data only** — all user data lives under `~/.mesh-talk/`; nothing is
  uploaded.

## Known limitations

This is an experimental project. The following are tracked, not yet hardened:

- Contact requests/responses are signed with the sender's ed25519 key and
  verified on receipt (a present-but-invalid signature is rejected). However,
  the binding between a `user_id` and its public key is trust-on-first-use —
  there is no key directory, so a first contact from an unknown key is accepted.
- LAN transport is unauthenticated at the network layer; trust the network you
  run on.

Please mention any of these in a report only if you have a concrete exploit
beyond the documented limitation.

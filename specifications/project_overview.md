# Mesh-Talk Project Overview

Mesh-Talk is a **decentralized, end-to-end-encrypted LAN messenger** — a Tauri desktop
app (Rust backend + React frontend) with no server. Peers discover each other over
Ed25519-signed UDP broadcasts, connect directly over a Noise-encrypted channel, and
store messages as an append-only, hash-linked event log that syncs CRDT-style; an
elected "post office" peer stores-and-forwards (still-encrypted) events when a recipient
is offline.

## Goals

1. Messaging with no central server — direct peer-to-peer on the local network.
2. End-to-end encryption with forward secrecy.
3. Automatic signed peer discovery; offline delivery via an elected relay.
4. Cross-platform desktop (macOS, Windows, Linux).

## Features

1:1 DMs and group channels (forward-secret via Double Ratchet / sender-key), file
sharing, reactions, replies/threads, @mentions, search, and multi-device (one account
across devices via device linking + account-addressed fan-out).

> The authoritative technical description is **[`docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md)**
> (layers, crypto, event log/sync, networking, binaries, security posture). This file is
> just the elevator pitch.

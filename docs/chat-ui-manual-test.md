# Chat UI — manual two-instance test

The `/` route exercises the node end-to-end. There is no
automated Tauri+Vue e2e; this is the manual acceptance check.

## Setup (two instances on one machine)
Run two app instances with separate HOME dirs so their per-account data
(`~/.mesh-talk/accounts/<account>/`) doesn't collide:

```bash
# Terminal 1
HOME=/tmp/mesh-a npm run tauri dev    # (or the project's run command)
# Terminal 2
HOME=/tmp/mesh-b npm run tauri dev
```

## Steps
1. In each instance, register/log in as a different user (e.g. `alice` / `bob`).
   Wait a few seconds after login for the node to finish opening
   (it does a password-KDF keystore unlock).
2. The app opens directly to the chat at `/`.
   The header shows `you: <32-hex-id>` once the node is up.
3. Within ~5s each should list the other under **Peers** (UDP discovery).
4. From Alice, select Bob, type a message, **Send**. It appears immediately on
   Alice (optimistic) and within a moment on Bob (live `dm-received`).
5. Send back from Bob → appears on Alice.
6. Restart Alice's instance, log in, open `/`, select Bob → **history**
   shows the prior messages (persisted), proving durability.

## Expected
- Peers appear by user_id/name; a running post office (if any) shows a `PO` tag.
- Messages flow both ways live; history survives restart.
- If a command errors with "node not started", the node is still
  opening — wait and retry.

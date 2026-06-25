// Phase 1 smoke test: load the wasm-pack (nodejs) build and prove the protocol crypto runs in
// a wasm/JS runtime. Generates two device fingerprints and asserts they are non-empty, stable
// in shape, and distinct (the RNG actually varies).
//   wasm-pack build crates/mesh-talk-wasm --target nodejs --dev && node crates/mesh-talk-wasm/smoke.mjs
import {
  create_identity_keystore,
  generate_identity_fingerprint,
  noise_loopback,
  WasmNode,
} from "./pkg/mesh_talk_wasm.js";

const a = generate_identity_fingerprint();
const b = generate_identity_fingerprint();
console.log("fingerprint A:", a);
console.log("fingerprint B:", b);

if (!a || !b) throw new Error("empty fingerprint — crypto did not run");
if (a === b) throw new Error("identical fingerprints — RNG is not producing fresh keys");
console.log("✓ wasm crypto runs in a JS runtime (fresh, distinct identities)");

// The Noise SecureChannel actually handshakes + encrypts in wasm (over an in-memory duplex).
const echoed = await noise_loopback("hello over noise");
console.log("noise_loopback:", echoed);
if (echoed !== "hello over noise")
  throw new Error("noise loopback did not round-trip the message");
console.log("✓ wasm Noise SecureChannel handshakes + encrypts in a JS runtime");

// The stateful node appends + reads messages in its event log.
const node = new WasmNode();
node.send_message("hello");
const count = node.send_message("world");
const msgs = JSON.parse(node.messages());
console.log("node messages:", msgs, "count:", count);
if (count !== 2 || msgs.length !== 2 || msgs[0] !== "hello" || msgs[1] !== "world")
  throw new Error("WasmNode did not store + return messages in order");
console.log("✓ wasm stateful node stores + reads messages");

// snapshot/restore round-trips the log (persistence primitive).
const blob = node.snapshot();
const restored = new WasmNode();
restored.restore(blob);
const restoredMsgs = JSON.parse(restored.messages());
if (restoredMsgs.length !== 2 || restoredMsgs[0] !== "hello" || restoredMsgs[1] !== "world")
  throw new Error("snapshot/restore did not round-trip the log");
console.log("✓ wasm node snapshot/restore round-trips the log");

// Per-recipient sealed DM: A seals to B, B opens; a wrong sender key is rejected.
const alice = new WasmNode();
const bob = new WasmNode();
const sealed = alice.seal_dm(bob.x25519_public(), "private hello");
const opened = bob.open_dm(alice.x25519_public(), sealed);
if (opened !== "private hello")
  throw new Error("recipient could not open the sealed DM");
// A wrong sender key (here bob's own, not alice's) must be rejected.
let rejected = false;
try {
  bob.open_dm(bob.x25519_public(), sealed);
} catch {
  rejected = true;
}
if (!rejected) throw new Error("a wrong sender key must be rejected");
console.log("✓ wasm node seals + opens per-recipient DMs (wrong key rejected)");

// --- forward-secret Double-Ratchet DMs (bidirectional) ---
{
  const a = new WasmNode();
  const b = new WasmNode();
  const aEd = a.ed25519_public(),
    aX = a.x25519_public();
  const bEd = b.ed25519_public(),
    bX = b.x25519_public();

  // Bidirectional exchange.
  const w1 = a.seal_dm_ratcheted(bEd, bX, "hi bob");
  if (b.open_dm_ratcheted(aEd, aX, w1) !== "hi bob")
    throw new Error("B could not open A's ratcheted DM");
  const w2 = b.seal_dm_ratcheted(aEd, aX, "hi alice");
  if (a.open_dm_ratcheted(bEd, bX, w2) !== "hi alice")
    throw new Error("A could not open B's ratcheted DM");

  // Forward secrecy: the same plaintext yields different ciphertext each send (chain advances).
  const c1 = a.seal_dm_ratcheted(bEd, bX, "same");
  const c2 = a.seal_dm_ratcheted(bEd, bX, "same");
  if (Buffer.from(c1).equals(Buffer.from(c2)))
    throw new Error("ratchet did not advance — identical ciphertext for repeated plaintext");
  if (b.open_dm_ratcheted(aEd, aX, c2) !== "same")
    throw new Error("B could not open out-of-order ratcheted DM (c2 before c1)");
  if (b.open_dm_ratcheted(aEd, aX, c1) !== "same")
    throw new Error("B could not open the skipped earlier message (c1 after c2)");
  console.log("✓ wasm node ratchets DMs bidirectionally (forward-secret, out-of-order opens)");

  // Simultaneous init: both send before receiving; the canonical initiator's message opens.
  const a2 = new WasmNode();
  const b2 = new WasmNode();
  const a2Ed = a2.ed25519_public(),
    a2X = a2.x25519_public();
  const b2Ed = b2.ed25519_public(),
    b2X = b2.x25519_public();
  const wa = a2.seal_dm_ratcheted(b2Ed, b2X, "from-a");
  const wb = b2.seal_dm_ratcheted(a2Ed, a2X, "from-b");
  if (a2.fingerprint() < b2.fingerprint()) {
    if (b2.open_dm_ratcheted(a2Ed, a2X, wa) !== "from-a")
      throw new Error("tie-break failed: canonical initiator A's message did not open");
  } else {
    if (a2.open_dm_ratcheted(b2Ed, b2X, wb) !== "from-b")
      throw new Error("tie-break failed: canonical initiator B's message did not open");
  }
  console.log("✓ wasm node resolves simultaneous-init by user-id tie-break");
}

// --- ratchet session persistence across a reload ---
{
  const pw = "ratchet-pw";
  const blobA = create_identity_keystore(pw);
  const A = WasmNode.from_keystore(blobA, pw);
  const B = new WasmNode();
  const aEd = A.ed25519_public(),
    aX = A.x25519_public();
  const bEd = B.ed25519_public(),
    bX = B.x25519_public();

  const w1 = A.seal_dm_ratcheted(bEd, bX, "one");
  if (B.open_dm_ratcheted(aEd, aX, w1) !== "one")
    throw new Error("B could not open A's first ratcheted DM");

  // "Reload" A: same identity from the keystore, DM state restored from the sealed snapshot.
  const snap = A.dm_state_snapshot(pw);
  const A2 = WasmNode.from_keystore(blobA, pw);
  A2.restore_dm_state(snap, pw);
  // A2 continues A's sending chain — B (whose receiving chain is mid-session) must open it.
  const w2 = A2.seal_dm_ratcheted(bEd, bX, "two");
  if (B.open_dm_ratcheted(aEd, aX, w2) !== "two")
    throw new Error("restored session did not continue the ratchet across reload");
  console.log("✓ wasm node persists + restores ratchet sessions across a reload");
}

// --- per-peer ratcheted DM conversation through the event log (send/receive/history) ---
{
  const a = new WasmNode();
  const b = new WasmNode();
  const aEd = a.ed25519_public(),
    aX = a.x25519_public();
  const bEd = b.ed25519_public(),
    bX = b.x25519_public();

  // A → B, then B → A (the serialized events stand in for what the gateway sync would carry).
  const e1 = a.send_ratcheted_dm(bEd, bX, "hey b");
  b.receive_ratcheted_dm(aEd, aX, e1);
  const e2 = b.send_ratcheted_dm(aEd, aX, "hi a");
  a.receive_ratcheted_dm(bEd, bX, e2);

  const aHist = JSON.parse(a.ratcheted_dm_history(bEd, bX));
  const bHist = JSON.parse(b.ratcheted_dm_history(aEd, aX));
  const shape = (h) => h.map((x) => `${x.from_me ? "me" : "peer"}:${x.text}`).join(",");
  // Both sides agree on the transcript; from_me is mirrored.
  if (shape(aHist) !== "me:hey b,peer:hi a")
    throw new Error("A's ratcheted DM history wrong: " + shape(aHist));
  if (shape(bHist) !== "peer:hey b,me:hi a")
    throw new Error("B's ratcheted DM history wrong: " + shape(bHist));
  console.log("✓ wasm node per-peer ratcheted DM conversation (send/receive/history agree)");
}

// --- the sync delivery path: events arrive raw (ingest, like the gateway sync), then process ---
{
  const a = new WasmNode();
  const b = new WasmNode();
  const aEd = a.ed25519_public(),
    aX = a.x25519_public();
  const bEd = b.ed25519_public(),
    bX = b.x25519_public();

  const e1 = a.send_ratcheted_dm(bEd, bX, "synced one");
  const e2 = a.send_ratcheted_dm(bEd, bX, "synced two");
  b.ingest_event(e1); // raw append — what request_round/serve_one do during a sync
  b.ingest_event(e2);
  b.process_dm_events_with(aEd, aX); // open the newly-arrived peer messages

  const txt = JSON.parse(b.ratcheted_dm_history(aEd, aX))
    .map((x) => x.text)
    .join(",");
  if (txt !== "synced one,synced two")
    throw new Error("process-after-sync history wrong: " + txt);
  if (!Array.isArray(JSON.parse(a.peer_roster())))
    throw new Error("peer_roster did not return a JSON array");
  console.log("✓ wasm node opens DM events delivered via sync (ingest → process)");
}

// --- presence: announce name + read the directory (latest-per-author, with last-seen) ---
{
  const n = new WasmNode();
  n.announce_self("alice");
  let dir = JSON.parse(n.directory());
  if (dir.length !== 1 || dir[0].id !== n.fingerprint())
    throw new Error("directory should list self after announce: " + JSON.stringify(dir));
  if (dir[0].name !== "alice")
    throw new Error("directory entry must carry the announced name: " + JSON.stringify(dir));
  if (typeof dir[0].last_seen_ms !== "number" || dir[0].last_seen_ms <= 0)
    throw new Error("directory entry must carry a last-seen timestamp");
  // Many heartbeats (exercises compaction of the append-only directory): still ONE entry per
  // author, carrying the latest name — compaction must not corrupt or duplicate it.
  for (let i = 0; i < 12; i++) n.announce_self("alice" + i);
  dir = JSON.parse(n.directory());
  if (dir.length !== 1 || dir[0].name !== "alice11")
    throw new Error(
      "re-announce/compaction must keep one entry per author with the latest name: " +
        JSON.stringify(dir),
    );
  console.log("✓ wasm node announces name + last-seen + compacts the presence directory");
}

// --- REPRO: does ingesting a peer's PRESENCE make it a DM peer? (phone↔desktop "can't message") ---
{
  // A = phone, B = a desktop ("curry") the phone only learns via the gossiped presence directory.
  const A = new WasmNode();
  const B = new WasmNode();
  B.announce_self("curry");
  const bId = B.fingerprint();

  // The phone pulls the desktop's presence over a sync (ingest its [7;32] announcement).
  A.ingest_event(Buffer.from(B.directory_event_hex(), "hex"));

  // The phone's directory shows the desktop...
  const dir = JSON.parse(A.directory());
  if (!dir.some((p) => p.id === bId))
    throw new Error("A.directory() should include the ingested peer B");

  // ...and CRUCIALLY, the phone must treat it as a DM peer (known_identities), or sync_all_over_dc
  // will never request the DM conversation with it — which is exactly the "online but can't
  // message" symptom.
  const known = JSON.parse(A.known_identity_ids());
  console.log("A.directory has B:", true, "| A.known_identity_ids:", known);
  if (!known.includes(bId))
    throw new Error(
      "BUG REPRODUCED: A ingested B's presence + shows B in directory, but B is NOT in " +
        "known_identities — so A never syncs the DM conversation with B. known=" +
        JSON.stringify(known) +
        " bId=" +
        bId,
    );
  console.log("✓ a presence-only peer becomes a DM peer (known_identities) — DM conv will sync");
}

// --- the native node frames DMs as MessageBody (MTB1); the receiver must strip it ---
{
  const a = new WasmNode();
  const b = new WasmNode();
  const aEd = a.ed25519_public(),
    aX = a.x25519_public();
  const bEd = b.ed25519_public(),
    bX = b.x25519_public();
  // A seals a NATIVE-style (MTB1-framed) DM; B must open it to the BARE text — no MTB1 prefix.
  const wire = a.seal_framed_dm_ratcheted(bEd, bX, "hello from desktop");
  const opened = b.open_dm_ratcheted(aEd, aX, wire);
  if (opened !== "hello from desktop")
    throw new Error("MTB1 frame not stripped — got " + JSON.stringify(opened));
  console.log("✓ wasm strips the native MessageBody (MTB1) frame on a received DM");
}

// --- a phone is a FIRST-CLASS directory participant: it ALWAYS syncs the presence ([7;32]) +
//     relay ([8;32]) directories, even with an empty log / before its first announce. Without this
//     a freshly-connected phone only learns the ONE peer it handshook (its hub) — never the other
//     PCs the hub learned over the LAN, nor other relays to fail over to. (regression: the phone
//     used to request a directory only if it already happened to be in its own log.) ---
{
  const DIR = "07".repeat(32); // [7u8;32] presence directory
  const RELAYS = "08".repeat(32); // [8u8;32] relay directory
  const phone = new WasmNode(); // fresh: empty log, never announced
  const set = JSON.parse(phone.sync_conv_set_hex());
  if (!set.includes(DIR) || !set.includes(RELAYS))
    throw new Error(
      "BUG: a fresh phone must ALWAYS sync the presence + relay directories so it sees EVERY " +
        "node (not just the hub it handshook). set=" + JSON.stringify(set),
    );
  console.log(
    "✓ a fresh phone always syncs the presence + relay directories (equal-node guarantee)",
  );
}

// --- mDNS integration (phone side): a relay URL with a `meshtalk-*.local` hostname flows through
//     the relay directory [8;32] and shows up in the phone's failover-candidate list exactly like
//     an IP relay. (The desktop announces BOTH ws://IP and ws://<host>.local; the phone's
//     runMeshSync rotation reads known_relay_urls(), so this is the candidate set that gains the
//     .local fallback. Actual .local *resolution* is the phone OS's job — out of scope here.) ---
{
  const n = new WasmNode();
  const ipUrl = "ws://192.168.1.10:47480";
  const localUrl = "ws://meshtalk-pc1.local:47480";
  n.announce_relay(ipUrl);
  n.announce_relay(localUrl);
  const relays = JSON.parse(n.known_relay_urls());
  if (!relays.includes(ipUrl) || !relays.includes(localUrl))
    throw new Error(
      "BUG: both the IP and the meshtalk-*.local relay URL must be failover candidates. got=" +
        JSON.stringify(relays),
    );
  console.log(
    "✓ a meshtalk-*.local relay URL flows through [8;32] into the phone's failover candidates",
  );
}

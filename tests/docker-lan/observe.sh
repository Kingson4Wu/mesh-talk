#!/usr/bin/env bash
# Observe the mesh logs in a 3-PC + phone scenario (separate container IPs on a /24 bridge):
# does the hub's presence directory converge to ALL other PCs, what do the [mesh] gossip logs say,
# and does the phone see every PC? RUST_LOG=info so the diagnostics emit.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
NET=meshobs
ROOM=lan-bridge
DP=47011
RELAY_PORT=47480

cleanup() {
  for j in $(jobs -p); do kill "$j" 2>/dev/null; done
  docker rm -f obs-relay obs-pc1 obs-pc2 obs-pc3 obs-phone >/dev/null 2>&1
  docker network rm $NET >/dev/null 2>&1
}
trap cleanup EXIT
cleanup
docker network create --subnet 192.168.99.0/24 $NET >/dev/null

echo "== relay (serves the PWA too) =="
docker run -d --rm --name obs-relay --network $NET -v "$ROOT/frontend/dist:/pwa:ro" mesh-lan \
  signal --host 0.0.0.0 --port $RELAY_PORT --serve-dir /pwa --http-port 8080 >/dev/null
sleep 1

run_node() { # <name> [extra args…]
  local name="$1"; shift
  ( while sleep 3; do echo /dir; done ) | docker run -i --rm --name "obs-$name" --network $NET \
    -e RUST_LOG=info mesh-lan \
    runtime-node --data-dir /d --account-id "$name" --name "$name" --discovery-port $DP "$@" \
    >"/tmp/obs-$name.out" 2>"/tmp/obs-$name.err" &
}
echo "== PC2, PC3 (plain LAN nodes) + PC1 (LAN + gateway hub) =="
run_node pc2
run_node pc3
run_node pc1 --signal-url "ws://obs-relay:$RELAY_PORT" --gateway-room $ROOM

echo "== waiting ~22s for LAN discovery + directory gossip to converge =="
sleep 22

echo
echo "################ PC1 (hub) [mesh] gossip log ################"
grep -aE "\[mesh\]" /tmp/obs-pc1.err 2>/dev/null | grep -aE "gossiped|DIAL|SYNC|announced" | tail -6
echo
echo "################ presence directory each node holds (/dir) ################"
for n in pc1 pc2 pc3; do
  echo "--- $n's directory ---"
  grep -aE "^dir " "/tmp/obs-$n.out" 2>/dev/null | sort -u
done

PC2ID=$(grep -aoE "node [0-9a-f]{32}" /tmp/obs-pc2.out 2>/dev/null | head -1 | awk '{print $2}')
PC3ID=$(grep -aoE "node [0-9a-f]{32}" /tmp/obs-pc3.out 2>/dev/null | head -1 | awk '{print $2}')
echo
echo "################ phone (browser) — does it see PC2 AND PC3 via the hub? ################"
echo "PC2=$PC2ID  PC3=$PC3ID"
docker run --rm --name obs-phone --network $NET \
  -e PWA_URL="http://obs-relay:8080/?relay=ws://obs-relay:$RELAY_PORT&room=$ROOM" \
  -e PC_IDS="$PC2ID,$PC3ID" -e CI=1 \
  -v "$ROOT/tests/docker-lan:/t:ro" \
  mcr.microsoft.com/playwright:v1.49.1-jammy \
  bash -c "mkdir -p /w && cd /w && cp /t/phone-multi.mjs . && npm i playwright@1.49.1 >/dev/null 2>&1 && node phone-multi.mjs"
echo "phone exit: $?"

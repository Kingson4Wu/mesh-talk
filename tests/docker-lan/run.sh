#!/usr/bin/env bash
# Full-flow LAN simulation on a Docker bridge — SEPARATE container IPs, real cross-host UDP
# discovery (not loopback): a relay + two desktop nodes (PC1 also a gateway hub) + a browser phone.
# Proves a phone sees a LAN PC (PC2) it never connected to, bridged through PC1.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
NET=meshlan
ROOM=lan-bridge
DP=47010

cleanup() {
  [ -n "${PC1_PIPE:-}" ] && kill "$PC1_PIPE" >/dev/null 2>&1 || true
  docker rm -f relay pc1 pc2 phone >/dev/null 2>&1 || true
  docker network rm $NET >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

docker network create --subnet 192.168.99.0/24 $NET >/dev/null
echo "== relay (signal, + serves the PWA over http for the phone) =="
docker run -d --rm --name relay --network $NET -v "$ROOT/frontend/dist:/pwa:ro" mesh-lan \
  signal --host 0.0.0.0 --port 47480 --serve-dir /pwa --http-port 8080 >/dev/null
sleep 1

echo "== PC2 (plain LAN desktop node) =="
docker run -d --rm --name pc2 --network $NET mesh-lan \
  runtime-node --data-dir /d --account-id pc2 --name PC2-Desktop --discovery-port $DP >/dev/null

echo "== PC1 (LAN desktop node + gateway hub), feeding it periodic /dir =="
( while sleep 2; do echo /dir; done ) | \
  docker run -i --rm --name pc1 --network $NET mesh-lan \
    runtime-node --data-dir /d --account-id pc1 --name PC1-Hub --discovery-port $DP \
    --signal-url ws://relay:47480 --gateway-room $ROOM > /tmp/pc1.log 2>&1 &
PC1_PIPE=$!

PC2ID=""
for i in $(seq 1 30); do
  PC2ID=$(docker logs pc2 2>&1 | sed -n 's/^node \([0-9a-f]*\) listening$/\1/p' | head -1)
  [ -n "$PC2ID" ] && break; sleep 1
done
echo "PC2 id: ${PC2ID:-<none>}"
[ -z "$PC2ID" ] && { echo "FAIL: PC2 never started"; exit 1; }

echo "== PHASE 1: PC1's presence directory must carry PC2 (cross-host discovery + gossip) =="
OK1=0
for i in $(seq 1 45); do
  grep -q "dir $PC2ID " /tmp/pc1.log 2>/dev/null && { OK1=1; break; }
  sleep 2
done
echo "--- PC1 /dir tail ---"; grep '^dir' /tmp/pc1.log 2>/dev/null | tail -4
[ "$OK1" = 1 ] && echo "PHASE1 OK: PC1 (hub) sees PC2 across the bridge LAN" \
              || { echo "PHASE1 FAIL: PC1 never saw PC2"; exit 1; }

if [ "${PHASE1_ONLY:-}" = 1 ]; then echo "ALL OK (phase 1)"; exit 0; fi

echo "== PHASE 2: a browser phone (in a container on the bridge) must see PC2 via the hub =="
docker run --rm --name phone --network $NET \
  -v "$ROOT/tests/docker-lan:/t:ro" \
  -e PWA_URL="http://relay:8080/?relay=ws://relay:47480&room=$ROOM" \
  -e PC2_ID="$PC2ID" \
  -e CI=1 \
  mcr.microsoft.com/playwright:v1.49.1-jammy \
  bash -c "mkdir -p /work && cd /work && cp /t/phone.mjs . && npm i playwright@1.49.1 >/dev/null 2>&1 && node phone.mjs"
RC=$?
echo "phone exit: $RC"
[ "$RC" = 0 ] && echo "ALL OK: the phone saw PC2 (a LAN PC it never connected to) via the hub" \
             || { echo "PHASE2 FAIL"; exit 1; }

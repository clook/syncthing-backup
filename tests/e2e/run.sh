#!/usr/bin/env bash
# End-to-end checks with two real local Syncthing instances ("phone" and "server") and the porter.
#
#   SYNCTHING_BIN=/path/to/syncthing tests/e2e/run.sh [scenario...]
#
# Phone folder: Send Only + Ignore Delete. Server folder: Send & Receive. The Syncthing data lives on
# $E2E_SRC_ROOT (default: a tmpfs dir) and the "HDD" on target/e2e (another filesystem), so the porter's
# cross-device copy path is exercised.
# Needs about 3x E2E_BIG_MB (default 2048) free in E2E_SRC_ROOT. Scenarios: burst resend edit st_restart kill_copy offline witness
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
ST=${SYNCTHING_BIN:?set SYNCTHING_BIN to a syncthing v2 binary}
SRC_ROOT=${E2E_SRC_ROOT:-${TMPDIR:-/tmp}/porter-e2e}
HDD=$ROOT/target/e2e/hdd
BIG_MB=${E2E_BIG_MB:-2048}
SERVER_TYPE=${E2E_SERVER_TYPE:-receiveonly}   # sendreceive: `edit` is expected to fail (decision 0009)
SCENARIOS=("$@")
((${#SCENARIOS[@]})) || SCENARIOS=(burst resend edit st_restart kill_copy offline witness)

PHONE=$SRC_ROOT/phone            # phone's folder
STORE=$SRC_ROOT/server-storage   # server Syncthing storage root
SRV_FOLDER=$STORE/phone/photos
KEY=e2e-api-key
P_GUI=127.0.0.1:18384 S_GUI=127.0.0.1:18385
P_LISTEN=tcp://127.0.0.1:22101 S_LISTEN=tcp://127.0.0.1:22102
LOG=$ROOT/target/e2e/logs
PIDS=()

log() { printf '\n[%s] %s\n' "$(date +%T)" "$*"; }
fail() {
  echo "E2E FAIL: $*"
  echo "--- diagnostics"
  for g in $P_GUI $S_GUI; do
    echo "## $g db/status:"; api "$g" GET "/rest/db/status?folder=photos" | head -c 1500; echo
    [[ -n ${DIAG_FILE:-} ]] && { echo "## $g db/file $DIAG_FILE:"; api "$g" GET "/rest/db/file?folder=photos&file=$DIAG_FILE" | head -c 3000; echo; }
  done
  exit 1
}
cleanup() { for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; }
trap cleanup EXIT

api() { # api <gui> <method> <path> [json]
  curl -sf -X "$2" -H "X-API-Key: $KEY" -H 'Content-Type: application/json' ${4:+-d "$4"} "http://$1$3"
}

wait_until() { # wait_until <timeout_s> <description> <command...>
  local t=$1 what=$2; shift 2
  local end=$((SECONDS + t))
  until "$@"; do
    ((SECONDS < end)) || fail "timeout ($t s): $what"
    sleep 0.5
  done
}

src_files() { find "$SRV_FOLDER" -type f ! -path '*/.st*' ! -name '.syncthing.*' 2>/dev/null | wc -l; }
src_empty() { [[ $(src_files) -eq 0 ]]; }
hdd_count() { find "$HDD/phone/photos" -type f ! -name '.syncthing-backup.tmp' 2>/dev/null | wc -l; }
porter_tmp_count() { find "$HDD" -name '.syncthing-backup.tmp' 2>/dev/null | wc -l; }

start_syncthing() { # start_syncthing <name> <gui>
  "$ST" serve --home "$SRC_ROOT/home-$1" --no-browser --no-restart --no-upgrade \
    --gui-address="http://$2" --gui-apikey=$KEY --log-file="$LOG/syncthing-$1.log" >/dev/null 2>&1 &
  PIDS+=($!)
  eval "PID_ST_$1=$!"
  wait_until 30 "syncthing $1 API" api "$2" GET /rest/system/ping
}

PORTER_PID=
start_porter() {
  DRY_RUN=false SYNCTHING_URL="http://$S_GUI" SYNCTHING_KEY=$KEY \
    SOURCE_DIRECTORY="$STORE" SYNCTHING_STORAGE_DIRECTORY="$STORE" DESTINATION_DIRECTORY="$HDD" \
    WITNESS_FILE=.mounted SCAN_INTERVAL_SECS=600 MIN_AGE_SECS=2 \
    "$ROOT/target/release/syncthing-backup" >>"$LOG/porter.log" 2>&1 &
  PORTER_PID=$!
  PIDS+=($PORTER_PID)
}
stop_porter() { kill "$PORTER_PID" 2>/dev/null || true; wait "$PORTER_PID" 2>/dev/null || true; }

setup() {
  log "Setup in $SRC_ROOT (source) and $HDD (destination); server folder type: $SERVER_TYPE"
  [[ $(stat -f -c %T "$SRC_ROOT" 2>/dev/null || stat -f -c %T "$(dirname "$SRC_ROOT")") != \
     $(stat -f -c %T "$ROOT/target") ]] || echo "warning: source and destination on the same fs type"
  rm -rf "$SRC_ROOT" "$ROOT/target/e2e"
  mkdir -p "$PHONE" "$SRV_FOLDER" "$HDD" "$LOG"
  touch "$HDD/.mounted"
  (cd "$ROOT" && cargo build --release --quiet)

  local p_id s_id
  p_id=$("$ST" generate --home "$SRC_ROOT/home-phone" 2>&1 | sed -nE 's/.*Device ID: *([A-Z0-9-]{63}).*/\1/p')
  s_id=$("$ST" generate --home "$SRC_ROOT/home-server" 2>&1 | sed -nE 's/.*Device ID: *([A-Z0-9-]{63}).*/\1/p')
  [[ -n $p_id && -n $s_id ]] || p_id=$("$ST" device-id --home "$SRC_ROOT/home-phone") s_id=$("$ST" device-id --home "$SRC_ROOT/home-server")
  start_syncthing phone $P_GUI
  start_syncthing server $S_GUI

  local opts='{"globalAnnounceEnabled":false,"localAnnounceEnabled":false,"relaysEnabled":false,"natEnabled":false,"urAccepted":-1,"crashReportingEnabled":false,"autoUpgradeIntervalH":0,"listenAddresses":["%s"]}'
  api $P_GUI PATCH /rest/config/options "$(printf "$opts" $P_LISTEN)"
  api $S_GUI PATCH /rest/config/options "$(printf "$opts" $S_LISTEN)"
  api $P_GUI PUT "/rest/config/devices/$s_id" "{\"deviceID\":\"$s_id\",\"name\":\"server\",\"addresses\":[\"$S_LISTEN\"]}"
  api $S_GUI PUT "/rest/config/devices/$p_id" "{\"deviceID\":\"$p_id\",\"name\":\"phone\",\"addresses\":[\"$P_LISTEN\"]}"
  api $P_GUI PUT /rest/config/folders/photos "{\"id\":\"photos\",\"path\":\"$PHONE\",\"type\":\"sendonly\",\"ignoreDelete\":true,\"rescanIntervalS\":3,\"fsWatcherEnabled\":true,\"fsWatcherDelayS\":1,\"devices\":[{\"deviceID\":\"$s_id\"}]}"
  api $S_GUI PUT /rest/config/folders/photos "{\"id\":\"photos\",\"path\":\"$SRV_FOLDER\",\"type\":\"$SERVER_TYPE\",\"rescanIntervalS\":3,\"fsWatcherEnabled\":true,\"fsWatcherDelayS\":1,\"devices\":[{\"deviceID\":\"$p_id\"}]}"
  wait_until 60 "devices connected" bash -c "curl -sf -H 'X-API-Key: $KEY' http://$S_GUI/rest/system/connections | grep -q '\"connected\": *true'"
  echo "phone=$p_id server=$s_id"
}

mkfile() { head -c "$2" /dev/urandom >"$1"; }

# --- Scenarios -------------------------------------------------------------------------------------

scenario_burst() {
  log "burst: one ${BIG_MB} MB file, then 100 x 200 KB written while it is being copied to the HDD"
  mkdir -p "$PHONE/burst"
  mkfile "$PHONE/burst/big.bin" "$((BIG_MB * 1024 * 1024))"
  sha256sum "$PHONE/burst/big.bin" | cut -d' ' -f1 >"$LOG/big.sha"
  local end=$((SECONDS + 300))
  until compgen -G "$HDD/phone/photos/burst/.syncthing-backup.tmp" >/dev/null; do
    ((SECONDS < end)) || fail "timeout: big.bin copy never observed"
    sleep 0.01
  done
  for i in $(seq -w 1 100); do mkfile "$PHONE/burst/small$i.bin" 204800; done
  wait_until 600 "101 burst files on HDD" bash -c "[[ \$(find '$HDD/phone/photos/burst' -type f 2>/dev/null | wc -l) -eq 101 ]]"
  wait_until 60 "source empty" src_empty
  [[ $(sha256sum "$HDD/phone/photos/burst/big.bin" | cut -d' ' -f1) == "$(cat "$LOG/big.sha")" ]] || fail "big.bin checksum differs"
  for f in "$PHONE"/burst/small*.bin; do cmp -s "$f" "$HDD/phone/photos/burst/$(basename "$f")" || fail "content differs: $f"; done
  [[ $(porter_tmp_count) -eq 0 ]] || fail "leftover temporary file"
  # Evidence: small files queued between "Processing big.bin" and "Moved big.bin".
  local during
  during=$(awk '/Processing .*big\.bin/{p=1} p&&/Queued .*small/{n++} /Moved .*big\.bin/{p=0} END{print n+0}' "$LOG/porter.log")
  echo "small files queued while big.bin was being copied: $during"
  ((during > 0)) || fail "no small file was queued during the big copy (copy too fast? raise E2E_BIG_MB)"
  rm "$PHONE/burst/big.bin" # frees tmpfs space; the phone's deletion is harmless (already moved)
  echo "OK burst"
}

scenario_resend() {
  log "resend: moved files must not come back after rescans, and the phone keeps them"
  mkdir -p "$PHONE/resend"
  for i in $(seq 1 10); do mkfile "$PHONE/resend/r$i.bin" 30000; done
  wait_until 90 "10 resend files on HDD" bash -c "[[ \$(find '$HDD/phone/photos/resend' -type f 2>/dev/null | wc -l) -eq 10 ]]"
  wait_until 30 "source empty" src_empty
  local before; before=$(hdd_count)
  api $S_GUI POST "/rest/db/scan?folder=photos" >/dev/null
  api $P_GUI POST "/rest/db/scan?folder=photos" >/dev/null
  sleep 15
  src_empty || fail "files reappeared in the server folder: $(src_files)"
  [[ $(hdd_count) -eq $before ]] || fail "HDD count changed ($before -> $(hdd_count))"
  [[ $(find "$PHONE/resend" -type f | wc -l) -eq 10 ]] || fail "phone lost files (send-only/ignoreDelete not respected)"
  echo "OK resend"
}

scenario_edit() {
  log "edit: an edited file on the phone is kept as name_1, the original backup is untouched"
  mkfile "$PHONE/edit.jpg" 10000
  wait_until 60 "edit.jpg backed up" test -f "$HDD/phone/photos/edit.jpg"
  wait_until 30 "source empty" src_empty
  local orig; orig=$(sha256sum "$HDD/phone/photos/edit.jpg" | cut -d' ' -f1)
  DIAG_FILE=edit.jpg
  mkfile "$PHONE/edit.jpg" 12000
  wait_until 90 "edit_1.jpg backed up" test -f "$HDD/phone/photos/edit_1.jpg"
  [[ $(sha256sum "$HDD/phone/photos/edit.jpg" | cut -d' ' -f1) == "$orig" ]] || fail "original overwritten"
  cmp -s "$PHONE/edit.jpg" "$HDD/phone/photos/edit_1.jpg" || fail "edit_1.jpg differs from phone"
  echo "OK edit"
}

scenario_st_restart() {
  log "st_restart: server Syncthing restarts; event IDs restart, files must still be moved"
  kill "$PID_ST_server"; wait "$PID_ST_server" 2>/dev/null || true
  mkdir -p "$PHONE/restart"
  for i in $(seq 1 20); do mkfile "$PHONE/restart/r$i.bin" 50000; done
  start_syncthing server $S_GUI
  wait_until 120 "20 restart files on HDD" bash -c "[[ \$(find '$HDD/phone/photos/restart' -type f 2>/dev/null | wc -l) -eq 20 ]]"
  wait_until 30 "source empty" src_empty
  grep -q "Syncthing restarted" "$LOG/porter.log" && echo "porter detected the restart" || echo "note: restart not logged (files caught by scan)"
  echo "OK st_restart"
}

scenario_kill_copy() {
  log "kill_copy: porter killed with SIGKILL during a large copy"
  stop_porter
  mkfile "$PHONE/kill.bin" "$((BIG_MB * 1024 * 1024))"
  wait_until 300 "kill.bin received by server" bash -c "[[ -f '$SRV_FOLDER/kill.bin' ]]"
  start_porter
  local end=$((SECONDS + 60))   # tight poll: the copy may only take a second or two
  until compgen -G "$HDD/phone/photos/.syncthing-backup.tmp" >/dev/null; do
    ((SECONDS < end)) || fail "timeout: copy never observed in progress"
    sleep 0.01
  done
  kill -9 "$PORTER_PID"; wait "$PORTER_PID" 2>/dev/null || true
  echo "killed during copy; leftovers: $(porter_tmp_count) tmp, final exists: $([[ -f $HDD/phone/photos/kill.bin ]] && echo yes || echo no)"
  [[ -f $SRV_FOLDER/kill.bin ]] || fail "source removed although copy was interrupted"
  start_porter
  wait_until 300 "kill.bin moved after restart" bash -c "[[ ! -f '$SRV_FOLDER/kill.bin' ]]"
  cmp -s "$PHONE/kill.bin" "$HDD/phone/photos/kill.bin" || fail "kill.bin differs"
  [[ ! -e $HDD/phone/photos/kill_1.bin ]] || fail "duplicate kill_1.bin created"
  [[ $(porter_tmp_count) -eq 0 ]] || fail "leftover temporary file after retry"
  echo "OK kill_copy"
}

scenario_offline() {
  log "offline: files arrive while the porter is down (no events seen) -> startup scan"
  stop_porter
  mkdir -p "$PHONE/offline"
  for i in $(seq 1 50); do mkfile "$PHONE/offline/o$i.bin" 20000; done
  wait_until 120 "50 files received by server" bash -c "[[ \$(find '$SRV_FOLDER/offline' -type f 2>/dev/null | wc -l) -eq 50 ]]"
  sleep 3 # older than MIN_AGE_SECS
  start_porter
  wait_until 120 "50 offline files on HDD" bash -c "[[ \$(find '$HDD/phone/photos/offline' -type f 2>/dev/null | wc -l) -eq 50 ]]"
  wait_until 30 "source empty" src_empty
  echo "OK offline"
}

scenario_witness() {
  log "witness: nothing is written while the witness file is missing"
  rm "$HDD/.mounted"
  mkfile "$PHONE/witness.bin" 1000
  wait_until 60 "witness.bin received by server" test -f "$SRV_FOLDER/witness.bin"
  sleep 5
  [[ ! -e $HDD/phone/photos/witness.bin ]] || fail "written without witness"
  [[ -f $SRV_FOLDER/witness.bin ]] || fail "source removed without witness"
  touch "$HDD/.mounted"
  wait_until 90 "witness.bin moved once witness is back" test -f "$HDD/phone/photos/witness.bin"
  echo "OK witness"
}

setup
start_porter
for s in "${SCENARIOS[@]}"; do "scenario_$s"; done
log "ALL E2E SCENARIOS PASSED: ${SCENARIOS[*]}"
sed -E 's/\x1b\[[0-9;]*m//g' "$LOG/porter.log" | grep -E ' (ERROR|WARN) ' | sed -E 's/^\S+ +//' | sort | uniq -c | sort -rn | head -20 || true

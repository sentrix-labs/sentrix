#!/usr/bin/env bash
# testnet-livelock-watchdog.sh — Workaround for BFT livelock #1d until the
# proper fix lands.
#
# Polls the local testnet RPC every 60 s; if chain height hasn't advanced
# for 5+ minutes, restart all 4 testnet validators in parallel. Logs
# every action to syslog (journalctl -t sentrix-livelock-watchdog) and
# keeps a short local state file under /run so the script is
# stateless across reboots.
#
# Install on Core node (the only host running the testnet validator cluster)
# as a systemd timer; see scripts/install-testnet-watchdog.sh for the
# one-shot installer.

set -euo pipefail

RPC_URL="${RPC_URL:-http://localhost:9545/chain/info}"
STATE_FILE="${STATE_FILE:-/run/sentrix-testnet-watchdog.state}"
STALL_THRESHOLD_SECS="${STALL_THRESHOLD_SECS:-300}"   # 5 minutes
COOLDOWN_SECS="${COOLDOWN_SECS:-180}"                 # don't restart more than once per 3 min
VALIDATORS=(sentrix-testnet-val1 sentrix-testnet-val2 sentrix-testnet-val3 sentrix-testnet-val4)

log() {
    logger -t sentrix-livelock-watchdog "$*"
    printf '[%s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"
}

current_height() {
    curl -sS --max-time 3 "$RPC_URL" 2>/dev/null \
        | grep -oE '"height":[0-9]+' \
        | grep -oE '[0-9]+' \
        | head -1
}

read_state() {
    [[ -f "$STATE_FILE" ]] || { echo "0 0 0"; return; }
    cat "$STATE_FILE"
}

write_state() {
    printf '%s %s %s\n' "$1" "$2" "$3" > "$STATE_FILE"
}

restart_validators() {
    log "RESTART: chain stalled — restarting ${VALIDATORS[*]}"
    for v in "${VALIDATORS[@]}"; do
        systemctl restart "$v" &
    done
    wait
    log "RESTART: completed"
}

main() {
    local now last_seen_height last_seen_ts last_restart_ts
    now=$(date +%s)
    read -r last_seen_height last_seen_ts last_restart_ts < <(read_state)

    local height
    height=$(current_height || true)
    if [[ -z "${height:-}" ]]; then
        log "WARN: RPC unreachable ($RPC_URL) — skipping this tick"
        return 0
    fi

    # First run or height advanced — just record.
    if [[ "$height" != "$last_seen_height" ]]; then
        log "OK: height $last_seen_height → $height (advancing)"
        write_state "$height" "$now" "$last_restart_ts"
        return 0
    fi

    # Same height as last tick — check stall duration.
    local stalled_for=$(( now - last_seen_ts ))
    local since_restart=$(( now - last_restart_ts ))

    if (( stalled_for < STALL_THRESHOLD_SECS )); then
        log "OK: height $height unchanged for ${stalled_for}s (<threshold=${STALL_THRESHOLD_SECS}s)"
        return 0
    fi

    if (( since_restart < COOLDOWN_SECS )); then
        log "COOLDOWN: height $height stalled ${stalled_for}s but last restart was ${since_restart}s ago (<cooldown=${COOLDOWN_SECS}s)"
        return 0
    fi

    log "STALL: height $height stalled for ${stalled_for}s — triggering restart"
    restart_validators
    write_state "$height" "$now" "$now"
}

main "$@"

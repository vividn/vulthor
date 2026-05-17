#!/usr/bin/env bash
# Spawn a vulthor instance against tests/fixtures/maildir inside a tmux
# session so polecats (or humans) can attach for manual debugging and
# tools like Playwright can drive the web pane.
#
# Output (stdout, one line each, key=value):
#   session=<tmux session name>
#   port=<TCP port the web pane is listening on>
#   maildir=<absolute path to the fixture MailDir>
#
# Exit codes: 0 on success, non-zero on failure to launch or bind.

set -euo pipefail

SESSION="${VULTHOR_FIXTURE_SESSION:-vulthor-fixture}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MAILDIR="${VULTHOR_FIXTURE_MAILDIR:-$REPO_ROOT/tests/fixtures/maildir}"
BIN="${VULTHOR_BIN:-}"
BUILD_PROFILE="${VULTHOR_PROFILE:-debug}"
READY_TIMEOUT="${VULTHOR_READY_TIMEOUT:-30}"

err() { echo "run-vulthor-fixture: $*" >&2; }

command -v tmux >/dev/null || { err "tmux not found"; exit 2; }
[ -d "$MAILDIR" ] || { err "MailDir missing: $MAILDIR"; exit 2; }

# Resolve the vulthor binary. Caller can pin one via $VULTHOR_BIN; otherwise
# use the prebuilt target if present, or fall back to `cargo run` so first-time
# users do not have to build manually.
if [ -z "$BIN" ]; then
  if [ -x "$REPO_ROOT/target/$BUILD_PROFILE/vulthor" ]; then
    BIN="$REPO_ROOT/target/$BUILD_PROFILE/vulthor"
  fi
fi

# Pick a free port. We bind ephemerally and release immediately; there is a
# tiny race window before vulthor grabs the port but that is acceptable for a
# dev harness.
pick_free_port() {
  if command -v python3 >/dev/null; then
    python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
    return
  fi
  # Fallback: scan ephemeral range with /dev/tcp.
  for _ in $(seq 1 200); do
    p=$(( (RANDOM % 20000) + 40000 ))
    (exec 3<>/dev/tcp/127.0.0.1/"$p") 2>/dev/null && { exec 3<&-; exec 3>&-; continue; }
    echo "$p"; return
  done
  err "could not find a free port"; exit 3
}

PORT="${VULTHOR_FIXTURE_PORT:-$(pick_free_port)}"

# Tear down any prior fixture session under the same name; this is a dev
# harness, not production state, so reusing the name is the right call.
tmux kill-session -t "$SESSION" 2>/dev/null || true

if [ -n "$BIN" ]; then
  CMD="$BIN -m '$MAILDIR' -p $PORT"
else
  CMD="cd '$REPO_ROOT' && cargo run --quiet -- -m '$MAILDIR' -p $PORT"
fi

tmux new-session -d -s "$SESSION" -x 200 -y 50 "$CMD"

# Wait for /health to return OK before declaring success. Polling beats
# sleeping because cargo-build latency dwarfs vulthor startup.
ready=0
for _ in $(seq 1 "$READY_TIMEOUT"); do
  if curl -fsS "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    ready=1; break
  fi
  if ! tmux has-session -t "$SESSION" 2>/dev/null; then
    err "tmux session '$SESSION' exited before serving; inspect with: tmux capture-pane -pt $SESSION"
    exit 4
  fi
  sleep 1
done

if [ "$ready" -ne 1 ]; then
  err "vulthor web pane did not become ready on port $PORT within ${READY_TIMEOUT}s"
  err "attach with: tmux attach -t $SESSION"
  exit 5
fi

cat <<EOF
session=$SESSION
port=$PORT
maildir=$MAILDIR
EOF

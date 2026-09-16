#!/usr/bin/env bash
# mail-renew.sh — extend a claim's lease (resets its deadline to now).
# usage: mail-renew.sh <mailbox> <claimed-file-name> <claim-token>
# Token must match the claim generation — same rule as mail-ack.sh.
set -euo pipefail

MAIL_ROOT="${AGENT_MAIL_ROOT:-/tmp/agent-mail}"

usage() {
  echo "usage: $(basename "$0") <mailbox> <claimed-file> <token>" >&2
  exit 2
}
[ $# -eq 3 ] || usage
box="$1"; name=$(basename "$2"); tok="$3"
[[ "$box" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]] || { echo "mail-renew: invalid mailbox '$box'" >&2; exit 2; }

claimed="$MAIL_ROOT/$box/claimed"
f="$claimed/$name"
[ -e "$f" ] || { echo "mail-renew: no such claim: $name" >&2; exit 1; }
[[ "$name" =~ -claim-"$tok"\.md$ ]] || {
  echo "mail-renew: stale or wrong token for $name — not renewed" >&2
  exit 1
}

exec 9>"$MAIL_ROOT/$box/.lock"; flock 9
touch "$f"
echo "renewed $name"

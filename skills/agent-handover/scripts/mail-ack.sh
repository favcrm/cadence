#!/usr/bin/env bash
# mail-ack.sh — acknowledge a claimed message: claimed/ → read/.
# usage: mail-ack.sh <mailbox> <claimed-file-name> <claim-token>
#        mail-ack.sh <mailbox> --token <claim-token>   (ack every claim under it)
# The token is the generation printed by mail-poll. It must match the
# -claim-<tok> embedded in the filename — a stale token (from an older,
# expired claim) is rejected and the current claim is preserved.
set -euo pipefail

MAIL_ROOT="${AGENT_MAIL_ROOT:-/tmp/agent-mail}"

usage() {
  echo "usage: $(basename "$0") <mailbox> (<claimed-file> <token> | --token <token>)" >&2
  exit 2
}
[ $# -ge 2 ] || usage
box="$1"; shift
[[ "$box" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]] || { echo "mail-ack: invalid mailbox '$box'" >&2; exit 2; }

claimed="$MAIL_ROOT/$box/claimed"
read_dir="$MAIL_ROOT/$box/read"
mkdir -p "$read_dir"
shopt -s nullglob

if [ "$1" = "--token" ]; then
  [ $# -eq 2 ] || usage
  tok="$2"
  n=0
  for f in "$claimed"/*-claim-"$tok".md; do
    mv "$f" "$read_dir/"; n=$((n + 1))
  done
  echo "acked $n message(s) under token $tok"
  exit 0
fi

[ $# -eq 2 ] || usage
name=$(basename "$1"); tok="$2"
f="$claimed/$name"
[ -e "$f" ] || { echo "mail-ack: no such claim: $name" >&2; exit 1; }
[[ "$name" =~ -claim-"$tok"\.md$ ]] || {
  echo "mail-ack: stale or wrong token for $name — claim preserved" >&2
  exit 1
}
mv "$f" "$read_dir/"
echo "acked $name"

#!/usr/bin/env bash
# mail-post.sh — deliver a message to an agent mailbox (atomic, unique, FIFO).
# usage: mail-post.sh <to-mailbox> <type> [--from <mailbox>] [--reply-to <msgid>] <file|->
# Filename: <ts%N>-<from>-<type>-<msgid>[-re-<msgid>].md — name sort = FIFO.
# Requires GNU date (%N). Identifiers are validated, never sanitized.
set -euo pipefail

MAIL_ROOT="${AGENT_MAIL_ROOT:-/tmp/agent-mail}"

usage() {
  echo "usage: $(basename "$0") <to-mailbox> <type> [--from <mailbox>] [--reply-to <msgid>] <file|->" >&2
  exit 2
}

[ $# -ge 3 ] || usage
to="$1"; type="$2"; shift 2
from="${AGENT_MAILBOX:-unknown}"; reply=""
while [ $# -gt 1 ]; do
  case "$1" in
    --from) from="$2"; shift 2 ;;
    --reply-to) reply="$2"; shift 2 ;;
    *) usage ;;
  esac
done
src="$1"

valid() { [[ "$1" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]]; }
for v in "$to" "$from" "$type"; do
  valid "$v" || { echo "mail-post: invalid identifier '$v' (want ^[a-z0-9][a-z0-9-]{0,31}\$)" >&2; exit 2; }
done
if [ -n "$reply" ] && ! valid "$reply"; then
  echo "mail-post: invalid --reply-to '$reply'" >&2; exit 2
fi

inbox="$MAIL_ROOT/$to/inbox"
mkdir -p "$inbox"
ts=$(date -u +%Y%m%d-%H%M%S%N)
msgid=$(openssl rand -hex 4)
dest="$inbox/${ts}-${from}-${type}-${msgid}${reply:+-re-$reply}.md"

tmp="$inbox/.tmp-$$"
if [ "$src" = "-" ]; then cat > "$tmp"; else cp "$src" "$tmp"; fi
mv -n "$tmp" "$dest"   # atomic + no-clobber; %N + msgid make collision a non-event
if [ -e "$tmp" ]; then
  rm -f "$tmp"
  echo "mail-post: destination collision — not delivered" >&2
  exit 1
fi
printf 'msgid=%s path=%s\n' "$msgid" "$dest"

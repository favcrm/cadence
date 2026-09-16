#!/usr/bin/env bash
# mail-poll.sh — claim-and-print messages, oldest first, at-least-once.
# usage: mail-poll.sh <mailbox> [--timeout N] [--match SUBSTR]
#                    [--reply-to <msgid>] [--lease S] [--peek]
#
# Claim = one serialized transition under <box>/.lock: atomic mv to
# claimed/<name>-claim-<tok>.md + fresh mtime (claim time, lease start).
# Exactly one poller wins a message; print ≠ done. Acknowledge with
#   mail-ack.sh <box> <claimed-file> <tok>     (per message)
#   mail-renew.sh <box> <claimed-file> <tok>   (extend lease)
# Claims idle past --lease (default 300s) are requeued on the next
# claiming poll — duplicates are possible; consumers dedupe by msgid.
# A stale token cannot ack a newer claim (generation check in mail-ack).
# --reply-to <msgid> matches the parsed -re-<msgid>.md suffix exactly.
# --match SUBSTR is generic substring inspection only.
# --peek lists inbox read-only: no claims, no requeue, no state change.
# Requires GNU date (%N), GNU stat, flock.
set -euo pipefail

MAIL_ROOT="${AGENT_MAIL_ROOT:-/tmp/agent-mail}"

usage() {
  echo "usage: $(basename "$0") <mailbox> [--timeout N] [--match S] [--reply-to ID] [--lease S] [--peek]" >&2
  exit 2
}

[ $# -ge 1 ] || usage
box="$1"; shift
timeout=600; peek=0; match=""; replyto=""; lease=300
while [ $# -gt 0 ]; do
  case "$1" in
    --timeout) timeout="$2"; shift 2 ;;
    --match) match="$2"; shift 2 ;;
    --reply-to) replyto="$2"; shift 2 ;;
    --lease) lease="$2"; shift 2 ;;
    --peek) peek=1; shift ;;
    *) usage ;;
  esac
done

[[ "$box" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]] || { echo "mail-poll: invalid mailbox '$box'" >&2; exit 2; }
if [ -n "$replyto" ] && ! [[ "$replyto" =~ ^[a-f0-9]{8}$ ]]; then
  echo "mail-poll: invalid --reply-to '$replyto' (want 8-hex msgid)" >&2; exit 2
fi

inbox="$MAIL_ROOT/$box/inbox"
claimed="$MAIL_ROOT/$box/claimed"
mkdir -p "$inbox" "$claimed"
shopt -s nullglob

# candidate glob: strict reply correlation, else optional substring filter
if [ -n "$replyto" ]; then
  glob_suffix="*-re-$replyto.md"   # strict: parsed suffix field, end of name
elif [ -n "$match" ]; then
  glob_suffix="*$match*.md"
else
  glob_suffix="*.md"
fi

exec 9>"$MAIL_ROOT/$box/.lock"
deadline=$(( $(date +%s) + timeout ))

while :; do
  if [ "$peek" = 1 ]; then
    found=0
    for f in "$inbox"/$glob_suffix; do
      printf '=== %s (peek)\n' "$f"
      cat "$f"; printf '\n'; found=1
    done
    [ "$found" = 1 ] && exit 0
  else
    found=0
    flock 9
    # requeue claims idle past their lease (claim time = file mtime)
    now=$(date +%s)
    for c in "$claimed"/*-claim-????????.md; do
      if [ $(( now - $(stat -c %Y "$c") )) -ge "$lease" ]; then
        cn=$(basename "$c")
        mv -n "$c" "$inbox/${cn%-claim-????????.md}.md" 2>/dev/null || true
      fi
    done
    # claim candidates oldest-first
    for f in "$inbox"/$glob_suffix; do
      name=$(basename "$f" .md)
      tok=$(openssl rand -hex 4)
      tgt="$claimed/${name}-claim-${tok}.md"
      if mv -n "$f" "$tgt" 2>/dev/null && [ ! -e "$f" ]; then
        touch "$tgt"   # claim time = lease start
        printf '=== %s\nclaim-token=%s\n' "$tgt" "$tok"
        cat "$tgt"; printf '\n'; found=1
      fi
    done
    flock -u 9
    [ "$found" = 1 ] && exit 0
  fi

  if [ "$timeout" -ne 0 ] && [ "$(date +%s)" -ge "$deadline" ]; then
    echo "mail-poll: timeout after ${timeout}s" >&2
    exit 1
  fi
  sleep 2
done

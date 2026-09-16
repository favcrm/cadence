#!/usr/bin/env bash
# note-publish.sh — publish a handover note under the notes lock.
# usage: note-publish.sh <session5> <slug> <kickoff|qa|verdict> <file|->
# Writers MUST use this script (not direct file writes): the lock is what
# makes append-vs-prune race-free. Regenerates index.html after publish.
set -euo pipefail

DIR="${AGENT_NOTES_DIR:-/var/www/agent-notes}"
HERE="$(cd "$(dirname "$0")" && pwd)"

usage() {
  echo "usage: $(basename "$0") <session5> <slug> <kickoff|qa|verdict> <file|->" >&2
  exit 2
}
[ $# -eq 4 ] || usage
sess="$1"; slug="$2"; type="$3"; src="$4"

[[ "$sess" =~ ^[a-f0-9]{5}$ ]] || { echo "note-publish: bad session '$sess' (want 5-hex)" >&2; exit 2; }
[[ "$slug" =~ ^[a-z0-9]([a-z0-9-]{0,46}[a-z0-9])?$ ]] || { echo "note-publish: bad slug" >&2; exit 2; }
[[ "$type" =~ ^(kickoff|qa|verdict)$ ]] || { echo "note-publish: bad type '$type'" >&2; exit 2; }

mkdir -p "$DIR"
exec 9>"$DIR/.notes.lock"; flock 9

# A chain gets at most one note per UTC second. This preserves the strict
# filename contract and makes its final note unambiguous for retention.
shopt -s nullglob
while :; do
  ts=$(date -u +%Y%m%d-%H%M%S)
  existing=("$DIR/${ts}-${sess}-"*.md)
  [ "${#existing[@]}" -eq 0 ] && break
  sleep 1
done
dest="$DIR/${ts}-${sess}-${slug}-${type}.md"

tmp="$DIR/.tmp-$$"
if [ "$src" = "-" ]; then cat > "$tmp"; else cp "$src" "$tmp"; fi
mv -n "$tmp" "$dest"
if [ -e "$tmp" ]; then rm -f "$tmp"; echo "note-publish: collision" >&2; exit 1; fi
flock -u 9

"$HERE/notes-index.sh" >/dev/null
printf '%s\n' "$dest"

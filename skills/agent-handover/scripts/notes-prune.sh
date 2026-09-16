#!/usr/bin/env bash
# notes-prune.sh — delete TERMINATED note chains idle >30d; rebuild index.
# A chain is prunable only when (a) it ends with a `verdict` note (the
# terminal record) AND (b) its newest note is >30d old. Chains without a
# verdict are retained regardless of age — inactivity is not terminal;
# clean those up manually. Only files matching the strict note contract
# are candidates; every other file is preserved. Writers share .notes.lock
# via note-publish.sh, so an append cannot race this scan+delete.
set -euo pipefail

DIR="${AGENT_NOTES_DIR:-/var/www/agent-notes}"
HERE="$(cd "$(dirname "$0")" && pwd)"
cutoff=$(( $(date +%s) - 30 * 86400 ))

exec 9>"$DIR/.notes.lock"; flock 9

declare -A newest latest terminal
for f in "$DIR"/*.md; do
  [ -e "$f" ] || continue
  base=$(basename "$f" .md)
  if [[ "$base" =~ ^([0-9]{8}-[0-9]{6})-([a-f0-9]{5})-.+-(kickoff|qa|verdict)$ ]]; then
    sess="${BASH_REMATCH[2]}"; ty="${BASH_REMATCH[3]}"
    mt=$(stat -c %Y "$f")
    if [ -z "${newest[$sess]:-}" ] || [ "$mt" -gt "${newest[$sess]}" ]; then
      newest[$sess]=$mt
    fi
    ts="${BASH_REMATCH[1]}"
    if [ -z "${latest[$sess]:-}" ] || [[ "$ts" > "${latest[$sess]}" ]]; then
      latest[$sess]="$ts"
      terminal[$sess]=0
      [ "$ty" != verdict ] || terminal[$sess]=1
    elif [ "$ts" = "${latest[$sess]}" ] && [ "$ty" != verdict ]; then
      # Legacy same-second ambiguity: retain rather than assume closed.
      terminal[$sess]=0
    fi
  fi
done

n=0
for f in "$DIR"/*.md; do
  [ -e "$f" ] || continue
  base=$(basename "$f" .md)
  if [[ "$base" =~ ^([0-9]{8}-[0-9]{6})-([a-f0-9]{5})-.+-(kickoff|qa|verdict)$ ]]; then
    sess="${BASH_REMATCH[2]}"
    if [ "${terminal[$sess]:-}" = 1 ] && [ "${newest[$sess]}" -lt "$cutoff" ]; then
      rm -f "$f"; n=$((n + 1))
    fi
  fi
done
flock -u 9

"$HERE/notes-index.sh" >/dev/null
printf '%s pruned %s note(s)\n' "$(date -u +%FT%TZ)" "$n" >> "$DIR/.prune.log"
echo "pruned $n note(s); index rebuilt"

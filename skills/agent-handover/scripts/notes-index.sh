#!/usr/bin/env bash
# notes-index.sh — rebuild /var/www/agent-notes/index.html from note files.
# Filename contract: YYYYMMDD-HHMMSS-<session5>-<slug>-<type>.md
# Groups by session: title (first note's H1), created, last update, count.
set -euo pipefail

DIR="${AGENT_NOTES_DIR:-/var/www/agent-notes}"
OUT="$DIR/index.html"
mkdir -p "$DIR"

# Share the publication/pruning lock while reading note files.
exec 8>"$DIR/.notes.lock"
flock -s 8

# serialize concurrent rebuilds — last writer wins, no torn index.html
exec 9>"$DIR/.index.lock"
flock 9

esc() { sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g; s/"/\&quot;/g'; }
pretty() { # 20260916-014111 -> 2026-09-16 01:41 UTC
  printf '%s-%s-%s %s:%s UTC' "${1:0:4}" "${1:4:2}" "${1:6:2}" "${1:9:2}" "${1:11:2}"
}

# collect: session|ts|type|title|file  (tab-separated, per note)
rows=$(mktemp)
trap 'rm -f "$rows"' EXIT
shopt -s nullglob
for f in "$DIR"/*.md; do
  base=$(basename "$f" .md)
  [[ "$base" =~ ^([0-9]{8}-[0-9]{6})-([a-f0-9]{5})-.+-(kickoff|qa|verdict)$ ]] || continue
  ts="${BASH_REMATCH[1]}"; sess="${BASH_REMATCH[2]}"; type="${BASH_REMATCH[3]}"
  title=$(grep -m1 '^# ' "$f" | sed 's/^# *//' || true)
  if [ -z "$title" ]; then title="$base"; fi
  printf '%s\t%s\t%s\t%s\t%s\n' "$sess" "$ts" "$type" "$title" "$(basename "$f")" >> "$rows"
done

# per-session aggregates, last-update desc: last_ts, session, first_ts, count, closed
agg=$(awk -F'\t' '
  { c[$1]++
    if (!($1 in hi) || $2 > hi[$1]) { hi[$1]=$2; t[$1]=($3 == "verdict") }
    else if ($2 == hi[$1] && $3 != "verdict") t[$1]=0
    if (!($1 in lo) || $2 < lo[$1]) lo[$1]=$2 }
  END { for (s in hi) print hi[s]"\t"s"\t"lo[s]"\t"c[s]"\t"(t[s] ? "closed" : "open") }' "$rows" | sort -r)

{
cat <<'HTML'
<!doctype html><meta charset="utf-8"><title>agent-notes</title>
<style>
body{font:14px/1.5 ui-monospace,monospace;max-width:1100px;margin:2rem auto;padding:0 1rem;color:#1a1a1a}
h1{font-size:1.1rem}table{border-collapse:collapse;width:100%}
td,th{border-bottom:1px solid #ddd;padding:.4rem .6rem;text-align:left;vertical-align:top}
th{font-size:.8rem;color:#666}details summary{cursor:pointer;color:#555}
details ul{margin:.3rem 0 .3rem 1.2rem;padding:0}a{color:#0645ad}
.meta{color:#888;font-size:.8rem}
</style>
HTML
printf '<h1>agent-notes</h1>\n<p class="meta">regenerated %s</p>\n' "$(date -u +'%F %T UTC')"
if [ -z "$agg" ]; then
  echo '<p class="meta">no notes</p>'
else
  echo '<table><tr><th>session</th><th>title</th><th>created</th><th>last update</th><th>notes</th></tr>'
  while IFS=$'\t' read -r last_ts sess first_ts count state; do
    # title + thread list, notes ordered oldest-first
    title=""; detail="<ul>"
    while IFS=$'\t' read -r s ts ty ti fn; do
      [ "$s" = "$sess" ] || continue
      if [ -z "$title" ]; then title="$ti"; fi
      detail+=$(printf '<li><a href="%s">%s</a> <span class="meta">%s · %s</span></li>' \
        "$(printf '%s' "$fn" | esc)" "$(printf '%s' "$ty" | esc)" "$(pretty "$ts")" "$(printf '%s' "$ti" | esc)")
    done < <(sort -t$'\t' -k2 "$rows")
    detail+="</ul>"
    printf '<tr><td><code>%s</code> <span class="meta">%s</span></td><td>%s<details><summary>thread</summary>%s</details></td><td>%s</td><td>%s</td><td>%s</td></tr>\n' \
      "$sess" "$state" "$(printf '%s' "$title" | esc)" "$detail" "$(pretty "$first_ts")" "$(pretty "$last_ts")" "$count"
  done <<< "$agg"
  echo '</table>'
fi
} > "$OUT.tmp.$$" && mv "$OUT.tmp.$$" "$OUT"
printf '%s\n' "$OUT"

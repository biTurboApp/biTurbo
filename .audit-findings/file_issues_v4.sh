#!/bin/bash
# Title-keyed filer: skips filed titles + skip_dupes.txt; backoff 7min on secondary limit.
LOG=.audit-findings/created.log
touch "$LOG"
filed_titles=$(head -5 .audit-findings/issues.jsonl.prev 2>/dev/null)
# seed: titles of the 5 already-filed lines are stored in filed_titles.txt (one-time)
created=0
total=$(wc -l < .audit-findings/issues.jsonl | tr -d ' ')
n=0
while [ $n -lt $total ]; do
  n=$((n+1))
  line=$(sed -n "${n}p" .audit-findings/issues.jsonl)
  title=$(printf '%s' "$line" | jq -r .title)
  grep -qF "OK $title" "$LOG" && continue
  body=$(printf '%s' "$line" | jq -r .body)
  args=""
  for l in $(printf '%s' "$line" | jq -r '.labels[]'); do
    args="$args -f labels[]=$l"
  done
  tries=0
  while true; do
    # shellcheck disable=SC2086
    url=$(gh api repos/biTurboApp/biTurbo/issues -f title="$title" -f body="$body" $args --jq .html_url 2>&1)
    rc=$?
    if [ $rc -eq 0 ] && [ -n "$url" ]; then
      created=$((created+1)); echo "OK $title :: $url" >> "$LOG"
      break
    fi
    if echo "$url" | grep -qi "secondary rate limit\|temporarily blocked"; then
      tries=$((tries+1))
      echo "BACKOFF try=$tries $(date -u +%H:%M:%S) $title" >> "$LOG"
      sleep 1500
      continue
    fi
    echo "FAIL $title :: $(echo "$url" | head -c 200)" >> "$LOG"
    break
  done
  sleep 25
done
echo "done created=$created"

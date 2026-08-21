#!/bin/bash
LOG=.audit-findings/created.log
touch "$LOG"
created=0
n=0
total=$(wc -l < .audit-findings/issues.jsonl)
while [ $n -lt $total ]; do
  n=$((n+1))
  grep -q "^OK $n " "$LOG" && continue
  line=$(sed -n "${n}p" .audit-findings/issues.jsonl)
  title=$(printf '%s' "$line" | jq -r .title)
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
      created=$((created+1)); echo "OK $n $url" >> "$LOG"
      break
    fi
    if echo "$url" | grep -qi "secondary rate limit\|temporarily blocked"; then
      tries=$((tries+1))
      echo "BACKOFF $n try=$tries $(date -u +%H:%M:%S)" >> "$LOG"
      sleep 420
      continue
    fi
    echo "FAIL $n :: $(echo "$url" | head -c 200)" >> "$LOG"
    break
  done
  sleep 20
done
echo "done created=$created"

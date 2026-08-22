#!/bin/bash
START=${1:-1}; END=${2:-999}; DELAY=${3:-11}
LOG=.audit-findings/created.log
touch "$LOG"
n=0; created=0; failed=0
while IFS= read -r line; do
  n=$((n+1))
  [ "$n" -lt "$START" ] && continue
  [ "$n" -gt "$END" ] && break
  grep -q "^OK $n " "$LOG" && continue
  title=$(printf '%s' "$line" | jq -r .title)
  body=$(printf '%s' "$line" | jq -r .body)
  args=""
  for l in $(printf '%s' "$line" | jq -r '.labels[]'); do
    args="$args -f labels[]=$l"
  done
  # shellcheck disable=SC2086
  url=$(gh api repos/biTurboApp/biTurbo/issues -f title="$title" -f body="$body" $args --jq .html_url 2>&1)
  rc=$?
  if [ $rc -eq 0 ] && [ -n "$url" ]; then
    created=$((created+1)); echo "OK $n $url" >> "$LOG"
  elif echo "$url" | grep -qi "secondary rate limit\|temporarily blocked"; then
    echo "RATELIMIT $n" >> "$LOG"; echo "rate-limited at $n, stopping"; break
  else
    failed=$((failed+1)); echo "FAIL $n :: $(echo "$url" | head -c 200)" >> "$LOG"
  fi
  sleep "$DELAY"
done < .audit-findings/issues.jsonl
echo "created=$created failed=$failed"

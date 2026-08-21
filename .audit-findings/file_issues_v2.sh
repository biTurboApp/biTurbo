#!/bin/bash
# Throttled issue filer with resume. usage: file_issues_v2.sh <start> <end> [delay_seconds]
START=${1:-1}; END=${2:-50}; DELAY=${3:-13}
LOG=.audit-findings/created.log
touch "$LOG"
n=0; created=0; failed=0
while IFS= read -r line; do
  n=$((n+1))
  [ "$n" -lt "$START" ] && continue
  [ "$n" -gt "$END" ] && break
  if grep -q "^OK $n " "$LOG"; then continue; fi
  title=$(printf '%s' "$line" | jq -r .title)
  printf '%s' "$line" | jq -r .body > /tmp/issue_body.md
  args=""
  for l in $(printf '%s' "$line" | jq -r '.labels[]'); do
    args="$args --add-label $l"
  done
  # shellcheck disable=SC2086
  url=$(gh issue create --repo biTurboApp/biTurbo --title "$title" --body-file /tmp/issue_body.md $args 2>&1)
  rc=$?
  if [ $rc -eq 0 ] && [ -n "$url" ]; then
    created=$((created+1)); echo "OK $n $url" >> "$LOG"
  elif echo "$url" | grep -qi "secondary rate limit\|temporarily blocked"; then
    echo "RATELIMIT $n" >> "$LOG"
    echo "rate-limited at $n, stopping"; failed=$((failed+1)); break
  else
    failed=$((failed+1)); echo "FAIL $n :: $url" >> "$LOG"
  fi
  sleep "$DELAY"
done < .audit-findings/issues.jsonl
echo "created=$created failed=$failed"

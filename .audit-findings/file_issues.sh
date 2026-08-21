#!/bin/bash
# usage: file_issues.sh <start_line> <end_line>
START=${1:-1}; END=${2:-35}
n=0; created=0; failed=0
: > .audit-findings/batch.log
while IFS= read -r line; do
  n=$((n+1))
  [ "$n" -lt "$START" ] && continue
  [ "$n" -gt "$END" ] && break
  title=$(jq -r .title <<<"$line")
  jq -r .body <<<"$line" > /tmp/issue_body_$$.md
  mapfile -t labels < <(jq -r '.labels[]' <<<"$line")
  args=()
  for l in "${labels[@]}"; do args+=(--label "$l"); done
  if url=$(gh issue create --repo biTurboApp/biTurbo --title "$title" --body-file /tmp/issue_body_$$.md "${args[@]}" 2>&1); then
    created=$((created+1)); echo "OK $n $url" >> .audit-findings/batch.log
  else
    failed=$((failed+1)); echo "FAIL $n $title :: $url" >> .audit-findings/batch.log
  fi
  rm -f /tmp/issue_body_$$.md
done < .audit-findings/issues.jsonl
echo "created=$created failed=$failed"

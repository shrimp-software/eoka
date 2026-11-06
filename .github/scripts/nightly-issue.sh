#!/usr/bin/env bash
# Keep one open issue per failing scheduled workflow: open or comment on
# failure, close on the next green run. Usage: nightly-issue.sh <success|failure>
set -euo pipefail

title="Nightly ${GITHUB_WORKFLOW} failing"
run_url="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
issue=$(gh issue list --state open --search "in:title \"${title}\"" --json number,title \
  --jq "map(select(.title == \"${title}\")) | .[0].number // empty")

if [[ "$1" == success ]]; then
  if [[ -n "$issue" ]]; then
    gh issue close "$issue" --comment "Green again: ${run_url}"
  fi
elif [[ -n "$issue" ]]; then
  gh issue comment "$issue" --body "Still failing: ${run_url}"
else
  gh issue create --title "$title" --body "Scheduled run failed: ${run_url}"
fi

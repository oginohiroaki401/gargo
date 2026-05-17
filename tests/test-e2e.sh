#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

TEST_THREADS="${TEST_THREADS:-1}"

run_test() {
  local test_name="$1"
  local retries="${2:-0}"
  local attempt=1

  while true; do
    echo "==> cargo test --test ${test_name} (attempt ${attempt})"
    if cargo test --test "${test_name}" -- --nocapture --test-threads="${TEST_THREADS}"; then
      return 0
    fi
    if (( attempt > retries )); then
      return 1
    fi

    echo "Retrying ${test_name}..."
    attempt=$((attempt + 1))
    sleep 1
  done
}

run_test editor_flow_e2e
run_test paste_multiline_e2e
run_test visual_yank_paste_e2e
run_test save_as_e2e
run_test diff_server_e2e 3
run_test github_preview_server_e2e 3
run_test github_server_e2e 3
run_test verify_history_e2e
run_test overlay_preview_scroll_e2e
run_test render_snapshot_e2e

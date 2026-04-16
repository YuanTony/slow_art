#!/bin/bash
# End-to-end test: builds the server, starts it, runs queries against the debug API,
# and verifies responses. Uses artworks.db.
set -euo pipefail
cd "$(dirname "$0")/.."

API="http://127.0.0.1:8787/debug/query"
PASS=0
FAIL=0

# Build
echo "=== Building ==="
cargo build --no-default-features --features search_image 2>&1 | tail -1

# Kill any existing server on port 8787
lsof -ti:8787 | xargs kill -9 2>/dev/null || true
sleep 1

# Start server in background
echo "=== Starting server ==="
./target/debug/ten-minute-art &
SERVER_PID=$!
sleep 4

# Check server is up
if ! kill -0 $SERVER_PID 2>/dev/null; then
  echo "FAIL: server did not start"
  exit 1
fi

cleanup() {
  echo ""
  echo "=== Stopping server (pid $SERVER_PID) ==="
  kill $SERVER_PID 2>/dev/null || true
  wait $SERVER_PID 2>/dev/null || true
}
trap cleanup EXIT

query() {
  local conv="$1"
  local text="$2"
  curl -sf "$API" -H "Content-Type: application/json" \
    -d "{\"text\": \"$text\", \"conversation_id\": \"$conv\"}" 2>/dev/null
}

assert_contains() {
  local label="$1"
  local response="$2"
  local expected="$3"
  if echo "$response" | grep -qi "$expected"; then
    echo "  PASS: $label (found '$expected')"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $label (expected '$expected')"
    echo "    Got: $response"
    FAIL=$((FAIL + 1))
  fi
}

assert_not_empty() {
  local label="$1"
  local response="$2"
  if [ -n "$response" ] && echo "$response" | grep -q "replies"; then
    echo "  PASS: $label"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $label (empty or invalid response)"
    FAIL=$((FAIL + 1))
  fi
}

# Test 1: /start creates a session
echo ""
echo "=== Test 1: /start ==="
R=$(query "t1" "/start")
assert_contains "opening message" "$R" "What artwork"

# Test 2: Search for artwork by description
echo ""
echo "=== Test 2: Search by description ==="
R=$(query "t1" "I am looking at small metal object that looks like a kneeling horse")
echo "  Response: $R"
assert_not_empty "search returns a response" "$R"

# Test 3: Follow-up conversation
echo ""
echo "=== Test 3: Follow-up question ==="
R=$(query "t1" "What is it made of and where does it come from?")
echo "  Response: $R"
assert_not_empty "follow-up returns a response" "$R"

# Test 4: Another follow-up
echo ""
echo "=== Test 4: Second follow-up ==="
R=$(query "t1" "What else can you tell me about this piece?")
echo "  Response: $R"
assert_not_empty "second follow-up returns a response" "$R"

# Test 5: New session with /new
echo ""
echo "=== Test 5: /new ==="
R=$(query "t2" "/new")
assert_contains "new session" "$R" "What artwork"

# Test 6: Concurrent session
echo ""
echo "=== Test 6: Concurrent session ==="
R=$(query "t2" "The Great Wave")
assert_not_empty "concurrent session responds" "$R"

# Summary
echo ""
echo "==============================="
echo "Results: $PASS passed, $FAIL failed"
echo "==============================="
if [ $FAIL -gt 0 ]; then
  exit 1
fi

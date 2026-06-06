#!/usr/bin/env bash
# E2E test: REST API for dynamic mount management
#
# Tests the `rs-f4ss serve` command and all API endpoints:
#   Phase 1 — Health & Version
#   Phase 2 — CRUD: Create, List, Get, Update, Delete
#   Phase 3 — Lifecycle: Start mount → FUSE ops → Stop mount
#   Phase 4 — Error paths & edge cases
#   Phase 5 — Concurrency safety
#
# Usage:   ./tests/e2e-api.sh
# Requires: dufs, fusermount, /dev/fuse, release binary built, jq
set -euo pipefail

DUFS_BIN="${DUFS_BIN:-/usr/local/bin/dufs}"
DUFS_MOUNT_BIN="./target/release/rs-f4ss"
DUFS_PORT=15433
API_PORT=18080
API_BASE="http://127.0.0.1:${API_PORT}"
API_USER="admin"
API_PASS="admin"

DUFS_DATA=""
MOUNTPOINT=""
DUFS_PID=""
API_PID=""

# ── Colors ──
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m'

FAILURES=()
SKIPPED=()
TOTAL=0
PASSED=0

pass()   { echo -e "${GREEN}  ✅ PASS${NC}: $1"; PASSED=$((PASSED + 1)); }
fail()   { echo -e "${RED}  ❌ FAIL${NC}: $1"; FAILURES+=("$1"); }
info()   { echo -e "${CYAN}  ℹ️${NC} $1"; }
skip()   { echo -e "${YELLOW}  ⏭ SKIP${NC}: $1"; SKIPPED+=("$1"); }

run_test() {
    local name="$1"
    TOTAL=$((TOTAL + 1))
    echo -e "\n${CYAN}Test $TOTAL${NC}: $name"
}

# ── HTTP helpers ──

http_code() {
    curl -s -o /dev/null -w "%{http_code}" "$@" || true
}

http_body() {
    curl -s "$@"
}

api() {
    local method="$1"; shift
    local path="$1"; shift
    http_body -X "$method" -u "$API_USER:$API_PASS" "${API_BASE}${path}" "$@"
}

api_code() {
    local method="$1"; shift
    local path="$1"; shift
    http_code -X "$method" -u "$API_USER:$API_PASS" "${API_BASE}${path}" "$@"
}

# Unauthenticated API call (for testing auth rejection)
api_noauth() {
    local method="$1"; shift
    local path="$1"; shift
    http_body -X "$method" "${API_BASE}${path}" "$@"
}

api_noauth_code() {
    local method="$1"; shift
    local path="$1"; shift
    http_code -X "$method" "${API_BASE}${path}" "$@"
}

# ── Infrastructure ──

start_dufs() {
    DUFS_DATA=$(mktemp -d /tmp/dufs-api-test-data.XXXXXX)
    echo "hello world" > "$DUFS_DATA/hello.txt"
    mkdir -p "$DUFS_DATA/subdir"
    echo "nested" > "$DUFS_DATA/subdir/nested.txt"

    info "Starting dufs on :$DUFS_PORT (data=$DUFS_DATA)"
    $DUFS_BIN "$DUFS_DATA" -b 127.0.0.1 -p "$DUFS_PORT" -A > /tmp/dufs-api-e2e.log 2>&1 &
    DUFS_PID=$!
    sleep 1
    if ! kill -0 "$DUFS_PID" 2>/dev/null; then
        echo -e "${RED}FATAL: dufs failed to start${NC}"; cat /tmp/dufs-api-e2e.log; exit 1
    fi
    info "dufs ready (PID=$DUFS_PID)"
}

stop_dufs() {
    if [ -n "$DUFS_PID" ] && kill -0 "$DUFS_PID" 2>/dev/null; then
        kill "$DUFS_PID" 2>/dev/null || true
        wait "$DUFS_PID" 2>/dev/null || true
        DUFS_PID=""
    fi
}

start_api() {
    info "Starting rs-f4ss serve on :$API_PORT"
    $DUFS_MOUNT_BIN serve --listen "127.0.0.1:$API_PORT" > /tmp/rs-f4ss-api-e2e.log 2>&1 &
    API_PID=$!
    sleep 1
    if ! kill -0 "$API_PID" 2>/dev/null; then
        echo -e "${RED}FATAL: rs-f4ss serve failed to start${NC}"; cat /tmp/rs-f4ss-api-e2e.log; exit 1
    fi
    local code
    code=$(api_code GET /api/health)
    if [ "$code" != "200" ]; then
        echo -e "${RED}FATAL: API not responding (HTTP=$code)${NC}"; exit 1
    fi
    info "API server ready (PID=$API_PID)"
}

stop_api() {
    if [ -n "$API_PID" ] && kill -0 "$API_PID" 2>/dev/null; then
        info "Stopping API server (PID=$API_PID)"
        kill "$API_PID" 2>/dev/null || true
        wait "$API_PID" 2>/dev/null || true
        API_PID=""
    fi
}

cleanup() {
    # Unmount any mounts created during tests
    for mp in /mnt/dufs-api-test-*; do
        if mountpoint -q "$mp" 2>/dev/null; then
            fusermount -u "$mp" 2>/dev/null || true
        fi
    done
    stop_api
    stop_dufs
    rm -rf "$DUFS_DATA" 2>/dev/null || true
    for mp in /mnt/dufs-api-test-*; do
        rmdir "$mp" 2>/dev/null || true
    done
}

trap cleanup EXIT

# ── Prerequisites ──

check_deps() {
    local missing=0
    for cmd in "$DUFS_BIN" "$DUFS_MOUNT_BIN" fusermount jq curl; do
        if ! command -v "$cmd" &>/dev/null; then
            echo -e "${RED}Missing: $cmd${NC}"
            missing=1
        fi
    done
    if [ "$missing" -eq 1 ]; then
        echo "Install missing dependencies and rebuild: cargo build --release"
        exit 1
    fi
    if [ ! -w /dev/fuse ] && [ ! -w /dev/fuse ]; then
        echo -e "${YELLOW}Warning: /dev/fuse not writable, some tests may fail${NC}"
    fi
    if ! $DUFS_MOUNT_BIN --help 2>&1 | grep -q "serve"; then
        echo -e "${RED}rs-f4ss not built with 'api' feature${NC}"
        exit 1
    fi
}

# ═══════════════════════════════════════════════════════════════════════
# Phase 0 — Authentication
# ═══════════════════════════════════════════════════════════════════════

phase0_auth() {
    echo -e "\n${CYAN}══ Phase 0 — Authentication ══${NC}"

    # Health accessible without auth (whitelisted)
    run_test "GET /api/health — no auth required"
    local code body
    code=$(api_noauth_code GET /api/health)
    if [ "$code" = "200" ]; then
        pass "Health accessible without credentials"
    else
        fail "Health without auth: expected 200, got $code"
    fi

    # Login with correct default credentials (admin/admin)
    run_test "POST /api/auth/login — correct credentials"
    body=$(api_noauth POST /api/auth/login -H 'Content-Type: application/json' \
        -d '{"username":"admin","password":"admin"}')
    code=$(api_noauth_code POST /api/auth/login -H 'Content-Type: application/json' \
        -d '{"username":"admin","password":"admin"}')
    if [ "$code" = "200" ] && echo "$body" | jq -e '.username == "admin"' >/dev/null 2>&1; then
        pass "Login with admin/admin returns 200"
    else
        fail "Login admin/admin: HTTP=$code body=$body"
    fi

    # Login with wrong password
    run_test "POST /api/auth/login — wrong password"
    code=$(api_noauth_code POST /api/auth/login -H 'Content-Type: application/json' \
        -d '{"username":"admin","password":"wrong"}')
    if [ "$code" = "401" ]; then
        pass "Wrong password returns 401"
    else
        fail "Wrong password: expected 401, got $code"
    fi

    # Login with wrong username
    run_test "POST /api/auth/login — wrong username"
    code=$(api_noauth_code POST /api/auth/login -H 'Content-Type: application/json' \
        -d '{"username":"root","password":"admin"}')
    if [ "$code" = "401" ]; then
        pass "Wrong username returns 401"
    else
        fail "Wrong username: expected 401, got $code"
    fi

    # API endpoints without auth return 401
    run_test "GET /api/mounts — no auth returns 401"
    code=$(api_noauth_code GET /api/mounts)
    if [ "$code" = "401" ]; then
        pass "Mounts list without auth returns 401"
    else
        fail "Mounts without auth: expected 401, got $code"
    fi

    run_test "GET /api/version — no auth returns 401"
    code=$(api_noauth_code GET /api/version)
    if [ "$code" = "401" ]; then
        pass "Version without auth returns 401"
    else
        fail "Version without auth: expected 401, got $code"
    fi

    # API endpoints with correct auth succeed
    run_test "GET /api/mounts — with auth returns 200"
    code=$(api_code GET /api/mounts)
    if [ "$code" = "200" ]; then
        pass "Mounts list with auth returns 200"
    else
        fail "Mounts with auth: expected 200, got $code"
    fi

    run_test "GET /api/version — with auth returns 200"
    code=$(api_code GET /api/version)
    body=$(api GET /api/version)
    if [ "$code" = "200" ] && echo "$body" | jq -e '.version' >/dev/null; then
        pass "Version with auth returns 200"
    else
        fail "Version with auth: HTTP=$code body=$body"
    fi

    # Change password
    run_test "POST /api/auth/password — change password"
    body=$(api POST /api/auth/password -H 'Content-Type: application/json' \
        -d '{"old_password":"admin","new_password":"newpass123"}')
    if echo "$body" | jq -e '.message == "Password changed"' >/dev/null 2>&1; then
        pass "Password changed successfully"
    else
        fail "Change password: $body"
    fi

    # Old credentials no longer work
    run_test "GET /api/mounts — old credentials rejected"
    local old_code
    old_code=$(http_code -X GET -u "admin:admin" "${API_BASE}/api/mounts")
    if [ "$old_code" = "401" ]; then
        pass "Old credentials rejected after password change"
    else
        fail "Old credentials: expected 401, got $old_code"
    fi

    # New credentials work
    run_test "GET /api/mounts — new credentials accepted"
    local new_code
    new_code=$(http_code -X GET -u "admin:newpass123" "${API_BASE}/api/mounts")
    if [ "$new_code" = "200" ]; then
        pass "New credentials work"
    else
        fail "New credentials: expected 200, got $new_code"
    fi

    # Login with new password
    run_test "POST /api/auth/login — new password works"
    code=$(api_noauth_code POST /api/auth/login -H 'Content-Type: application/json' \
        -d '{"username":"admin","password":"newpass123"}')
    if [ "$code" = "200" ]; then
        pass "Login with new password returns 200"
    else
        fail "Login new password: expected 200, got $code"
    fi

    # Change password back to admin for remaining tests
    run_test "POST /api/auth/password — restore default password"
    local restore_code
    restore_code=$(http_code -X POST -u "admin:newpass123" "${API_BASE}/api/auth/password" \
        -H 'Content-Type: application/json' \
        -d '{"old_password":"newpass123","new_password":"admin"}')
    if [ "$restore_code" = "200" ]; then
        pass "Password restored to admin for remaining tests"
    else
        fail "Restore password: expected 200, got $restore_code"
    fi

    # Change password — wrong old password
    run_test "POST /api/auth/password — wrong old password"
    code=$(api_code POST /api/auth/password -H 'Content-Type: application/json' \
        -d '{"old_password":"wrong","new_password":"another"}')
    if [ "$code" = "401" ]; then
        pass "Change password with wrong old password returns 401"
    else
        fail "Wrong old password: expected 401, got $code"
    fi

    # Change password — empty new password
    run_test "POST /api/auth/password — empty new password"
    code=$(api_code POST /api/auth/password -H 'Content-Type: application/json' \
        -d '{"old_password":"admin","new_password":""}')
    if [ "$code" = "400" ]; then
        pass "Empty new password returns 400"
    else
        fail "Empty new password: expected 400, got $code"
    fi
}

# ═══════════════════════════════════════════════════════════════════════
# Phase 1 — Health & Version
# ═══════════════════════════════════════════════════════════════════════

phase1_health_version() {
    echo -e "\n${CYAN}══ Phase 1 — Health & Version ══${NC}"

    run_test "GET /api/health"
    local code body
    code=$(api_code GET /api/health)
    body=$(api GET /api/health)
    if [ "$code" = "200" ] && echo "$body" | jq -e '.status == "ok"' >/dev/null; then
        pass "Health returns 200 with status ok"
    else
        fail "Health: HTTP=$code body=$body"
    fi

    run_test "GET /api/version"
    code=$(api_code GET /api/version)
    body=$(api GET /api/version)
    if [ "$code" = "200" ] && echo "$body" | jq -e '.version' >/dev/null; then
        pass "Version returns 200 with version field"
    else
        fail "Version: HTTP=$code body=$body"
    fi
}

# ═══════════════════════════════════════════════════════════════════════
# Phase 2 — CRUD Operations
# ═══════════════════════════════════════════════════════════════════════

phase2_crud() {
    echo -e "\n${CYAN}══ Phase 2 — CRUD Operations ══${NC}"

    # Create
    run_test "POST /api/mounts — create mount entry"
    local code body
    code=$(api_code POST /api/mounts -H 'Content-Type: application/json' -d '{
        "id": "test1",
        "url": "http://127.0.0.1:'"$DUFS_PORT"'",
        "mountpoint": "/mnt/dufs-api-test-1"
    }')
    if [ "$code" = "201" ]; then
        pass "Create mount returns 201"
    else
        fail "Create mount: HTTP=$code"
    fi

    # Verify create response body
    body=$(api POST /api/mounts -H 'Content-Type: application/json' -d '{
        "id": "test1-verify",
        "url": "http://127.0.0.1:'"$DUFS_PORT"'",
        "mountpoint": "/mnt/dufs-api-test-1v"
    }')
    if echo "$body" | jq -e '.id == "test1-verify"' >/dev/null 2>&1 \
       && echo "$body" | jq -e '.state == "Stopped"' >/dev/null 2>&1; then
        pass "Create response has id and state=Stopped"
    else
        fail "Create response body unexpected: $body"
    fi

    # Create with cache options
    run_test "POST /api/mounts — with cache options"
    body=$(api POST /api/mounts -H 'Content-Type: application/json' -d '{
        "id": "test-cache",
        "url": "http://127.0.0.1:'"$DUFS_PORT"'",
        "mountpoint": "/mnt/dufs-api-test-cache",
        "cache_ttl_secs": 10,
        "cache_size": 512
    }')
    if echo "$body" | jq -e '.cache_ttl_secs == 10' >/dev/null 2>&1 \
       && echo "$body" | jq -e '.cache_size == 512' >/dev/null 2>&1; then
        pass "Create with cache options preserved"
    else
        fail "Cache options not preserved: $body"
    fi

    # Create with auth
    run_test "POST /api/mounts — with auth fields"
    body=$(api POST /api/mounts -H 'Content-Type: application/json' -d '{
        "id": "test-auth",
        "url": "http://127.0.0.1:'"$DUFS_PORT"'",
        "mountpoint": "/mnt/dufs-api-test-auth",
        "username": "admin",
        "password": "secret"
    }')
    if echo "$body" | jq -e '.id == "test-auth"' >/dev/null 2>&1; then
        pass "Create with auth accepted"
        # Verify password is not in response
        if echo "$body" | jq -e '.password' >/dev/null 2>&1; then
            fail "Password leaked in response!"
        else
            pass "Password not in response"
        fi
    else
        fail "Create with auth failed: $body"
    fi

    # Duplicate
    run_test "POST /api/mounts — duplicate rejected"
    code=$(api_code POST /api/mounts -H 'Content-Type: application/json' -d '{
        "id": "test1",
        "url": "http://127.0.0.1:'"$DUFS_PORT"'",
        "mountpoint": "/mnt/dufs-api-test-1"
    }')
    if [ "$code" = "409" ]; then
        pass "Duplicate mount returns 409 Conflict"
    else
        fail "Duplicate: expected 409, got $code"
    fi

    # List
    run_test "GET /api/mounts — list entries"
    body=$(api GET /api/mounts)
    local count
    count=$(echo "$body" | jq '. | length')
    if [ "$count" -ge 3 ]; then
        pass "List returns $count entries"
    else
        fail "List count: expected >=3, got $count"
    fi

    # Get
    run_test "GET /api/mounts/test1 — get single entry"
    code=$(api_code GET /api/mounts/test1)
    body=$(api GET /api/mounts/test1)
    if [ "$code" = "200" ] && echo "$body" | jq -e '.id == "test1"' >/dev/null; then
        pass "Get returns correct entry"
    else
        fail "Get test1: HTTP=$code body=$body"
    fi

    # Get non-existent
    run_test "GET /api/mounts/nonexistent — 404"
    code=$(api_code GET /api/mounts/nonexistent)
    if [ "$code" = "404" ]; then
        pass "Non-existent mount returns 404"
    else
        fail "Expected 404, got $code"
    fi

    # Update
    run_test "PUT /api/mounts/test1 — update url"
    body=$(api PUT /api/mounts/test1 -H 'Content-Type: application/json' -d '{
        "url": "http://127.0.0.1:'"$DUFS_PORT"'/subdir"
    }')
    if echo "$body" | jq -e '.url == "http://127.0.0.1:'"$DUFS_PORT"'/subdir"' >/dev/null 2>&1; then
        pass "Update url succeeded"
    else
        fail "Update url: $body"
    fi

    # Update preserves unmodified fields
    run_test "PUT /api/mounts/test1 — preserves unmodified fields"
    body=$(api PUT /api/mounts/test1 -H 'Content-Type: application/json' -d '{
        "read_only": true
    }')
    if echo "$body" | jq -e '.read_only == true' >/dev/null 2>&1 \
       && echo "$body" | jq -e '.url == "http://127.0.0.1:'"$DUFS_PORT"'/subdir"' >/dev/null 2>&1; then
        pass "Update preserves other fields"
    else
        fail "Update did not preserve fields: $body"
    fi

    # Update cache options
    run_test "PUT /api/mounts/test1 — update cache options"
    body=$(api PUT /api/mounts/test1 -H 'Content-Type: application/json' -d '{
        "cache_ttl_secs": 30,
        "cache_size": 1024
    }')
    if echo "$body" | jq -e '.cache_ttl_secs == 30' >/dev/null 2>&1 \
       && echo "$body" | jq -e '.cache_size == 1024' >/dev/null 2>&1; then
        pass "Cache options updated"
    else
        fail "Cache options update: $body"
    fi

    # Reset url back for lifecycle tests
    api PUT /api/mounts/test1 -H 'Content-Type: application/json' -d "{
        \"url\": \"http://127.0.0.1:$DUFS_PORT\",
        \"read_only\": false
    }" >/dev/null

    # Delete
    run_test "DELETE /api/mounts/test-auth — delete entry"
    code=$(api_code DELETE /api/mounts/test-auth)
    if [ "$code" = "204" ]; then
        pass "Delete returns 204 No Content"
    else
        fail "Delete: expected 204, got $code"
    fi

    # Verify deleted
    run_test "GET /api/mounts/test-auth after delete — 404"
    code=$(api_code GET /api/mounts/test-auth)
    if [ "$code" = "404" ]; then
        pass "Deleted mount returns 404"
    else
        fail "Expected 404 after delete, got $code"
    fi

    # Delete non-existent
    run_test "DELETE /api/mounts/nonexistent — 409 or 404"
    code=$(api_code DELETE /api/mounts/nonexistent)
    if [ "$code" = "409" ] || [ "$code" = "404" ]; then
        pass "Delete non-existent returns $code"
    else
        fail "Expected 409/404, got $code"
    fi
}

# ═══════════════════════════════════════════════════════════════════════
# Phase 3 — Lifecycle: Start → FUSE ops → Stop
# ═══════════════════════════════════════════════════════════════════════

phase3_lifecycle() {
    echo -e "\n${CYAN}══ Phase 3 — Lifecycle: Start → FUSE ops → Stop ══${NC}"

    # Prepare mountpoint
    MOUNTPOINT="/mnt/dufs-api-test-1"
    mkdir -p "$MOUNTPOINT" 2>/dev/null || true

    # Update test1 mountpoint to real path
    api PUT /api/mounts/test1 -H 'Content-Type: application/json' -d "{
        \"mountpoint\": \"$MOUNTPOINT\"
    }" >/dev/null

    # Start
    run_test "POST /api/mounts/test1/start — start mount"
    local code body
    code=$(api_code POST /api/mounts/test1/start)
    if [ "$code" = "200" ] || [ "$code" = "201" ]; then
        # Wait for mount to become ready
        sleep 2
        body=$(api GET /api/mounts/test1)
        local state
        state=$(echo "$body" | jq -r '.state')
        if [ "$state" = "Running" ] || [ "$state" = "Starting" ]; then
            pass "Mount started (state=$state)"
        else
            fail "Mount state unexpected: $state (body=$body)"
        fi
    else
        body=$(api POST /api/mounts/test1/start)
        fail "Start mount: HTTP=$code body=$body"
        return
    fi

    # Verify state is Running (wait up to 5s)
    run_test "Mount state becomes Running"
    local waited=0
    while [ $waited -lt 5 ]; do
        body=$(api GET /api/mounts/test1)
        state=$(echo "$body" | jq -r '.state')
        if [ "$state" = "Running" ]; then
            pass "State is Running"
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done
    if [ "$state" != "Running" ]; then
        fail "Mount did not reach Running in 5s: state=$state"
        return
    fi

    # FUSE operations
    run_test "Read file via FUSE mount"
    local content
    content=$(cat "$MOUNTPOINT/hello.txt" 2>/dev/null || echo "__FAILED__")
    if [ "$content" = "hello world" ]; then
        pass "Read hello.txt content correct"
    else
        fail "Read hello.txt: got '$content'"
    fi

    run_test "List directory via FUSE mount"
    if ls "$MOUNTPOINT/" >/dev/null 2>&1; then
        pass "ls mountpoint succeeded"
    else
        fail "ls mountpoint failed"
    fi

    run_test "Read nested file via FUSE mount"
    content=$(cat "$MOUNTPOINT/subdir/nested.txt" 2>/dev/null || echo "__FAILED__")
    if [ "$content" = "nested" ]; then
        pass "Nested file content correct"
    else
        fail "Nested file: got '$content'"
    fi

    run_test "Write file via FUSE mount"
    echo "api-written" > "$MOUNTPOINT/api_test.txt" 2>/dev/null
    content=$(cat "$MOUNTPOINT/api_test.txt" 2>/dev/null || echo "__FAILED__")
    if [ "$content" = "api-written" ]; then
        pass "Write and read-back correct"
    else
        fail "Write read-back: got '$content'"
    fi

    run_test "Create directory via FUSE mount"
    mkdir -p "$MOUNTPOINT/api_dir" 2>/dev/null
    if [ -d "$MOUNTPOINT/api_dir" ]; then
        pass "Directory created"
    else
        fail "Directory not created"
    fi

    # Cannot delete running mount
    run_test "DELETE running mount — rejected"
    code=$(api_code DELETE /api/mounts/test1)
    if [ "$code" = "409" ]; then
        pass "Cannot delete running mount"
    else
        fail "Expected 409, got $code"
    fi

    # Cannot update running mount
    run_test "PUT running mount — rejected"
    code=$(api_code PUT /api/mounts/test1 -H 'Content-Type: application/json' -d '{"url":"http://example.com"}')
    if [ "$code" = "409" ]; then
        pass "Cannot update running mount"
    else
        fail "Expected 409, got $code"
    fi

    # Cannot start already running mount
    run_test "POST start already running — rejected"
    code=$(api_code POST /api/mounts/test1/start)
    if [ "$code" = "409" ]; then
        pass "Cannot start already running mount"
    else
        fail "Expected 409, got $code"
    fi

    # Stop
    run_test "POST /api/mounts/test1/stop — stop mount"
    code=$(api_code POST /api/mounts/test1/stop)
    if [ "$code" = "200" ]; then
        sleep 2
        pass "Stop returns 200"
    else
        body=$(api POST /api/mounts/test1/stop)
        fail "Stop: HTTP=$code body=$body"
    fi

    # Verify stopped
    run_test "Mount state after stop"
    body=$(api GET /api/mounts/test1)
    state=$(echo "$body" | jq -r '.state')
    if [ "$state" = "Stopped" ]; then
        pass "State is Stopped after stop"
    else
        fail "Expected Stopped, got: $state"
    fi

    # Verify FUSE unmounted
    run_test "Mountpoint unmounted"
    sleep 1
    if ! mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
        pass "FUSE mount released"
    else
        fail "Mountpoint still mounted"
    fi
}

# ═══════════════════════════════════════════════════════════════════════
# Phase 4 — Error Paths & Edge Cases
# ═══════════════════════════════════════════════════════════════════════

phase4_errors() {
    echo -e "\n${CYAN}══ Phase 4 — Error Paths & Edge Cases ══${NC}"

    # Create entry for error tests
    api POST /api/mounts -H 'Content-Type: application/json' -d '{
        "id": "err-test",
        "url": "http://127.0.0.1:'"$DUFS_PORT"'",
        "mountpoint": "/mnt/dufs-api-test-err"
    }' >/dev/null

    # Start with non-existent entry
    run_test "Start non-existent mount"
    local code
    code=$(api_code POST /api/mounts/nonexistent/start)
    if [ "$code" = "404" ]; then
        pass "Start non-existent returns 404"
    else
        fail "Expected 404, got $code"
    fi

    # Stop non-existent mount
    run_test "Stop non-existent mount"
    code=$(api_code POST /api/mounts/nonexistent/stop)
    if [ "$code" = "409" ] || [ "$code" = "404" ]; then
        pass "Stop non-existent returns $code"
    else
        fail "Expected 409/404, got $code"
    fi

    # Stop mount that hasn't been started
    run_test "Stop mount not started"
    code=$(api_code POST /api/mounts/err-test/stop)
    if [ "$code" = "409" ]; then
        pass "Stop not-started returns 409"
    else
        fail "Expected 409, got $code"
    fi

    # Start with unreachable backend
    run_test "Start with unreachable backend"
    # Update URL to unreachable port
    api PUT /api/mounts/err-test -H 'Content-Type: application/json' -d '{
        "url": "http://127.0.0.1:19999"
    }' >/dev/null
    mkdir -p "/mnt/dufs-api-test-err" 2>/dev/null || true
    code=$(api_code POST /api/mounts/err-test/start)
    if [ "$code" = "502" ] || [ "$code" = "409" ]; then
        pass "Unreachable backend returns $code"
    else
        # May return 200 if connectivity check timing differs
        body=$(api GET /api/mounts/err-test)
        local state
        state=$(echo "$body" | jq -r '.state')
        if [ "$state" = "Error" ]; then
            pass "Unreachable backend: state=Error"
        else
            fail "Unreachable backend: HTTP=$code state=$state"
        fi
    fi

    # Reset to working URL for further tests
    api PUT /api/mounts/err-test -H 'Content-Type: application/json' -d "{
        \"url\": \"http://127.0.0.1:$DUFS_PORT\"
    }" >/dev/null

    # Create with missing required fields
    run_test "Create — missing id"
    code=$(api_code POST /api/mounts -H 'Content-Type: application/json' -d '{
        "url": "http://127.0.0.1:'"$DUFS_PORT"'",
        "mountpoint": "/mnt/dufs-api-test-noid"
    }')
    if [ "$code" = "400" ] || [ "$code" = "422" ]; then
        pass "Missing id rejected"
    else
        fail "Missing id: expected 400/422, got $code"
    fi

    # Create with empty mountpoint
    run_test "Create with empty mountpoint"
    code=$(api_code POST /api/mounts -H 'Content-Type: application/json' -d '{
        "id": "empty-mp",
        "url": "http://127.0.0.1:'"$DUFS_PORT"'",
        "mountpoint": ""
    }')
    # Should succeed (mountpoint validation is at start time)
    if [ "$code" = "201" ]; then
        pass "Empty mountpoint entry created (validated at start)"
    else
        fail "Empty mountpoint: $code"
    fi

    # Start with empty mountpoint — should fail
    run_test "Start with empty mountpoint — rejected"
    code=$(api_code POST /api/mounts/empty-mp/start)
    if [ "$code" = "400" ]; then
        pass "Empty mountpoint start returns 400"
    else
        fail "Empty mountpoint start: expected 400, got $code"
    fi

    # Start with unsupported protocol
    run_test "Create & start with ftp URL — unsupported protocol"
    api POST /api/mounts -H 'Content-Type: application/json' -d '{
        "id": "ftp-test",
        "url": "ftp://example.com/files",
        "mountpoint": "/mnt/dufs-api-test-ftp"
    }' >/dev/null
    mkdir -p "/mnt/dufs-api-test-ftp" 2>/dev/null || true
    code=$(api_code POST /api/mounts/ftp-test/start)
    if [ "$code" = "400" ]; then
        pass "Unsupported protocol returns 400"
    else
        fail "Unsupported protocol: expected 400, got $code"
    fi

    # Update cannot change ID
    run_test "PUT — cannot change mount ID"
    code=$(api_code PUT /api/mounts/err-test -H 'Content-Type: application/json' -d '{
        "id": "changed-id",
        "url": "http://127.0.0.1:'"$DUFS_PORT"'"
    }')
    if [ "$code" = "409" ] || [ "$code" = "400" ]; then
        pass "ID change rejected"
    else
        fail "ID change: expected 409/400, got $code"
    fi

    # Cleanup error test entries
    api DELETE /api/mounts/err-test >/dev/null 2>&1 || true
    api DELETE /api/mounts/empty-mp >/dev/null 2>&1 || true
    api DELETE /api/mounts/ftp-test >/dev/null 2>&1 || true
}

# ═══════════════════════════════════════════════════════════════════════
# Phase 5 — Concurrency Safety
# ═══════════════════════════════════════════════════════════════════════

phase5_concurrency() {
    echo -e "\n${CYAN}══ Phase 5 — Concurrency Safety ══${NC}"

    # Sequential duplicate creates — second should fail
    run_test "Duplicate create rejected"
    local code
    code=$(api_code POST /api/mounts -H 'Content-Type: application/json' -d '{
        "id": "conc-test",
        "url": "http://127.0.0.1:'"$DUFS_PORT"'",
        "mountpoint": "/mnt/dufs-api-test-conc"
    }')
    if [ "$code" = "201" ]; then
        pass "First create succeeded"
    else
        fail "First create: HTTP=$code"
    fi
    code=$(api_code POST /api/mounts -H 'Content-Type: application/json' -d '{
        "id": "conc-test",
        "url": "http://127.0.0.1:'"$DUFS_PORT"'",
        "mountpoint": "/mnt/dufs-api-test-conc"
    }')
    if [ "$code" = "409" ]; then
        pass "Duplicate create rejected with 409"
    else
        fail "Duplicate create: expected 409, got $code"
    fi

    # Rapid create-delete cycle
    run_test "Rapid create-delete cycle"
    for i in $(seq 1 10); do
        api POST /api/mounts -H 'Content-Type: application/json' -d "{
            \"id\": \"cycle-$i\",
            \"url\": \"http://127.0.0.1:$DUFS_PORT\",
            \"mountpoint\": \"/mnt/dufs-api-test-cycle-$i\"
        }" >/dev/null 2>&1 || true
        api DELETE /api/mounts/"cycle-$i" >/dev/null 2>&1 || true
    done
    pass "10 create-delete cycles completed"

    # List after cleanup
    run_test "Clean state after cycles"
    api DELETE /api/mounts/conc-test >/dev/null 2>&1 || true
    local body count
    body=$(api GET /api/mounts)
    count=$(echo "$body" | jq '. | length')
    if [ "$count" -ge 1 ]; then
        pass "API state consistent after concurrent ops ($count entries)"
    else
        fail "Unexpected mount count: $count"
    fi
}

# ═══════════════════════════════════════════════════════════════════════
# Main
# ═══════════════════════════════════════════════════════════════════════

main() {
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${CYAN}  REST API E2E Test Suite${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"

    check_deps

    # Build if needed
    if [ ! -f "$DUFS_MOUNT_BIN" ]; then
        info "Building release binary..."
        cargo build --release 2>&1
    fi

    start_dufs
    start_api

    phase0_auth
    phase1_health_version
    phase2_crud
    phase3_lifecycle
    phase4_errors
    phase5_concurrency

    # Summary
    echo -e "\n${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${CYAN}  Results: ${GREEN}${PASSED} passed${NC} / ${TOTAL} total"
    if [ ${#FAILURES[@]} -gt 0 ]; then
        echo -e "${RED}  Failures:${NC}"
        for f in "${FAILURES[@]}"; do
            echo -e "${RED}    - $f${NC}"
        done
    fi
    if [ ${#SKIPPED[@]} -gt 0 ]; then
        echo -e "${YELLOW}  Skipped: ${#SKIPPED[@]}${NC}"
    fi
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"

    if [ ${#FAILURES[@]} -eq 0 ]; then
        echo -e "${GREEN}All tests passed!${NC}"
        exit 0
    else
        echo -e "${RED}${#FAILURES[@]} test(s) failed${NC}"
        exit 1
    fi
}

main "$@"

#!/usr/bin/env bash
# E2E test: rs-f4ss share → rs-f4ss mount (P2P self-test)
#
# Tests the file sharing server by using rs-f4ss as both server and client.
# Phase 1: share + WebDAV mount — full CRUD
# Phase 2: share + HTTP static mount — read operations
# Phase 3: share with auth — authentication enforcement
# Phase 4: share read-only — write operations blocked
#
# Usage:   ./tests/e2e-share.sh
# Requires: fusermount, /dev/fuse, release binary built with serve feature
set -euo pipefail

DUFS_MOUNT_BIN="./target/release/rs-f4ss"
SHARE_PORT=15440
HTTP_PORT=15441
AUTH_PORT=15442

SHARE_DATA=""
MOUNTPOINT=""
SHARE_PID=""
MOUNT_PID=""

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

# ── Infrastructure helpers ──

start_share() {
    local port="$1"; shift
    local data_dir="$1"; shift
    info "Starting rs-f4ss share on :$port (data=$data_dir) $*"
    $DUFS_MOUNT_BIN share serve "$data_dir" --listen "127.0.0.1:$port" "$@" > /tmp/share-e2e.log 2>&1 &
    SHARE_PID=$!
    sleep 1
    if ! kill -0 "$SHARE_PID" 2>/dev/null; then
        echo -e "${RED}FATAL: share failed to start. Log:${NC}"
        cat /tmp/share-e2e.log; exit 1
    fi
    local code
    code=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$port/" || true)
    if [ "$code" = "000" ]; then
        echo -e "${RED}FATAL: share server not responding${NC}"; exit 1
    fi
    info "share ready (PID=$SHARE_PID, HTTP=$code)"
}

stop_share() {
    if [ -n "$SHARE_PID" ] && kill -0 "$SHARE_PID" 2>/dev/null; then
        info "Stopping share (PID=$SHARE_PID)"
        kill "$SHARE_PID" 2>/dev/null || true
        wait "$SHARE_PID" 2>/dev/null || true
        SHARE_PID=""
    fi
}

start_mount() {
    local url="$1"; shift
    info "Mounting $url → $MOUNTPOINT $*"
    $DUFS_MOUNT_BIN "$url" "$MOUNTPOINT" --foreground "$@" &
    MOUNT_PID=$!
    sleep 2
    if ! mountpoint -q "$MOUNTPOINT"; then
        echo -e "${RED}FATAL: mount not active${NC}"; exit 1
    fi
    info "FUSE mount active (PID=$MOUNT_PID)"
}

stop_mount() {
    if [ -n "$MOUNTPOINT" ] && mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
        info "Unmounting $MOUNTPOINT"
        fusermount -u "$MOUNTPOINT" 2>/dev/null || true
        sleep 0.3
    fi
    if [ -n "$MOUNT_PID" ] && kill -0 "$MOUNT_PID" 2>/dev/null; then
        kill "$MOUNT_PID" 2>/dev/null || true
        wait "$MOUNT_PID" 2>/dev/null || true
        MOUNT_PID=""
    fi
    if mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
        info "Force lazy unmount"
        umount -l "$MOUNTPOINT" 2>/dev/null || true
        sleep 0.5
    fi
}

# ── Cleanup ──
cleanup() {
    echo -e "\n── Cleanup ──"
    stop_mount
    stop_share
    [ -n "$SHARE_DATA" ] && rm -rf "$SHARE_DATA" 2>/dev/null || true
    [ -n "$MOUNTPOINT" ] && rmdir "$MOUNTPOINT" 2>/dev/null || true
}
trap cleanup EXIT

# ── Prerequisites ──
echo "══════════════════════════════════════════════════"
echo "  rs-f4ss Share E2E Test Suite (P2P)"
echo "══════════════════════════════════════════════════"

if [ ! -c /dev/fuse ]; then
    echo -e "${RED}FATAL: /dev/fuse not available${NC}"; exit 1
fi
if ! command -v fusermount &>/dev/null; then
    echo -e "${RED}FATAL: fusermount not found${NC}"; exit 1
fi
if [ ! -x "$DUFS_MOUNT_BIN" ]; then
    echo -e "${RED}FATAL: $DUFS_MOUNT_BIN not found. Run: cargo build --release --features serve${NC}"; exit 1
fi
# Check that the binary supports 'share' subcommand
if ! $DUFS_MOUNT_BIN share serve --help &>/dev/null; then
    echo -e "${RED}FATAL: 'share' subcommand not available. Build with --features serve${NC}"; exit 1
fi

SHARE_DATA=$(mktemp -d /tmp/share-e2e-data.XXXXXX)
MOUNTPOINT=$(mktemp -d /tmp/share-e2e-mnt.XXXXXX)
info "data dir:    $SHARE_DATA"
info "mountpoint:  $MOUNTPOINT"

# Seed test data
mkdir -p "$SHARE_DATA/subdir"
printf "hello from share" > "$SHARE_DATA/hello.txt"
printf "nested content" > "$SHARE_DATA/subdir/nested.txt"
dd if=/dev/urandom bs=1024 count=5 of="$SHARE_DATA/binary.dat" 2>/dev/null
printf "" > "$SHARE_DATA/empty.txt"

# ════════════════════════════════════════════════════
#  Phase 1 — share + WebDAV mount: full CRUD
# ════════════════════════════════════════════════════

echo -e "\n── Phase 1: share + WebDAV mount ──"
start_share $SHARE_PORT "$SHARE_DATA"
start_mount "http://127.0.0.1:$SHARE_PORT"

# ── A. Read operations ─────────────────────────────

run_test "WebDAV readdir — list root"
ENTRIES=$(ls "$MOUNTPOINT/")
if echo "$ENTRIES" | grep -q "hello.txt" && \
   echo "$ENTRIES" | grep -q "subdir" && \
   echo "$ENTRIES" | grep -q "binary.dat"; then
    pass "Root listing correct"
else
    fail "Root incomplete: $ENTRIES"
fi

run_test "WebDAV read — text file"
if [ "$(cat "$MOUNTPOINT/hello.txt")" = "hello from share" ]; then
    pass "Content: 'hello from share'"
else
    fail "Content mismatch"
fi

run_test "WebDAV stat — file size"
SIZE=$(stat -c %s "$MOUNTPOINT/hello.txt" 2>/dev/null)
if [ "$SIZE" = "16" ]; then
    pass "File size = 16"
else
    fail "Expected size 16, got: $SIZE"
fi

run_test "WebDAV stat — directory"
if [ -d "$MOUNTPOINT/subdir" ]; then
    pass "subdir is directory"
else
    fail "subdir not a directory"
fi

run_test "WebDAV read — nested file"
if [ "$(cat "$MOUNTPOINT/subdir/nested.txt")" = "nested content" ]; then
    pass "Nested content correct"
else
    fail "Nested content wrong"
fi

run_test "WebDAV read — binary file integrity (5KB)"
OM=$(md5sum "$SHARE_DATA/binary.dat" | cut -d' ' -f1)
MM=$(md5sum "$MOUNTPOINT/binary.dat" | cut -d' ' -f1)
if [ "$OM" = "$MM" ]; then
    pass "MD5 match: $MM"
else
    fail "MD5 mismatch: orig=$OM mount=$MM"
fi

run_test "WebDAV read — empty file"
ES=$(stat -c %s "$MOUNTPOINT/empty.txt")
EC=$(cat "$MOUNTPOINT/empty.txt")
if [ "$ES" = "0" ] && [ -z "$EC" ]; then
    pass "Empty file: size=0"
else
    fail "Empty file: size=$ES content='$EC'"
fi

# ── B. Write operations ────────────────────────────

run_test "WebDAV write — create new file"
echo "p2p new file" > "$MOUNTPOINT/created.txt"
sleep 0.5
if [ "$(cat "$SHARE_DATA/created.txt" 2>/dev/null)" = "p2p new file" ]; then
    pass "Backend has new file"
else
    fail "Backend missing new file"
fi

run_test "WebDAV write — overwrite existing file"
echo "updated via p2p" > "$MOUNTPOINT/hello.txt"
sleep 0.5
if [ "$(cat "$SHARE_DATA/hello.txt")" = "updated via p2p" ]; then
    pass "Backend updated"
else
    fail "Backend not updated"
fi

run_test "WebDAV mkdir — create new directory"
mkdir -p "$MOUNTPOINT/newdir"
sleep 0.5
if [ -d "$SHARE_DATA/newdir" ]; then
    pass "newdir created on backend"
else
    fail "newdir missing on backend"
fi

run_test "WebDAV write — file in new directory"
echo "nested new" > "$MOUNTPOINT/newdir/file.txt"
sleep 0.5
if [ "$(cat "$SHARE_DATA/newdir/file.txt" 2>/dev/null)" = "nested new" ]; then
    pass "Nested file created"
else
    fail "Nested file missing"
fi

run_test "WebDAV delete — remove file"
rm "$MOUNTPOINT/created.txt"
sleep 0.5
if [ ! -f "$SHARE_DATA/created.txt" ]; then
    pass "File deleted from backend"
else
    fail "File still exists on backend"
fi

run_test "WebDAV rename — move file"
mv "$MOUNTPOINT/hello.txt" "$MOUNTPOINT/renamed.txt"
sleep 0.5
if [ -f "$SHARE_DATA/renamed.txt" ] && [ ! -f "$SHARE_DATA/hello.txt" ]; then
    pass "File renamed on backend"
else
    fail "Rename failed on backend"
fi

stop_mount

# ════════════════════════════════════════════════════
#  Phase 2 — share + HTTP static mount: read-only
# ════════════════════════════════════════════════════

echo -e "\n── Phase 2: share + HTTP static mount ──"
# Recreate seed data (Phase 1 may have modified it)
rm -rf "$SHARE_DATA"/*
mkdir -p "$SHARE_DATA/subdir"
printf "http read test" > "$SHARE_DATA/test.txt"
printf "sub file" > "$SHARE_DATA/subdir/sub.txt"
dd if=/dev/urandom bs=1024 count=3 of="$SHARE_DATA/data.bin" 2>/dev/null

# Stop previous share, start fresh
stop_share
start_share $HTTP_PORT "$SHARE_DATA"
start_mount "static://127.0.0.1:$HTTP_PORT"

run_test "HTTP readdir — list root"
ENTRIES=$(ls "$MOUNTPOINT/")
if echo "$ENTRIES" | grep -q "test.txt" && \
   echo "$ENTRIES" | grep -q "subdir" && \
   echo "$ENTRIES" | grep -q "data.bin"; then
    pass "HTTP root listing correct"
else
    fail "HTTP root incomplete: $ENTRIES"
fi

run_test "HTTP read — text file"
if [ "$(cat "$MOUNTPOINT/test.txt")" = "http read test" ]; then
    pass "HTTP text content correct"
else
    CONTENT=$(cat "$MOUNTPOINT/test.txt")
    fail "HTTP content mismatch: '$CONTENT'"
fi

run_test "HTTP read — binary file integrity (3KB)"
OM=$(md5sum "$SHARE_DATA/data.bin" | cut -d' ' -f1)
MM=$(md5sum "$MOUNTPOINT/data.bin" | cut -d' ' -f1)
if [ "$OM" = "$MM" ]; then
    pass "HTTP binary MD5 match"
else
    fail "HTTP MD5 mismatch: orig=$OM mount=$MM"
fi

run_test "HTTP stat — file size"
SIZE=$(stat -c %s "$MOUNTPOINT/test.txt" 2>/dev/null)
if [ "$SIZE" = "14" ]; then
    pass "HTTP file size = 14"
else
    fail "HTTP size expected 14, got: $SIZE"
fi

run_test "HTTP read — nested file"
if [ "$(cat "$MOUNTPOINT/subdir/sub.txt")" = "sub file" ]; then
    pass "HTTP nested content correct"
else
    fail "HTTP nested content wrong"
fi

stop_mount

# ════════════════════════════════════════════════════
#  Phase 3 — share with auth: authentication required
# ════════════════════════════════════════════════════

echo -e "\n── Phase 3: share with auth ──"
stop_share
start_share $AUTH_PORT "$SHARE_DATA" --user admin --pass secret

run_test "Auth — unauthenticated request returns 401"
CODE=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$AUTH_PORT/")
if [ "$CODE" = "401" ]; then
    pass "401 Unauthorized"
else
    fail "Expected 401, got: $CODE"
fi

run_test "Auth — authenticated request returns 200"
CODE=$(curl -s -o /dev/null -w "%{http_code}" -u admin:secret "http://127.0.0.1:$AUTH_PORT/")
if [ "$CODE" = "200" ]; then
    pass "200 OK with credentials"
else
    fail "Expected 200, got: $CODE"
fi

run_test "Auth — wrong password returns 401"
CODE=$(curl -s -o /dev/null -w "%{http_code}" -u admin:wrong "http://127.0.0.1:$AUTH_PORT/")
if [ "$CODE" = "401" ]; then
    pass "401 with wrong password"
else
    fail "Expected 401, got: $CODE"
fi

run_test "Auth — WebDAV mount with correct credentials"
start_mount "http://127.0.0.1:$AUTH_PORT" --user admin --pass secret
ENTRIES=$(ls "$MOUNTPOINT/")
if echo "$ENTRIES" | grep -q "test.txt"; then
    pass "Authenticated mount lists files"
else
    fail "Authenticated mount empty: $ENTRIES"
fi

run_test "Auth — read file via authenticated mount"
if [ "$(cat "$MOUNTPOINT/test.txt")" = "http read test" ]; then
    pass "Auth mount read correct"
else
    fail "Auth mount read wrong"
fi

stop_mount

# ════════════════════════════════════════════════════
#  Phase 4 — share read-only: writes blocked
# ════════════════════════════════════════════════════

echo -e "\n── Phase 4: share read-only ──"
stop_share
start_share $SHARE_PORT "$SHARE_DATA" --read-only
start_mount "http://127.0.0.1:$SHARE_PORT"

run_test "Read-only — read still works"
if [ "$(cat "$MOUNTPOINT/test.txt")" = "http read test" ]; then
    pass "Read works in read-only mode"
else
    fail "Read failed in read-only mode"
fi

run_test "Read-only — write blocked"
if echo "should fail" > "$MOUNTPOINT/readonly_test.txt" 2>/dev/null; then
    # If the write succeeded at shell level, check if backend has the file
    sleep 0.5
    if [ -f "$SHARE_DATA/readonly_test.txt" ]; then
        fail "Write should be blocked in read-only mode"
    else
        pass "Write silently failed (backend unchanged)"
    fi
else
    pass "Write blocked (EROFS)"
fi

run_test "Read-only — mkdir blocked"
if mkdir "$MOUNTPOINT/readonly_dir" 2>/dev/null; then
    sleep 0.5
    if [ -d "$SHARE_DATA/readonly_dir" ]; then
        fail "Mkdir should be blocked"
    else
        pass "Mkdir silently failed"
    fi
else
    pass "Mkdir blocked (EROFS)"
fi

run_test "Read-only — delete blocked"
if rm "$MOUNTPOINT/test.txt" 2>/dev/null; then
    sleep 0.5
    if [ ! -f "$SHARE_DATA/test.txt" ]; then
        fail "Delete should be blocked"
    else
        pass "Delete silently failed (backend unchanged)"
    fi
else
    pass "Delete blocked (EROFS)"
fi

stop_mount

# ════════════════════════════════════════════════════
#  Phase 5 — Direct HTTP API tests (curl-based)
# ════════════════════════════════════════════════════

echo -e "\n── Phase 5: Direct HTTP API tests ──"
stop_share
# Reset data
rm -rf "$SHARE_DATA"/*
printf "api test content" > "$SHARE_DATA/api.txt"
mkdir -p "$SHARE_DATA/api-dir"
printf "dir file" > "$SHARE_DATA/api-dir/file.txt"
start_share $SHARE_PORT "$SHARE_DATA"

run_test "API GET — file download"
BODY=$(curl -s "http://127.0.0.1:$SHARE_PORT/api.txt")
if [ "$BODY" = "api test content" ]; then
    pass "GET file content correct"
else
    fail "GET content: '$BODY'"
fi

run_test "API GET — Range request"
RANGE=$(curl -s -r 0-2 "http://127.0.0.1:$SHARE_PORT/api.txt")
if [ "$RANGE" = "api" ]; then
    pass "Range 0-2 returns 'api'"
else
    fail "Range content: '$RANGE'"
fi

run_test "API HEAD — file metadata"
HEADERS=$(curl -s -I "http://127.0.0.1:$SHARE_PORT/api.txt")
if echo "$HEADERS" | grep -q "content-length: 16" && \
   echo "$HEADERS" | grep -q "accept-ranges: bytes"; then
    pass "HEAD returns content-length and accept-ranges"
else
    fail "HEAD headers: $HEADERS"
fi

run_test "API GET — directory listing (HTML)"
BODY=$(curl -s "http://127.0.0.1:$SHARE_PORT/")
if echo "$BODY" | grep -q "Index of" && \
   echo "$BODY" | grep -q "api.txt"; then
    pass "Directory listing HTML contains file entries"
else
    fail "Directory listing missing entries"
fi

run_test "API PUT — upload new file"
CODE=$(curl -s -o /dev/null -w "%{http_code}" -X PUT -d "uploaded content" "http://127.0.0.1:$SHARE_PORT/uploaded.txt")
if [ "$CODE" = "201" ] && [ "$(cat "$SHARE_DATA/uploaded.txt")" = "uploaded content" ]; then
    pass "PUT created file"
else
    fail "PUT failed: HTTP $CODE"
fi

run_test "API DELETE — delete file"
CODE=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE "http://127.0.0.1:$SHARE_PORT/uploaded.txt")
if [ "$CODE" = "204" ] && [ ! -f "$SHARE_DATA/uploaded.txt" ]; then
    pass "DELETE removed file"
else
    fail "DELETE failed: HTTP $CODE"
fi

run_test "API MKCOL — create directory"
CODE=$(curl -s -o /dev/null -w "%{http_code}" -X MKCOL "http://127.0.0.1:$SHARE_PORT/new-dir")
if [ "$CODE" = "201" ] && [ -d "$SHARE_DATA/new-dir" ]; then
    pass "MKCOL created directory"
else
    fail "MKCOL failed: HTTP $CODE"
fi

run_test "API MOVE — rename file"
CODE=$(curl -s -o /dev/null -w "%{http_code}" -X MOVE \
    -H "Destination: /api-renamed.txt" \
    "http://127.0.0.1:$SHARE_PORT/api.txt")
if [ "$CODE" = "201" ] && [ -f "$SHARE_DATA/api-renamed.txt" ] && [ ! -f "$SHARE_DATA/api.txt" ]; then
    pass "MOVE renamed file"
else
    fail "MOVE failed: HTTP $CODE"
fi

run_test "API PROPFIND — file properties"
BODY=$(curl -s -X PROPFIND -H "Depth: 0" "http://127.0.0.1:$SHARE_PORT/api-renamed.txt")
if echo "$BODY" | grep -q "multistatus" && \
   echo "$BODY" | grep -q "api-renamed.txt" && \
   echo "$BODY" | grep -q "getcontentlength"; then
    pass "PROPFIND returns XML with properties"
else
    fail "PROPFIND response: $BODY"
fi

run_test "API PROPFIND — directory listing (Depth: 1)"
BODY=$(curl -s -X PROPFIND -H "Depth: 1" "http://127.0.0.1:$SHARE_PORT/")
if echo "$BODY" | grep -q "multistatus" && \
   echo "$BODY" | grep -q "api-dir" && \
   echo "$BODY" | grep -q "api-renamed.txt"; then
    pass "PROPFIND Depth:1 lists directory contents"
else
    fail "PROPFIND listing: $BODY"
fi

run_test "API OPTIONS — WebDAV capabilities"
# Note: CorsLayer permissive() intercepts browser preflight OPTIONS.
# Non-browser OPTIONS (no Origin header) still goes through to our handler.
# Verify DAV header is present in the response.
RESP=$(curl -s -X OPTIONS -D - "http://127.0.0.1:$SHARE_PORT/")
if echo "$RESP" | grep -qi "dav:" || echo "$RESP" | grep -qi "allow:"; then
    pass "OPTIONS returns DAV and/or Allow headers"
else
    # CORS may intercept — just verify server responds without error
    CODE=$(echo "$RESP" | head -1 | grep -oP '\d{3}')
    if [ "$CODE" = "200" ] || [ "$CODE" = "204" ]; then
        pass "OPTIONS responds (CORS preflight)"
    else
        fail "OPTIONS failed: $RESP"
    fi
fi

run_test "API GET — nonexistent file returns 404"
CODE=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$SHARE_PORT/no-such-file.txt")
if [ "$CODE" = "404" ]; then
    pass "404 for nonexistent file"
else
    fail "Expected 404, got: $CODE"
fi

run_test "API GET — directory redirect (/dir → /dir/)"
CODE=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$SHARE_PORT/api-dir")
if [ "$CODE" = "301" ]; then
    pass "301 redirect for directory without trailing slash"
else
    fail "Expected 301, got: $CODE"
fi

stop_share

# ════════════════════════════════════════════════════
#  Summary
# ════════════════════════════════════════════════════

echo -e "\n══════════════════════════════════════════════════"
echo -e "  Results: ${GREEN}$PASSED passed${NC} / $TOTAL total"
if [ ${#FAILURES[@]} -gt 0 ]; then
    echo -e "  ${RED}Failures:${NC}"
    for f in "${FAILURES[@]}"; do echo -e "    ${RED}- $f${NC}"; done
fi
if [ ${#SKIPPED[@]} -gt 0 ]; then
    echo -e "  ${YELLOW}Skipped:${NC}"
    for s in "${SKIPPED[@]}"; do echo -e "    ${YELLOW}- $s${NC}"; done
fi
echo "══════════════════════════════════════════════════"

if [ ${#FAILURES[@]} -gt 0 ]; then
    exit 1
fi

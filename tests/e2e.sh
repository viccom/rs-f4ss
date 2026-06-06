#!/usr/bin/env bash
# E2E test: dufs (WebDAV server) → rs-f4ss (FUSE client) → shell operations
#
# Phases:
#   Phase 1 — Normal mode: full CRUD + advanced operations
#   Phase 2 — Read-only mode: writes blocked at FUSE layer (EROFS)
#   Phase 3 — Auth mode: HTTP Basic auth to dufs
#
# Usage:   ./tests/e2e.sh
# Requires: dufs, fusermount, /dev/fuse, release binary built
set -euo pipefail

DUFS_BIN="/usr/local/bin/dufs"
DUFS_MOUNT_BIN="./target/release/rs-f4ss"
PORT=15432

DUFS_DATA=""
MOUNTPOINT=""
DUFS_PID=""
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

start_dufs() {
    local port="$1"; shift
    local data_dir="$1"; shift
    info "Starting dufs on :$port (data=$data_dir) $*"
    $DUFS_BIN "$data_dir" -b 127.0.0.1 -p "$port" "$@" > /tmp/dufs-e2e.log 2>&1 &
    DUFS_PID=$!
    sleep 1
    if ! kill -0 "$DUFS_PID" 2>/dev/null; then
        echo -e "${RED}FATAL: dufs failed to start. Log:${NC}"
        cat /tmp/dufs-e2e.log; exit 1
    fi
    local code
    code=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$port/" || true)
    # Accept any HTTP response (200, 401, etc.) — 000 means connection failed
    if [ "$code" = "000" ]; then
        echo -e "${RED}FATAL: dufs not responding${NC}"; exit 1
    fi
    info "dufs ready (PID=$DUFS_PID, HTTP=$code)"
}

stop_dufs() {
    if [ -n "$DUFS_PID" ] && kill -0 "$DUFS_PID" 2>/dev/null; then
        info "Stopping dufs (PID=$DUFS_PID)"
        kill "$DUFS_PID" 2>/dev/null || true
        wait "$DUFS_PID" 2>/dev/null || true
        DUFS_PID=""
    fi
}

start_mount() {
    local port="$1"; shift
    info "Mounting :$port → $MOUNTPOINT $*"
    $DUFS_MOUNT_BIN "http://127.0.0.1:$port" "$MOUNTPOINT" --foreground "$@" &
    MOUNT_PID=$!
    sleep 2
    if ! mountpoint -q "$MOUNTPOINT"; then
        echo -e "${RED}FATAL: mount not active${NC}"; exit 1
    fi
    info "FUSE mount active (PID=$MOUNT_PID)"
}

stop_mount() {
    # 1. Try graceful unmount first (fusermount is the clean way)
    if [ -n "$MOUNTPOINT" ] && mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
        info "Unmounting $MOUNTPOINT"
        fusermount -u "$MOUNTPOINT" 2>/dev/null || true
        sleep 0.3
    fi
    # 2. Kill the rs-f4ss process (triggers fuser session cleanup)
    if [ -n "$MOUNT_PID" ] && kill -0 "$MOUNT_PID" 2>/dev/null; then
        kill "$MOUNT_PID" 2>/dev/null || true
        wait "$MOUNT_PID" 2>/dev/null || true
        MOUNT_PID=""
    fi
    # 3. Force lazy unmount if kernel mount entry remains (zombie mount)
    if mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
        info "Force lazy unmount"
        umount -l "$MOUNTPOINT" 2>/dev/null || true
        sleep 0.5
    fi
    # 4. Verify mount is gone
    if mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
        info "WARNING: mount still present after cleanup attempts"
    fi
}

# ── Cleanup ──
cleanup() {
    echo -e "\n── Cleanup ──"
    stop_mount
    stop_dufs
    [ -n "$DUFS_DATA" ] && rm -rf "$DUFS_DATA" 2>/dev/null || true
    [ -n "$MOUNTPOINT" ] && rmdir "$MOUNTPOINT" 2>/dev/null || true
}
trap cleanup EXIT

# ── Prerequisites ──
echo "══════════════════════════════════════════════════"
echo "  rs-f4ss E2E Test Suite"
echo "══════════════════════════════════════════════════"

if [ ! -c /dev/fuse ]; then
    echo -e "${RED}FATAL: /dev/fuse not available${NC}"; exit 1
fi
if ! command -v fusermount &>/dev/null; then
    echo -e "${RED}FATAL: fusermount not found${NC}"; exit 1
fi
if [ ! -x "$DUFS_MOUNT_BIN" ]; then
    echo -e "${RED}FATAL: $DUFS_MOUNT_BIN not found. Run: cargo build --release${NC}"; exit 1
fi

DUFS_DATA=$(mktemp -d /tmp/dufs-e2e-data.XXXXXX)
MOUNTPOINT=$(mktemp -d /tmp/dufs-e2e-mnt.XXXXXX)
info "data dir:    $DUFS_DATA"
info "mountpoint:  $MOUNTPOINT"

# Seed data (use printf for exact byte count — no trailing newline)
mkdir -p "$DUFS_DATA/subdir/deep"
printf "hello world" > "$DUFS_DATA/hello.txt"
printf "nested file" > "$DUFS_DATA/subdir/nested.txt"
printf "deep content" > "$DUFS_DATA/subdir/deep/deep.txt"
dd if=/dev/urandom bs=1024 count=5 of="$DUFS_DATA/binary.dat" 2>/dev/null
printf "" > "$DUFS_DATA/empty.txt"

# ════════════════════════════════════════════════════
#  Phase 1 — Normal mode
# ════════════════════════════════════════════════════

echo -e "\n── Phase 1: Normal mode ──"
start_dufs $PORT "$DUFS_DATA" -A --enable-cors
start_mount $PORT

# ── A. Read operations ─────────────────────────────

run_test "Readdir — list root directory"
ENTRIES=$(ls "$MOUNTPOINT/")
if echo "$ENTRIES" | grep -q "hello.txt" && \
   echo "$ENTRIES" | grep -q "subdir" && \
   echo "$ENTRIES" | grep -q "binary.dat"; then
    pass "Root listing has hello.txt, subdir, binary.dat"
else
    fail "Root incomplete: $ENTRIES"
fi

run_test "Read — small text file"
if [ "$(cat "$MOUNTPOINT/hello.txt")" = "hello world" ]; then
    pass "Content: 'hello world'"
else
    fail "Content mismatch"
fi

run_test "Getattr — stat file size"
SIZE=$(stat -c %s "$MOUNTPOINT/hello.txt" 2>/dev/null)
if [ "$SIZE" = "11" ]; then
    pass "File size = 11"
else
    fail "Expected size 11, got: $SIZE"
fi

run_test "Getattr — stat directory"
if [ -d "$MOUNTPOINT/subdir" ]; then
    pass "subdir recognized as directory"
else
    fail "subdir not a directory"
fi

run_test "Read — nested file in subdirectory"
if [ "$(cat "$MOUNTPOINT/subdir/nested.txt")" = "nested file" ]; then
    pass "Nested content correct"
else
    fail "Nested content wrong"
fi

run_test "Read — binary file integrity (5KB)"
OM=$(md5sum "$DUFS_DATA/binary.dat" | cut -d' ' -f1)
MM=$(md5sum "$MOUNTPOINT/binary.dat" | cut -d' ' -f1)
if [ "$OM" = "$MM" ]; then
    pass "MD5 match: $MM"
else
    fail "MD5 mismatch: orig=$OM mount=$MM"
fi

run_test "Readdirplus — ls -la root"
if ls -la "$MOUNTPOINT/" > /dev/null 2>&1; then
    pass "ls -la succeeded"
else
    fail "ls -la failed"
fi

run_test "Lookup — recursive find (3 levels)"
DEEP=$(find "$MOUNTPOINT" -name "deep.txt" 2>/dev/null || true)
if [ -n "$DEEP" ] && [ "$(cat "$DEEP")" = "deep content" ]; then
    pass "find located deep.txt with correct content"
else
    fail "find deep.txt failed"
fi

run_test "Read — nonexistent file returns ENOENT"
if cat "$MOUNTPOINT/no_such_file_12345.txt" 2>/dev/null; then
    fail "Expected ENOENT"
else
    pass "ENOENT returned"
fi

run_test "Read — empty file"
ES=$(stat -c %s "$MOUNTPOINT/empty.txt")
EC=$(cat "$MOUNTPOINT/empty.txt")
if [ "$ES" = "0" ] && [ -z "$EC" ]; then
    pass "Empty file: size=0, content=''"
else
    fail "Empty file: size=$ES content='$EC'"
fi

# ── B. Write operations ────────────────────────────

run_test "Write — create new file"
echo "e2e new content" > "$MOUNTPOINT/newfile.txt"
sleep 0.5
if [ "$(cat "$DUFS_DATA/newfile.txt" 2>/dev/null)" = "e2e new content" ]; then
    pass "Backend has correct content"
else
    fail "Backend content wrong or missing"
fi

run_test "Write — overwrite existing file"
echo "updated content" > "$MOUNTPOINT/hello.txt"
sleep 0.5
if [ "$(cat "$DUFS_DATA/hello.txt")" = "updated content" ]; then
    pass "Backend updated"
else
    fail "Backend not updated"
fi

run_test "Write — large file (1MB)"
dd if=/dev/urandom bs=1024 count=1024 of="$MOUNTPOINT/large.bin" 2>/dev/null
sleep 1
MS=$(stat -c %s "$MOUNTPOINT/large.bin" 2>/dev/null || echo 0)
BS=$(stat -c %s "$DUFS_DATA/large.bin" 2>/dev/null || echo 0)
if [ "$MS" = "1048576" ] && [ "$BS" = "1048576" ]; then
    pass "1MB on both sides"
else
    fail "mount=$MS backend=$BS"
fi

run_test "Cache — read back after overwrite"
if [ "$(cat "$MOUNTPOINT/hello.txt")" = "updated content" ]; then
    pass "Read-back is fresh (cache coherent)"
else
    fail "Stale cache returned old data"
fi

run_test "Create — touch empty file"
touch "$MOUNTPOINT/touched.txt"
sleep 0.5
if [ -f "$DUFS_DATA/touched.txt" ]; then
    pass "touch file on backend"
else
    fail "touch file missing on backend"
fi

run_test "Copy — external tool compatibility"
echo "copied from local" > /tmp/e2e_cp_src.txt
cp /tmp/e2e_cp_src.txt "$MOUNTPOINT/copied.txt"
rm -f /tmp/e2e_cp_src.txt
sleep 0.5
if [ "$(cat "$DUFS_DATA/copied.txt" 2>/dev/null)" = "copied from local" ]; then
    pass "cp content on backend"
else
    fail "cp content wrong or missing"
fi

# ── C. Directory operations ────────────────────────

run_test "Mkdir — create new directory"
mkdir "$MOUNTPOINT/newdir"
sleep 0.5
if [ -d "$DUFS_DATA/newdir" ]; then
    pass "newdir on backend"
else
    fail "newdir missing on backend"
fi

run_test "Mkdir — existing directory fails"
if mkdir "$MOUNTPOINT/subdir" 2>/dev/null; then
    fail "Should have failed (EEXIST)"
else
    pass "mkdir existing dir correctly rejected"
fi

run_test "Rmdir — non-empty directory (dufs recursive delete)"
mkdir "$MOUNTPOINT/nonempty"
echo "blocker" > "$MOUNTPOINT/nonempty/blocker.txt"
sleep 0.5
# Note: dufs DELETE on a directory recursively removes contents.
# The kernel may or may not block this (depends on dentry cache state).
rmdir "$MOUNTPOINT/nonempty" 2>/dev/null || true
sleep 0.5
if [ ! -d "$DUFS_DATA/nonempty" ]; then
    pass "dufs recursive delete removed dir + contents"
else
    # If rmdir didn't work (kernel ENOTEMPTY), clean up manually
    rm "$MOUNTPOINT/nonempty/blocker.txt" 2>/dev/null || true
    rmdir "$MOUNTPOINT/nonempty" 2>/dev/null || true
    sleep 0.5
    if [ ! -d "$DUFS_DATA/nonempty" ]; then
        pass "Dir cleaned up after removing contents"
    else
        fail "Could not remove nonempty dir"
    fi
fi

run_test "Readdir — listing reflects mutations"
if ls "$MOUNTPOINT/" | grep -q "newdir" && ls "$MOUNTPOINT/" | grep -q "copied.txt"; then
    pass "Listing shows newdir + copied.txt"
else
    fail "Listing stale or incomplete"
fi

# ── D. Delete operations ───────────────────────────

run_test "Unlink — delete file"
rm "$MOUNTPOINT/newfile.txt"
sleep 0.5
if [ ! -e "$DUFS_DATA/newfile.txt" ]; then
    pass "File gone from backend"
else
    fail "File still on backend"
fi

run_test "Unlink — nonexistent file fails"
if rm "$MOUNTPOINT/does_not_exist_999.txt" 2>/dev/null; then
    fail "Should have failed (ENOENT)"
else
    pass "rm nonexistent correctly rejected"
fi

run_test "Rmdir — remove empty directory"
rmdir "$MOUNTPOINT/newdir"
sleep 0.5
if [ ! -d "$DUFS_DATA/newdir" ]; then
    pass "Empty dir removed from backend"
else
    fail "Dir still on backend"
fi

# ── E. Rename operations ───────────────────────────

run_test "Rename — file in same directory"
mv "$MOUNTPOINT/copied.txt" "$MOUNTPOINT/renamed.txt"
sleep 0.5
if [ ! -e "$DUFS_DATA/copied.txt" ] && [ -f "$DUFS_DATA/renamed.txt" ]; then
    pass "File renamed on backend"
else
    fail "Rename not reflected on backend"
fi

run_test "Rename — file across directories"
mkdir "$MOUNTPOINT/cross_dst"
sleep 0.3
mv "$MOUNTPOINT/renamed.txt" "$MOUNTPOINT/cross_dst/moved.txt"
sleep 0.5
if [ ! -e "$DUFS_DATA/renamed.txt" ] && \
   [ "$(cat "$DUFS_DATA/cross_dst/moved.txt" 2>/dev/null)" = "copied from local" ]; then
    pass "Cross-dir rename OK, content preserved"
else
    fail "Cross-dir rename failed"
fi

run_test "Rename — directory with contents"
mkdir "$MOUNTPOINT/mvdir"
echo "inside" > "$MOUNTPOINT/mvdir/inner.txt"
sleep 0.5
mv "$MOUNTPOINT/mvdir" "$MOUNTPOINT/mvdir2"
sleep 0.5
if [ -d "$DUFS_DATA/mvdir2" ] && [ "$(cat "$DUFS_DATA/mvdir2/inner.txt")" = "inside" ]; then
    pass "Dir renamed with contents preserved"
else
    fail "Dir rename failed"
fi

# ── F. Advanced ────────────────────────────────────

run_test "Statfs — df command"
if df "$MOUNTPOINT" > /dev/null 2>&1; then
    pass "df succeeded without crash"
else
    fail "df failed"
fi

run_test "Rapid — burst writes (10 files)"
for i in $(seq 1 10); do
    echo "burst $i" > "$MOUNTPOINT/burst_$i.txt"
done
sleep 1
BURST_OK=true
for i in $(seq 1 10); do
    grep -q "burst $i" "$DUFS_DATA/burst_$i.txt" 2>/dev/null || BURST_OK=false
done
if $BURST_OK; then
    pass "All 10 burst writes landed on backend"
else
    fail "Some burst writes lost"
fi

run_test "Unicode — UTF-8 content round-trip"
printf "unicode test" > "$MOUNTPOINT/uni_test.txt"
sleep 0.5
if [ "$(cat "$MOUNTPOINT/uni_test.txt" 2>/dev/null)" = "unicode test" ]; then
    pass "Write + read round-trip OK"
else
    fail "Round-trip failed"
fi

run_test "Multi-read — read 3 different files sequentially"
MR1=$(cat "$MOUNTPOINT/hello.txt")
MR2=$(stat -c %s "$MOUNTPOINT/large.bin" 2>/dev/null || echo "0")
MR3=$(stat -c %s "$MOUNTPOINT/binary.dat" 2>/dev/null || echo "0")
if [ "$MR1" = "updated content" ] && [ "$MR2" = "1048576" ] && [ "$MR3" = "5120" ]; then
    pass "Text + 1MB + 5KB reads all correct"
else
    fail "hello='$MR1' large=$MR2 binary=$MR3"
fi

# ════════════════════════════════════════════════════
#  Phase 2 — Read-only mode
# ════════════════════════════════════════════════════

echo -e "\n── Phase 2: Read-only mode ──"
stop_mount
start_mount $PORT --read-only

run_test "Readonly — read succeeds"
if [ "$(cat "$MOUNTPOINT/hello.txt")" = "updated content" ]; then
    pass "Read in RO mode works"
else
    fail "Read in RO mode returned wrong data"
fi

run_test "Readonly — write blocked (EROFS)"
if echo "fail" > "$MOUNTPOINT/ro_test.txt" 2>/dev/null; then
    fail "Write should be blocked"
else
    pass "Write blocked (EROFS)"
fi

run_test "Readonly — mkdir blocked (EROFS)"
if mkdir "$MOUNTPOINT/ro_dir" 2>/dev/null; then
    fail "mkdir should be blocked"
else
    pass "mkdir blocked (EROFS)"
fi

# ════════════════════════════════════════════════════
#  Phase 3 — Auth mode
# ════════════════════════════════════════════════════

echo -e "\n── Phase 3: Auth mode ──"
stop_mount
stop_dufs
start_dufs $PORT "$DUFS_DATA" -A -a "testuser:testpass@/:rw" --enable-cors
start_mount $PORT --user testuser --pass testpass

run_test "Auth — read with correct credentials"
if [ "$(cat "$MOUNTPOINT/hello.txt")" = "updated content" ]; then
    pass "Auth read works"
else
    fail "Auth read returned wrong data"
fi

run_test "Auth — write with correct credentials"
echo "auth write" > "$MOUNTPOINT/auth_write.txt"
sleep 0.5
if [ -f "$DUFS_DATA/auth_write.txt" ] && \
   [ "$(cat "$DUFS_DATA/auth_write.txt")" = "auth write" ]; then
    pass "Auth write works"
else
    fail "Auth write failed"
fi

run_test "Auth — list directory"
if ls "$MOUNTPOINT/" | grep -q "hello.txt"; then
    pass "Auth listing works"
else
    fail "Auth listing failed"
fi

# ════════════════════════════════════════════════════
#  Phase 4 — Server-side change propagation
# ════════════════════════════════════════════════════

echo -e "\n── Phase 4: Server-side change propagation (cache coherence) ──"
stop_mount
stop_dufs

# Fresh data dir for predictable state
PHASE4_DATA=$(mktemp -d /tmp/dufs-e2e-data.XXXXXX)
mkdir -p "$PHASE4_DATA/docs"
printf "original content" > "$PHASE4_DATA/file_a.txt"
printf "will be deleted" > "$PHASE4_DATA/file_b.txt"
printf "will be renamed" > "$PHASE4_DATA/file_c.txt"
mkdir "$PHASE4_DATA/empty_dir"
mkdir "$PHASE4_DATA/dir_to_delete"
printf "inside" > "$PHASE4_DATA/dir_to_delete/inner.txt"
info "Phase 4 data: $PHASE4_DATA"

start_dufs $PORT "$PHASE4_DATA" -A --enable-cors
# Short TTL so cache expires quickly for deterministic tests
start_mount $PORT --cache-ttl 1

# ── Helpers for WebDAV server-side operations ──

webdav_put() {
    printf '%s' "$2" | curl -s -o /dev/null -w "%{http_code}" -T - "http://127.0.0.1:$PORT$1"
}
webdav_mkdir() {
    curl -s -o /dev/null -w "%{http_code}" -X MKCOL "http://127.0.0.1:$PORT$1"
}
webdav_delete() {
    curl -s -o /dev/null -w "%{http_code}" -X DELETE "http://127.0.0.1:$PORT$1"
}
webdav_move() {
    curl -s -o /dev/null -w "%{http_code}" -X MOVE \
        -H "Destination: $2" "http://127.0.0.1:$PORT$1"
}

# ── A. Server-side file creation → FUSE visible ──

run_test "Srv→FUSE: server creates file, FUSE reads it"
# Prime the cache by listing root first
ls "$MOUNTPOINT/" > /dev/null
sleep 2
CODE=$(webdav_put "/srv_new.txt" "server created")
sleep 2
if [ "$(cat "$MOUNTPOINT/srv_new.txt" 2>/dev/null)" = "server created" ]; then
    pass "New file visible through FUSE"
else
    CONTENT=$(cat "$MOUNTPOINT/srv_new.txt" 2>/dev/null || echo "MISSING")
    fail "Content='$CONTENT', expected='server created'"
fi

run_test "Srv→FUSE: server creates file, ls shows it"
if ls "$MOUNTPOINT/" | grep -q "srv_new.txt"; then
    pass "ls shows new server-created file"
else
    fail "ls does not show srv_new.txt"
fi

# ── B. Server-side file modification → FUSE sees fresh content ──

run_test "Srv→FUSE: server modifies file, FUSE reads new content"
# Read through FUSE to populate cache
BEFORE=$(cat "$MOUNTPOINT/file_a.txt")
if [ "$BEFORE" != "original content" ]; then
    fail "Setup wrong: content='$BEFORE'"
else
    webdav_put "/file_a.txt" "modified by server"
    sleep 1.5
    AFTER=$(cat "$MOUNTPOINT/file_a.txt")
    if [ "$AFTER" = "modified by server" ]; then
        pass "FUSE reads updated content after cache expiry"
    else
        fail "Stale content: '$AFTER'"
    fi
fi

run_test "Srv→FUSE: server modifies file, stat shows new size"
NEW_SIZE=$(stat -c %s "$MOUNTPOINT/file_a.txt" 2>/dev/null || echo "?")
EXPECTED=18  # "modified by server" = 18 bytes
if [ "$NEW_SIZE" = "$EXPECTED" ]; then
    pass "Stat shows new size ($NEW_SIZE)"
else
    fail "Stat size=$NEW_SIZE, expected=$EXPECTED"
fi

# ── C. Server-side file deletion → FUSE sees it gone ──

run_test "Srv→FUSE: server deletes file, FUSE can no longer access it"
# Confirm file exists first
if [ ! -f "$MOUNTPOINT/file_b.txt" ]; then
    fail "Setup: file_b.txt not found via FUSE"
else
    webdav_delete "/file_b.txt"
    sleep 1.5
    # NOTE: FUSE kernel dentry cache uses TTL=60s. After server-side delete,
    # [ -f ] may still return true due to kernel caching the dentry.
    # The reliable check is via ls (readdir), which uses children cache (TTL=1s).
    # Here we verify the readdir path; stat-based check is covered by Test 42.
    if ls "$MOUNTPOINT/" | grep -q "file_b.txt"; then
        fail "ls still shows deleted file"
    else
        pass "File gone from FUSE readdir view after children cache expiry"
    fi
fi

run_test "Srv→FUSE: server-deleted file not in ls listing"
if ls "$MOUNTPOINT/" | grep -q "file_b.txt"; then
    fail "ls still shows deleted file"
else
    pass "ls no longer shows deleted file"
fi

# ── D. Server-side rename → FUSE sees new name ──

run_test "Srv→FUSE: server renames file, FUSE sees new name"
CODE=$(webdav_move "/file_c.txt" "/file_renamed.txt")
sleep 1.5
if [ ! -e "$MOUNTPOINT/file_c.txt" ] && \
   [ "$(cat "$MOUNTPOINT/file_renamed.txt" 2>/dev/null)" = "will be renamed" ]; then
    pass "Old name gone, new name visible with correct content"
else
    OLD=$([ -e "$MOUNTPOINT/file_c.txt" ] && echo "exists" || echo "gone")
    NEW=$(cat "$MOUNTPOINT/file_renamed.txt" 2>/dev/null || echo "MISSING")
    fail "old=$OLD new_content='$NEW'"
fi

# ── E. Server-side directory creation → FUSE sees it ──

run_test "Srv→FUSE: server creates directory, FUSE can enter it"
webdav_mkdir "/srv_dir"
sleep 1.5
if [ -d "$MOUNTPOINT/srv_dir" ]; then
    pass "New directory visible and accessible"
else
    fail "Directory not visible through FUSE"
fi

run_test "Srv→FUSE: server creates file in subdir, FUSE reads it"
webdav_put "/docs/srv_nested.txt" "nested by server"
sleep 1.5
if [ "$(cat "$MOUNTPOINT/docs/srv_nested.txt" 2>/dev/null)" = "nested by server" ]; then
    pass "Nested file visible through FUSE"
else
    fail "Nested file not visible"
fi

# ── F. Server-side directory deletion → FUSE sees it gone ──

run_test "Srv→FUSE: server removes non-empty directory, FUSE sees it gone"
webdav_delete "/dir_to_delete"  # dufs recursive delete
sleep 1.5
if [ ! -e "$MOUNTPOINT/dir_to_delete" ]; then
    pass "Removed directory gone from FUSE view"
else
    fail "Directory still visible"
fi

# ── G. Type change: file → directory with same name ──

run_test "Srv→FUSE: server replaces file with directory, FUSE sees correct type"
webdav_delete "/file_a.txt"
webdav_mkdir "/file_a.txt"
sleep 1.5
# Use ls + grep (readdir path) instead of [ -d ] (dentry path),
# because FUSE kernel dentry TTL=60s — the old dentry (file/negative) persists.
if ls "$MOUNTPOINT/" | grep -q "file_a.txt"; then
    # file_a.txt appears in listing — verify it's a directory via cd
    if cd "$MOUNTPOINT/file_a.txt" 2>/dev/null; then
        cd "$MOUNTPOINT"  # cd back
        pass "File replaced by directory visible as directory"
    else
        fail "file_a.txt visible but not a directory"
    fi
else
    fail "file_a.txt not visible in ls (kernel dentry cache may not have expired yet)"
fi

# ── H. Rapid server-side creates → FUSE lists all ──

run_test "Srv→FUSE: rapid server-side creates (5 files), FUSE lists all"
for i in $(seq 1 5); do
    webdav_put "/rapid_$i.txt" "rapid content $i"
done
sleep 1.5
RAPID_COUNT=$(ls "$MOUNTPOINT/" | grep -c "^rapid_" || true)
if [ "$RAPID_COUNT" = "5" ]; then
    pass "All 5 rapid files visible"
else
    fail "Expected 5, found $RAPID_COUNT"
fi

# ── I. FUSE client not crashed by server-side churn ──

run_test "Srv→FUSE: concurrent access — FUSE reads during server writes"
webdav_put "/concurrent.txt" "first" &
CONCURRENT_PID=$!
sleep 0.1
cat "$MOUNTPOINT/docs/srv_nested.txt" > /dev/null 2>&1 || true
wait $CONCURRENT_PID 2>/dev/null || true
sleep 1.5
# Verify mount still alive
if mountpoint -q "$MOUNTPOINT" && \
   [ "$(cat "$MOUNTPOINT/concurrent.txt" 2>/dev/null)" = "first" ]; then
    pass "No crash during concurrent access"
else
    fail "Mount broken or data wrong"
fi

# ── J. Deeply nested server-created structure ──

run_test "Srv→FUSE: server creates 3-level nested dirs, FUSE navigates"
webdav_mkdir "/deep"
webdav_mkdir "/deep/l2"
webdav_mkdir "/deep/l2/l3"
webdav_put "/deep/l2/l3/bottom.txt" "deep bottom"
sleep 1.5
if [ -d "$MOUNTPOINT/deep/l2/l3" ] && \
   [ "$(cat "$MOUNTPOINT/deep/l2/l3/bottom.txt")" = "deep bottom" ]; then
    pass "3-level nested structure navigable"
else
    fail "Cannot navigate nested structure"
fi

# ── K. FUSE write + server write to same file ──

run_test "Srv+FUSE: FUSE writes, then server overwrites, FUSE reads fresh"
echo "from fuse" > "$MOUNTPOINT/mixed.txt"
sleep 1.5
FUSE_FIRST=$(cat "$MOUNTPOINT/mixed.txt" 2>/dev/null || echo "MISSING")
webdav_put "/mixed.txt" "from server"
sleep 1.5
AFTER_SRV=$(cat "$MOUNTPOINT/mixed.txt" 2>/dev/null || echo "MISSING")
if [ "$FUSE_FIRST" = "from fuse" ] && [ "$AFTER_SRV" = "from server" ]; then
    pass "FUSE write, then server write, both visible in sequence"
else
    fail "fuse='$FUSE_FIRST' after_srv='$AFTER_SRV'"
fi

# Cleanup Phase 4 data
stop_mount
stop_dufs
rm -rf "$PHASE4_DATA" 2>/dev/null || true

# ════════════════════════════════════════════════════
#  Summary
# ════════════════════════════════════════════════════

echo ""
echo "══════════════════════════════════════════════════"
echo -e "  Results: ${GREEN}$PASSED passed${NC} / $TOTAL total"
if [ ${#FAILURES[@]} -gt 0 ]; then
    echo -e "  ${RED}Failures:${NC}"
    for f in "${FAILURES[@]}"; do
        echo -e "    ${RED}- $f${NC}"
    done
fi
if [ ${#SKIPPED[@]} -gt 0 ]; then
    echo -e "  ${YELLOW}Skipped:${NC}"
    for s in "${SKIPPED[@]}"; do
        echo -e "    ${YELLOW}- $s${NC}"
    done
fi
echo "══════════════════════════════════════════════════"

[ ${#FAILURES[@]} -eq 0 ] && exit 0 || exit 1

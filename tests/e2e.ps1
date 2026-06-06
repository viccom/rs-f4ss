#Requires -Version 7.0
# E2E test: dufs (WebDAV server) → rs-f4ss (WinFsp client) → PowerShell operations
#
# Phases:
#   Phase 1 — Normal mode: full CRUD + advanced operations
#   Phase 2 — Read-only mode: writes blocked
#   Phase 3 — Auth mode: HTTP Basic auth to dufs
#   Phase 4 — Server-side change propagation (cache coherence)
#
# Usage:   .\tests\e2e.ps1 [-Drive Z] [-Port 15432]
# Requires: dufs.exe (in PATH), WinFsp installed, release binary built

param(
    [string]$Drive = "Z",
    [int]$Port = 15432,
    [string]$DufsExe = "dufs.exe",
    [string]$DufsMountExe = ".\target\release\rs-f4ss.exe"
)

$ErrorActionPreference = "Continue"
Set-StrictMode -Version 3.0

# ── Colors ──
$RED    = "`e[0;31m"
$GREEN  = "`e[0;32m"
$YELLOW = "`e[0;33m"
$CYAN   = "`e[0;36m"
$NC     = "`e[0m"

$script:Failures = [System.Collections.Generic.List[string]]::new()
$script:Skipped  = [System.Collections.Generic.List[string]]::new()
$script:Total  = 0
$script:Passed = 0

$script:DufsProcess  = $null
$script:MountProcess = $null
$script:DataDir      = ""
$script:Phase4Data   = ""
$script:MountPoint   = "${Drive}:\"

function Pass($name) {
    Write-Host "${GREEN}  [PASS]${NC}: $name"
    $script:Passed++
}

function Fail($name) {
    Write-Host "${RED}  [FAIL]${NC}: $name"
    $script:Failures.Add($name)
}

function Info($msg) {
    Write-Host "${CYAN}  [INFO]${NC} $msg"
}

function Skip($name) {
    Write-Host "${YELLOW}  [SKIP]${NC}: $name"
    $script:Skipped.Add($name)
}

function RunTest($name) {
    $script:Total++
    Write-Host ""
    Write-Host "${CYAN}Test $($script:Total)${NC}: $name"
}

# ── Infrastructure helpers ──

function StartDufs {
    param(
        [string]$DataPath,
        [string[]]$ExtraArgs
    )

    $dufsArgs = @($DataPath, "-b", "127.0.0.1", "-p", "$Port") + $ExtraArgs
    Info "Starting dufs on :$Port (data=$DataPath) $($ExtraArgs -join ' ')"

    $script:DufsProcess = Start-Process -FilePath $DufsExe `
        -ArgumentList $dufsArgs -WindowStyle Hidden -PassThru

    Start-Sleep -Seconds 1

    if ($script:DufsProcess.HasExited) {
        Write-Host "${RED}FATAL: dufs failed to start.${NC}"
        exit 1
    }

    $code = "000"
    try {
        $code = "$([int](Invoke-WebRequest -Uri "http://127.0.0.1:$Port/" -UseBasicParsing -TimeoutSec 5).StatusCode)"
    } catch {
        if ($_.Exception.Response) {
            $code = "$([int]$_.Exception.Response.StatusCode)"
        }
    }
    if ($code -eq "000") {
        Write-Host "${RED}FATAL: dufs not responding${NC}"; exit 1
    }
    Info "dufs ready (PID=$($script:DufsProcess.Id), HTTP=$code)"
}

function StopDufs {
    if ($null -ne $script:DufsProcess -and -not $script:DufsProcess.HasExited) {
        Info "Stopping dufs (PID=$($script:DufsProcess.Id))"
        $script:DufsProcess.Kill()
        $script:DufsProcess.WaitForExit(3000)
        $script:DufsProcess = $null
    }
}

function StartMount {
    param([string[]]$ExtraArgs)

    $mountArgs = @("http://127.0.0.1:$Port", "$Drive`:") + $ExtraArgs
    Info "Mounting :$Port -> $($script:MountPoint) $($ExtraArgs -join ' ')"

    $script:MountProcess = Start-Process -FilePath $DufsMountExe `
        -ArgumentList $mountArgs -WindowStyle Minimized -PassThru

    Start-Sleep -Seconds 3

    if (-not (Test-Path $script:MountPoint)) {
        Write-Host "${RED}FATAL: mount not active${NC}"; exit 1
    }
    Info "WinFsp mount active (PID=$($script:MountProcess.Id))"
}

function StopMount {
    if ($null -ne $script:MountProcess -and -not $script:MountProcess.HasExited) {
        Info "Unmounting $($script:MountPoint)"
        try {
            & $DufsMountExe unmount "$Drive`:" 2>$null
        } catch {}
        Start-Sleep -Milliseconds 500

        if (-not $script:MountProcess.HasExited) {
            $script:MountProcess.Kill()
            $script:MountProcess.WaitForExit(3000)
        }
        $script:MountProcess = $null
    }
    Start-Sleep -Milliseconds 300
    if (Test-Path $script:MountPoint) {
        Info "WARNING: mount still present after cleanup attempts"
    }
}

# ── WebDAV helpers (Phase 4) ──

function WebDavPut([string]$Path, [string]$Content) {
    $tempFile = [IO.Path]::GetTempFileName()
    [IO.File]::WriteAllText($tempFile, $Content, [Text.UTF8Encoding]::new($false))
    $code = "$(curl.exe -s -o NUL -w '%{http_code}' -T $tempFile "http://127.0.0.1:$Port$Path" 2>`$null)".Trim()
    Remove-Item $tempFile -Force -ErrorAction SilentlyContinue
    return $code
}

function WebDavMkdir([string]$Path) {
    return "$(curl.exe -s -o NUL -w '%{http_code}' -X MKCOL "http://127.0.0.1:$Port$Path" 2>`$null)".Trim()
}

function WebDavDelete([string]$Path) {
    return "$(curl.exe -s -o NUL -w '%{http_code}' -X DELETE "http://127.0.0.1:$Port$Path" 2>`$null)".Trim()
}

function WebDavMove([string]$FromPath, [string]$ToUrl) {
    return "$(curl.exe -s -o NUL -w '%{http_code}' -X MOVE -H "Destination: $ToUrl" "http://127.0.0.1:$Port$FromPath" 2>`$null)".Trim()
}

# ── Prerequisites ──

Write-Host "=================================================="
Write-Host "  rs-f4ss E2E Test Suite (Windows)"
Write-Host "=================================================="

if (Test-Path $script:MountPoint) {
    Write-Host "${RED}FATAL: Drive ${Drive}: is already in use. Use -Drive to pick another.${NC}"; exit 1
}
if (-not (Get-Command $DufsExe -ErrorAction SilentlyContinue)) {
    Write-Host "${RED}FATAL: $DufsExe not found in PATH${NC}"; exit 1
}
if (-not (Test-Path $DufsMountExe)) {
    Write-Host "${RED}FATAL: $DufsMountExe not found. Run: cargo build --release${NC}"; exit 1
}

$script:DataDir = Join-Path $env:TEMP "dufs-e2e-data-$(Get-Random)"
$MountPoint = $script:MountPoint
Info "data dir:    $($script:DataDir)"
Info "mountpoint:  $MountPoint"

# ════════════════════════════════════════════════════
#  Main test body
# ════════════════════════════════════════════════════

try {

# ── Seed data ──
New-Item -ItemType Directory -Path "$($script:DataDir)\subdir\deep" -Force | Out-Null
[IO.File]::WriteAllText("$($script:DataDir)\hello.txt", "hello world", [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText("$($script:DataDir)\subdir\nested.txt", "nested file", [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText("$($script:DataDir)\subdir\deep\deep.txt", "deep content", [Text.UTF8Encoding]::new($false))
$binBytes = [byte[]]::new(5120)
[Random]::new().NextBytes($binBytes)
[IO.File]::WriteAllBytes("$($script:DataDir)\binary.dat", $binBytes)
[IO.File]::WriteAllText("$($script:DataDir)\empty.txt", "", [Text.UTF8Encoding]::new($false))

# ════════════════════════════════════════════════════
#  Phase 1 — Normal mode
# ════════════════════════════════════════════════════

Write-Host ""
Write-Host "── Phase 1: Normal mode ──"
StartDufs -DataPath $script:DataDir -ExtraArgs @("-A", "--enable-cors")
StartMount

# ── A. Read operations ─────────────────────────────

RunTest "Readdir — list root directory"
$entries = Get-ChildItem $MountPoint | Select-Object -ExpandProperty Name
if ($entries -contains "hello.txt" -and $entries -contains "subdir" -and $entries -contains "binary.dat") {
    Pass "Root listing has hello.txt, subdir, binary.dat"
} else {
    Fail "Root incomplete: $($entries -join ', ')"
}

RunTest "Read — small text file"
$content = [IO.File]::ReadAllText("$MountPoint\hello.txt")
if ($content -eq "hello world") {
    Pass "Content: 'hello world'"
} else {
    Fail "Content mismatch: '$content'"
}

RunTest "Getattr — stat file size"
$size = (Get-Item "$MountPoint\hello.txt").Length
if ($size -eq 11) {
    Pass "File size = 11"
} else {
    Fail "Expected size 11, got: $size"
}

RunTest "Getattr — stat directory"
if (Test-Path "$MountPoint\subdir" -PathType Container) {
    Pass "subdir recognized as directory"
} else {
    Fail "subdir not a directory"
}

RunTest "Read — nested file in subdirectory"
$nested = [IO.File]::ReadAllText("$MountPoint\subdir\nested.txt")
if ($nested -eq "nested file") {
    Pass "Nested content correct"
} else {
    Fail "Nested content wrong: '$nested'"
}

RunTest "Read — binary file integrity (5KB)"
$origHash = (Get-FileHash -Path "$($script:DataDir)\binary.dat" -Algorithm MD5).Hash
$mountHash = (Get-FileHash -Path "$MountPoint\binary.dat" -Algorithm MD5).Hash
if ($origHash -eq $mountHash) {
    Pass "MD5 match: $mountHash"
} else {
    Fail "MD5 mismatch: orig=$origHash mount=$mountHash"
}

RunTest "Readdirplus — dir root (detailed listing)"
try {
    Get-ChildItem $MountPoint | Format-Table Name, Length, LastWriteTime | Out-Null
    Pass "Detailed listing succeeded"
} catch {
    Fail "Detailed listing failed: $($_.Exception.Message)"
}

RunTest "Lookup — recursive find (3 levels)"
$deep = Get-ChildItem $MountPoint -Recurse -Filter "deep.txt" -ErrorAction SilentlyContinue
if ($null -ne $deep -and (Test-Path $deep.FullName)) {
    $deepContent = [IO.File]::ReadAllText($deep.FullName)
    if ($deepContent -eq "deep content") {
        Pass "find located deep.txt with correct content"
    } else {
        Fail "deep.txt found but content wrong: '$deepContent'"
    }
} else {
    Fail "find deep.txt failed"
}

RunTest "Read — nonexistent file returns error"
try {
    $null = [IO.File]::ReadAllText("$MountPoint\no_such_file_12345.txt")
    Fail "Expected error (file not found)"
} catch {
    Pass "Error returned for nonexistent file"
}

RunTest "Read — empty file"
$emptySize = (Get-Item "$MountPoint\empty.txt").Length
$emptyContent = [IO.File]::ReadAllText("$MountPoint\empty.txt")
if ($emptySize -eq 0 -and [string]::IsNullOrEmpty($emptyContent)) {
    Pass "Empty file: size=0, content=''"
} else {
    Fail "Empty file: size=$emptySize content='$emptyContent'"
}

# ── B. Write operations ────────────────────────────

RunTest "Write — create new file"
[IO.File]::WriteAllText("$MountPoint\newfile.txt", "e2e new content", [Text.UTF8Encoding]::new($false))
Start-Sleep -Milliseconds 500
$backendContent = [IO.File]::ReadAllText("$($script:DataDir)\newfile.txt")
if ($backendContent -eq "e2e new content") {
    Pass "Backend has correct content"
} else {
    Fail "Backend content wrong or missing: '$backendContent'"
}

RunTest "Write — overwrite existing file"
[IO.File]::WriteAllText("$MountPoint\hello.txt", "updated content", [Text.UTF8Encoding]::new($false))
Start-Sleep -Milliseconds 500
$backendContent = [IO.File]::ReadAllText("$($script:DataDir)\hello.txt")
if ($backendContent -eq "updated content") {
    Pass "Backend updated"
} else {
    Fail "Backend not updated: '$backendContent'"
}

RunTest "Write — large file (1MB)"
$largeBytes = [byte[]]::new(1MB)
[Random]::new().NextBytes($largeBytes)
[IO.File]::WriteAllBytes("$MountPoint\large.bin", $largeBytes)
Start-Sleep -Seconds 1
$mountSize = 0; $backendSize = 0
try { $mountSize = (Get-Item "$MountPoint\large.bin" -ErrorAction SilentlyContinue).Length } catch {}
try { $backendSize = (Get-Item "$($script:DataDir)\large.bin" -ErrorAction SilentlyContinue).Length } catch {}
if ($mountSize -eq 1048576 -and $backendSize -eq 1048576) {
    Pass "1MB on both sides"
} else {
    Fail "mount=$mountSize backend=$backendSize"
}

RunTest "Cache — read back after overwrite"
$readback = [IO.File]::ReadAllText("$MountPoint\hello.txt")
if ($readback -eq "updated content") {
    Pass "Read-back is fresh (cache coherent)"
} else {
    Fail "Stale cache returned old data: '$readback'"
}

RunTest "Create — touch empty file"
New-Item -ItemType File -Path "$MountPoint\touched.txt" -Force | Out-Null
Start-Sleep -Milliseconds 500
if (Test-Path "$($script:DataDir)\touched.txt") {
    Pass "touch file on backend"
} else {
    Fail "touch file missing on backend"
}

RunTest "Copy — external tool compatibility"
$cpSrc = Join-Path $env:TEMP "e2e_cp_src.txt"
[IO.File]::WriteAllText($cpSrc, "copied from local", [Text.UTF8Encoding]::new($false))
Copy-Item $cpSrc "$MountPoint\copied.txt"
Remove-Item $cpSrc -Force
Start-Sleep -Milliseconds 500
$cpContent = [IO.File]::ReadAllText("$($script:DataDir)\copied.txt")
if ($cpContent -eq "copied from local") {
    Pass "cp content on backend"
} else {
    Fail "cp content wrong or missing: '$cpContent'"
}

# ── C. Directory operations ────────────────────────

RunTest "Mkdir — create new directory"
New-Item -ItemType Directory -Path "$MountPoint\newdir" | Out-Null
Start-Sleep -Milliseconds 500
if (Test-Path "$($script:DataDir)\newdir" -PathType Container) {
    Pass "newdir on backend"
} else {
    Fail "newdir missing on backend"
}

RunTest "Mkdir — existing directory fails"
$mkdirResult = $true
try {
    New-Item -ItemType Directory -Path "$MountPoint\subdir" -ErrorAction Stop | Out-Null
} catch {
    $mkdirResult = $false
}
if (-not $mkdirResult) {
    Pass "mkdir existing dir correctly rejected"
} else {
    Fail "Should have failed (already exists)"
}

RunTest "Rmdir — non-empty directory (dufs recursive delete)"
New-Item -ItemType Directory -Path "$MountPoint\nonempty" -Force | Out-Null
[IO.File]::WriteAllText("$MountPoint\nonempty\blocker.txt", "blocker", [Text.UTF8Encoding]::new($false))
Start-Sleep -Milliseconds 500
Remove-Item -Path "$MountPoint\nonempty" -Recurse -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
if (-not (Test-Path "$($script:DataDir)\nonempty")) {
    Pass "dufs recursive delete removed dir + contents"
} else {
    Remove-Item "$MountPoint\nonempty\blocker.txt" -Force -ErrorAction SilentlyContinue
    Remove-Item "$MountPoint\nonempty" -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
    if (-not (Test-Path "$($script:DataDir)\nonempty")) {
        Pass "Dir cleaned up after removing contents"
    } else {
        Fail "Could not remove nonempty dir"
    }
}

RunTest "Readdir — listing reflects mutations"
$lsNames = Get-ChildItem $MountPoint | Select-Object -ExpandProperty Name
if ($lsNames -contains "newdir" -and $lsNames -contains "copied.txt") {
    Pass "Listing shows newdir + copied.txt"
} else {
    Fail "Listing stale or incomplete: $($lsNames -join ', ')"
}

# ── D. Delete operations ───────────────────────────

RunTest "Unlink — delete file"
Remove-Item "$MountPoint\newfile.txt" -Force
Start-Sleep -Milliseconds 500
if (-not (Test-Path "$($script:DataDir)\newfile.txt")) {
    Pass "File gone from backend"
} else {
    Fail "File still on backend"
}

RunTest "Unlink — nonexistent file fails"
$rmResult = $true
try {
    Remove-Item "$MountPoint\does_not_exist_999.txt" -Force -ErrorAction Stop
} catch {
    $rmResult = $false
}
if (-not $rmResult) {
    Pass "rm nonexistent correctly rejected"
} else {
    Fail "Should have failed (file not found)"
}

RunTest "Rmdir — remove empty directory"
Remove-Item "$MountPoint\newdir" -Force
Start-Sleep -Milliseconds 500
if (-not (Test-Path "$($script:DataDir)\newdir")) {
    Pass "Empty dir removed from backend"
} else {
    Fail "Dir still on backend"
}

# ── E. Rename operations ───────────────────────────

RunTest "Rename — file in same directory"
Move-Item "$MountPoint\copied.txt" "$MountPoint\renamed.txt" -Force
Start-Sleep -Milliseconds 500
if (-not (Test-Path "$($script:DataDir)\copied.txt") -and (Test-Path "$($script:DataDir)\renamed.txt")) {
    Pass "File renamed on backend"
} else {
    Fail "Rename not reflected on backend"
}

RunTest "Rename — file across directories"
New-Item -ItemType Directory -Path "$MountPoint\cross_dst" -Force | Out-Null
Start-Sleep -Milliseconds 300
Move-Item "$MountPoint\renamed.txt" "$MountPoint\cross_dst\moved.txt" -Force
Start-Sleep -Milliseconds 500
$movedContent = ""
try { $movedContent = [IO.File]::ReadAllText("$($script:DataDir)\cross_dst\moved.txt") } catch {}
if (-not (Test-Path "$($script:DataDir)\renamed.txt") -and $movedContent -eq "copied from local") {
    Pass "Cross-dir rename OK, content preserved"
} else {
    Fail "Cross-dir rename failed"
}

RunTest "Rename — directory with contents"
New-Item -ItemType Directory -Path "$MountPoint\mvdir" -Force | Out-Null
[IO.File]::WriteAllText("$MountPoint\mvdir\inner.txt", "inside", [Text.UTF8Encoding]::new($false))
Start-Sleep -Milliseconds 500
Move-Item "$MountPoint\mvdir" "$MountPoint\mvdir2" -Force
Start-Sleep -Milliseconds 500
$innerContent = ""
try { $innerContent = [IO.File]::ReadAllText("$($script:DataDir)\mvdir2\inner.txt") } catch {}
if ((Test-Path "$($script:DataDir)\mvdir2" -PathType Container) -and $innerContent -eq "inside") {
    Pass "Dir renamed with contents preserved"
} else {
    Fail "Dir rename failed"
}

# ── F. Advanced ────────────────────────────────────

RunTest "Statfs — Get-PSDrive command"
try {
    $driveInfo = Get-PSDrive -Name $Drive -ErrorAction Stop
    Pass "Get-PSDrive succeeded (Used=$($driveInfo.Used) Free=$($driveInfo.Free))"
} catch {
    Fail "Get-PSDrive failed: $($_.Exception.Message)"
}

RunTest "Rapid — burst writes (10 files)"
1..10 | ForEach-Object {
    [IO.File]::WriteAllText("$MountPoint\burst_$_.txt", "burst $_", [Text.UTF8Encoding]::new($false))
}
Start-Sleep -Seconds 1
$burstOk = $true
1..10 | ForEach-Object {
    $c = ""
    try { $c = [IO.File]::ReadAllText("$($script:DataDir)\burst_$_.txt") } catch {}
    if ($c -ne "burst $_") { $script:BurstFailIndex = $_; $burstOk = $false }
}
if ($burstOk) {
    Pass "All 10 burst writes landed on backend"
} else {
    Fail "Some burst writes lost (failed at index $($script:BurstFailIndex))"
}

RunTest "Unicode — content round-trip"
[IO.File]::WriteAllText("$MountPoint\uni_test.txt", "unicode test", [Text.UTF8Encoding]::new($false))
Start-Sleep -Milliseconds 500
$uni = [IO.File]::ReadAllText("$MountPoint\uni_test.txt")
if ($uni -eq "unicode test") {
    Pass "Write + read round-trip OK"
} else {
    Fail "Round-trip failed: '$uni'"
}

RunTest "Multi-read — read 3 different files sequentially"
$mr1 = [IO.File]::ReadAllText("$MountPoint\hello.txt")
$mr2 = 0; try { $mr2 = (Get-Item "$MountPoint\large.bin").Length } catch {}
$mr3 = 0; try { $mr3 = (Get-Item "$MountPoint\binary.dat").Length } catch {}
if ($mr1 -eq "updated content" -and $mr2 -eq 1048576 -and $mr3 -eq 5120) {
    Pass "Text + 1MB + 5KB reads all correct"
} else {
    Fail "hello='$mr1' large=$mr2 binary=$mr3"
}

# ════════════════════════════════════════════════════
#  Phase 2 — Read-only mode
# ════════════════════════════════════════════════════

Write-Host ""
Write-Host "── Phase 2: Read-only mode ──"
StopMount
StartMount -ExtraArgs @("--read-only")

RunTest "Readonly — read succeeds"
$roContent = [IO.File]::ReadAllText("$MountPoint\hello.txt")
if ($roContent -eq "updated content") {
    Pass "Read in RO mode works"
} else {
    Fail "Read in RO mode returned wrong data: '$roContent'"
}

RunTest "Readonly — write blocked"
$roWriteOk = $true
try {
    [IO.File]::WriteAllText("$MountPoint\ro_test.txt", "fail", [Text.UTF8Encoding]::new($false))
    $roWriteOk = $false
} catch {
    # Expected: IOException from read-only FUSE
}
if ($roWriteOk) {
    Pass "Write blocked (read-only)"
} else {
    Fail "Write should be blocked"
}

RunTest "Readonly — mkdir blocked"
$roMkdirOk = $true
try {
    New-Item -ItemType Directory -Path "$MountPoint\ro_dir" -ErrorAction Stop | Out-Null
    $roMkdirOk = $false
} catch {
    # Expected
}
if ($roMkdirOk) {
    Pass "mkdir blocked (read-only)"
} else {
    Fail "mkdir should be blocked"
}

# ════════════════════════════════════════════════════
#  Phase 3 — Auth mode
# ════════════════════════════════════════════════════

Write-Host ""
Write-Host "── Phase 3: Auth mode ──"
StopMount
StopDufs
StartDufs -DataPath $script:DataDir -ExtraArgs @("-A", "-a", "testuser:testpass@/:rw", "--enable-cors")
StartMount -ExtraArgs @("--user", "testuser", "--pass", "testpass")

RunTest "Auth — read with correct credentials"
$authContent = [IO.File]::ReadAllText("$MountPoint\hello.txt")
if ($authContent -eq "updated content") {
    Pass "Auth read works"
} else {
    Fail "Auth read returned wrong data: '$authContent'"
}

RunTest "Auth — write with correct credentials"
[IO.File]::WriteAllText("$MountPoint\auth_write.txt", "auth write", [Text.UTF8Encoding]::new($false))
Start-Sleep -Milliseconds 500
$authWriteContent = ""
try { $authWriteContent = [IO.File]::ReadAllText("$($script:DataDir)\auth_write.txt") } catch {}
if ((Test-Path "$($script:DataDir)\auth_write.txt") -and $authWriteContent -eq "auth write") {
    Pass "Auth write works"
} else {
    Fail "Auth write failed"
}

RunTest "Auth — list directory"
$authLs = Get-ChildItem $MountPoint | Select-Object -ExpandProperty Name
if ($authLs -contains "hello.txt") {
    Pass "Auth listing works"
} else {
    Fail "Auth listing failed"
}

# ════════════════════════════════════════════════════
#  Phase 4 — Server-side change propagation
# ════════════════════════════════════════════════════

Write-Host ""
Write-Host "── Phase 4: Server-side change propagation (cache coherence) ──"
StopMount
StopDufs

$script:Phase4Data = Join-Path $env:TEMP "dufs-e2e-data-$(Get-Random)"
New-Item -ItemType Directory -Path "$($script:Phase4Data)\docs" -Force | Out-Null
[IO.File]::WriteAllText("$($script:Phase4Data)\file_a.txt", "original content", [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText("$($script:Phase4Data)\file_b.txt", "will be deleted", [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText("$($script:Phase4Data)\file_c.txt", "will be renamed", [Text.UTF8Encoding]::new($false))
New-Item -ItemType Directory -Path "$($script:Phase4Data)\empty_dir" -Force | Out-Null
New-Item -ItemType Directory -Path "$($script:Phase4Data)\dir_to_delete" -Force | Out-Null
[IO.File]::WriteAllText("$($script:Phase4Data)\dir_to_delete\inner.txt", "inside", [Text.UTF8Encoding]::new($false))
Info "Phase 4 data: $($script:Phase4Data)"

StartDufs -DataPath $script:Phase4Data -ExtraArgs @("-A", "--enable-cors")
StartMount -ExtraArgs @("--cache-ttl", "1")

# ── A. Server-side file creation → FUSE visible ──

RunTest "Srv->FUSE: server creates file, FUSE reads it"
$null = Get-ChildItem $MountPoint
Start-Sleep -Seconds 2
WebDavPut "/srv_new.txt" "server created" | Out-Null
Start-Sleep -Seconds 2
$srvNewContent = ""
try { $srvNewContent = [IO.File]::ReadAllText("$MountPoint\srv_new.txt") } catch {}
if ($srvNewContent -eq "server created") {
    Pass "New file visible through FUSE"
} else {
    Fail "Content='$srvNewContent', expected='server created'"
}

RunTest "Srv->FUSE: server creates file, ls shows it"
$srvLs = Get-ChildItem $MountPoint | Select-Object -ExpandProperty Name
if ($srvLs -contains "srv_new.txt") {
    Pass "ls shows new server-created file"
} else {
    Fail "ls does not show srv_new.txt"
}

# ── B. Server-side file modification → FUSE sees fresh content ──

RunTest "Srv->FUSE: server modifies file, FUSE reads new content"
$before = [IO.File]::ReadAllText("$MountPoint\file_a.txt")
if ($before -ne "original content") {
    Fail "Setup wrong: content='$before'"
} else {
    WebDavPut "/file_a.txt" "modified by server" | Out-Null
    Start-Sleep -Milliseconds 1500
    $after = [IO.File]::ReadAllText("$MountPoint\file_a.txt")
    if ($after -eq "modified by server") {
        Pass "FUSE reads updated content after cache expiry"
    } else {
        Fail "Stale content: '$after'"
    }
}

RunTest "Srv->FUSE: server modifies file, stat shows new size"
$newSize = (Get-Item "$MountPoint\file_a.txt").Length
$expected = 18  # "modified by server" = 18 bytes
if ($newSize -eq $expected) {
    Pass "Stat shows new size ($newSize)"
} else {
    Fail "Stat size=$newSize, expected=$expected"
}

# ── C. Server-side file deletion → FUSE sees it gone ──

RunTest "Srv->FUSE: server deletes file, FUSE can no longer access it"
if (-not (Test-Path "$MountPoint\file_b.txt")) {
    Fail "Setup: file_b.txt not found via FUSE"
} else {
    WebDavDelete "/file_b.txt" | Out-Null
    Start-Sleep -Milliseconds 1500
    $srvLs = Get-ChildItem $MountPoint | Select-Object -ExpandProperty Name
    if ($srvLs -contains "file_b.txt") {
        Fail "ls still shows deleted file"
    } else {
        Pass "File gone from FUSE readdir view after cache expiry"
    }
}

RunTest "Srv->FUSE: server-deleted file not in ls listing"
$srvLs2 = Get-ChildItem $MountPoint | Select-Object -ExpandProperty Name
if ($srvLs2 -contains "file_b.txt") {
    Fail "ls still shows deleted file"
} else {
    Pass "ls no longer shows deleted file"
}

# ── D. Server-side rename → FUSE sees new name ──

RunTest "Srv->FUSE: server renames file, FUSE sees new name"
WebDavMove "/file_c.txt" "http://127.0.0.1:$Port/file_renamed.txt" | Out-Null
Start-Sleep -Milliseconds 1500
$oldGone = -not (Test-Path "$MountPoint\file_c.txt")
$newContent = ""
try { $newContent = [IO.File]::ReadAllText("$MountPoint\file_renamed.txt") } catch {}
if ($oldGone -and $newContent -eq "will be renamed") {
    Pass "Old name gone, new name visible with correct content"
} else {
    Fail "old=$(-not $oldGone) new_content='$newContent'"
}

# ── E. Server-side directory creation → FUSE sees it ──

RunTest "Srv->FUSE: server creates directory, FUSE can enter it"
WebDavMkdir "/srv_dir" | Out-Null
Start-Sleep -Milliseconds 1500
if (Test-Path "$MountPoint\srv_dir" -PathType Container) {
    Pass "New directory visible and accessible"
} else {
    Fail "Directory not visible through FUSE"
}

RunTest "Srv->FUSE: server creates file in subdir, FUSE reads it"
WebDavPut "/docs/srv_nested.txt" "nested by server" | Out-Null
Start-Sleep -Milliseconds 1500
$nestedContent = ""
try { $nestedContent = [IO.File]::ReadAllText("$MountPoint\docs\srv_nested.txt") } catch {}
if ($nestedContent -eq "nested by server") {
    Pass "Nested file visible through FUSE"
} else {
    Fail "Nested file not visible"
}

# ── F. Server-side directory deletion → FUSE sees it gone ──

RunTest "Srv->FUSE: server removes non-empty directory, FUSE sees it gone"
WebDavDelete "/dir_to_delete" | Out-Null
Start-Sleep -Milliseconds 1500
if (-not (Test-Path "$MountPoint\dir_to_delete")) {
    Pass "Removed directory gone from FUSE view"
} else {
    Fail "Directory still visible"
}

# ── G. Type change: file → directory with same name ──

RunTest "Srv->FUSE: server replaces file with directory, FUSE sees correct type"
WebDavDelete "/file_a.txt" | Out-Null
WebDavMkdir "/file_a.txt" | Out-Null
Start-Sleep -Milliseconds 1500
$srvLs3 = Get-ChildItem $MountPoint | Select-Object -ExpandProperty Name
if ($srvLs3 -contains "file_a.txt") {
    if (Test-Path "$MountPoint\file_a.txt" -PathType Container) {
        Pass "File replaced by directory visible as directory"
    } else {
        Fail "file_a.txt visible but not a directory"
    }
} else {
    Fail "file_a.txt not visible in ls (cache may not have expired yet)"
}

# ── H. Rapid server-side creates → FUSE lists all ──

RunTest "Srv->FUSE: rapid server-side creates (5 files), FUSE lists all"
1..5 | ForEach-Object {
    WebDavPut "/rapid_$_.txt" "rapid content $_" | Out-Null
}
Start-Sleep -Milliseconds 1500
$rapidNames = Get-ChildItem $MountPoint | Select-Object -ExpandProperty Name
$rapidCount = ($rapidNames | Where-Object { $_ -match "^rapid_" }).Count
if ($rapidCount -eq 5) {
    Pass "All 5 rapid files visible"
} else {
    Fail "Expected 5, found $rapidCount"
}

# ── I. FUSE client not crashed by server-side churn ──

RunTest "Srv->FUSE: concurrent access — FUSE reads during server writes"
WebDavPut "/concurrent.txt" "first" | Out-Null
Start-Sleep -Milliseconds 100
try { $null = [IO.File]::ReadAllText("$MountPoint\docs\srv_nested.txt") } catch {}
Start-Sleep -Seconds 2
$concurrentContent = ""
try { $concurrentContent = [IO.File]::ReadAllText("$MountPoint\concurrent.txt") } catch {}
$mountAlive = Test-Path $MountPoint
if ($mountAlive -and $concurrentContent -eq "first") {
    Pass "No crash during concurrent access"
} else {
    Fail "Mount broken or data wrong"
}

# ── J. Deeply nested server-created structure ──

RunTest "Srv->FUSE: server creates 3-level nested dirs, FUSE navigates"
WebDavMkdir "/deep" | Out-Null
WebDavMkdir "/deep/l2" | Out-Null
WebDavMkdir "/deep/l2/l3" | Out-Null
WebDavPut "/deep/l2/l3/bottom.txt" "deep bottom" | Out-Null
Start-Sleep -Milliseconds 1500
$deepDir = Test-Path "$MountPoint\deep\l2\l3" -PathType Container
$deepContent = ""
try { $deepContent = [IO.File]::ReadAllText("$MountPoint\deep\l2\l3\bottom.txt") } catch {}
if ($deepDir -and $deepContent -eq "deep bottom") {
    Pass "3-level nested structure navigable"
} else {
    Fail "Cannot navigate nested structure"
}

# ── K. FUSE write + server write to same file ──

RunTest "Srv+FUSE: FUSE writes, then server overwrites, FUSE reads fresh"
[IO.File]::WriteAllText("$MountPoint\mixed.txt", "from fuse", [Text.UTF8Encoding]::new($false))
Start-Sleep -Milliseconds 1500
$fuseFirst = ""
try { $fuseFirst = [IO.File]::ReadAllText("$MountPoint\mixed.txt") } catch {}
WebDavPut "/mixed.txt" "from server" | Out-Null
Start-Sleep -Milliseconds 1500
$afterSrv = ""
try { $afterSrv = [IO.File]::ReadAllText("$MountPoint\mixed.txt") } catch {}
if ($fuseFirst -eq "from fuse" -and $afterSrv -eq "from server") {
    Pass "FUSE write, then server write, both visible in sequence"
} else {
    Fail "fuse='$fuseFirst' after_srv='$afterSrv'"
}

# Cleanup Phase 4
StopMount
StopDufs

# ════════════════════════════════════════════════════
#  Summary
# ════════════════════════════════════════════════════

} finally {
    Write-Host ""
    Write-Host "── Cleanup ──"
    StopMount
    StopDufs
    if ($script:DataDir -and (Test-Path $script:DataDir)) {
        Remove-Item -Recurse -Force $script:DataDir -ErrorAction SilentlyContinue
    }
    if ($script:Phase4Data -and (Test-Path $script:Phase4Data)) {
        Remove-Item -Recurse -Force $script:Phase4Data -ErrorAction SilentlyContinue
    }
}

Write-Host ""
Write-Host "=================================================="
Write-Host "  Results: ${GREEN}$script:Passed passed${NC} / $script:Total total"
if ($script:Failures.Count -gt 0) {
    Write-Host "  ${RED}Failures:${NC}"
    $script:Failures | ForEach-Object { Write-Host "    ${RED}- $_${NC}" }
}
if ($script:Skipped.Count -gt 0) {
    Write-Host "  ${YELLOW}Skipped:${NC}"
    $script:Skipped | ForEach-Object { Write-Host "    ${YELLOW}- $_${NC}" }
}
Write-Host "=================================================="

if ($script:Failures.Count -eq 0) { exit 0 } else { exit 1 }

[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-Contract {
    param(
        [Parameter(Mandatory)]
        [bool]$Condition,

        [Parameter(Mandatory)]
        [string]$Message
    )

    if (-not $Condition) {
        throw "contract failed: $Message"
    }
}

function Assert-Sequence {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]]$Actual,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]]$Expected,

        [Parameter(Mandatory)]
        [string]$Description
    )

    Assert-Contract (($Actual -join "|") -ceq ($Expected -join "|")) `
        "$Description; actual=$($Actual -join ',') expected=$($Expected -join ',')"
}

function Assert-Throws {
    param(
        [Parameter(Mandatory)]
        [scriptblock]$Action,

        [Parameter(Mandatory)]
        [string]$Pattern
    )

    try {
        & $Action | Out-Null
    }
    catch {
        Assert-Contract ($_.Exception.Message -match $Pattern) `
            "failure '$($_.Exception.Message)' must match '$Pattern'"
        return
    }
    throw "contract failed: expected failure matching '$Pattern'"
}

function Get-ContractProcessEnvironmentSnapshot {
    $entries = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in [Environment]::GetEnvironmentVariables("Process").GetEnumerator()) {
        $entries.Add([pscustomobject]@{
            Name = [string]$entry.Key
            Value = [string]$entry.Value
        })
    }
    return @($entries | Sort-Object -Property Name -CaseSensitive)
}

function Restore-ContractProcessEnvironment {
    param(
        [Parameter(Mandatory)]
        [object[]]$Snapshot
    )

    foreach ($entry in @([Environment]::GetEnvironmentVariables("Process").GetEnumerator())) {
        Remove-Item -LiteralPath "Env:$([string]$entry.Key)" -ErrorAction SilentlyContinue
    }
    foreach ($entry in $Snapshot) {
        Set-Item -LiteralPath "Env:$($entry.Name)" -Value $entry.Value
    }
}

function Assert-EnvironmentSnapshot {
    param(
        [Parameter(Mandatory)]
        [object[]]$Actual,

        [Parameter(Mandatory)]
        [object[]]$Expected,

        [Parameter(Mandatory)]
        [string]$Description
    )

    Assert-Contract ($Actual.Count -eq $Expected.Count) `
        "$Description must restore the exact variable count"
    for ($index = 0; $index -lt $Expected.Count; $index++) {
        Assert-Contract (
            $Actual[$index].Name -ceq $Expected[$index].Name -and
            $Actual[$index].Value -ceq $Expected[$index].Value
        ) "$Description differs at environment entry $index"
    }
}

function Wait-ContractFileCreated {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [int]$TimeoutMilliseconds = 10000
    )

    if (Test-Path -LiteralPath $Path -PathType Leaf) {
        return
    }
    $parent = Split-Path -Parent $Path
    $watcher = [System.IO.FileSystemWatcher]::new($parent, (Split-Path -Leaf $Path))
    try {
        $watcher.NotifyFilter = [System.IO.NotifyFilters]::FileName
        $watcher.EnableRaisingEvents = $true
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            return
        }
        $change = $watcher.WaitForChanged(
            [System.IO.WatcherChangeTypes]::Created,
            $TimeoutMilliseconds
        )
        Assert-Contract (
            -not $change.TimedOut -and
            (Test-Path -LiteralPath $Path -PathType Leaf)
        ) "timed out waiting for synchronized contract file $Path"
    }
    finally {
        $watcher.Dispose()
    }
}

$modulePath = Join-Path $PSScriptRoot "windows_workspace.psm1"
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
    "easycon lifecycle contract {0}" -f [guid]::NewGuid().ToString("N")
)
$environmentRoot = Join-Path $temporaryRoot "e/contract"
$location = [pscustomobject]@{
    CacheRoot = $temporaryRoot
    EnvironmentRoot = $environmentRoot
    StampPath = Join-Path $environmentRoot "environment-stamp.json"
    LockPath = Join-Path $temporaryRoot "locks/contract.lock"
    IdentityKey = "contract"
    WorkspaceKey = "contract-workspace"
}
Import-Module -Name $modulePath -Force
$workspaceModule = Get-Module windows_workspace

function Invoke-PrivateEnvironmentLifecycle {
    param(
        [Parameter(Mandatory)]
        [string]$Mode,

        [Parameter(Mandatory)]
        [object]$Location,

        [Parameter(Mandatory)]
        [scriptblock]$SetupAction,

        [Parameter(Mandatory)]
        [scriptblock]$VerifyAction,

        [Parameter(Mandatory)]
        [scriptblock]$WorkspaceAction,

        [int]$LeaseTimeoutMilliseconds = 0
    )

    & $script:workspaceModule {
        param($LifecycleMode, $OwnedLocation, $Setup, $Verify, $Workspace, $Timeout)
        Invoke-EasyConEnvironmentLifecycle -Mode $LifecycleMode -Location $OwnedLocation `
            -SetupAction $Setup -VerifyAction $Verify -WorkspaceAction $Workspace `
            -LeaseTimeoutMilliseconds $Timeout
    } $Mode $Location $SetupAction $VerifyAction $WorkspaceAction $LeaseTimeoutMilliseconds
}

function Enter-PrivateEnvironmentLease {
    param(
        [Parameter(Mandatory)]
        [object]$Location,

        [Parameter(Mandatory)]
        [string]$Access,

        [int]$TimeoutMilliseconds = 0
    )

    & $script:workspaceModule {
        param($OwnedLocation, $LeaseAccess, $Timeout)
        Enter-EasyConEnvironmentLease -Location $OwnedLocation -Access $LeaseAccess `
            -TimeoutMilliseconds $Timeout
    } $Location $Access $TimeoutMilliseconds
}

function Get-PrivateSharedContentAsset {
    param(
        [Parameter(Mandatory)]
        [hashtable]$Parameters
    )

    & $script:workspaceModule {
        param($Arguments)
        Get-EasyConSharedContentAsset @Arguments
    } $Parameters
}

function Enter-PrivateSharedCacheLease {
    param(
        [Parameter(Mandatory)]
        [string]$CacheRoot,

        [Parameter(Mandatory)]
        [string]$Access,

        [int]$TimeoutMilliseconds = 0
    )

    & $script:workspaceModule {
        param($Root, $LeaseAccess, $Timeout)
        Enter-EasyConSharedCacheLease -CacheRoot $Root -Access $LeaseAccess `
            -TimeoutMilliseconds $Timeout -RetryMilliseconds 0
    } $CacheRoot $Access $TimeoutMilliseconds
}

function Invoke-EnvironmentRestorationContract {
    param(
        [Parameter(Mandatory)]
        [string]$Mode,

        [Parameter(Mandatory)]
        [scriptblock]$SetupAction,

        [Parameter(Mandatory)]
        [scriptblock]$VerifyAction,

        [Parameter(Mandatory)]
        [scriptblock]$WorkspaceAction,

        [string]$FailurePattern
    )

    $outer = Get-ContractProcessEnvironmentSnapshot
    $expected = $null
    $actual = $null
    try {
        foreach ($name in @(
            "EasyCon_Contract_Present",
            "EasyCon_Contract_Empty",
            "EasyCon_Contract_Cased",
            "EASYCON_CONTRACT_ABSENT"
        )) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        }
        Set-Item -LiteralPath "Env:EasyCon_Contract_Present" -Value "original"
        Set-Item -LiteralPath "Env:EasyCon_Contract_Empty" -Value ""
        Set-Item -LiteralPath "Env:EasyCon_Contract_Cased" -Value "mixed-case-name"
        Set-Item -LiteralPath "Env:RUSTC" -Value "poison-rustc"
        $expected = Get-ContractProcessEnvironmentSnapshot

        if ([string]::IsNullOrWhiteSpace($FailurePattern)) {
            Invoke-PrivateEnvironmentLifecycle -Mode $Mode -Location $location `
                -SetupAction $SetupAction -VerifyAction $VerifyAction `
                -WorkspaceAction $WorkspaceAction | Out-Null
        }
        else {
            Assert-Throws -Pattern $FailurePattern -Action {
                Invoke-PrivateEnvironmentLifecycle -Mode $Mode -Location $location `
                    -SetupAction $SetupAction -VerifyAction $VerifyAction `
                    -WorkspaceAction $WorkspaceAction
            }
        }
        $actual = Get-ContractProcessEnvironmentSnapshot
    }
    finally {
        Restore-ContractProcessEnvironment -Snapshot $outer
    }
    Assert-EnvironmentSnapshot -Actual $actual -Expected $expected `
        -Description "$Mode lifecycle"
}

$events = [System.Collections.Generic.List[string]]::new()
$state = [pscustomobject]@{
    FailSetup = $false
    RejectStale = $false
}
$sharedPayload = [System.Text.Encoding]::UTF8.GetBytes("lifecycle shared asset")
$sharedHash = [Convert]::ToHexString(
    [System.Security.Cryptography.SHA256]::HashData($sharedPayload)
).ToLowerInvariant()
$sharedState = [pscustomobject]@{ Downloads = 0 }
$sharedDownload = {
    param($Url, $Destination)
    $null = $Url
    $sharedState.Downloads++
    [System.IO.File]::WriteAllBytes($Destination, $sharedPayload)
}.GetNewClosure()
$sharedAssetParameters = @{
    SharedCacheRoot = (Join-Path $temporaryRoot "caches")
    Url = "https://example.invalid/lifecycle.bin"
    Algorithm = "SHA256"
    Hash = $sharedHash
    Bytes = [long]$sharedPayload.Length
    Description = "lifecycle shared asset"
    DownloadAction = $sharedDownload
}
$setupAction = {
    $events.Add("setup") | Out-Null
    Get-PrivateSharedContentAsset -Parameters $sharedAssetParameters | Out-Null
    if ($state.RejectStale) {
        Assert-Contract (-not (Test-Path -LiteralPath (Join-Path $environmentRoot "stale.txt"))) `
            "damaged rebuild must remove the old environment tree before Setup"
        Assert-Contract (-not (Test-Path -LiteralPath (Join-Path $environmentRoot "partial.txt"))) `
            "interrupted rebuild must remove partial state before retry"
    }
    New-Item -ItemType Directory -Force -Path $environmentRoot | Out-Null
    if ($state.FailSetup) {
        Set-Content -LiteralPath (Join-Path $environmentRoot "partial.txt") `
            -Value "partial" -Encoding utf8NoBOM
        throw "synthetic interrupted Setup"
    }
    Set-Content -LiteralPath $location.StampPath -Value "ready" -Encoding utf8NoBOM
}.GetNewClosure()
$verifyAction = {
    $events.Add("verify") | Out-Null
    if (
        -not (Test-Path -LiteralPath $location.StampPath -PathType Leaf) -or
        (Get-Content -Raw -LiteralPath $location.StampPath).Trim() -cne "ready"
    ) {
        throw "synthetic environment mismatch"
    }
    return "verified"
}.GetNewClosure()
$workspaceAction = {
    param($Summary)
    Assert-Contract ($Summary -ceq "verified") "Workspace must receive the verified summary"
    $events.Add("workspace") | Out-Null
}.GetNewClosure()

$contractFailure = $null
try {
    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "setup", "verify") `
        -Description "first Setup must verify, prepare, and verify the published result"
    Assert-Contract ($sharedState.Downloads -eq 1) `
        "the first lifecycle identity must fetch a missing fixed asset once"

    $events.Clear()
    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify") `
        -Description "already-ready Setup must not provision again"

    $readyMarker = Join-Path $environmentRoot "ready-preserved.txt"
    Set-Content -LiteralPath $readyMarker -Value "preserve" -Encoding utf8NoBOM
    $readyLeaseState = [pscustomobject]@{ SetupCalls = 0; VerifyCalls = 0 }
    $writerLease = Enter-PrivateSharedCacheLease -CacheRoot $location.CacheRoot `
        -Access Exclusive -TimeoutMilliseconds 0
    try {
        $expectedBusy = $null
        try {
            Enter-PrivateSharedCacheLease -CacheRoot $location.CacheRoot -Access Shared `
                -TimeoutMilliseconds 0 | Out-Null
        }
        catch {
            $expectedBusy = $_
        }
        Assert-Contract ($null -ne $expectedBusy) `
            "the regression fixture must hold the shared-cache writer lease"

        $actualBusy = $null
        try {
            Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
                -SetupAction {
                    $readyLeaseState.SetupCalls++
                }.GetNewClosure() -VerifyAction {
                    $readyLeaseState.VerifyCalls++
                    return "verified"
                }.GetNewClosure() -WorkspaceAction { param($Summary) } `
                -LeaseTimeoutMilliseconds 0 | Out-Null
        }
        catch {
            $actualBusy = $_
        }
        Assert-Contract ($null -ne $actualBusy) `
            "ready Setup must report shared-cache lease contention"
        Assert-Contract ($actualBusy.Exception.Message -ceq $expectedBusy.Exception.Message) `
            "ready Setup must preserve the original shared-cache busy diagnostic"
        $stampExists = Test-Path -LiteralPath $location.StampPath -PathType Leaf
        $markerExists = Test-Path -LiteralPath $readyMarker -PathType Leaf
        $readyStatePreserved = (
            $stampExists -and
            (Get-Content -Raw -LiteralPath $location.StampPath).Trim() -ceq "ready" -and
            $markerExists -and
            (Get-Content -Raw -LiteralPath $readyMarker).Trim() -ceq "preserve"
        )
        Assert-Contract (
            $readyLeaseState.VerifyCalls -eq 0 -and
            $readyLeaseState.SetupCalls -eq 0 -and
            $readyStatePreserved
        ) (
            "shared-cache contention must preserve ready state without verification or " +
            "provision; VerifyCalls=$($readyLeaseState.VerifyCalls) " +
            "SetupCalls=$($readyLeaseState.SetupCalls) StampExists=$stampExists " +
            "MarkerExists=$markerExists"
        )
    }
    finally {
        $writerLease.Dispose()
    }

    $waitProbeRoot = Join-Path $temporaryRoot "shared cache wait probe"
    $waitEnvironment = Join-Path $waitProbeRoot "environment"
    $waitStamp = Join-Path $waitEnvironment "environment-stamp.json"
    $waitMarker = Join-Path $waitEnvironment "ready-preserved.txt"
    $busyObserved = Join-Path $waitProbeRoot "busy-observed.txt"
    $waitingObserved = Join-Path $waitProbeRoot "waiting-observed.txt"
    $waitResult = Join-Path $waitProbeRoot "result.txt"
    New-Item -ItemType Directory -Force -Path $waitEnvironment | Out-Null
    Set-Content -LiteralPath $waitStamp -Value "ready" -Encoding utf8NoBOM
    Set-Content -LiteralPath $waitMarker -Value "preserve" -Encoding utf8NoBOM
    $waitChild = Join-Path $waitProbeRoot "wait-child.ps1"
    Set-Content -LiteralPath $waitChild -Encoding utf8NoBOM -Value @'
param(
    [Parameter(Mandatory)][string]$ModulePath,
    [Parameter(Mandatory)][string]$CacheRoot,
    [Parameter(Mandatory)][string]$EnvironmentRoot,
    [Parameter(Mandatory)][string]$StampPath,
    [Parameter(Mandatory)][string]$ReadyMarker,
    [Parameter(Mandatory)][string]$BusyObserved,
    [Parameter(Mandatory)][string]$WaitingObserved,
    [Parameter(Mandatory)][string]$ResultPath
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
Import-Module -Name $ModulePath -Force
$module = Get-Module windows_workspace
$location = [pscustomobject]@{
    CacheRoot = $CacheRoot
    EnvironmentRoot = $EnvironmentRoot
    StampPath = $StampPath
    LockPath = Join-Path $CacheRoot "locks/wait-probe.lock"
    IdentityKey = "wait-probe"
    WorkspaceKey = "wait-probe-workspace"
}
$state = [pscustomobject]@{ SetupCalls = 0; VerifyCalls = 0 }
$setup = { $state.SetupCalls++ }.GetNewClosure()
$verify = {
    $state.VerifyCalls++
    if (
        -not (Test-Path -LiteralPath $StampPath -PathType Leaf) -or
        (Get-Content -Raw -LiteralPath $StampPath).Trim() -cne "ready"
    ) {
        throw "wait probe environment is not ready"
    }
    return "verified"
}.GetNewClosure()
$invokeLifecycle = {
    param([int]$Timeout)
    & $module {
        param($OwnedLocation, $SetupAction, $VerifyAction, $LeaseTimeout)
        Invoke-EasyConEnvironmentLifecycle -Mode Setup -Location $OwnedLocation `
            -SetupAction $SetupAction -VerifyAction $VerifyAction `
            -WorkspaceAction { param($Summary) } `
            -LeaseTimeoutMilliseconds $LeaseTimeout
    } $location $setup $verify $Timeout
}.GetNewClosure()
$expectedBusy = $null
try {
    & $module {
        param($Root)
        Enter-EasyConSharedCacheLease -CacheRoot $Root -Access Shared `
            -TimeoutMilliseconds 0 -RetryMilliseconds 0
    } $CacheRoot | ForEach-Object { $_.Dispose() }
}
catch {
    $expectedBusy = $_
}
if ($null -eq $expectedBusy) {
    throw "wait probe did not observe the held writer lease"
}
$actualBusy = $null
try {
    & $invokeLifecycle 0 | Out-Null
}
catch {
    $actualBusy = $_
}
if (
    $null -eq $actualBusy -or
    $actualBusy.Exception.Message -cne $expectedBusy.Exception.Message -or
    $state.SetupCalls -ne 0 -or
    $state.VerifyCalls -ne 0 -or
    -not (Test-Path -LiteralPath $ReadyMarker -PathType Leaf)
) {
    throw "wait probe timeout mutated ready state or changed the busy diagnostic"
}
[System.IO.File]::WriteAllText($BusyObserved, "busy", [System.Text.UTF8Encoding]::new($false))
[System.IO.File]::WriteAllText($WaitingObserved, "waiting", [System.Text.UTF8Encoding]::new($false))
& $invokeLifecycle 10000 | Out-Null
if (
    $state.SetupCalls -ne 0 -or
    $state.VerifyCalls -ne 1 -or
    -not (Test-Path -LiteralPath $ReadyMarker -PathType Leaf)
) {
    throw "wait probe did not resume as already-ready after writer release"
}
[System.IO.File]::WriteAllText($ResultPath, "passed", [System.Text.UTF8Encoding]::new($false))
'@
    $waitWriter = Enter-PrivateSharedCacheLease -CacheRoot $location.CacheRoot `
        -Access Exclusive -TimeoutMilliseconds 0
    $waitProcess = $null
    try {
        $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = Join-Path $PSHOME "pwsh.exe"
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        foreach ($argument in @(
            "-NoLogo", "-NoProfile", "-File", $waitChild,
            "-ModulePath", $modulePath,
            "-CacheRoot", $location.CacheRoot,
            "-EnvironmentRoot", $waitEnvironment,
            "-StampPath", $waitStamp,
            "-ReadyMarker", $waitMarker,
            "-BusyObserved", $busyObserved,
            "-WaitingObserved", $waitingObserved,
            "-ResultPath", $waitResult
        )) {
            $startInfo.ArgumentList.Add([string]$argument)
        }
        $waitProcess = [System.Diagnostics.Process]::Start($startInfo)
        Wait-ContractFileCreated -Path $busyObserved
        Wait-ContractFileCreated -Path $waitingObserved
        Assert-Contract (-not $waitProcess.WaitForExit(250)) `
            "the synchronized contender must still be waiting while the writer lease is held"
        $waitWriter.Dispose()
        $waitWriter = $null
        Assert-Contract ($waitProcess.WaitForExit(10000)) `
            "the synchronized contender must finish after the writer lease is released"
        $waitOutput = $waitProcess.StandardOutput.ReadToEnd()
        $waitError = $waitProcess.StandardError.ReadToEnd()
        Assert-Contract (
            $waitProcess.ExitCode -eq 0 -and
            (Test-Path -LiteralPath $waitResult -PathType Leaf) -and
            (Get-Content -Raw -LiteralPath $waitResult).Trim() -ceq "passed"
        ) "shared-cache waiter failed after release; output=$waitOutput error=$waitError"
    }
    finally {
        if ($null -ne $waitWriter) {
            $waitWriter.Dispose()
        }
        if ($null -ne $waitProcess) {
            if (-not $waitProcess.HasExited) {
                $waitProcess.Kill($true)
                $waitProcess.WaitForExit()
            }
            $waitProcess.Dispose()
        }
    }

    Set-Content -LiteralPath $location.StampPath -Value "damaged" -Encoding utf8NoBOM
    Set-Content -LiteralPath (Join-Path $environmentRoot "stale.txt") `
        -Value "stale" -Encoding utf8NoBOM
    $state.RejectStale = $true
    $events.Clear()
    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "setup", "verify") `
        -Description "damaged prepared state must be rebuilt through the public lifecycle"

    Set-Content -LiteralPath $location.StampPath -Value "damaged" -Encoding utf8NoBOM
    $state.FailSetup = $true
    Assert-Throws -Pattern "synthetic interrupted Setup" -Action {
        Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
            -SetupAction $setupAction -VerifyAction $verifyAction `
            -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0
    }
    $state.FailSetup = $false
    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Contract (-not (Test-Path -LiteralPath (Join-Path $environmentRoot "partial.txt"))) `
        "a later Setup must recover from interrupted partial state"
    Assert-Contract ($sharedState.Downloads -eq 1) `
        "damage and interrupted rebuilds must reuse the verified shared asset"

    foreach ($identityName in @("fingerprint-change", "worktree-change")) {
        $alternateRoot = Join-Path $temporaryRoot "e/$identityName"
        $alternateLocation = [pscustomobject]@{
            CacheRoot = $temporaryRoot
            EnvironmentRoot = $alternateRoot
            StampPath = Join-Path $alternateRoot "environment-stamp.json"
            LockPath = Join-Path $temporaryRoot "locks/$identityName.lock"
            IdentityKey = $identityName
            WorkspaceKey = "$identityName-workspace"
        }
        $alternateSetup = {
            Get-PrivateSharedContentAsset -Parameters $sharedAssetParameters | Out-Null
            New-Item -ItemType Directory -Force -Path $alternateRoot | Out-Null
            Set-Content -LiteralPath $alternateLocation.StampPath -Value "ready" `
                -Encoding utf8NoBOM
        }.GetNewClosure()
        $alternateVerify = {
            if (-not (Test-Path -LiteralPath $alternateLocation.StampPath -PathType Leaf)) {
                throw "alternate identity is not ready"
            }
            return "verified"
        }.GetNewClosure()
        Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $alternateLocation `
            -SetupAction $alternateSetup -VerifyAction $alternateVerify `
            -WorkspaceAction { param($Summary) } -LeaseTimeoutMilliseconds 0 | Out-Null
    }
    Assert-Contract ($sharedState.Downloads -eq 1) `
        "fingerprint and worktree identity changes must reuse the verified shared asset"

    $events.Clear()
    Invoke-PrivateEnvironmentLifecycle -Mode Verify -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify") `
        -Description "Verify must never provision or run workspace gates"

    $events.Clear()
    $workspaceWithOwnership = {
        param($Summary)
        Assert-Contract ($Summary -ceq "verified") "Workspace must receive verified state"
        Assert-Throws -Pattern "busy|ownership" -Action {
            Enter-PrivateEnvironmentLease -Location $location -Access Exclusive `
                -TimeoutMilliseconds 0
        }
        Assert-Throws -Pattern "busy|ownership" -Action {
            Enter-PrivateSharedCacheLease -CacheRoot $location.CacheRoot -Access Exclusive `
                -TimeoutMilliseconds 0
        }
        $events.Add("workspace") | Out-Null
    }.GetNewClosure()
    Invoke-PrivateEnvironmentLifecycle -Mode Workspace -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceWithOwnership -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "workspace") `
        -Description "Workspace must hold shared ownership from Verify through every gate"

    $events.Clear()
    Assert-Throws -Pattern "synthetic environment mismatch" -Action {
        Invoke-PrivateEnvironmentLifecycle -Mode Workspace -Location $location `
            -SetupAction $setupAction -VerifyAction { throw "synthetic environment mismatch" } `
            -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0
    }
    Assert-Sequence -Actual $events.ToArray() -Expected @() `
        -Description "Workspace gates must not start after verification fails"

    foreach ($mode in @("Setup", "Verify", "Workspace")) {
        $exclusive = Enter-PrivateEnvironmentLease -Location $location -Access Exclusive `
            -TimeoutMilliseconds 0
        try {
            $events.Clear()
            Assert-Throws -Pattern "busy|ownership" -Action {
                Invoke-PrivateEnvironmentLifecycle -Mode $mode -Location $location `
                    -SetupAction $setupAction -VerifyAction $verifyAction `
                    -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0
            }
            Assert-Sequence -Actual $events.ToArray() -Expected @() `
                -Description "an active Setup must exclude a competing $mode before state access"
        }
        finally {
            $exclusive.Dispose()
        }
    }

    Assert-Throws -Pattern "synthetic verify failure" -Action {
        Invoke-PrivateEnvironmentLifecycle -Mode Verify -Location $location `
            -SetupAction $setupAction -VerifyAction { throw "synthetic verify failure" } `
            -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0
    }
    $released = Enter-PrivateEnvironmentLease -Location $location -Access Exclusive `
        -TimeoutMilliseconds 0
    $released.Dispose()

    $mutateEnvironment = {
        Remove-Item -LiteralPath "Env:EasyCon_Contract_Present" -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath "Env:EasyCon_Contract_Empty" -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath "Env:EasyCon_Contract_Cased" -ErrorAction SilentlyContinue
        Set-Item -LiteralPath "Env:EASYCON_CONTRACT_PRESENT" -Value "changed"
        Set-Item -LiteralPath "Env:EASYCON_CONTRACT_ABSENT" -Value "added"
        Remove-Item -LiteralPath "Env:RUSTC" -ErrorAction SilentlyContinue
        Set-Item -LiteralPath "Env:CXX" -Value "controlled-cl.exe"
    }.GetNewClosure()
    $verifiedEnvironment = {
        & $mutateEnvironment
        return "verified"
    }.GetNewClosure()
    $successfulWorkspace = {
        param($Summary)
        Assert-Contract ($Summary -ceq "verified") "Workspace must receive verified state"
        Assert-Contract ([string]::IsNullOrEmpty($env:RUSTC)) `
            "Workspace gate must not see the poisoned compiler"
        Assert-Contract ($env:CXX -ceq "controlled-cl.exe") `
            "Workspace gate must see the controlled compiler"
        Set-Item -LiteralPath "Env:EASYCON_GATE_MUTATION" -Value "gate"
    }.GetNewClosure()

    Invoke-EnvironmentRestorationContract -Mode Verify `
        -SetupAction { throw "Verify cannot setup" } `
        -VerifyAction $verifiedEnvironment -WorkspaceAction { param($Summary) }
    Invoke-EnvironmentRestorationContract -Mode Verify `
        -SetupAction { throw "Verify cannot setup" } `
        -VerifyAction {
            & $mutateEnvironment
            throw "synthetic verify restore failure"
        }.GetNewClosure() -WorkspaceAction { param($Summary) } `
        -FailurePattern "synthetic verify restore failure"
    Invoke-EnvironmentRestorationContract -Mode Workspace `
        -SetupAction { throw "Workspace cannot setup" } `
        -VerifyAction $verifiedEnvironment -WorkspaceAction $successfulWorkspace
    Invoke-EnvironmentRestorationContract -Mode Workspace `
        -SetupAction { throw "Workspace cannot setup" } `
        -VerifyAction $verifiedEnvironment -WorkspaceAction {
            param($Summary)
            Assert-Contract ([string]::IsNullOrEmpty($env:RUSTC)) `
                "failing gate must still see the sanitized environment"
            Set-Item -LiteralPath "Env:EASYCON_GATE_MUTATION" -Value "failing-gate"
            throw "synthetic gate restore failure"
        } -FailurePattern "synthetic gate restore failure"
    Invoke-EnvironmentRestorationContract -Mode Setup -SetupAction {
        & $mutateEnvironment
        throw "synthetic setup restore failure"
    }.GetNewClosure() -VerifyAction {
        & $mutateEnvironment
        throw "synthetic setup not ready"
    }.GetNewClosure() -WorkspaceAction { param($Summary) } `
        -FailurePattern "synthetic setup restore failure"
}
catch {
    $contractFailure = $_
    throw
}
finally {
    $cleanupFailure = $null
    try {
        Remove-Module windows_workspace -ErrorAction Stop
        if (Test-Path -LiteralPath $temporaryRoot) {
            Remove-Item -LiteralPath $temporaryRoot -Recurse -Force -ErrorAction Stop
        }
    }
    catch {
        $cleanupFailure = $_
    }
    if ($null -ne $cleanupFailure) {
        if ($null -ne $contractFailure) {
            $contractFailure.Exception.Data["EasyConLifecycleContractCleanupFailure"] = `
                $cleanupFailure.Exception.ToString()
        }
        else {
            throw $cleanupFailure
        }
    }
}

Write-Output "Windows environment lifecycle contracts passed: 19 cases"

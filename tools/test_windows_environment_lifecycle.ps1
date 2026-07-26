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
$setupAction = {
    $events.Add("setup") | Out-Null
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

try {
    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "setup", "verify") `
        -Description "first Setup must verify, prepare, and verify the published result"

    $events.Clear()
    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify") `
        -Description "already-ready Setup must not provision again"

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
finally {
    Remove-Module windows_workspace -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $temporaryRoot) {
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}

Write-Output "Windows environment lifecycle contracts passed: 16 cases"

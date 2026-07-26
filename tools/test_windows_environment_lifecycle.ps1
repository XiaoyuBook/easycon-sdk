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
    Invoke-EasyConEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "setup", "verify") `
        -Description "first Setup must verify, prepare, and verify the published result"

    $events.Clear()
    Invoke-EasyConEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify") `
        -Description "already-ready Setup must not provision again"

    Set-Content -LiteralPath $location.StampPath -Value "damaged" -Encoding utf8NoBOM
    Set-Content -LiteralPath (Join-Path $environmentRoot "stale.txt") `
        -Value "stale" -Encoding utf8NoBOM
    $state.RejectStale = $true
    $events.Clear()
    Invoke-EasyConEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "setup", "verify") `
        -Description "damaged prepared state must be rebuilt through the public lifecycle"

    Set-Content -LiteralPath $location.StampPath -Value "damaged" -Encoding utf8NoBOM
    $state.FailSetup = $true
    Assert-Throws -Pattern "synthetic interrupted Setup" -Action {
        Invoke-EasyConEnvironmentLifecycle -Mode Setup -Location $location `
            -SetupAction $setupAction -VerifyAction $verifyAction `
            -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0
    }
    $state.FailSetup = $false
    Invoke-EasyConEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Contract (-not (Test-Path -LiteralPath (Join-Path $environmentRoot "partial.txt"))) `
        "a later Setup must recover from interrupted partial state"

    $events.Clear()
    Invoke-EasyConEnvironmentLifecycle -Mode Verify -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify") `
        -Description "Verify must never provision or run workspace gates"

    $events.Clear()
    $workspaceWithOwnership = {
        param($Summary)
        Assert-Contract ($Summary -ceq "verified") "Workspace must receive verified state"
        Assert-Throws -Pattern "busy|ownership" -Action {
            Enter-EasyConEnvironmentLease -Location $location -Access Exclusive `
                -TimeoutMilliseconds 0
        }
        $events.Add("workspace") | Out-Null
    }.GetNewClosure()
    Invoke-EasyConEnvironmentLifecycle -Mode Workspace -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceWithOwnership -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "workspace") `
        -Description "Workspace must hold shared ownership from Verify through every gate"

    $events.Clear()
    Assert-Throws -Pattern "synthetic environment mismatch" -Action {
        Invoke-EasyConEnvironmentLifecycle -Mode Workspace -Location $location `
            -SetupAction $setupAction -VerifyAction { throw "synthetic environment mismatch" } `
            -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0
    }
    Assert-Sequence -Actual $events.ToArray() -Expected @() `
        -Description "Workspace gates must not start after verification fails"

    foreach ($mode in @("Setup", "Verify", "Workspace")) {
        $exclusive = Enter-EasyConEnvironmentLease -Location $location -Access Exclusive `
            -TimeoutMilliseconds 0
        try {
            $events.Clear()
            Assert-Throws -Pattern "busy|ownership" -Action {
                Invoke-EasyConEnvironmentLifecycle -Mode $mode -Location $location `
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
        Invoke-EasyConEnvironmentLifecycle -Mode Verify -Location $location `
            -SetupAction $setupAction -VerifyAction { throw "synthetic verify failure" } `
            -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0
    }
    $released = Enter-EasyConEnvironmentLease -Location $location -Access Exclusive `
        -TimeoutMilliseconds 0
    $released.Dispose()
}
finally {
    Remove-Module windows_workspace -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $temporaryRoot) {
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}

Write-Output "Windows environment lifecycle contracts passed: 11 cases"

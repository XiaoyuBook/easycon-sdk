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

$modulePath = Join-Path $PSScriptRoot "windows_workspace.psm1"
Import-Module -Name $modulePath -Force

$events = [System.Collections.Generic.List[string]]::new()
$setupAction = { $events.Add("setup") | Out-Null }.GetNewClosure()
$verifyAction = { $events.Add("verify") | Out-Null; return "verified" }.GetNewClosure()
$workspaceAction = {
    param($Summary)
    Assert-Contract ($Summary -ceq "verified") "Workspace must receive the verified summary"
    $events.Add("workspace") | Out-Null
}.GetNewClosure()

Invoke-EasyConEnvironmentLifecycle -Mode Setup -SetupAction $setupAction `
    -VerifyAction $verifyAction -WorkspaceAction $workspaceAction | Out-Null
Assert-Sequence -Actual $events.ToArray() -Expected @("setup", "verify") `
    -Description "Setup must provision once and finish by verifying the installed environment"

$events.Clear()
Invoke-EasyConEnvironmentLifecycle -Mode Setup -SetupAction $setupAction `
    -VerifyAction $verifyAction -WorkspaceAction $workspaceAction | Out-Null
Assert-Sequence -Actual $events.ToArray() -Expected @("setup", "verify") `
    -Description "repeated Setup must remain rerunnable and verify its result"

$events.Clear()
Invoke-EasyConEnvironmentLifecycle -Mode Verify -SetupAction $setupAction `
    -VerifyAction $verifyAction -WorkspaceAction $workspaceAction | Out-Null
Assert-Sequence -Actual $events.ToArray() -Expected @("verify") `
    -Description "Verify must never provision or run workspace gates"

$events.Clear()
Invoke-EasyConEnvironmentLifecycle -Mode Workspace -SetupAction $setupAction `
    -VerifyAction $verifyAction -WorkspaceAction $workspaceAction | Out-Null
Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "workspace") `
    -Description "Workspace must verify first and must never provision"

$events.Clear()
try {
    Invoke-EasyConEnvironmentLifecycle -Mode Workspace -SetupAction $setupAction `
        -VerifyAction { throw "synthetic environment mismatch" } `
        -WorkspaceAction $workspaceAction | Out-Null
    throw "contract failed: Workspace should reject an invalid prepared environment"
}
catch {
    Assert-Contract ($_.Exception.Message -match "synthetic environment mismatch") `
        "Workspace must preserve the environment verification failure"
}
Assert-Sequence -Actual $events.ToArray() -Expected @() `
    -Description "Workspace gates must not start after verification fails"

Remove-Module windows_workspace -ErrorAction SilentlyContinue
Write-Output "Windows environment lifecycle contracts passed: 5 cases"

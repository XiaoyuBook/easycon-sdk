[CmdletBinding()]
param(
    [ValidateSet("Setup", "Verify", "Workspace", "Targeted")]
    [string]$Mode = "Workspace",

    [string]$CacheRoot,

    [string]$VsWherePath,

    [string]$BaseSha = $env:BASE_SHA,

    [switch]$RequireCleanTree,

    [switch]$RequireStagedCandidate,

    [ValidateSet("check", "clippy", "test")]
    [string]$TargetedCargoCommand,

    [string[]]$TargetedCargoArguments = @()
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest

if (-not $IsWindows) {
    throw "run_windows_workspace.ps1 supports Windows only"
}

$targetedParametersProvided = (
    $PSBoundParameters.ContainsKey("TargetedCargoCommand") -or
    $PSBoundParameters.ContainsKey("TargetedCargoArguments")
)
if ($Mode -cne "Targeted" -and $targetedParametersProvided) {
    throw "Targeted Cargo parameters require -Mode Targeted"
}
if ($RequireCleanTree -and $RequireStagedCandidate) {
    throw "-RequireCleanTree and -RequireStagedCandidate are mutually exclusive"
}
if ($Mode -ceq "Targeted" -and ($RequireCleanTree -or $RequireStagedCandidate)) {
    throw "Targeted mode cannot use Workspace candidate requirements"
}
if ($Mode -cne "Workspace" -and $RequireStagedCandidate) {
    throw "-RequireStagedCandidate requires -Mode Workspace"
}

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$configurationPath = Join-Path $PSScriptRoot "windows_build_environment.json"
$modulePath = Join-Path $PSScriptRoot "windows_workspace.psm1"
Import-Module -Name $modulePath -Force

$commonParameters = @{
    RepositoryRoot = $repositoryRoot
    ConfigurationPath = $configurationPath
}
foreach ($entry in @(
    @("CacheRoot", $CacheRoot),
    @("VsWherePath", $VsWherePath)
)) {
    if (-not [string]::IsNullOrWhiteSpace([string]$entry[1])) {
        $commonParameters[$entry[0]] = $entry[1]
    }
}

switch ($Mode) {
    "Setup" {
        Invoke-EasyConWindowsSetup @commonParameters | Out-Null
    }
    "Verify" {
        Invoke-EasyConWindowsVerify @commonParameters | Out-Null
    }
    "Workspace" {
        Invoke-EasyConWindowsWorkspace @commonParameters -BaseSha $BaseSha `
            -RequireCleanTree:$RequireCleanTree `
            -RequireStagedCandidate:$RequireStagedCandidate | Out-Null
    }
    "Targeted" {
        Invoke-EasyConWindowsWorkspace @commonParameters -GateMode Targeted `
            -TargetedCargoCommand $TargetedCargoCommand `
            -TargetedCargoArguments $TargetedCargoArguments | Out-Null
    }
}

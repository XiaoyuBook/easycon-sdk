[CmdletBinding()]
param(
    [ValidateSet("Setup", "Verify", "Workspace")]
    [string]$Mode = "Workspace",

    [string]$CacheRoot,

    [string]$VsWherePath,

    [string]$BaseSha = $env:BASE_SHA,

    [switch]$RequireCleanTree
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest

if (-not $IsWindows) {
    throw "run_windows_workspace.ps1 supports Windows only"
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
            -RequireCleanTree:$RequireCleanTree | Out-Null
    }
}

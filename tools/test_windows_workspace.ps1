[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false
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

function Invoke-ContractCase {
    param(
        [Parameter(Mandatory)]
        [string]$Name,

        [Parameter(Mandatory)]
        [scriptblock]$Action
    )

    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    & $Action
    $timer.Stop()
    Write-Output ("CONTRACT_PASS name={0} durationMs={1}" -f $Name, $timer.ElapsedMilliseconds)
}

function Set-ContractFile {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$Value
    )

    $parent = Split-Path -Parent $Path
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    Set-Content -LiteralPath $Path -Value $Value -Encoding utf8NoBOM -NoNewline
}

function Set-ContractUtf8Text {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$Value
    )

    $parent = Split-Path -Parent $Path
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    [System.IO.File]::WriteAllText(
        $Path,
        $Value,
        [System.Text.UTF8Encoding]::new($false, $true)
    )
}

$modulePath = Join-Path $PSScriptRoot "windows_workspace.psm1"
$repository = Resolve-Path (Join-Path $PSScriptRoot "..")
$configurationPath = Join-Path $PSScriptRoot "windows_build_environment.json"
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
    "easycon environment contract {0}" -f [guid]::NewGuid().ToString("N")
)
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
Import-Module -Name $modulePath -Force
$workspaceModule = Get-Module windows_workspace

function Invoke-PrivateWorkspaceGates {
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [scriptblock]$GateInvoker,

        [switch]$RequireCleanTree
    )

    & $script:workspaceModule {
        param($Root, $Invoker, $RequireClean)
        Invoke-EasyConWindowsWorkspaceGates -RepositoryRoot $Root -GateInvoker $Invoker `
            -RequireCleanTree:$RequireClean
    } $RepositoryRoot $GateInvoker $RequireCleanTree.IsPresent
}

function Set-PrivateVerifiedProcessEnvironment {
    param(
        [Parameter(Mandatory)]
        [string[]]$PathDirectories,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary]$Variables
    )

    & $script:workspaceModule {
        param($Directories, $Values)
        Set-EasyConVerifiedProcessEnvironment -PathDirectories $Directories -Variables $Values
    } $PathDirectories $Variables
}

function Publish-PrivateDirectoryAtomically {
    param(
        [Parameter(Mandatory)]
        [hashtable]$Parameters
    )

    & $script:workspaceModule {
        param($Arguments)
        Publish-EasyConDirectoryAtomically @Arguments
    } $Parameters
}

function Remove-PrivateTransientBuildTree {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$TrustedRoot
    )

    & $script:workspaceModule {
        param($Tree, $Root)
        Remove-EasyConTransientBuildTree -Path $Tree -TrustedRoot $Root
    } $Path $TrustedRoot
}

function Invoke-PrivateCommandWithNativeCapture {
    param(
        [Parameter(Mandatory)]
        [string]$CommandName,

        [Parameter(Mandatory)]
        [hashtable]$Parameters,

        [Parameter(Mandatory)]
        [scriptblock]$NativeCapture
    )

    & $script:workspaceModule {
        param($Name, $Arguments, $Capture)
        $original = ${function:Invoke-EasyConNativeCapture}
        try {
            Set-Item -LiteralPath Function:script:Invoke-EasyConNativeCapture -Value $Capture
            & $Name @Arguments
        }
        finally {
            Set-Item -LiteralPath Function:script:Invoke-EasyConNativeCapture -Value $original
        }
    } $CommandName $Parameters $NativeCapture
}

function Invoke-PrivateCommand {
    param(
        [Parameter(Mandatory)]
        [string]$CommandName,

        [Parameter(Mandatory)]
        [hashtable]$Parameters
    )

    & $script:workspaceModule {
        param($Name, $Arguments)
        & $Name @Arguments
    } $CommandName $Parameters
}

function Get-PrivateWindowsBuildConfiguration {
    param([Parameter(Mandatory)][string]$Path)
    Invoke-PrivateCommand -CommandName "Get-EasyConWindowsBuildConfiguration" `
        -Parameters @{ Path = $Path }
}

function Get-PrivateEnvironmentFingerprint {
    param(
        [Parameter(Mandatory)][string]$RepositoryRoot,
        [Parameter(Mandatory)][object]$Configuration
    )
    Invoke-PrivateCommand -CommandName "Get-EasyConEnvironmentFingerprint" `
        -Parameters @{ RepositoryRoot = $RepositoryRoot; Configuration = $Configuration }
}

function Get-PrivateEnvironmentLocation {
    param(
        [Parameter(Mandatory)][string]$RepositoryRoot,
        [Parameter(Mandatory)][string]$Fingerprint,
        [Parameter(Mandatory)][object]$Configuration,
        [Parameter(Mandatory)][string]$CacheRoot
    )
    Invoke-PrivateCommand -CommandName "Get-EasyConEnvironmentLocation" -Parameters @{
        RepositoryRoot = $RepositoryRoot
        Fingerprint = $Fingerprint
        Configuration = $Configuration
        CacheRoot = $CacheRoot
    }
}

function New-ContractEnvironmentStamp {
    param(
        [Parameter(Mandatory)][object]$Location,
        [Parameter(Mandatory)][object]$Fingerprint,
        [Parameter(Mandatory)][object]$Configuration,
        [Parameter(Mandatory)][object[]]$Tools
    )

    $cmake = @($Configuration.vcpkg.internalTools | Where-Object { $_.name -ceq "cmake" })[0]
    $ninja = @($Configuration.vcpkg.internalTools | Where-Object { $_.name -ceq "ninja" })[0]
    $sevenZip = @($Configuration.vcpkg.internalTools | Where-Object { $_.name -ceq "7zip" })[0]
    return [ordered]@{
        schemaVersion = 2
        environmentSchema = [int]$Location.EnvironmentSchema
        hostTargetIdentity = [string]$Location.HostTargetIdentity
        fingerprint = [string]$Fingerprint.Value
        fingerprintInputs = @($Fingerprint.Inputs)
        createdUtc = "2026-01-01T00:00:00.0000000Z"
        environmentRoot = [string]$Location.EnvironmentRoot
        target = [string]$Configuration.target
        tools = $Tools
        versions = [ordered]@{
            rust = "1.97.1"
            cargo = "1.97.1"
            python = "3.12.0"
            cmake = [string]$cmake.version
            ninja = [string]$ninja.version
            sevenZip = [string]$sevenZip.version
            msvcTools = [string]$Configuration.hostTools.msvcToolsVersion
            windowsSdk = [string]$Configuration.hostTools.windowsSdkVersion
            vcpkgScripts = [string]$Configuration.vcpkg.scriptsCommit
            vcpkgTool = [string]$Configuration.vcpkg.toolRelease
        }
        paths = [ordered]@{
            cargoVendor = Join-Path $Location.EnvironmentRoot "cargo-vendor"
            vcpkgScriptsRoot = Join-Path $Location.EnvironmentRoot "vcpkg/scripts"
            vcpkgInstalled = Join-Path $Location.EnvironmentRoot "setup/vcpkg/installed"
            ocrModel = Join-Path $Location.EnvironmentRoot "vision-models/contract"
        }
        nativeTree = [ordered]@{ files = 1; sha256 = ("0" * 64) }
        cargoSources = [ordered]@{ files = 1; sha256 = ("0" * 64) }
    }
}

function Test-PrivateVcpkgVersionRecord {
    param(
        [Parameter(Mandatory)][object]$Record,
        [Parameter(Mandatory)][object]$Expected
    )
    Invoke-PrivateCommand -CommandName "Test-EasyConVcpkgVersionRecord" `
        -Parameters @{ Record = $Record; Expected = $Expected }
}

function Test-PrivateVcpkgToolManifestRecord {
    param(
        [Parameter(Mandatory)][object]$Record,
        [Parameter(Mandatory)][object]$Expected
    )
    Invoke-PrivateCommand -CommandName "Test-EasyConVcpkgToolManifestRecord" `
        -Parameters @{ Record = $Record; Expected = $Expected }
}

function Install-PrivatePinnedExecutable {
    param(
        [Parameter(Mandatory)][string]$Source,
        [Parameter(Mandatory)][string]$Destination,
        [Parameter(Mandatory)][string]$Sha512,
        [Parameter(Mandatory)][string]$TrustedRoot,
        [Parameter(Mandatory)][string]$Description
    )
    Invoke-PrivateCommand -CommandName "Install-EasyConPinnedExecutable" -Parameters @{
        Source = $Source
        Destination = $Destination
        Sha512 = $Sha512
        TrustedRoot = $TrustedRoot
        Description = $Description
    }
}

function Initialize-PrivateMsvcEnvironment {
    param(
        [Parameter(Mandatory)][string]$MsvcToolsVersion,
        [Parameter(Mandatory)][string]$WindowsSdkVersion
    )
    Invoke-PrivateCommand -CommandName "Initialize-EasyConMsvcEnvironment" -Parameters @{
        MsvcToolsVersion = $MsvcToolsVersion
        WindowsSdkVersion = $WindowsSdkVersion
    }
}

function Assert-PrivatePhysicalPath {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$TrustedRoot,
        [scriptblock]$ReparsePointClassifier
    )
    Invoke-PrivateCommand -CommandName "Assert-EasyConPhysicalPath" -Parameters @{
        Path = $Path
        TrustedRoot = $TrustedRoot
        ReparsePointClassifier = $ReparsePointClassifier
    }
}

function Invoke-PrivatePublicWrapperProbe {
    param(
        [Parameter(Mandatory)]
        [ValidateSet("Setup", "Verify", "Workspace")]
        [string]$Mode,

        [switch]$LifecycleFailure
    )

    & $script:workspaceModule {
        param($WrapperMode, $FailLifecycle)

        $originalContext = ${function:Get-EasyConWindowsEnvironmentContext}
        $originalSetupCore = ${function:Invoke-EasyConWindowsSetupCore}
        $originalVerifyCore = ${function:Invoke-EasyConWindowsVerifyCore}
        $originalWorkspaceGates = ${function:Invoke-EasyConWindowsWorkspaceGates}
        $originalLifecycle = ${function:Invoke-EasyConEnvironmentLifecycle}
        $location = [pscustomobject]@{
            CacheRoot = "resolved-cache"
            EnvironmentRoot = "resolved-environment"
            IdentityKey = "resolved-identity"
        }
        $context = [pscustomobject]@{
            Repository = "resolved-repository"
            ConfigurationPath = "resolved-configuration"
            Location = $location
        }
        $state = [pscustomobject]@{
            ContextCall = $null
            LifecycleCall = $null
            ResolvedContext = $context
            SetupCalls = [System.Collections.Generic.List[object]]::new()
            VerifyCalls = [System.Collections.Generic.List[object]]::new()
            WorkspaceCalls = [System.Collections.Generic.List[object]]::new()
            InputGateInvoker = { param($Name, $Program, $Arguments, $Root) }
        }

        $contextProbe = {
            param($RepositoryRoot, $ConfigurationPath, $CacheRoot)
            $state.ContextCall = [pscustomobject]@{
                RepositoryRoot = $RepositoryRoot
                ConfigurationPath = $ConfigurationPath
                CacheRoot = $CacheRoot
            }
            return $context
        }.GetNewClosure()
        $setupProbe = {
            param($RepositoryRoot, $ConfigurationPath, $CacheRoot, $VsWherePath, $Context)
            $state.SetupCalls.Add([pscustomobject]@{
                RepositoryRoot = $RepositoryRoot
                ConfigurationPath = $ConfigurationPath
                CacheRoot = $CacheRoot
                VsWherePath = $VsWherePath
                Context = $Context
            }) | Out-Null
        }.GetNewClosure()
        $verifyProbe = {
            param(
                $RepositoryRoot,
                $ConfigurationPath,
                $CacheRoot,
                $VsWherePath,
                $Context,
                [switch]$AllowProxy
            )
            $state.VerifyCalls.Add([pscustomobject]@{
                RepositoryRoot = $RepositoryRoot
                ConfigurationPath = $ConfigurationPath
                CacheRoot = $CacheRoot
                VsWherePath = $VsWherePath
                Context = $Context
                AllowProxy = $AllowProxy.IsPresent
            }) | Out-Null
            return [pscustomobject]@{ Status = "probe-ready" }
        }.GetNewClosure()
        $workspaceProbe = {
            param($RepositoryRoot, $BaseSha, [switch]$RequireCleanTree, $GateInvoker)
            $state.WorkspaceCalls.Add([pscustomobject]@{
                RepositoryRoot = $RepositoryRoot
                BaseSha = $BaseSha
                RequireCleanTree = $RequireCleanTree.IsPresent
                GateInvoker = $GateInvoker
            }) | Out-Null
        }.GetNewClosure()
        $lifecycleProbe = {
            param(
                $Mode,
                $Location,
                $SetupAction,
                $VerifyAction,
                $WorkspaceAction,
                $LeaseTimeoutMilliseconds
            )
            $state.LifecycleCall = [pscustomobject]@{
                Mode = $Mode
                Location = $Location
                LeaseTimeoutMilliseconds = $LeaseTimeoutMilliseconds
            }
            if ($FailLifecycle) {
                throw "synthetic public $Mode lifecycle failure"
            }
            switch ($Mode) {
                "Setup" {
                    & $SetupAction | Out-Null
                    return & $VerifyAction
                }
                "Verify" {
                    return & $VerifyAction
                }
                "Workspace" {
                    $summary = & $VerifyAction
                    & $WorkspaceAction $summary | Out-Null
                    return $summary
                }
            }
        }.GetNewClosure()

        try {
            Set-Item -LiteralPath Function:script:Get-EasyConWindowsEnvironmentContext `
                -Value $contextProbe
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsSetupCore `
                -Value $setupProbe
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsVerifyCore `
                -Value $verifyProbe
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsWorkspaceGates `
                -Value $workspaceProbe
            Set-Item -LiteralPath Function:script:Invoke-EasyConEnvironmentLifecycle `
                -Value $lifecycleProbe
            $parameters = @{
                RepositoryRoot = "input-repository"
                ConfigurationPath = "input-configuration"
                CacheRoot = "input-cache"
                VsWherePath = "input-vswhere"
                LeaseTimeoutMilliseconds = 321
            }
            switch ($WrapperMode) {
                "Setup" {
                    Invoke-EasyConWindowsSetup @parameters | Out-Null
                }
                "Verify" {
                    Invoke-EasyConWindowsVerify @parameters | Out-Null
                }
                "Workspace" {
                    Invoke-EasyConWindowsWorkspace @parameters -BaseSha ("a" * 40) `
                        -RequireCleanTree -GateInvoker $state.InputGateInvoker | Out-Null
                }
            }
            return $state
        }
        finally {
            Set-Item -LiteralPath Function:script:Get-EasyConWindowsEnvironmentContext `
                -Value $originalContext
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsSetupCore `
                -Value $originalSetupCore
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsVerifyCore `
                -Value $originalVerifyCore
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsWorkspaceGates `
                -Value $originalWorkspaceGates
            Set-Item -LiteralPath Function:script:Invoke-EasyConEnvironmentLifecycle `
                -Value $originalLifecycle
        }
    } $Mode $LifecycleFailure.IsPresent
}

function Invoke-LocationCleanupFailureProbe {
    param(
        [Parameter(Mandatory)]
        [ValidateSet("Native", "Gate")]
        [string]$Kind,

        [Parameter(Mandatory)]
        [int]$ExitCode,

        [Parameter(Mandatory)]
        [string]$Marker
    )

    $probeRoot = Join-Path $temporaryRoot (
        "{0} location cleanup {1}" -f $Kind.ToLowerInvariant(), [guid]::NewGuid().ToString("N")
    )
    $previous = Join-Path $probeRoot "deleted previous"
    $working = Join-Path $probeRoot "working"
    New-Item -ItemType Directory -Force -Path $previous, $working | Out-Null
    $childPath = Join-Path $probeRoot "delete-caller-location.ps1"
    Set-ContractFile -Path $childPath -Value @'
param(
    [Parameter(Mandatory)][string]$DeletePath,
    [Parameter(Mandatory)][string]$Marker,
    [Parameter(Mandatory)][int]$ExitCode
)
Remove-Item -LiteralPath $DeletePath -Recurse -Force -ErrorAction Stop
Write-Output $Marker
exit $ExitCode
'@
    $program = Join-Path $PSHOME "pwsh.exe"
    $arguments = @(
        "-NoLogo", "-NoProfile", "-File", $childPath,
        "-DeletePath", $previous, "-Marker", $Marker, "-ExitCode", [string]$ExitCode
    )
    $observed = [System.Collections.Generic.List[string]]::new()
    $failure = $null
    Set-Location -LiteralPath $previous
    try {
        try {
            if ($Kind -ceq "Native") {
                Invoke-PrivateCommand -CommandName "Invoke-EasyConNativeCapture" -Parameters @{
                    Program = $program
                    Arguments = $arguments
                    Description = "contract native nonzero"
                    WorkingDirectory = $working
                } | ForEach-Object { $observed.Add([string]$_) | Out-Null }
            }
            else {
                Invoke-PrivateCommand -CommandName "Invoke-EasyConGate" -Parameters @{
                    Name = "contract gate nonzero"
                    Program = $program
                    Arguments = $arguments
                    RepositoryRoot = $working
                } | ForEach-Object { $observed.Add([string]$_) | Out-Null }
            }
        }
        catch {
            $failure = $_
        }
    }
    finally {
        Set-Location -LiteralPath $repository.Path
    }
    return [pscustomobject]@{
        Failure = $failure
        Marker = $Marker
        Observed = @($observed)
    }
}

$contractFailure = $null
try {
    Invoke-ContractCase -Name "strict-environment-configuration" -Action {
        $configurationText = Get-Content -Raw -LiteralPath $configurationPath
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        Assert-Contract ($configuration.version -eq 4) "environment config schema must be v4"
        Assert-Contract ($configuration.vcpkg.internalTools.Count -eq 4) `
            "CMake, Ninja, 7-Zip, and its 7zr bootstrap must be audited"
        Assert-Contract ($configuration.vcpkg.nativeDependencies.Count -eq 3) `
            "direct native dependencies must be audited"
        Assert-Contract ($configuration.fingerprintInputs.path -ccontains "Cargo.lock") `
            "Cargo.lock must invalidate the prepared dependency environment"
        Assert-Contract ($configuration.fingerprintInputs.path -ccontains "tools/windows_workspace.psm1") `
            "environment implementation changes must invalidate the prepared environment"

        $reversedConfiguration = $configurationText | ConvertFrom-Json -Depth 32
        [array]::Reverse($reversedConfiguration.fingerprintInputs)
        $reversedFingerprintInputs = $reversedConfiguration | ConvertTo-Json -Depth 32

        $mutations = [ordered]@{
            "duplicate key" = $configurationText.Replace(
                '"version": 4', '"version": 4, "version": 4'
            )
            "old schema" = $configurationText.Replace('"version": 4', '"version": 3')
            "unfrozen target" = $configurationText.Replace(
                '"x86_64-pc-windows-msvc"', '"x86_64-unknown-linux-gnu"'
            )
            "missing fingerprint input" = $configurationText.Replace(
                '"path": "tools/provision_vision_test_model.py"',
                '"path": "../outside.py"'
            )
            "invalid fingerprint kind" = $configurationText.Replace(
                '"path": "Cargo.lock", "kind": "text"',
                '"path": "Cargo.lock", "kind": "auto"'
            )
            "reversed fingerprint inputs" = $reversedFingerprintInputs
            "Windows case alias duplicate" = $configurationText.Replace(
                '"path": "tools/provision_vision_test_model.py"',
                '"path": "TOOLS/WINDOWS_BUILD_ENVIRONMENT.JSON"'
            )
            "dot component fingerprint path" = $configurationText.Replace(
                '"path": "tools/provision_vision_test_model.py"',
                '"path": "tools/./provision_vision_test_model.py"'
            )
            "internal tool hash" = $configurationText.Replace(
                '55d3d891e8fc6c8ad7f92e172125319896761e57c5125944613d9bbfa5b9374387e9fc1468ad5bcb31464f43fb1c455ea251343942595f42955dc67090aa12ee',
                ('A' * 128)
            )
            "7zip executable hash" = $configurationText.Replace(
                '2bff20bd679d45166b8c2d039044a4ca16189e6d69ff9c82345b4c1306986ec4',
                ('A' * 64)
            )
            "native tree" = $configurationText.Replace(
                '0e28cd8713d7b810bef28ed0d7859dd761215854',
                ('A' * 40)
            )
        }
        foreach ($entry in $mutations.GetEnumerator()) {
            $path = Join-Path $temporaryRoot ("configuration-{0}.json" -f $entry.Key)
            Set-ContractFile -Path $path -Value $entry.Value
            try {
                Assert-Throws -Pattern "config|version|target|fingerprint|SHA|git tree|duplicate" `
                    -Action {
                        Get-PrivateWindowsBuildConfiguration -Path $path
                    }
            }
            catch {
                throw "configuration mutation '$($entry.Key)' was accepted: $($_.Exception.Message)"
            }
            $setupCache = Join-Path $temporaryRoot ("setup-cache-{0}" -f $entry.Key)
            Assert-Throws -Pattern "config|version|target|fingerprint|SHA|git tree|duplicate" `
                -Action {
                    Invoke-EasyConWindowsSetup -RepositoryRoot $repository `
                        -ConfigurationPath $path -CacheRoot $setupCache
                }
            Assert-Contract (-not (Test-Path -LiteralPath $setupCache)) `
                "configuration mutation '$($entry.Key)' must fail before Setup creates its cache root"
        }
    }

    Invoke-ContractCase -Name "python-version-comparison-uses-numeric-semantics" -Action {
        $minimum = [version]"3.8.0"
        $pythonPath = $null
        foreach ($accepted in @("3.12.10", "3.10.14", "3.8.0")) {
            $versionOutput = "Python $accepted"
            $capture = {
                param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
                $null = $Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput
                return $versionOutput
            }.GetNewClosure()
            $python = Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Get-EasyConPythonVersion" -Parameters @{} `
                -NativeCapture $capture
            Assert-Contract (-not ($python.Version -lt $minimum)) `
                "Python $accepted must not compare older than $minimum"
            Assert-Contract ($python.Version -is [version]) `
                "Python $accepted must remain System.Version until serialization"
            $pythonPath = $python.Path
        }

        $rejectedCases = @(
            [pscustomobject]@{ Name = "below minimum"; Output = @("Python 3.7.99") },
            [pscustomobject]@{ Name = "invalid format"; Output = @("Python not-a-version") },
            [pscustomobject]@{
                Name = "same-line trailing output"
                Output = @("Python 3.12.10 unexpected")
            },
            [pscustomobject]@{
                Name = "second trailing line"
                Output = @("Python 3.12.10", "unexpected trailing line")
            },
            [pscustomobject]@{ Name = "empty output"; Output = @() }
        )
        foreach ($rejectedCase in $rejectedCases) {
            $versionOutput = @($rejectedCase.Output)
            $capture = {
                param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
                $null = $Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput
                return $versionOutput
            }.GetNewClosure()
            try {
                Assert-Throws -Pattern "older than|required|unrecognized version" -Action {
                    Invoke-PrivateCommandWithNativeCapture `
                        -CommandName "Get-EasyConPythonVersion" -Parameters @{} `
                        -NativeCapture $capture
                }
            }
            catch {
                throw "Python $($rejectedCase.Name) was accepted or failed unclearly: $($_.Exception.Message)"
            }
        }

        $preparedOutput = @("Python 3.12.10")
        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput
            return $preparedOutput
        }.GetNewClosure()
        $prepared = Invoke-PrivateCommandWithNativeCapture `
            -CommandName "Assert-EasyConPreparedPythonVersion" -Parameters @{
                PythonPath = $pythonPath
                ExpectedVersion = "3.12.10"
                MinimumVersion = $minimum
            } -NativeCapture $capture
        Assert-Contract ($prepared -is [version] -and $prepared -eq [version]"3.12.10") `
            "prepared Python Verify path must retain strict System.Version semantics"

        foreach ($rejectedCase in $rejectedCases) {
            $preparedOutput = @($rejectedCase.Output)
            $capture = {
                param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
                $null = $Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput
                return $preparedOutput
            }.GetNewClosure()
            Assert-Throws -Pattern "older than|required|unrecognized version" -Action {
                Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Assert-EasyConPreparedPythonVersion" -Parameters @{
                        PythonPath = $pythonPath
                        ExpectedVersion = "3.12.10"
                        MinimumVersion = $minimum
                    } -NativeCapture $capture
            }
        }
    }

    Invoke-ContractCase -Name "vswhere-empty-output-has-actionable-diagnostic" -Action {
        $vswhere = Join-Path $temporaryRoot "vswhere contract/vswhere.exe"
        Set-ContractFile -Path $vswhere -Value "contract"
        foreach ($case in @(
            [pscustomobject]@{ Name = "empty"; Output = [string[]]@() },
            [pscustomobject]@{ Name = "whitespace"; Output = [string[]]@("", "   ", "`t") }
        )) {
            $discoveryOutput = [string[]]$case.Output
            $capture = {
                param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
                $null = $WorkingDirectory, $StreamOutput
                Assert-Contract ($Program -ceq $vswhere) `
                    "Visual Studio discovery must execute the selected vswhere path"
                Assert-Contract ($Description -ceq "Visual Studio 2022 discovery") `
                    "Visual Studio discovery must retain its native diagnostic description"
                Assert-Contract (
                    [Array]::IndexOf([object[]]$Arguments, "installationPath") -ge 0
                ) "vswhere must request the installationPath property"
                return $discoveryOutput
            }.GetNewClosure()
            try {
                Assert-Throws `
                    -Pattern "Visual Studio 2022 with the x64 C\+\+ toolchain was not found" `
                    -Action {
                        Invoke-PrivateCommandWithNativeCapture `
                            -CommandName "Find-EasyConVisualStudio" `
                            -Parameters @{ VsWherePath = $vswhere } `
                            -NativeCapture $capture
                    }
            }
            catch {
                throw "vswhere $($case.Name) output lost its actionable diagnostic: $($_.Exception.Message)"
            }
        }

        $visualStudio = Join-Path $temporaryRoot "Visual Studio contract"
        Set-ContractFile -Path (Join-Path $visualStudio `
            "Common7/Tools/Microsoft.VisualStudio.DevShell.dll") -Value "contract"
        $discoveryOutput = [string[]]@("", "   ", $visualStudio, "ignored-second-match")
        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput
            return $discoveryOutput
        }.GetNewClosure()
        $selected = Invoke-PrivateCommandWithNativeCapture `
            -CommandName "Find-EasyConVisualStudio" `
            -Parameters @{ VsWherePath = $vswhere } -NativeCapture $capture
        Assert-Contract ($selected -ceq $visualStudio) `
            "Visual Studio discovery must keep the first valid nonblank match"

        $malformedInstallation = Join-Path $temporaryRoot "malformed Visual Studio output"
        New-Item -ItemType Directory -Force -Path $malformedInstallation | Out-Null
        $discoveryOutput = [string[]]@($malformedInstallation)
        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput
            return $discoveryOutput
        }.GetNewClosure()
        Assert-Throws -Pattern "Developer Shell module is missing" -Action {
            Invoke-PrivateCommandWithNativeCapture -CommandName "Find-EasyConVisualStudio" `
                -Parameters @{ VsWherePath = $vswhere } -NativeCapture $capture
        }

        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Arguments, $WorkingDirectory, $StreamOutput
            throw "$Description failed with exit code 23"
        }
        Assert-Throws -Pattern "Visual Studio 2022 discovery failed with exit code 23" -Action {
            Invoke-PrivateCommandWithNativeCapture -CommandName "Find-EasyConVisualStudio" `
                -Parameters @{ VsWherePath = $vswhere } -NativeCapture $capture
        }
    }

    Invoke-ContractCase -Name "public-module-surface-hides-lifecycle-bypasses" -Action {
        foreach ($name in @(
            "Enter-EasyConEnvironmentLease",
            "Invoke-EasyConEnvironmentLifecycle",
            "Invoke-EasyConWindowsWorkspaceGates",
            "Publish-EasyConDirectoryAtomically",
            "Set-EasyConVerifiedProcessEnvironment"
        )) {
            Assert-Contract ($null -eq (Get-Command -Name $name -Module windows_workspace `
                -ErrorAction SilentlyContinue)) `
                "private lifecycle helper must not be exported: $name"
        }
        Assert-Contract ($null -ne (Get-Command -Name "Invoke-EasyConWindowsWorkspace" `
            -Module windows_workspace -ErrorAction SilentlyContinue)) `
            "the verified public Workspace entry must remain exported"
        $publicCommands = @(Get-Command -Module windows_workspace |
            Select-Object -ExpandProperty Name | Sort-Object)
        $expectedCommands = @(
            "Invoke-EasyConWindowsSetup",
            "Invoke-EasyConWindowsVerify",
            "Invoke-EasyConWindowsWorkspace"
        )
        Assert-Contract (
            ($publicCommands -join "`n") -ceq
            ($expectedCommands -join "`n")
        ) "exported function set must contain exactly Setup, Verify, and Workspace; actual=$($publicCommands -join ',')"

        $started = [pscustomobject]@{ Gates = 0 }
        Assert-Throws -Pattern "not prepared.*Mode Setup" -Action {
            Invoke-EasyConWindowsWorkspace -RepositoryRoot $repository `
                -ConfigurationPath $configurationPath `
                -CacheRoot (Join-Path $temporaryRoot "public workspace bypass cache") `
                -GateInvoker {
                    param($Name, $Program, $Arguments, $Root)
                    $started.Gates++
                }.GetNewClosure()
        }
        Assert-Contract ($started.Gates -eq 0) `
            "public Workspace must start zero gates before Verify succeeds"
    }

    Invoke-ContractCase -Name "public-wrappers-forward-controlled-lifecycle" -Action {
        foreach ($mode in @("Setup", "Verify", "Workspace")) {
            $state = Invoke-PrivatePublicWrapperProbe -Mode $mode
            Assert-Contract (
                $state.ContextCall.RepositoryRoot -ceq "input-repository" -and
                $state.ContextCall.ConfigurationPath -ceq "input-configuration" -and
                $state.ContextCall.CacheRoot -ceq "input-cache"
            ) "$mode must forward caller inputs to context resolution"
            Assert-Contract (
                $state.LifecycleCall.Mode -ceq $mode -and
                $state.LifecycleCall.Location.IdentityKey -ceq "resolved-identity" -and
                $state.LifecycleCall.LeaseTimeoutMilliseconds -eq 321
            ) "$mode must forward the resolved identity and lease timeout"
            Assert-Contract ($state.VerifyCalls.Count -eq 1) `
                "$mode must execute exactly one controlled Verify action"
            $verify = $state.VerifyCalls[0]
            Assert-Contract (
                $verify.RepositoryRoot -ceq "resolved-repository" -and
                $verify.ConfigurationPath -ceq "resolved-configuration" -and
                $verify.CacheRoot -ceq "resolved-cache" -and
                $verify.VsWherePath -ceq "input-vswhere" -and
                [object]::ReferenceEquals($verify.Context, $state.ResolvedContext)
            ) "$mode must pass only resolved context values to its Verify core"
            Assert-Contract ($verify.AllowProxy -eq ($mode -ceq "Setup")) `
                "only Setup final verification may retain the online proxy environment"

            if ($mode -ceq "Setup") {
                Assert-Contract ($state.SetupCalls.Count -eq 1) `
                    "Setup must execute exactly one controlled Setup action"
                $setup = $state.SetupCalls[0]
                Assert-Contract (
                    $setup.RepositoryRoot -ceq "resolved-repository" -and
                    $setup.ConfigurationPath -ceq "resolved-configuration" -and
                    $setup.CacheRoot -ceq "resolved-cache" -and
                    $setup.VsWherePath -ceq "input-vswhere" -and
                    [object]::ReferenceEquals($setup.Context, $state.ResolvedContext)
                ) "Setup must pass only resolved context values to its Setup core"
            }
            else {
                Assert-Contract ($state.SetupCalls.Count -eq 0) `
                    "$mode must not execute the Setup core"
            }
            if ($mode -ceq "Workspace") {
                Assert-Contract ($state.WorkspaceCalls.Count -eq 1) `
                    "Workspace must execute exactly one gate action after Verify"
                $workspace = $state.WorkspaceCalls[0]
                Assert-Contract (
                    $workspace.RepositoryRoot -ceq "resolved-repository" -and
                    $workspace.BaseSha -ceq ("a" * 40) -and
                    $workspace.RequireCleanTree -and
                    [object]::ReferenceEquals($workspace.GateInvoker, $state.InputGateInvoker)
                ) "Workspace must forward base, clean-tree, and gate invoker parameters"
            }
            else {
                Assert-Contract ($state.WorkspaceCalls.Count -eq 0) `
                    "$mode must not execute Workspace gates"
            }

            Assert-Throws -Pattern "synthetic public $mode lifecycle failure" -Action {
                Invoke-PrivatePublicWrapperProbe -Mode $mode -LifecycleFailure
            }
        }
    }

    Invoke-ContractCase -Name "nonzero-exit-preserves-primary-during-location-cleanup" -Action {
        $native = Invoke-LocationCleanupFailureProbe -Kind Native -ExitCode 42 `
            -Marker "native-output-42"
        $gate = Invoke-LocationCleanupFailureProbe -Kind Gate -ExitCode 43 `
            -Marker "gate-output-43"

        Assert-Contract ($null -ne $native.Failure) `
            "native nonzero plus location cleanup failure must fail"
        Assert-Contract ($null -ne $gate.Failure) `
            "gate nonzero plus location cleanup failure must fail"
        $nativePrimary = (
            $native.Failure.Exception.Message -match "contract native nonzero" -and
            $native.Failure.Exception.Message -match "exit code 42" -and
            $native.Failure.Exception.Message -match "native-output-42"
        )
        $gatePrimary = (
            $gate.Failure.Exception.Message -match "contract gate nonzero" -and
            $gate.Failure.Exception.Message -match "exit code 43"
        )
        Assert-Contract ($nativePrimary -and $gatePrimary) `
            "nonzero exit must remain primary; native='$($native.Failure.Exception.Message)' gate='$($gate.Failure.Exception.Message)'"
        Assert-Contract (
            $native.Failure.Exception.Data.Contains("EasyConLocationCleanupFailure")
        ) "native location restore failure must be attached as cleanup diagnostic"

        Assert-Contract ($gate.Observed -ccontains "gate-output-43") `
            "gate output emitted before failure must remain observable"
        Assert-Contract (
            $gate.Failure.Exception.Data.Contains("EasyConGateLocationCleanupFailure")
        ) "gate location restore failure must be attached as cleanup diagnostic"

        $nativeCleanup = Invoke-LocationCleanupFailureProbe -Kind Native -ExitCode 0 `
            -Marker "native-output-success"
        $gateCleanup = Invoke-LocationCleanupFailureProbe -Kind Gate -ExitCode 0 `
            -Marker "gate-output-success"
        foreach ($cleanup in @($nativeCleanup, $gateCleanup)) {
            Assert-Contract ($null -ne $cleanup.Failure) `
                "location restore failure without an earlier primary must still fail"
            Assert-Contract (
                $cleanup.Failure.Exception.Message -match "Cannot find path" -and
                $cleanup.Failure.Exception.Message -notmatch "exit code"
            ) "location restore failure must remain primary after a successful child exit"
        }
        Assert-Contract (
            (Get-Location).Path -ceq $repository.Path
        ) "cwd failure probes must return the contract process to its safe repository location"
    }

    Invoke-ContractCase -Name "vcpkg-version-record-shapes" -Action {
        $expected = [pscustomobject]@{
            version = "1.2.3"
            portVersion = 4
            gitTree = ("a" * 40)
        }
        foreach ($field in @("version", "version-semver", "version-string")) {
            $record = [pscustomobject]@{
                "port-version" = 4
                "git-tree" = ("a" * 40)
            }
            $record | Add-Member -NotePropertyName $field -NotePropertyValue "1.2.3"
            Assert-Contract (Test-PrivateVcpkgVersionRecord -Record $record -Expected $expected) `
                "vcpkg audit must accept the $field record shape without reading absent fields"
        }
        $incomplete = [pscustomobject]@{ version = "1.2.3" }
        Assert-Contract (-not (Test-PrivateVcpkgVersionRecord `
            -Record $incomplete -Expected $expected)) `
            "vcpkg audit must reject a record without port-version or git-tree"
    }

    Invoke-ContractCase -Name "vcpkg-tool-manifest-record-shapes" -Action {
        $sevenZr = [pscustomobject]@{ name = "7zr"; os = "windows" }
        Assert-Contract (Test-PrivateVcpkgToolManifestRecord `
            -Record $sevenZr -Expected ([pscustomobject]@{ name = "7zr" })) `
            "7zr must be audited without reading an absent architecture"
        Assert-Contract (-not (Test-PrivateVcpkgToolManifestRecord `
            -Record $sevenZr -Expected ([pscustomobject]@{ name = "cmake" }))) `
            "an architecture-less record must not satisfy a normal x64 tool pin"
        $cmake = [pscustomobject]@{ name = "cmake"; os = "windows"; arch = "x64" }
        Assert-Contract (Test-PrivateVcpkgToolManifestRecord `
            -Record $cmake -Expected ([pscustomobject]@{ name = "cmake" })) `
            "ordinary Windows tools must retain an x64 architecture pin"
    }

    Invoke-ContractCase -Name "vcpkg-sevenzip-materialization-ignores-host-path" -Action {
        $caseRoot = Join-Path $temporaryRoot "controlled sevenzip"
        $downloads = Join-Path $caseRoot "downloads"
        $hostedDirectory = Join-Path $caseRoot "hosted system tools"
        $incompatibleDirectory = Join-Path $caseRoot "incompatible system tools"
        $vcpkgRoot = Join-Path $caseRoot "vcpkg"
        New-Item -ItemType Directory -Force `
            -Path $downloads, $hostedDirectory, $incompatibleDirectory, $vcpkgRoot | Out-Null
        Set-ContractFile -Path (Join-Path $hostedDirectory "7z.exe") `
            -Value "hosted compatible system sevenzip"
        Set-ContractFile -Path (Join-Path $incompatibleDirectory "7z.exe") `
            -Value "hosted incompatible system sevenzip"
        $vcpkgExecutable = Join-Path $vcpkgRoot "vcpkg.exe"
        Set-ContractFile -Path $vcpkgExecutable -Value "contract vcpkg"

        $archivePayload = "pinned sevenzip installer"
        $sevenZrPayload = "pinned sevenzr bootstrap"
        $controlledPayload = "controlled extracted sevenzip"
        $sevenZipPin = [pscustomobject]@{
            name = "7zip"
            version = "26.01"
            url = "https://example.invalid/7zip.exe"
            archive = "7z2601-x64.7z.exe"
            executable = "7z.exe"
            sha512 = $null
            executableSha256 = $null
        }
        $sevenZrPin = [pscustomobject]@{
            name = "7zr"
            version = "26.01"
            url = "https://example.invalid/7zr.exe"
            archive = "contract-7zr.exe"
            executable = "7zr.exe"
            sha512 = $null
        }
        $sevenZipArchive = Join-Path $downloads $sevenZipPin.archive
        $sevenZrDownload = Join-Path $downloads $sevenZrPin.archive
        Set-ContractFile -Path $sevenZipArchive -Value $archivePayload
        Set-ContractFile -Path $sevenZrDownload -Value $sevenZrPayload
        $sevenZipPin.sha512 = (Get-FileHash -LiteralPath $sevenZipArchive -Algorithm SHA512).
            Hash.ToLowerInvariant()
        $sevenZrPin.sha512 = (Get-FileHash -LiteralPath $sevenZrDownload -Algorithm SHA512).
            Hash.ToLowerInvariant()
        $sevenZipPin.executableSha256 = [Convert]::ToHexString(
            [System.Security.Cryptography.SHA256]::HashData(
                [System.Text.Encoding]::UTF8.GetBytes($controlledPayload)
            )
        ).ToLowerInvariant()
        $expectedPath = Join-Path $downloads "tools/7zip-26.01-windows/7z.exe"
        $isolatedPath = Join-Path $caseRoot "tools/vcpkg-fetch-path"
        $vcpkg = [pscustomobject]@{ Root = $vcpkgRoot; Executable = $vcpkgExecutable }
        $state = [pscustomobject]@{ Mode = "hosted"; Fetches = 0; VersionChecks = 0 }
        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Arguments, $WorkingDirectory, $StreamOutput
            if ($Description -ceq "prepare audited vcpkg 7-Zip tool") {
                $state.Fetches++
                Assert-Contract ($env:PATH -ceq $isolatedPath) `
                    "vcpkg fetch must use only the controlled empty PATH"
                Assert-Contract ($env:VCPKG_FORCE_DOWNLOADED_BINARIES -ceq "1") `
                    "vcpkg fetch must reject all host internal-tool discoveries"
                $pathEntries = @($env:PATH -split [System.IO.Path]::PathSeparator)
                if (
                    $state.Mode -ceq "external-output" -or
                    ($state.Mode -ceq "hosted" -and @($pathEntries | Where-Object {
                        $_ -cin @($hostedDirectory, $incompatibleDirectory)
                    }).Count -ne 0)
                ) {
                    return "C:\ProgramData\Chocolatey\bin\7z.exe"
                }
                if ($state.Mode -notin @("missing", "tampered")) {
                    Set-ContractFile -Path $expectedPath -Value $controlledPayload
                }
                return @(
                    "A suitable version of 7zip was not found (required v26.1.0).",
                    "Extracting 7zip...",
                    $expectedPath
                )
            }
            if ($Description -ceq "verify controlled 7-Zip version") {
                $state.VersionChecks++
                if ($state.Mode -ceq "wrong-version") {
                    return @("", "7-Zip 25.00 (x64) : contract", "")
                }
                return @("", "7-Zip 26.01 (x64) : contract", "")
            }
            throw "unexpected native capture: $Description"
        }.GetNewClosure()
        $parameters = @{
            Vcpkg = $vcpkg
            DownloadsRoot = $downloads
            CacheRoot = $caseRoot
            SevenZipPin = $sevenZipPin
            SevenZrPin = $sevenZrPin
        }

        $originalPath = $env:PATH
        $originalForceDownloaded = [Environment]::GetEnvironmentVariable(
            "VCPKG_FORCE_DOWNLOADED_BINARIES", "Process"
        )
        try {
            $env:VCPKG_FORCE_DOWNLOADED_BINARIES = "host-poison"
            foreach ($hostDirectory in @($hostedDirectory, $incompatibleDirectory)) {
                $env:PATH = "$hostDirectory$([System.IO.Path]::PathSeparator)$originalPath"
                $prepared = Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Install-EasyConControlledSevenZip" -Parameters $parameters `
                    -NativeCapture $capture
                Assert-Contract ($env:PATH.StartsWith(
                    $hostDirectory, [System.StringComparison]::Ordinal
                )) "controlled fetch must restore the caller PATH"
                Assert-Contract ($env:VCPKG_FORCE_DOWNLOADED_BINARIES -ceq "host-poison") `
                    "controlled fetch must restore the caller vcpkg discovery setting"
                Assert-Contract ($prepared.SevenZipPath -ceq $expectedPath) `
                    "compatible or incompatible host 7z must not change the controlled path"
            }
        }
        finally {
            $env:PATH = $originalPath
            [Environment]::SetEnvironmentVariable(
                "VCPKG_FORCE_DOWNLOADED_BINARIES", $originalForceDownloaded, "Process"
            )
        }
        Assert-Contract (
            (Get-FileHash -LiteralPath $prepared.SevenZipPath -Algorithm SHA256).
                Hash.ToLowerInvariant() -ceq $sevenZipPin.executableSha256
        ) "controlled 7-Zip must match its pinned executable hash"
        Assert-Contract ($state.VersionChecks -eq 2) `
            "each controlled 7-Zip materialization must receive one exact version check"

        $state.Mode = "no-system"
        $env:PATH = $PSHOME
        try {
            $preparedWithoutSystem = Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Install-EasyConControlledSevenZip" -Parameters $parameters `
                -NativeCapture $capture
        }
        finally {
            $env:PATH = $originalPath
        }
        Assert-Contract ($preparedWithoutSystem.SevenZipPath -ceq $expectedPath) `
            "absence of a system 7z must retain the same controlled path"

        $state.Mode = "external-output"
        Assert-Throws -Pattern "fetch output|controlled.*path|trusted root" -Action {
            Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Install-EasyConControlledSevenZip" -Parameters $parameters `
                -NativeCapture $capture
        }

        $state.Mode = "missing"
        Remove-Item -LiteralPath $expectedPath -Force
        Assert-Throws -Pattern "missing|7-Zip" -Action {
            Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Install-EasyConControlledSevenZip" -Parameters $parameters `
                -NativeCapture $capture
        }

        $state.Mode = "tampered"
        Set-ContractFile -Path $expectedPath -Value "tampered extracted sevenzip"
        Assert-Throws -Pattern "SHA-256|7-Zip" -Action {
            Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Install-EasyConControlledSevenZip" -Parameters $parameters `
                -NativeCapture $capture
        }

        $state.Mode = "wrong-version"
        Set-ContractFile -Path $expectedPath -Value $controlledPayload
        Assert-Throws -Pattern "version|7-Zip" -Action {
            Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Install-EasyConControlledSevenZip" -Parameters $parameters `
                -NativeCapture $capture
        }
    }

    Invoke-ContractCase -Name "shared-content-cache-revalidates-and-recovers" -Action {
        $sharedRoot = Join-Path $temporaryRoot "shared content cache"
        $payload = "verified shared payload"
        $payloadBytes = [System.Text.Encoding]::UTF8.GetBytes($payload)
        $expectedHash = [Convert]::ToHexString(
            [System.Security.Cryptography.SHA256]::HashData($payloadBytes)
        ).ToLowerInvariant()
        $state = [pscustomobject]@{ Downloads = 0 }
        $download = {
            param($Url, $Destination)
            $null = $Url
            $state.Downloads++
            [System.IO.File]::WriteAllBytes($Destination, $payloadBytes)
        }.GetNewClosure()
        $parameters = @{
            SharedCacheRoot = $sharedRoot
            Url = "https://example.invalid/verified.bin"
            Algorithm = "SHA256"
            Hash = $expectedHash
            Bytes = [long]$payloadBytes.Length
            Description = "contract shared asset"
            DownloadAction = $download
        }

        $first = Invoke-PrivateCommand -CommandName "Get-EasyConSharedContentAsset" `
            -Parameters $parameters
        Assert-Contract ($state.Downloads -eq 1) "a missing shared asset must download once"
        Assert-Contract (
            (Get-FileHash -LiteralPath $first -Algorithm SHA256).Hash.ToLowerInvariant() -ceq
                $expectedHash
        ) "the first shared asset publication must match its content hash"

        $blockedNetwork = {
            param($Url, $Destination)
            $null = $Url, $Destination
            throw "network seam must not run on a complete cache hit"
        }
        $hitParameters = $parameters.Clone()
        $hitParameters.DownloadAction = $blockedNetwork
        $second = Invoke-PrivateCommand -CommandName "Get-EasyConSharedContentAsset" `
            -Parameters $hitParameters
        Assert-Contract ($second -ceq $first) `
            "a verified shared cache hit must retain the content-addressed path"
        Assert-Contract ($state.Downloads -eq 1) `
            "a complete shared cache hit must not start a download"

        [System.IO.File]::WriteAllText($first, "damaged cache entry")
        $repaired = Invoke-PrivateCommand -CommandName "Get-EasyConSharedContentAsset" `
            -Parameters $parameters
        Assert-Contract ($state.Downloads -eq 2) `
            "a damaged shared cache entry must be isolated and fetched once"
        Assert-Contract (
            (Get-FileHash -LiteralPath $repaired -Algorithm SHA256).Hash.ToLowerInvariant() -ceq
                $expectedHash
        ) "a repaired shared cache entry must be revalidated"
        $quarantine = Join-Path $sharedRoot "assets-v1/quarantine"
        Assert-Contract (
            @(Get-ChildItem -LiteralPath $quarantine -File -ErrorAction Stop).Count -eq 1
        ) "a damaged shared cache entry must be isolated exactly once"

        $wrongPayload = [System.Text.Encoding]::UTF8.GetBytes("wrong payload")
        $wrongHash = "0" * 64
        $wrongParameters = $parameters.Clone()
        $wrongParameters.Hash = $wrongHash
        $wrongParameters.Bytes = [long]$wrongPayload.Length
        $wrongParameters.DownloadAction = {
            param($Url, $Destination)
            $null = $Url
            [System.IO.File]::WriteAllBytes($Destination, $wrongPayload)
        }.GetNewClosure()
        Assert-Throws -Pattern "SHA-256|hash|content" -Action {
            Invoke-PrivateCommand -CommandName "Get-EasyConSharedContentAsset" `
                -Parameters $wrongParameters
        }
        $wrongPath = Join-Path $sharedRoot "assets-v1/blobs/sha256/$wrongHash"
        Assert-Contract (-not (Test-Path -LiteralPath $wrongPath)) `
            "a wrong-hash download must never publish a cache entry"
    }

    Invoke-ContractCase -Name "default-shared-download-retains-module-scope" -Action {
        $sharedRoot = Join-Path $temporaryRoot "default shared download"
        $payloadBytes = [System.Text.Encoding]::UTF8.GetBytes("default download payload")
        $expectedHash = [Convert]::ToHexString(
            [System.Security.Cryptography.SHA256]::HashData($payloadBytes)
        ).ToLowerInvariant()
        $state = [pscustomobject]@{ Downloads = 0 }
        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $WorkingDirectory, $StreamOutput
            Assert-Contract ($Description -ceq "download contract default asset") `
                "the default shared download must retain its module description"
            $continueIndex = [Array]::IndexOf([object[]]$Arguments, "--continue-at")
            Assert-Contract (
                $continueIndex -ge 0 -and
                $continueIndex + 1 -lt $Arguments.Count -and
                [string]$Arguments[$continueIndex + 1] -ceq "-"
            ) "the default shared download must resume its unique verified temporary"
            $speedIndex = [Array]::IndexOf([object[]]$Arguments, "--speed-limit")
            Assert-Contract (
                $speedIndex -ge 0 -and
                $speedIndex + 3 -lt $Arguments.Count -and
                [string]$Arguments[$speedIndex + 1] -ceq "1024" -and
                [string]$Arguments[$speedIndex + 2] -ceq "--speed-time" -and
                [string]$Arguments[$speedIndex + 3] -ceq "60"
            ) "the default shared download must retry a stalled transfer promptly"
            $retryIndex = [Array]::IndexOf([object[]]$Arguments, "--retry")
            $retryDelayIndex = [Array]::IndexOf([object[]]$Arguments, "--retry-delay")
            $maximumIndex = [Array]::IndexOf([object[]]$Arguments, "--max-time")
            Assert-Contract (
                $retryIndex -ge 0 -and
                [string]$Arguments[$retryIndex + 1] -ceq "15" -and
                $retryDelayIndex -ge 0 -and
                [string]$Arguments[$retryDelayIndex + 1] -ceq "1" -and
                $maximumIndex -ge 0 -and
                [string]$Arguments[$maximumIndex + 1] -ceq "120"
            ) "the default shared download must bound each resumable transfer attempt"
            $outputIndex = [Array]::IndexOf([object[]]$Arguments, "--output")
            Assert-Contract ($outputIndex -ge 0 -and $outputIndex + 1 -lt $Arguments.Count) `
                "the default shared download must pass a controlled output path"
            $state.Downloads++
            [System.IO.File]::WriteAllBytes(
                [string]$Arguments[$outputIndex + 1], $payloadBytes
            )
            return @()
        }.GetNewClosure()
        $asset = Invoke-PrivateCommandWithNativeCapture `
            -CommandName "Get-EasyConSharedContentAsset" -Parameters @{
                SharedCacheRoot = $sharedRoot
                Url = "https://example.invalid/default.bin"
                Algorithm = "SHA256"
                Hash = $expectedHash
                Bytes = [long]$payloadBytes.Length
                Description = "contract default asset"
            } -NativeCapture $capture
        Assert-Contract ($state.Downloads -eq 1) `
            "the default shared download path must execute exactly once"
        Assert-Contract (
            (Get-FileHash -LiteralPath $asset -Algorithm SHA256).Hash.ToLowerInvariant() -ceq
                $expectedHash
        ) "the default shared download must publish only verified content"
    }

    Invoke-ContractCase -Name "controlled-tools-preseed-shared-vcpkg-downloads" -Action {
        $sharedRoot = Join-Path $temporaryRoot "controlled tool shared cache"
        $fixtureRoot = Join-Path $temporaryRoot "controlled tool archives"
        $cmakeSource = Join-Path $fixtureRoot "cmake source"
        $ninjaSource = Join-Path $fixtureRoot "ninja source"
        $cmakeExecutable = Join-Path $cmakeSource "cmake-contract/bin/cmake.exe"
        $ninjaExecutable = Join-Path $ninjaSource "ninja.exe"
        Set-ContractFile -Path $cmakeExecutable -Value "contract cmake"
        Set-ContractFile -Path $ninjaExecutable -Value "contract ninja"
        $cmakeArchive = Join-Path $fixtureRoot "cmake-contract.zip"
        $ninjaArchive = Join-Path $fixtureRoot "ninja-contract.zip"
        Compress-Archive -LiteralPath (Join-Path $cmakeSource "cmake-contract") `
            -DestinationPath $cmakeArchive
        Compress-Archive -LiteralPath $ninjaExecutable -DestinationPath $ninjaArchive
        $cmakeHash = (Get-FileHash -LiteralPath $cmakeArchive -Algorithm SHA512).Hash.ToLowerInvariant()
        $ninjaHash = (Get-FileHash -LiteralPath $ninjaArchive -Algorithm SHA512).Hash.ToLowerInvariant()
        foreach ($fixture in @(
            [pscustomobject]@{ Path = $cmakeArchive; Hash = $cmakeHash },
            [pscustomobject]@{ Path = $ninjaArchive; Hash = $ninjaHash }
        )) {
            $blob = Join-Path $sharedRoot (Join-Path "assets-v1/blobs/sha512" $fixture.Hash)
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $blob) | Out-Null
            Copy-Item -LiteralPath $fixture.Path -Destination $blob
        }
        $commit = "d" * 40
        $configuration = [pscustomobject]@{
            vcpkg = [pscustomobject]@{
                scriptsCommit = $commit
                internalTools = @(
                    [pscustomobject]@{
                        name = "cmake"
                        version = "contract"
                        url = "https://example.invalid/cmake-contract.zip"
                        archive = "cmake-contract.zip"
                        executable = "cmake-contract/bin/cmake.exe"
                        sha512 = $cmakeHash
                    },
                    [pscustomobject]@{
                        name = "ninja"
                        version = "contract"
                        url = "https://example.invalid/ninja-contract.zip"
                        archive = "ninja-contract.zip"
                        executable = "ninja.exe"
                        sha512 = $ninjaHash
                    }
                )
            }
        }
        $blockedNetwork = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Arguments, $WorkingDirectory, $StreamOutput
            throw "network seam must not run: $Description"
        }
        $downloadRoot = Join-Path $sharedRoot (Join-Path "vcpkg-downloads" $commit)
        $parameters = @{
            Configuration = $configuration
            SharedCacheRoot = $sharedRoot
        }

        foreach ($identity in @("first identity", "second identity")) {
            $environment = Join-Path $temporaryRoot $identity
            New-Item -ItemType Directory -Force -Path $environment | Out-Null
            $parameters.EnvironmentRoot = $environment
            $tools = Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Install-EasyConControlledBuildTools" `
                -Parameters $parameters -NativeCapture $blockedNetwork
            Assert-Contract (
                (Test-Path -LiteralPath $tools.cmake -PathType Leaf) -and
                (Test-Path -LiteralPath $tools.ninja -PathType Leaf)
            ) "each environment must materialize controlled tools from shared archives"
        }

        $sharedCmakeArchive = Join-Path $downloadRoot "cmake-contract.zip"
        $sharedNinjaArchive = Join-Path $downloadRoot "ninja-contract.zip"
        Assert-Contract (
            (Get-FileHash -LiteralPath $sharedCmakeArchive -Algorithm SHA512).Hash.ToLowerInvariant() -ceq
                $cmakeHash -and
            (Get-FileHash -LiteralPath $sharedNinjaArchive -Algorithm SHA512).Hash.ToLowerInvariant() -ceq
                $ninjaHash
        ) "controlled archives must preseed the commit-scoped vcpkg download cache"

        Set-ContractFile -Path $sharedCmakeArchive -Value "damaged"
        $repairEnvironment = Join-Path $temporaryRoot "repair identity"
        New-Item -ItemType Directory -Force -Path $repairEnvironment | Out-Null
        $parameters.EnvironmentRoot = $repairEnvironment
        Invoke-PrivateCommandWithNativeCapture `
            -CommandName "Install-EasyConControlledBuildTools" `
            -Parameters $parameters -NativeCapture $blockedNetwork | Out-Null
        Assert-Contract (
            (Get-FileHash -LiteralPath $sharedCmakeArchive -Algorithm SHA512).Hash.ToLowerInvariant() -ceq
                $cmakeHash
        ) "a damaged vcpkg tool archive must be repaired from the verified blob without network"
    }

    Invoke-ContractCase -Name "vcpkg-install-retains-commit-scoped-downloads" -Action {
        $cacheRoot = Join-Path $temporaryRoot "vcpkg dependency environment"
        $downloadsTrustRoot = Join-Path $temporaryRoot "vcpkg dependency shared cache"
        $downloadsRoot = Join-Path $downloadsTrustRoot "vcpkg-downloads/$("e" * 40)"
        $vcpkgRoot = Join-Path $cacheRoot "vcpkg root"
        $layoutRoot = Join-Path $cacheRoot "layout"
        $layout = [pscustomobject]@{
            Buildtrees = Join-Path $layoutRoot "buildtrees"
            Packages = Join-Path $layoutRoot "packages"
            Installed = Join-Path $layoutRoot "installed"
            ManifestRoot = Join-Path $layoutRoot "manifest"
        }
        foreach ($directory in @(
            $cacheRoot,
            $downloadsRoot,
            $vcpkgRoot,
            $layout.Buildtrees,
            $layout.Packages,
            $layout.Installed,
            $layout.ManifestRoot
        )) {
            New-Item -ItemType Directory -Force -Path $directory | Out-Null
        }
        $vcpkg = [pscustomobject]@{
            Executable = Join-Path $cacheRoot "contract-vcpkg.exe"
            Root = $vcpkgRoot
        }
        $state = [pscustomobject]@{ Installs = 0; FailuresRemaining = 2 }
        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, [switch]$StreamOutput)
            $null = $Program
            Assert-Contract ($Description -ceq "install verified vcpkg dependencies") `
                "the vcpkg dependency seam must retain its controlled description"
            Assert-Contract ($StreamOutput) `
                "the vcpkg dependency install must stream hosted diagnostics"
            Assert-Contract (
                (Resolve-Path -LiteralPath $WorkingDirectory).Path -ceq $repository.Path
            ) "the vcpkg dependency install must run from the repository root"
            Assert-Contract (
                [Array]::IndexOf(
                    [object[]]$Arguments, "--downloads-root=$downloadsRoot"
                ) -ge 0
            ) "vcpkg must receive the commit-scoped downloads root"
            Assert-Contract (
                [Array]::IndexOf(
                    [object[]]$Arguments, "--downloads-root=$downloadsTrustRoot"
                ) -lt 0
            ) "the broader downloads trusted root must never replace the cache path"
            $state.Installs++
            if ($state.FailuresRemaining -gt 0) {
                $state.FailuresRemaining--
                throw "synthetic transient vcpkg download failure"
            }
            return @()
        }.GetNewClosure()

        Invoke-PrivateCommandWithNativeCapture `
            -CommandName "Install-EasyConVcpkgDependencies" -Parameters @{
                Vcpkg = $vcpkg
                RepositoryRoot = $repository.Path
                DownloadsRoot = $downloadsRoot
                DownloadsTrustedRoot = $downloadsTrustRoot
                CacheRoot = $cacheRoot
                WorkspaceLayout = $layout
                RetryDelayMilliseconds = 0
            } -NativeCapture $capture | Out-Null
        Assert-Contract ($state.Installs -eq 3) `
            "the controlled vcpkg dependency install must recover on its third attempt"

        $state.FailuresRemaining = 99
        Assert-Throws -Pattern "synthetic transient vcpkg download failure" -Action {
            Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Install-EasyConVcpkgDependencies" -Parameters @{
                    Vcpkg = $vcpkg
                    RepositoryRoot = $repository.Path
                    DownloadsRoot = $downloadsRoot
                    DownloadsTrustedRoot = $downloadsTrustRoot
                    CacheRoot = $cacheRoot
                    WorkspaceLayout = $layout
                    RetryDelayMilliseconds = 0
                } -NativeCapture $capture
        }
        Assert-Contract ($state.Installs -eq 6) `
            "a persistently failing vcpkg install must stop after three attempts"
    }

    Invoke-ContractCase -Name "vcpkg-install-failure-junction-rerun-recovery" -Action {
        $environment = Join-Path $temporaryRoot "vcpkg failed install recovery environment"
        $downloads = Join-Path $temporaryRoot "vcpkg failed install recovery downloads"
        $external = Join-Path $temporaryRoot "vcpkg failed install external target"
        $layoutRoot = Join-Path $environment "layout"
        $layout = [pscustomobject]@{
            Buildtrees = Join-Path $layoutRoot "buildtrees"
            Packages = Join-Path $layoutRoot "packages"
            Installed = Join-Path $layoutRoot "installed"
            ManifestRoot = Join-Path $layoutRoot "manifest"
        }
        foreach ($directory in @(
            $environment,
            $downloads,
            $external,
            $layout.Buildtrees,
            $layout.Packages,
            $layout.Installed,
            $layout.ManifestRoot
        )) {
            New-Item -ItemType Directory -Force -Path $directory | Out-Null
        }
        $externalMarker = Join-Path $external "keep-external.txt"
        $installedMarker = Join-Path $layout.Installed "keep-installed.txt"
        Set-ContractFile -Path $externalMarker -Value "external"
        Set-ContractFile -Path $installedMarker -Value "installed"
        $vcpkg = [pscustomobject]@{
            Executable = Join-Path $environment "contract-vcpkg.exe"
            Root = Join-Path $environment "vcpkg-root"
        }
        New-Item -ItemType Directory -Force -Path $vcpkg.Root | Out-Null
        $state = [pscustomobject]@{
            Installs = 0
            FailInstall = $true
            RealJunctionsCreated = $false
        }
        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, [switch]$StreamOutput)
            $null = $Program, $Arguments, $WorkingDirectory, $StreamOutput
            Assert-Contract ($Description -ceq "install verified vcpkg dependencies") `
                "the recovery fixture must execute the vcpkg install seam"
            $state.Installs++
            if ($state.FailInstall) {
                foreach ($root in @($layout.Buildtrees, $layout.Packages)) {
                    $junction = Join-Path $root "hosted-port-source"
                    if (-not (Test-Path -LiteralPath $junction)) {
                        New-Item -ItemType Junction -Path $junction -Target $external | Out-Null
                    }
                    $item = Get-Item -Force -LiteralPath $junction
                    Assert-Contract (
                        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
                    ) "the failed-install regression fixture must contain a real NTFS junction"
                }
                $state.RealJunctionsCreated = $true
                throw "synthetic failed vcpkg install with hosted junction"
            }
            return @()
        }.GetNewClosure()
        $parameters = @{
            Vcpkg = $vcpkg
            RepositoryRoot = $repository.Path
            DownloadsRoot = $downloads
            DownloadsTrustedRoot = $temporaryRoot
            CacheRoot = $environment
            WorkspaceLayout = $layout
            RetryDelayMilliseconds = 0
        }

        Assert-Throws -Pattern "synthetic failed vcpkg install with hosted junction" -Action {
            Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Install-EasyConVcpkgDependencies" -Parameters $parameters `
                -NativeCapture $capture
        }
        Assert-Contract ($state.Installs -eq 3 -and $state.RealJunctionsCreated) `
            "the failed install must exhaust three attempts with real junction residue"
        Assert-Contract (Test-Path -LiteralPath $externalMarker -PathType Leaf) `
            "failed-install cleanup must never follow a junction into its external target"
        Assert-Contract (Test-Path -LiteralPath $installedMarker -PathType Leaf) `
            "failed-install transient cleanup must not delete the installed tree"

        $rerunCleanupFailure = $null
        try {
            Invoke-PrivateCommand -CommandName "Remove-EasyConSafeTree" -Parameters @{
                Path = $environment
                TrustedRoot = $temporaryRoot
            }
        }
        catch {
            $rerunCleanupFailure = $_
        }
        $rerunCleanupDiagnostic = if ($null -eq $rerunCleanupFailure) {
            "none"
        }
        else {
            $rerunCleanupFailure.Exception.Message
        }
        Assert-Contract ($null -eq $rerunCleanupFailure) (
            "a later Setup must safely remove the failed environment without manual link " +
            "cleanup; failure=$rerunCleanupDiagnostic"
        )
        Assert-Contract (Test-Path -LiteralPath $externalMarker -PathType Leaf) `
            "later Setup recovery must leave the junction target intact"

        foreach ($directory in @(
            $environment,
            $layout.Buildtrees,
            $layout.Packages,
            $layout.Installed,
            $layout.ManifestRoot,
            $vcpkg.Root
        )) {
            New-Item -ItemType Directory -Force -Path $directory | Out-Null
        }
        $replacementInstalledMarker = Join-Path $layout.Installed "replacement-installed.txt"
        Set-ContractFile -Path $replacementInstalledMarker -Value "replacement"
        $state.FailInstall = $false
        Invoke-PrivateCommandWithNativeCapture `
            -CommandName "Install-EasyConVcpkgDependencies" -Parameters $parameters `
            -NativeCapture $capture | Out-Null
        Assert-Contract ($state.Installs -eq 4) `
            "the next Setup must reach and complete its vcpkg install"
        Assert-Contract (
            -not (Test-Path -LiteralPath $layout.Buildtrees) -and
            -not (Test-Path -LiteralPath $layout.Packages)
        ) "successful rerun must remove both transient vcpkg trees"
        Assert-Contract (Test-Path -LiteralPath $replacementInstalledMarker -PathType Leaf) `
            "successful rerun cleanup must preserve the installed tree"
        Assert-Contract (Test-Path -LiteralPath $externalMarker -PathType Leaf) `
            "successful rerun must preserve the former junction target"

        foreach ($directory in @($layout.Buildtrees, $layout.Packages)) {
            New-Item -ItemType Directory -Force -Path $directory | Out-Null
        }
        $lockedBuildtree = Join-Path $layout.Buildtrees "locked-output.bin"
        $lockedHandle = [System.IO.FileStream]::new(
            $lockedBuildtree,
            [System.IO.FileMode]::Create,
            [System.IO.FileAccess]::ReadWrite,
            [System.IO.FileShare]::None
        )
        $state.FailInstall = $true
        $lockedFailure = $null
        try {
            try {
                Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Install-EasyConVcpkgDependencies" -Parameters $parameters `
                    -NativeCapture $capture | Out-Null
            }
            catch {
                $lockedFailure = $_
            }
            Assert-Contract ($null -ne $lockedFailure) `
                "a failed install with locked transient output must remain a failure"
            Assert-Contract (
                $lockedFailure.Exception.Message -match
                    "synthetic failed vcpkg install with hosted junction"
            ) "transient cleanup failure must not replace the vcpkg install primary error"
            Assert-Contract (
                $lockedFailure.Exception.Data.Contains(
                    "EasyConVcpkgTransientCleanupFailure0"
                ) -and
                $lockedFailure.Exception.Data.Contains(
                    "EasyConResidualVcpkgTransientTree0"
                ) -and
                [string]$lockedFailure.Exception.Data[
                    "EasyConResidualVcpkgTransientTree0"
                ] -ceq $layout.Buildtrees
            ) "locked transient cleanup must attach failure and residual-tree diagnostics"
            Assert-Contract (Test-Path -LiteralPath $replacementInstalledMarker -PathType Leaf) `
                "cleanup failure must not delete the installed tree"
            Assert-Contract (Test-Path -LiteralPath $externalMarker -PathType Leaf) `
                "cleanup failure must not follow a junction target"
        }
        finally {
            $lockedHandle.Dispose()
        }
        Remove-PrivateTransientBuildTree -Path $layout.Buildtrees -TrustedRoot $environment
        Remove-PrivateTransientBuildTree -Path $layout.Packages -TrustedRoot $environment
        Assert-Contract (Test-Path -LiteralPath $externalMarker -PathType Leaf) `
            "released cleanup recovery must still preserve the external target"
    }

    Invoke-ContractCase -Name "shared-vcpkg-scripts-revalidate-across-identities" -Action {
        $sharedRoot = Join-Path $temporaryRoot "shared vcpkg source cache"
        $commit = "a" * 40
        $configuration = [pscustomobject]@{
            vcpkg = [pscustomobject]@{ scriptsCommit = $commit }
        }
        $state = [pscustomobject]@{ Installs = 0; Validations = 0 }
        $install = {
            param($Root)
            $state.Installs++
            New-Item -ItemType Directory -Force -Path $Root | Out-Null
            Set-ContractFile -Path (Join-Path $Root "verified.txt") -Value $commit
        }.GetNewClosure()
        $validate = {
            param($Root)
            $state.Validations++
            $marker = Join-Path $Root "verified.txt"
            if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
                throw "cached vcpkg scripts marker is missing"
            }
            if ((Get-Content -Raw -LiteralPath $marker) -cne $commit) {
                throw "cached vcpkg scripts marker is damaged"
            }
            return $Root
        }.GetNewClosure()
        $parameters = @{
            SharedCacheRoot = $sharedRoot
            Configuration = $configuration
            VcpkgExecutable = (Join-Path $temporaryRoot "unused-vcpkg.exe")
            ValidateAction = $validate
            InstallAction = $install
        }

        $first = Invoke-PrivateCommand -CommandName "Get-EasyConSharedVcpkgCheckout" `
            -Parameters $parameters
        $blocked = $parameters.Clone()
        $blocked.InstallAction = { param($Root); throw "network/install seam must not run" }
        $second = Invoke-PrivateCommand -CommandName "Get-EasyConSharedVcpkgCheckout" `
            -Parameters $blocked
        Assert-Contract ($first -ceq $second) `
            "different environment identities must resolve the same pinned source cache"
        Assert-Contract ($state.Installs -eq 1 -and $state.Validations -eq 2) `
            "a shared vcpkg source hit must revalidate without reinstalling"

        Set-ContractFile -Path (Join-Path $first "verified.txt") -Value "damaged"
        $repaired = Invoke-PrivateCommand -CommandName "Get-EasyConSharedVcpkgCheckout" `
            -Parameters $parameters
        Assert-Contract ($repaired -ceq $first -and $state.Installs -eq 2) `
            "damaged cached vcpkg scripts must be isolated and rebuilt at the fixed path"
        Assert-Contract (
            @(Get-ChildItem -LiteralPath (Join-Path $sharedRoot "vcpkg-scripts-quarantine") `
                -Directory -ErrorAction Stop).Count -eq 1
        ) "damaged cached vcpkg scripts must have one quarantined tree"
    }

    Invoke-ContractCase -Name "vcpkg-network-fetch-uses-windows-tls" -Action {
        $cacheRoot = Join-Path $temporaryRoot "vcpkg fetch cache"
        $checkoutRoot = Join-Path $cacheRoot "checkout"
        $vcpkgExecutable = Join-Path $cacheRoot "vcpkg.exe"
        New-Item -ItemType Directory -Force -Path $cacheRoot | Out-Null
        $executableBytes = [System.Text.Encoding]::ASCII.GetBytes("contract vcpkg executable")
        [System.IO.File]::WriteAllBytes($vcpkgExecutable, $executableBytes)
        $executableHash = [Convert]::ToHexString(
            [System.Security.Cryptography.SHA256]::HashData($executableBytes)
        ).ToLowerInvariant()
        $commit = "b" * 40
        $toolCommit = "c" * 40
        $toolRelease = "2026-01-01"
        $configuration = [pscustomobject]@{
            vcpkg = [pscustomobject]@{
                scriptsRepository = "https://github.com/microsoft/vcpkg.git"
                scriptsCommit = $commit
                toolRelease = $toolRelease
                toolCommit = $toolCommit
                windowsAsset = [pscustomobject]@{
                    bytes = $executableBytes.Length
                    sha256 = $executableHash
                }
            }
        }
        $state = [pscustomobject]@{ Fetches = 0 }
        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $WorkingDirectory, $StreamOutput
            switch ($Description) {
                "fetch pinned vcpkg scripts" {
                    $state.Fetches++
                    $backendIndex = [Array]::IndexOf(
                        [object[]]$Arguments, "http.sslBackend=schannel"
                    )
                    Assert-Contract (
                        $backendIndex -gt 0 -and
                        [string]$Arguments[$backendIndex - 1] -ceq "-c"
                    ) "the pinned vcpkg fetch must use the Windows TLS backend"
                    return @()
                }
                "check out pinned vcpkg scripts" {
                    $rootIndex = [Array]::IndexOf([object[]]$Arguments, "-C")
                    $root = [string]$Arguments[$rootIndex + 1]
                    foreach ($relative in @(
                        ".vcpkg-root",
                        "bootstrap-vcpkg.bat",
                        "bootstrap-vcpkg.sh",
                        "scripts/buildsystems/vcpkg.cmake"
                    )) {
                        $path = Join-Path $root $relative
                        [System.IO.Directory]::CreateDirectory(
                            [System.IO.Path]::GetDirectoryName($path)
                        ) | Out-Null
                        [System.IO.File]::WriteAllText($path, "contract")
                    }
                    return @()
                }
                "vcpkg scripts commit check" { return $commit }
                "vcpkg required tracked file check" { return [string]$Arguments[-1] }
                "vcpkg scripts cleanliness check" { return @() }
                "vcpkg tool version check" {
                    return "vcpkg package management program version $toolRelease-$toolCommit"
                }
                default { return @() }
            }
        }.GetNewClosure()

        Invoke-PrivateCommandWithNativeCapture `
            -CommandName "Install-EasyConVcpkgCheckout" -Parameters @{
                VcpkgRoot = $checkoutRoot
                CacheRoot = $cacheRoot
                Configuration = $configuration
                VcpkgExecutable = $vcpkgExecutable
            } -NativeCapture $capture | Out-Null
        Assert-Contract ($state.Fetches -eq 1) `
            "the pinned vcpkg checkout must perform exactly one controlled fetch"
        Assert-Contract (Test-Path -LiteralPath $checkoutRoot -PathType Container) `
            "the mocked pinned vcpkg checkout must publish atomically"
    }

    Invoke-ContractCase -Name "prepared-vcpkg-validation-keeps-index-read-only" -Action {
        $git = (Get-Command git.exe -ErrorAction Stop).Source
        $checkoutRoot = Join-Path $temporaryRoot "read-only prepared vcpkg checkout"
        foreach ($relative in @(
            ".vcpkg-root",
            "bootstrap-vcpkg.bat",
            "bootstrap-vcpkg.sh",
            "scripts/buildsystems/vcpkg.cmake"
        )) {
            Set-ContractFile -Path (Join-Path $checkoutRoot $relative) -Value "contract"
        }
        & $git init --quiet $checkoutRoot
        Assert-Contract ($LASTEXITCODE -eq 0) "the read-only vcpkg fixture must initialize Git"
        & $git -C $checkoutRoot add -- .
        Assert-Contract ($LASTEXITCODE -eq 0) "the read-only vcpkg fixture must stage required files"
        & $git -c user.name=EasyConContract -c user.email=contract@example.invalid `
            -C $checkoutRoot commit --quiet -m "contract fixture"
        Assert-Contract ($LASTEXITCODE -eq 0) "the read-only vcpkg fixture must commit required files"
        $commit = @(& $git -C $checkoutRoot rev-parse HEAD)[0]

        $toolRelease = "2026-01-01"
        $toolCommit = "1" * 40
        $vcpkgExecutable = Join-Path $temporaryRoot "read-only-vcpkg.cmd"
        Set-ContractFile -Path $vcpkgExecutable -Value (
            "@echo off`r`necho vcpkg package management program version " +
            "$toolRelease-$toolCommit`r`n"
        )
        $asset = Get-Item -LiteralPath $vcpkgExecutable
        $assetHash = (Get-FileHash -LiteralPath $vcpkgExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
        $configuration = [pscustomobject]@{
            vcpkg = [pscustomobject]@{
                scriptsCommit = $commit
                toolRelease = $toolRelease
                toolCommit = $toolCommit
                windowsAsset = [pscustomobject]@{
                    bytes = [long]$asset.Length
                    sha256 = $assetHash
                }
            }
        }

        $trackedFile = Get-Item -LiteralPath (Join-Path $checkoutRoot "bootstrap-vcpkg.bat")
        $trackedFile.LastWriteTimeUtc = $trackedFile.LastWriteTimeUtc.AddMinutes(-10)
        $indexPath = Join-Path $checkoutRoot ".git/index"
        $beforeHash = (Get-FileHash -LiteralPath $indexPath -Algorithm SHA256).Hash
        $beforeWrite = (Get-Item -LiteralPath $indexPath).LastWriteTimeUtc.Ticks
        Invoke-PrivateCommand -CommandName "Assert-EasyConVcpkgCheckout" -Parameters @{
            VcpkgRoot = $checkoutRoot
            Configuration = $configuration
            VcpkgExecutable = $vcpkgExecutable
        } | Out-Null
        $afterHash = (Get-FileHash -LiteralPath $indexPath -Algorithm SHA256).Hash
        $afterWrite = (Get-Item -LiteralPath $indexPath).LastWriteTimeUtc.Ticks
        Assert-Contract (
            $afterHash -ceq $beforeHash -and
            $afterWrite -eq $beforeWrite -and
            -not (Test-Path -LiteralPath (Join-Path $checkoutRoot ".git/index.lock"))
        ) "Verify must not refresh the prepared vcpkg index or create an optional lock"
    }

    Invoke-ContractCase -Name "pinned-cargo-source-rejects-ambient-path-contamination" -Action {
        $pin = [pscustomobject]@{
            Channel = "1.97.1"
            Target = "x86_64-pc-windows-msvc"
            Components = @("clippy", "rustfmt")
        }
        $rustupHome = Join-Path $temporaryRoot "cargo provenance rustup home"
        $controlledCargo = Join-Path $rustupHome (
            "toolchains/1.97.1-x86_64-pc-windows-msvc/bin/cargo.exe"
        )
        $ambientDirectory = Join-Path $temporaryRoot "ambient cargo contamination"
        $ambientCargo = Join-Path $ambientDirectory "cargo.exe"
        foreach ($path in @($controlledCargo, $ambientCargo)) {
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path) | Out-Null
            Copy-Item -LiteralPath (Join-Path $PSHOME "pwsh.exe") -Destination $path
        }
        $vendorEnvironment = Join-Path $temporaryRoot "cargo provenance environment"
        $vendorDownloads = Join-Path $temporaryRoot "cargo provenance downloads"
        New-Item -ItemType Directory -Force -Path $vendorEnvironment, $vendorDownloads | Out-Null
        $state = [pscustomobject]@{
            CargoWhichPath = $controlledCargo
            CargoVersion = "1.97.1"
            RustRelease = "1.97.1"
            RustHost = "x86_64-pc-windows-msvc"
            Targets = @("x86_64-pc-windows-msvc")
            Components = @(
                "clippy-x86_64-pc-windows-msvc (installed)",
                "rustfmt-x86_64-pc-windows-msvc (installed)"
            )
            CargoWhichFailure = $false
            AmbientExecutions = 0
            VendorProgram = $null
            Events = [System.Collections.Generic.List[string]]::new()
        }
        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, [switch]$StreamOutput)
            $null = $WorkingDirectory, $StreamOutput
            if ($Program.Equals($ambientCargo, [System.StringComparison]::OrdinalIgnoreCase)) {
                $state.AmbientExecutions++
            }
            switch ($Description) {
                "frozen rustc version check" {
                    $state.Events.Add("rustc") | Out-Null
                    return @(
                        "rustc $($state.RustRelease) (contract)",
                        "binary: rustc",
                        "host: $($state.RustHost)",
                        "release: $($state.RustRelease)"
                    )
                }
                "frozen Rust target check" {
                    $state.Events.Add("targets") | Out-Null
                    return @($state.Targets)
                }
                "frozen Rust component check" {
                    $state.Events.Add("components") | Out-Null
                    return @($state.Components)
                }
                "frozen Cargo path check" {
                    $state.Events.Add("cargo-which") | Out-Null
                    if ($state.CargoWhichFailure) {
                        throw "synthetic missing frozen rustup toolchain"
                    }
                    return $state.CargoWhichPath
                }
                "frozen Cargo version check" {
                    $state.Events.Add("cargo-version") | Out-Null
                    return @(
                        "cargo $($state.CargoVersion) (contract 2026-01-01)",
                        "release: $($state.CargoVersion)",
                        "host: x86_64-pc-windows-msvc"
                    )
                }
                "install locked Cargo sources" {
                    $state.Events.Add("cargo-vendor") | Out-Null
                    $state.VendorProgram = $Program
                    $vendorRoot = [string]$Arguments[-1]
                    New-Item -ItemType Directory -Force -Path $vendorRoot | Out-Null
                    Set-ContractFile -Path (Join-Path $vendorRoot "contract-crate/Cargo.toml") `
                        -Value "[package]`nname='contract-crate'`nversion='1.0.0'"
                    return @()
                }
                default {
                    throw "unexpected Cargo provenance native capture: $Description"
                }
            }
        }.GetNewClosure()
        $savedPath = [Environment]::GetEnvironmentVariable("PATH", "Process")
        $savedRustupHome = [Environment]::GetEnvironmentVariable("RUSTUP_HOME", "Process")
        $savedToolchain = [Environment]::GetEnvironmentVariable("RUSTUP_TOOLCHAIN", "Process")
        try {
            $env:PATH = $ambientDirectory + [System.IO.Path]::PathSeparator + $savedPath
            $env:RUSTUP_HOME = $rustupHome
            $rust = Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Assert-EasyConRustToolchain" -Parameters @{ Pin = $pin } `
                -NativeCapture $capture
            Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Install-EasyConCargoSources" -Parameters @{
                    CargoPath = $rust.CargoPath
                    RepositoryRoot = $repository.Path
                    EnvironmentRoot = $vendorEnvironment
                    DownloadCacheRoot = $vendorDownloads
                } -NativeCapture $capture | Out-Null
            Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Get-EasyConCargoVersion" -Parameters @{
                    CargoPath = $rust.CargoPath
                    ExpectedVersion = $pin.Channel
                } -NativeCapture $capture | Out-Null

            $cargoWhichIndex = $state.Events.IndexOf("cargo-which")
            $cargoVersionIndex = $state.Events.IndexOf("cargo-version")
            $cargoVendorIndex = $state.Events.IndexOf("cargo-vendor")
            Assert-Contract (
                $rust.CargoPath.Equals(
                    $controlledCargo, [System.StringComparison]::OrdinalIgnoreCase
                ) -and
                $state.AmbientExecutions -eq 0 -and
                $state.VendorProgram.Equals(
                    $controlledCargo, [System.StringComparison]::OrdinalIgnoreCase
                ) -and
                $cargoWhichIndex -ge 0 -and
                $cargoVersionIndex -gt $cargoWhichIndex -and
                $cargoVendorIndex -gt $cargoVersionIndex
            ) (
                "Cargo must be resolved and version-checked from the pinned rustup toolchain " +
                "before vendor; CargoPath=$($rust.CargoPath) AmbientExecutions=" +
                "$($state.AmbientExecutions) Events=$($state.Events -join ',')"
            )

            $state.CargoWhichFailure = $true
            Assert-Throws -Pattern "missing frozen rustup toolchain" -Action {
                Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Assert-EasyConRustToolchain" -Parameters @{ Pin = $pin } `
                    -NativeCapture $capture
            }
            $state.CargoWhichFailure = $false

            $state.CargoWhichPath = $ambientCargo
            Assert-Throws -Pattern "escaped|frozen toolchain" -Action {
                Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Assert-EasyConRustToolchain" -Parameters @{ Pin = $pin } `
                    -NativeCapture $capture
            }
            $state.CargoWhichPath = $controlledCargo

            $state.CargoVersion = "1.96.0"
            Assert-Throws -Pattern "Cargo.*frozen Rust version" -Action {
                Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Assert-EasyConRustToolchain" -Parameters @{ Pin = $pin } `
                    -NativeCapture $capture
            }
            $state.CargoVersion = "1.97.1"

            $state.RustRelease = "1.96.0"
            Assert-Throws -Pattern "rustc.*frozen channel" -Action {
                Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Assert-EasyConRustToolchain" -Parameters @{ Pin = $pin } `
                    -NativeCapture $capture
            }
            $state.RustRelease = "1.97.1"

            $state.RustHost = "x86_64-unknown-linux-gnu"
            Assert-Throws -Pattern "rustc host.*frozen Windows target" -Action {
                Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Assert-EasyConRustToolchain" -Parameters @{ Pin = $pin } `
                    -NativeCapture $capture
            }
            $state.RustHost = "x86_64-pc-windows-msvc"

            $state.Targets = @()
            Assert-Throws -Pattern "Rust target is not installed" -Action {
                Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Assert-EasyConRustToolchain" -Parameters @{ Pin = $pin } `
                    -NativeCapture $capture
            }
            $state.Targets = @("x86_64-pc-windows-msvc")

            $state.Components = @("clippy-x86_64-pc-windows-msvc (installed)")
            Assert-Throws -Pattern "Rust component is not installed: rustfmt" -Action {
                Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Assert-EasyConRustToolchain" -Parameters @{ Pin = $pin } `
                    -NativeCapture $capture
            }
            Assert-Contract ($state.AmbientExecutions -eq 0) `
                "all rejected Rust/Cargo states must fail before ambient cargo execution"
        }
        finally {
            [Environment]::SetEnvironmentVariable("PATH", $savedPath, "Process")
            [Environment]::SetEnvironmentVariable("RUSTUP_HOME", $savedRustupHome, "Process")
            [Environment]::SetEnvironmentVariable("RUSTUP_TOOLCHAIN", $savedToolchain, "Process")
        }
    }

    Invoke-ContractCase -Name "shared-rust-toolchain-retains-rustup-validation" -Action {
        $pin = [pscustomobject]@{
            Channel = "1.97.1"
            Target = "x86_64-pc-windows-msvc"
            Components = @("clippy", "rustfmt")
        }
        $rustupHome = Join-Path $temporaryRoot "shared Rust contract home"
        $cargoPath = Join-Path $rustupHome (
            "toolchains/1.97.1-x86_64-pc-windows-msvc/bin/cargo.exe"
        )
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $cargoPath) | Out-Null
        Copy-Item -LiteralPath (Join-Path $PSHOME "pwsh.exe") -Destination $cargoPath
        $state = [pscustomobject]@{
            Installs = 0
            FailuresRemaining = 2
            RustcChecks = 0
            TargetChecks = 0
            ComponentChecks = 0
            CargoPathChecks = 0
            CargoVersionChecks = 0
        }
        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Arguments, $WorkingDirectory, $StreamOutput
            switch ($Description) {
                "frozen rustc version check" {
                    $state.RustcChecks++
                    return @(
                        "rustc 1.97.1 (contract)",
                        "binary: rustc",
                        "host: x86_64-pc-windows-msvc",
                        "release: 1.97.1"
                    )
                }
                "frozen Rust target check" {
                    $state.TargetChecks++
                    return "x86_64-pc-windows-msvc"
                }
                "frozen Rust component check" {
                    $state.ComponentChecks++
                    return @(
                        "clippy-x86_64-pc-windows-msvc (installed)",
                        "rustfmt-x86_64-pc-windows-msvc (installed)"
                    )
                }
                "frozen Cargo path check" {
                    $state.CargoPathChecks++
                    return $cargoPath
                }
                "frozen Cargo version check" {
                    $state.CargoVersionChecks++
                    return @(
                        "cargo 1.97.1 (contract 2026-01-01)",
                        "release: 1.97.1",
                        "host: x86_64-pc-windows-msvc"
                    )
                }
                "install frozen Rust toolchain" {
                    $state.Installs++
                    if ($state.FailuresRemaining -gt 0) {
                        $state.FailuresRemaining--
                        throw "synthetic transient Rust download failure"
                    }
                    return "synthetic Rust install"
                }
                default {
                    throw "unexpected native capture: $Description"
                }
            }
        }.GetNewClosure()
        $savedToolchain = [Environment]::GetEnvironmentVariable("RUSTUP_TOOLCHAIN", "Process")
        $savedRustupHome = [Environment]::GetEnvironmentVariable("RUSTUP_HOME", "Process")
        try {
            $env:RUSTUP_HOME = $rustupHome
            $parameters = @{ Pin = $pin; RetryDelayMilliseconds = 0 }
            Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Install-EasyConRustToolchain" -Parameters $parameters `
                -NativeCapture $capture | Out-Null
            Invoke-PrivateCommandWithNativeCapture `
                -CommandName "Install-EasyConRustToolchain" -Parameters $parameters `
                -NativeCapture $capture | Out-Null
            Assert-Contract (
                $state.Installs -eq 4 -and
                $state.RustcChecks -eq 2 -and
                $state.TargetChecks -eq 2 -and
                $state.ComponentChecks -eq 2 -and
                $state.CargoPathChecks -eq 2 -and
                $state.CargoVersionChecks -eq 2
            ) "Rust retries must resume before each successful Setup revalidates shared state"

            $state.FailuresRemaining = 99
            Assert-Throws -Pattern "synthetic transient Rust download failure" -Action {
                Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Install-EasyConRustToolchain" -Parameters $parameters `
                    -NativeCapture $capture
            }
            Assert-Contract ($state.Installs -eq 7) `
                "a persistently failing Rust install must stop after three attempts"
        }
        finally {
            [Environment]::SetEnvironmentVariable(
                "RUSTUP_TOOLCHAIN", $savedToolchain, "Process"
            )
            [Environment]::SetEnvironmentVariable(
                "RUSTUP_HOME", $savedRustupHome, "Process"
            )
        }
    }

    Invoke-ContractCase -Name "shared-cache-publication-lease-is-cross-identity" -Action {
        $sharedRoot = Join-Path $temporaryRoot "shared publication lease"
        $lockA = Join-Path $sharedRoot "assets-v1/locks/sha256-a.lock"
        $lockB = Join-Path $sharedRoot "assets-v1/locks/sha256-b.lock"
        $parametersA = @{
            Path = $lockA
            TrustedRoot = $sharedRoot
            Description = "contract asset A"
            TimeoutMilliseconds = 0
            RetryMilliseconds = 0
        }
        $leaseA = Invoke-PrivateCommand -CommandName "Enter-EasyConExclusiveFileLease" `
            -Parameters $parametersA
        try {
            Assert-Throws -Pattern "busy|ownership" -Action {
                Invoke-PrivateCommand -CommandName "Enter-EasyConExclusiveFileLease" `
                    -Parameters $parametersA
            }
            $leaseB = Invoke-PrivateCommand -CommandName "Enter-EasyConExclusiveFileLease" `
                -Parameters @{
                    Path = $lockB
                    TrustedRoot = $sharedRoot
                    Description = "contract asset B"
                    TimeoutMilliseconds = 0
                    RetryMilliseconds = 0
                }
            $leaseB.Dispose()
        }
        finally {
            $leaseA.Dispose()
        }
        $released = Invoke-PrivateCommand -CommandName "Enter-EasyConExclusiveFileLease" `
            -Parameters $parametersA
        $released.Dispose()
    }

    Invoke-ContractCase -Name "fingerprint-and-manifest-change" -Action {
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        $fixture = Join-Path $temporaryRoot "fingerprint repository"
        foreach ($relative in @($configuration.fingerprintInputs.path)) {
            Set-ContractFile -Path (Join-Path $fixture $relative) -Value "fixture:$relative"
        }
        $first = Get-PrivateEnvironmentFingerprint -RepositoryRoot $fixture `
            -Configuration $configuration
        $again = Get-PrivateEnvironmentFingerprint -RepositoryRoot $fixture `
            -Configuration $configuration
        Assert-Contract ($first.Value -ceq $again.Value) "unchanged inputs need one stable fingerprint"
        Add-Content -LiteralPath (Join-Path $fixture "vcpkg.json") -Value "changed" -Encoding utf8NoBOM
        $changed = Get-PrivateEnvironmentFingerprint -RepositoryRoot $fixture `
            -Configuration $configuration
        Assert-Contract ($first.Value -cne $changed.Value) `
            "a frozen manifest change must require a different prepared environment"
        Add-Content -LiteralPath (Join-Path $fixture "Cargo.lock") -Value "changed" -Encoding utf8NoBOM
        $lockChanged = Get-PrivateEnvironmentFingerprint -RepositoryRoot $fixture `
            -Configuration $configuration
        Assert-Contract ($changed.Value -cne $lockChanged.Value) `
            "a Cargo lock change must require a different prepared environment"
    }

    Invoke-ContractCase -Name "text-fingerprint-canonicalizes-checkout-line-endings" -Action {
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        $fixtures = [ordered]@{
            lf = "alpha`nbeta`ngamma`n"
            crlf = "alpha`r`nbeta`r`ngamma`r`n"
            mixed = "alpha`r`nbeta`rgamma`n"
        }
        $fingerprints = @{}
        foreach ($fixture in $fixtures.GetEnumerator()) {
            $root = Join-Path $temporaryRoot ("fingerprint line endings {0}" -f $fixture.Key)
            foreach ($relative in @($configuration.fingerprintInputs.path)) {
                Set-ContractUtf8Text -Path (Join-Path $root $relative) -Value $fixture.Value
            }
            $fingerprints[$fixture.Key] = (Get-PrivateEnvironmentFingerprint `
                -RepositoryRoot $root -Configuration $configuration).Value
        }
        Assert-Contract ($fingerprints.lf -ceq $fingerprints.crlf) `
            "LF and CRLF checkouts of identical text must share one fingerprint"
        Assert-Contract ($fingerprints.lf -ceq $fingerprints.mixed) `
            "mixed checkout line endings must canonicalize like LF"

        $changedRoot = Join-Path $temporaryRoot "fingerprint changed text"
        foreach ($relative in @($configuration.fingerprintInputs.path)) {
            Set-ContractUtf8Text -Path (Join-Path $changedRoot $relative) `
                -Value "alpha`nbeta`ngamma`n"
        }
        Set-ContractUtf8Text -Path (Join-Path $changedRoot "Cargo.lock") `
            -Value "alpha`nchanged`ngamma`n"
        $changed = Get-PrivateEnvironmentFingerprint -RepositoryRoot $changedRoot `
            -Configuration $configuration
        Assert-Contract ($fingerprints.lf -cne $changed.Value) `
            "a real canonical text change must invalidate the fingerprint"

        $binaryRoot = Join-Path $temporaryRoot "fingerprint binary bytes"
        $binaryPath = Join-Path $binaryRoot "input.bin"
        New-Item -ItemType Directory -Force -Path $binaryRoot | Out-Null
        [System.IO.File]::WriteAllBytes($binaryPath, [byte[]](0x61, 0x0d, 0x0a, 0x62))
        $binaryConfiguration = [pscustomobject]@{
            fingerprintInputs = @([pscustomobject]@{ path = "input.bin"; kind = "binary" })
        }
        $binaryCrLf = Get-PrivateEnvironmentFingerprint -RepositoryRoot $binaryRoot `
            -Configuration $binaryConfiguration
        [System.IO.File]::WriteAllBytes($binaryPath, [byte[]](0x61, 0x0a, 0x62))
        $binaryLf = Get-PrivateEnvironmentFingerprint -RepositoryRoot $binaryRoot `
            -Configuration $binaryConfiguration
        Assert-Contract ($binaryCrLf.Value -cne $binaryLf.Value) `
            "binary fingerprint inputs must hash raw bytes without newline normalization"

        [System.IO.File]::WriteAllBytes($binaryPath, [byte[]](0xc3, 0x28))
        $textConfiguration = [pscustomobject]@{
            fingerprintInputs = @([pscustomobject]@{ path = "input.bin"; kind = "text" })
        }
        Assert-Throws -Pattern "valid UTF-8" -Action {
            Get-PrivateEnvironmentFingerprint -RepositoryRoot $binaryRoot `
                -Configuration $textConfiguration
        }
    }

    Invoke-ContractCase -Name "worktree-shared-prepared-environment" -Action {
        $cache = Join-Path $temporaryRoot "shared cache"
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        $first = Get-PrivateEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree one") `
            -Fingerprint ("a" * 64) -Configuration $configuration -CacheRoot $cache
        $firstAgain = Get-PrivateEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree one") `
            -Fingerprint ("a" * 64) -Configuration $configuration -CacheRoot $cache
        $second = Get-PrivateEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree two") `
            -Fingerprint ("a" * 64) -Configuration $configuration -CacheRoot $cache
        $changed = Get-PrivateEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree one") `
            -Fingerprint ("b" * 64) -Configuration $configuration -CacheRoot $cache
        Assert-Contract ($first.EnvironmentRoot -ceq $firstAgain.EnvironmentRoot) `
            "one worktree and fingerprint must resolve stably"
        Assert-Contract ($first.EnvironmentRoot -ceq $second.EnvironmentRoot) `
            "different worktrees with one fingerprint must share one prepared environment"
        Assert-Contract ($first.LockPath -ceq $second.LockPath) `
            "different worktrees with one fingerprint must share one environment lease"
        Assert-Contract ($first.WorkspaceRoot -cne $second.WorkspaceRoot) `
            "different worktrees must retain separate writable workspace roots"
        Assert-Contract ($first.CargoTargetDirectory -cne $second.CargoTargetDirectory) `
            "different worktrees must retain separate Cargo target directories"
        Assert-Contract ($first.EnvironmentRoot -cne $changed.EnvironmentRoot) `
            "a fingerprint change must select a new environment root"

        $pythonVariables = @(
            "TEMP", "TMP", "TMPDIR", "PYTHONPYCACHEPREFIX", "PYTHONDONTWRITEBYTECODE"
        )
        $savedPythonVariables = @{}
        foreach ($name in $pythonVariables) {
            $savedPythonVariables[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        }
        try {
            $runtime = Invoke-PrivateCommand `
                -CommandName "Set-EasyConWorkspaceRuntimeEnvironment" -Parameters @{
                    WorkspaceRoot = $first.WorkspaceRoot
                    WritableRoot = $first.WritableRoot
                }
            Assert-Contract (
                $runtime.Temporary.StartsWith(
                    $first.WorkspaceRoot,
                    [System.StringComparison]::OrdinalIgnoreCase
                ) -and
                $runtime.PythonCache.StartsWith(
                    $first.WorkspaceRoot,
                    [System.StringComparison]::OrdinalIgnoreCase
                )
            ) "Python temporary and bytecode roots must remain under this worktree w/"
            Assert-Contract (
                $env:TEMP -ceq $runtime.Temporary -and
                $env:TMP -ceq $runtime.Temporary -and
                $env:TMPDIR -ceq $runtime.Temporary -and
                $env:PYTHONPYCACHEPREFIX -ceq $runtime.PythonCache -and
                $env:PYTHONDONTWRITEBYTECODE -ceq "1"
            ) "Python runtime variables must prevent source-tree bytecode and temp writes"
        }
        finally {
            foreach ($name in $pythonVariables) {
                [Environment]::SetEnvironmentVariable(
                    $name,
                    $savedPythonVariables[$name],
                    "Process"
                )
            }
        }
    }

    Invoke-ContractCase -Name "real-git-worktrees-share-prepared-identity" -Action {
        $git = (Get-Command git.exe -ErrorAction Stop).Source
        $cache = Join-Path $temporaryRoot "real git worktree shared cache"
        $checkoutRoot = Join-Path $temporaryRoot "fixed SHA worktrees"
        $firstWorktree = Join-Path $checkoutRoot "one"
        $secondWorktree = Join-Path $checkoutRoot "two"
        $head = @(& $git -C $repository.Path rev-parse --verify HEAD)
        Assert-Contract (
            $LASTEXITCODE -eq 0 -and
            $head.Count -eq 1 -and
            $head[0] -cmatch '^[0-9a-f]{40}$'
        ) "the real worktree contract must resolve one fixed Git SHA"
        $fixedSha = $head[0]
        $createdWorktrees = [System.Collections.Generic.List[string]]::new()
        try {
            New-Item -ItemType Directory -Force -Path $checkoutRoot | Out-Null
            foreach ($worktree in @($firstWorktree, $secondWorktree)) {
                & $git -C $repository.Path worktree add --detach $worktree $fixedSha | Out-Null
                Assert-Contract ($LASTEXITCODE -eq 0) `
                    "the real worktree contract must create both fixed-SHA checkouts"
                $createdWorktrees.Add($worktree) | Out-Null

                $checkoutHead = @(& $git -C $worktree rev-parse --verify HEAD)
                $checkoutStatus = @(& $git -C $worktree status --porcelain=v1 --untracked-files=all)
                Assert-Contract (
                    $LASTEXITCODE -eq 0 -and
                    $checkoutHead.Count -eq 1 -and
                    $checkoutHead[0] -ceq $fixedSha -and
                    $checkoutStatus.Count -eq 0
                ) "each real worktree fixture must be one unmodified fixed-SHA checkout"
            }

            $firstConfiguration = Get-PrivateWindowsBuildConfiguration `
                -Path (Join-Path $firstWorktree "tools/windows_build_environment.json")
            $secondConfiguration = Get-PrivateWindowsBuildConfiguration `
                -Path (Join-Path $secondWorktree "tools/windows_build_environment.json")
            $firstFingerprint = Get-PrivateEnvironmentFingerprint -RepositoryRoot $firstWorktree `
                -Configuration $firstConfiguration
            $secondFingerprint = Get-PrivateEnvironmentFingerprint -RepositoryRoot $secondWorktree `
                -Configuration $secondConfiguration
            Assert-Contract ($firstFingerprint.Value -ceq $secondFingerprint.Value) `
                "clean fixed-SHA worktrees must calculate one fingerprint without copied inputs"
            $firstLocation = Get-PrivateEnvironmentLocation -RepositoryRoot $firstWorktree `
                -Fingerprint $firstFingerprint.Value -Configuration $firstConfiguration -CacheRoot $cache
            $secondLocation = Get-PrivateEnvironmentLocation -RepositoryRoot $secondWorktree `
                -Fingerprint $secondFingerprint.Value -Configuration $secondConfiguration -CacheRoot $cache
            Assert-Contract ($firstLocation.EnvironmentRoot -ceq $secondLocation.EnvironmentRoot) `
                "matching real worktrees must share one prepared EnvironmentRoot"
            Assert-Contract ($firstLocation.LockPath -ceq $secondLocation.LockPath) `
                "matching real worktrees must share one environment lease"

            $firstWritable = [ordered]@{
                CargoTarget = $firstLocation.CargoTargetDirectory
                CMakeCache = Join-Path $firstLocation.CargoTargetDirectory "cmake"
                CargoHome = Join-Path $firstLocation.WorkspaceRoot "cargo-home"
                CargoConfig = Join-Path $firstLocation.WorkspaceRoot "cargo-home/config.toml"
                VcpkgWrapper = Join-Path $firstLocation.WorkspaceRoot "vcpkg"
                VcpkgDownloads = Join-Path $firstLocation.WorkspaceRoot "vcpkg/downloads"
                Temporary = Join-Path $firstLocation.WorkspaceRoot "tmp"
                PythonCache = Join-Path $firstLocation.WorkspaceRoot "python-cache"
            }
            $secondWritable = [ordered]@{
                CargoTarget = $secondLocation.CargoTargetDirectory
                CMakeCache = Join-Path $secondLocation.CargoTargetDirectory "cmake"
                CargoHome = Join-Path $secondLocation.WorkspaceRoot "cargo-home"
                CargoConfig = Join-Path $secondLocation.WorkspaceRoot "cargo-home/config.toml"
                VcpkgWrapper = Join-Path $secondLocation.WorkspaceRoot "vcpkg"
                VcpkgDownloads = Join-Path $secondLocation.WorkspaceRoot "vcpkg/downloads"
                Temporary = Join-Path $secondLocation.WorkspaceRoot "tmp"
                PythonCache = Join-Path $secondLocation.WorkspaceRoot "python-cache"
            }
            foreach ($name in @($firstWritable.Keys)) {
                Assert-Contract ($firstWritable[$name] -cne $secondWritable[$name]) `
                    "matching real worktrees must isolate writable $name output"
            }

            $state = [pscustomobject]@{ Provisions = 0; Downloads = 0 }
            $readyMarker = Join-Path $firstLocation.EnvironmentRoot "contract-ready.txt"
            $setup = {
                $state.Provisions++
                $state.Downloads++
                Set-ContractFile -Path $readyMarker -Value $fixedSha
            }.GetNewClosure()
            $newVerify = {
                param($ownedLocation)
                if (
                    -not (Test-Path -LiteralPath $readyMarker -PathType Leaf) -or
                    (Get-Content -Raw -LiteralPath $readyMarker) -cne $fixedSha
                ) {
                    throw "fixed-SHA prepared environment is not ready"
                }
                $unexpectedLease = $null
                $leaseFailure = $null
                try {
                    $unexpectedLease = Invoke-PrivateCommand `
                        -CommandName "Enter-EasyConWorkspaceLease" -Parameters @{
                            Location = $ownedLocation
                            TimeoutMilliseconds = 0
                            RetryMilliseconds = 0
                        }
                }
                catch {
                    $leaseFailure = $_
                }
                finally {
                    if ($null -ne $unexpectedLease) {
                        $unexpectedLease.Dispose()
                    }
                }
                Assert-Contract (
                    $null -ne $leaseFailure -and
                    $leaseFailure.Exception.Message -match 'busy|ownership'
                ) "Setup VerifyCore must own this worktree's writable lease before touching w/"
                New-Item -ItemType Directory -Force -Path $ownedLocation.WorkspaceRoot | Out-Null
                Set-ContractFile -Path (Join-Path $ownedLocation.WorkspaceRoot "verify.txt") `
                    -Value $fixedSha
                return [pscustomobject]@{
                    status = "ready"
                    environmentRoot = $ownedLocation.EnvironmentRoot
                }
            }.GetNewClosure()
            $firstVerify = { & $newVerify $firstLocation }.GetNewClosure()
            $secondVerify = { & $newVerify $secondLocation }.GetNewClosure()
            $lifecycleParameters = @{
                Mode = "Setup"
                SetupAction = $setup
                WorkspaceAction = { param($Summary) $null = $Summary }
                LeaseTimeoutMilliseconds = 0
            }
            $firstLifecycle = $lifecycleParameters.Clone()
            $firstLifecycle.Location = $firstLocation
            $firstLifecycle.VerifyAction = $firstVerify
            Invoke-PrivateCommand -CommandName "Invoke-EasyConEnvironmentLifecycle" `
                -Parameters $firstLifecycle | Out-Null
            Assert-Contract ($state.Provisions -eq 1 -and $state.Downloads -eq 1) `
                "the first real worktree Setup must provision and download exactly once"

            $secondLifecycle = $lifecycleParameters.Clone()
            $secondLifecycle.Location = $secondLocation
            $secondLifecycle.VerifyAction = $secondVerify
            Invoke-PrivateCommand -CommandName "Invoke-EasyConEnvironmentLifecycle" `
                -Parameters $secondLifecycle | Out-Null
            Assert-Contract ($state.Provisions -eq 1 -and $state.Downloads -eq 1) `
                "the second real worktree Setup must be already-ready with zero repeat work"
            $environmentDirectories = @(Get-ChildItem -LiteralPath (Join-Path $cache "e") `
                -Directory -Force)
            Assert-Contract (
                $environmentDirectories.Count -eq 1 -and
                $environmentDirectories[0].FullName -ceq $firstLocation.EnvironmentRoot
            ) "two real worktrees must leave exactly one shared prepared environment"
            foreach ($worktree in @($firstWorktree, $secondWorktree)) {
                $finalHead = @(& $git -C $worktree rev-parse --verify HEAD)
                $finalStatus = @(& $git -C $worktree status --porcelain=v1 --untracked-files=all)
                Assert-Contract (
                    $LASTEXITCODE -eq 0 -and
                    $finalHead.Count -eq 1 -and
                    $finalHead[0] -ceq $fixedSha -and
                    $finalStatus.Count -eq 0
                ) "both Setup lifecycles must leave each fixed-SHA checkout clean"
            }
        }
        finally {
            $cleanupWorktrees = @($createdWorktrees.ToArray())
            [array]::Reverse($cleanupWorktrees)
            foreach ($worktree in $cleanupWorktrees) {
                & $git -C $repository.Path worktree remove $worktree | Out-Null
                if ($LASTEXITCODE -ne 0) {
                    throw "the real worktree contract failed to remove a temporary clean checkout"
                }
            }
        }
    }

    Invoke-ContractCase -Name "prepared-and-workspace-path-boundary" -Action {
        $cache = Join-Path $temporaryRoot "prepared workspace boundary cache"
        $repositoryRoot = Join-Path $temporaryRoot "prepared workspace boundary source"
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        Set-ContractFile -Path (Join-Path $repositoryRoot "vcpkg.json") -Value "{}"
        $location = Get-PrivateEnvironmentLocation -RepositoryRoot $repositoryRoot `
            -Fingerprint ("c" * 64) -Configuration $configuration -CacheRoot $cache
        $vendor = Join-Path $location.EnvironmentRoot "cargo-vendor"
        $installed = Join-Path $location.EnvironmentRoot "setup/vcpkg/installed"
        $vcpkgRoot = Join-Path $location.EnvironmentRoot "vcpkg/scripts"
        $toolchain = Join-Path $vcpkgRoot "scripts/buildsystems/vcpkg.cmake"
        Set-ContractFile -Path (Join-Path $vendor "contract-crate/Cargo.toml") `
            -Value "[package]`nname='contract-crate'`nversion='1.0.0'"
        Set-ContractFile -Path (Join-Path $installed "x64-windows-static-md/lib/contract.lib") `
            -Value "native"
        Set-ContractFile -Path $toolchain -Value "# shared vcpkg toolchain"

        $cargoLayout = Invoke-PrivateCommand -CommandName "New-EasyConCargoWorkspaceLayout" `
            -Parameters @{
                WorkspaceRoot = $location.WorkspaceRoot
                CacheRoot = $location.CacheRoot
                VendorRoot = $vendor
                EnvironmentRoot = $location.EnvironmentRoot
            }
        $vcpkgLayout = Invoke-PrivateCommand -CommandName "New-EasyConVcpkgWorkspaceLayout" `
            -Parameters @{
                Vcpkg = [pscustomobject]@{ Root = $vcpkgRoot; Toolchain = $toolchain }
                RepositoryRoot = $repositoryRoot
                WorkspaceRoot = $location.WorkspaceRoot
                CacheRoot = $location.CacheRoot
                EnvironmentRoot = $location.EnvironmentRoot
                InstalledRoot = $installed
            }
        $cargoConfig = Get-Content -Raw -LiteralPath $cargoLayout.Configuration
        $vcpkgWrapper = Get-Content -Raw -LiteralPath $vcpkgLayout.Toolchain
        Assert-Contract ($cargoLayout.CargoHome.StartsWith(
            $location.WorkspaceRoot, [System.StringComparison]::OrdinalIgnoreCase
        )) "Cargo home must be per-worktree writable state"
        Assert-Contract ($vcpkgLayout.Root.StartsWith(
            $location.WorkspaceRoot, [System.StringComparison]::OrdinalIgnoreCase
        )) "vcpkg wrapper must be per-worktree writable state"
        Assert-Contract ($cargoConfig.Contains($vendor.Replace("\", "/"))) `
            "workspace Cargo config must reference the shared vendor tree"
        Assert-Contract (-not $cargoConfig.Contains($repositoryRoot)) `
            "workspace Cargo config must not bind to the source worktree"
        Assert-Contract ($vcpkgWrapper.Contains($installed.Replace("\", "/"))) `
            "workspace vcpkg wrapper must reference the shared installed tree"
        Assert-Contract ($vcpkgWrapper.Contains($toolchain.Replace("\", "/"))) `
            "workspace vcpkg wrapper must include the shared scripts checkout"
        Assert-Contract (-not $vcpkgWrapper.Contains($repositoryRoot)) `
            "workspace vcpkg wrapper must not bind to the source worktree"

        $leak = Join-Path $installed "contract-leak.cmake"
        Set-ContractFile -Path $leak -Value "set(CONTRACT_SOURCE `"$repositoryRoot`")"
        Assert-Throws -Pattern "source worktree absolute path" -Action {
            Invoke-PrivateCommand -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                -Parameters @{
                    Path = $installed
                    TrustedRoot = $location.EnvironmentRoot
                    RepositoryRoot = $repositoryRoot
                    Description = "contract prepared installed tree"
                }
        }
        Remove-Item -LiteralPath $leak -Force

        $cleanText = Join-Path $installed "contract-clean.cmake.in"
        Set-ContractFile -Path $cleanText -Value "set(CONTRACT_MODE portable)"
        $cleanUtf8Bom = Join-Path $installed "contract-clean-utf8-bom"
        [System.IO.File]::WriteAllText(
            $cleanUtf8Bom,
            "portable UTF-8 contract text",
            [System.Text.UTF8Encoding]::new($true, $true)
        )
        $cleanUtf16Le = Join-Path $installed "contract-clean-utf16le.targets"
        [System.IO.File]::WriteAllText(
            $cleanUtf16Le,
            "portable UTF-16LE contract text",
            [System.Text.UnicodeEncoding]::new($false, $true, $true)
        )
        $cleanUtf16Be = Join-Path $installed "contract-clean-utf16be.template"
        [System.IO.File]::WriteAllText(
            $cleanUtf16Be,
            "portable UTF-16BE contract text",
            [System.Text.UnicodeEncoding]::new($true, $true, $true)
        )
        $binaryControl = Join-Path $installed "contract-binary.targets"
        [System.IO.File]::WriteAllBytes(
            $binaryControl,
            [byte[]](0x00, 0xff, 0x80, 0x43, 0x4f, 0x4e, 0x54, 0x52, 0x41, 0x43, 0x54)
        )
        Invoke-PrivateCommand `
            -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
            -Parameters @{
                Path = $installed
                TrustedRoot = $location.EnvironmentRoot
                RepositoryRoot = $repositoryRoot
                WritableRoot = $location.WritableRoot
                Description = "contract clean content-classified prepared tree"
            }

        $otherWritableTextPath = Join-Path $location.WritableRoot `
            "another-worktree/tmp/generated.txt"
        foreach ($textLeak in @(
            [pscustomobject]@{
                Path = Join-Path $installed "contract-leak.cmake.in"
                Value = "set(CONTRACT_SOURCE `"$repositoryRoot`")"
                Encoding = [System.Text.UTF8Encoding]::new($false, $true)
                Pattern = "source worktree absolute path"
            },
            [pscustomobject]@{
                Path = Join-Path $installed "contract-leak.targets"
                Value = "<Path>$otherWritableTextPath</Path>"
                Encoding = [System.Text.UnicodeEncoding]::new($false, $true, $true)
                Pattern = "writable absolute path"
            },
            [pscustomobject]@{
                Path = Join-Path $installed "contract-leak-extensionless"
                Value = "source=$repositoryRoot"
                Encoding = [System.Text.UTF8Encoding]::new($false, $true)
                Pattern = "source worktree absolute path"
            },
            [pscustomobject]@{
                Path = Join-Path $installed "contract-leak-utf8-bom.template"
                Value = ("x" * 16380) + $repositoryRoot
                Encoding = [System.Text.UTF8Encoding]::new($true, $true)
                Pattern = "source worktree absolute path"
            },
            [pscustomobject]@{
                Path = Join-Path $installed "contract-leak-utf16be.template"
                Value = "writable=$otherWritableTextPath"
                Encoding = [System.Text.UnicodeEncoding]::new($true, $true, $true)
                Pattern = "writable absolute path"
            }
        )) {
            [System.IO.File]::WriteAllText(
                $textLeak.Path,
                $textLeak.Value,
                $textLeak.Encoding
            )
            Assert-Throws -Pattern $textLeak.Pattern -Action {
                Invoke-PrivateCommand `
                    -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                    -Parameters @{
                        Path = $installed
                        TrustedRoot = $location.EnvironmentRoot
                        RepositoryRoot = $repositoryRoot
                        WritableRoot = $location.WritableRoot
                        Description = "contract content-classified prepared tree"
                    }
            }
            Remove-Item -LiteralPath $textLeak.Path -Force
        }

        $escapedSourceLeak = Join-Path $installed "contract-escaped-source.json"
        Set-ContractFile -Path $escapedSourceLeak -Value (
            [ordered]@{ generated = [ordered]@{ path = $repositoryRoot } } |
                ConvertTo-Json -Depth 4
        )
        Assert-Throws -Pattern "source worktree absolute path" -Action {
            Invoke-PrivateCommand -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                -Parameters @{
                    Path = $installed
                    TrustedRoot = $location.EnvironmentRoot
                    RepositoryRoot = $repositoryRoot
                    Description = "contract escaped prepared source tree"
                }
        }
        Remove-Item -LiteralPath $escapedSourceLeak -Force

        $allWritableRoot = Join-Path $cache "w"
        $otherWorktreeWrite = Join-Path $allWritableRoot "another-worktree/tmp/tool.exe"
        $escapedWritableLeak = Join-Path $installed "contract-escaped-writable.json"
        Set-ContractFile -Path $escapedWritableLeak -Value (
            [ordered]@{ generated = [ordered]@{ path = $otherWorktreeWrite } } |
                ConvertTo-Json -Depth 4
        )
        Assert-Throws -Pattern "source worktree absolute path|writable absolute path" -Action {
            Invoke-PrivateCommand -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                -Parameters @{
                    Path = $installed
                    TrustedRoot = $location.EnvironmentRoot
                    RepositoryRoot = $allWritableRoot
                    Description = "contract escaped prepared writable tree"
                }
        }
    }

    Invoke-ContractCase -Name "missing-damaged-and-mismatched-stamp" -Action {
        $cache = Join-Path $temporaryRoot "stamp cache"
        Assert-Throws -Pattern "not prepared.*Run:.*Mode Setup" -Action {
            Invoke-EasyConWindowsVerify -RepositoryRoot $repository `
                -ConfigurationPath $configurationPath -CacheRoot $cache
        }
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        $fingerprint = Get-PrivateEnvironmentFingerprint -RepositoryRoot $repository `
            -Configuration $configuration
        $location = Get-PrivateEnvironmentLocation -RepositoryRoot $repository `
            -Fingerprint $fingerprint.Value -Configuration $configuration -CacheRoot $cache
        Set-ContractFile -Path $location.StampPath -Value "{not-json"
        Assert-Throws -Pattern "stamp is damaged.*Rerun Setup" -Action {
            Invoke-EasyConWindowsVerify -RepositoryRoot $repository `
                -ConfigurationPath $configurationPath -CacheRoot $cache
        }
        $mismatch = [ordered]@{
            schemaVersion = 1
            fingerprint = ("0" * 64)
            workspaceKey = $location.WorkspaceKey
            environmentRoot = $location.EnvironmentRoot
            target = $configuration.target
        }
        Set-ContractFile -Path $location.StampPath -Value ($mismatch | ConvertTo-Json -Depth 4)
        Assert-Throws -Pattern "stamp is damaged|unsupported schema|does not match.*Rerun Setup" -Action {
            Invoke-EasyConWindowsVerify -RepositoryRoot $repository `
                -ConfigurationPath $configurationPath -CacheRoot $cache
        }
    }

    Invoke-ContractCase -Name "stamp-v2-json-schema-is-strict" -Action {
        $cache = Join-Path $temporaryRoot "strict stamp cache"
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        $fingerprint = Get-PrivateEnvironmentFingerprint -RepositoryRoot $repository `
            -Configuration $configuration
        $location = Get-PrivateEnvironmentLocation -RepositoryRoot $repository `
            -Fingerprint $fingerprint.Value -Configuration $configuration -CacheRoot $cache
        $tool = Join-Path $location.EnvironmentRoot "tools/cmake.exe"
        Set-ContractFile -Path $tool -Value "strict stamp fixture"
        $toolHash = (Get-FileHash -LiteralPath $tool -Algorithm SHA256).Hash.ToLowerInvariant()
        $baseStamp = New-ContractEnvironmentStamp -Location $location `
            -Fingerprint $fingerprint -Configuration $configuration -Tools @(
                [ordered]@{
                    name = "cmake"
                    path = $tool
                    sha256 = $toolHash
                    controlled = $true
                }
            )
        $baseText = $baseStamp | ConvertTo-Json -Depth 32

        $stringSchema = $baseText | ConvertFrom-Json -Depth 32 -DateKind String
        $stringSchema.schemaVersion = "2"
        $fractionalFiles = $baseText | ConvertFrom-Json -Depth 32 -DateKind String
        $fractionalFiles.nativeTree.files = 1.5
        $stringBoolean = $baseText | ConvertFrom-Json -Depth 32 -DateKind String
        $stringBoolean.tools[0].controlled = "true"
        $unknownVersion = $baseText | ConvertFrom-Json -Depth 32 -DateKind String
        $unknownVersion.versions | Add-Member -NotePropertyName unknown -NotePropertyValue "damaged"
        $missingPath = $baseText | ConvertFrom-Json -Depth 32 -DateKind String
        $missingPath.paths.PSObject.Properties.Remove("ocrModel")
        $wrongTools = $baseText | ConvertFrom-Json -Depth 32 -DateKind String
        $wrongTools.tools = [ordered]@{}
        $mutations = @(
            [pscustomobject]@{
                Label = "duplicate root key"
                Pattern = "duplicate key 'schemaVersion'"
                Text = $baseText.Replace(
                    '"schemaVersion": 2',
                    '"schemaVersion": 2, "schemaVersion": 2'
                )
            },
            [pscustomobject]@{
                Label = "duplicate nested tool key"
                Pattern = "duplicate key 'name'"
                Text = $baseText.Replace('"name": "cmake"', '"name": "cmake", "name": "cmake"')
            },
            [pscustomobject]@{
                Label = "string schema number"
                Pattern = "schemaVersion must be .*JSON integer"
                Text = $stringSchema | ConvertTo-Json -Depth 32
            },
            [pscustomobject]@{
                Label = "fractional tree file count"
                Pattern = "nativeTree files must be a positive JSON integer"
                Text = $fractionalFiles | ConvertTo-Json -Depth 32
            },
            [pscustomobject]@{
                Label = "string Boolean"
                Pattern = "tool controlled must be a JSON Boolean"
                Text = $stringBoolean | ConvertTo-Json -Depth 32
            },
            [pscustomobject]@{
                Label = "unknown versions field"
                Pattern = "versions keys must be exactly"
                Text = $unknownVersion | ConvertTo-Json -Depth 32
            },
            [pscustomobject]@{
                Label = "missing prepared path field"
                Pattern = "paths keys must be exactly"
                Text = $missingPath | ConvertTo-Json -Depth 32
            },
            [pscustomobject]@{
                Label = "wrong tools JSON type"
                Pattern = "tools must be a JSON array"
                Text = $wrongTools | ConvertTo-Json -Depth 32
            }
        )
        foreach ($mutation in $mutations) {
            Set-ContractFile -Path $location.StampPath -Value $mutation.Text
            Assert-Throws -Pattern $mutation.Pattern -Action {
                Invoke-EasyConWindowsVerify -RepositoryRoot $repository `
                    -ConfigurationPath $configurationPath -CacheRoot $cache
            }
        }
    }

    Invoke-ContractCase -Name "stamp-tools-reject-source-and-all-writable-worktrees" -Action {
        $cache = Join-Path $temporaryRoot "stamp tool boundary cache"
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        $fingerprint = Get-PrivateEnvironmentFingerprint -RepositoryRoot $repository `
            -Configuration $configuration
        $location = Get-PrivateEnvironmentLocation -RepositoryRoot $repository `
            -Fingerprint $fingerprint.Value -Configuration $configuration -CacheRoot $cache
        $preparedTool = Join-Path $location.EnvironmentRoot "tools/contract.exe"
        Set-ContractFile -Path $preparedTool -Value "prepared contract tool"
        $preparedHash = (Get-FileHash -LiteralPath $preparedTool -Algorithm SHA256).Hash.ToLowerInvariant()
        $controlledNames = @("cmake", "ninja", "7zip", "7zr", "vcpkg")
        $expectedNames = @(
            "python", "7zip", "7zr", "cargo", "cl", "cmake", "git", "lib", "link",
            "mt", "ninja", "pwsh", "rc", "rustup", "vcpkg"
        )
        $otherWritableTool = Join-Path $cache "w/another-worktree/tools/python.exe"
        Set-ContractFile -Path $otherWritableTool -Value "writable contract tool"
        foreach ($forbidden in @(
            [pscustomobject]@{
                Label = "source repository"
                Path = Join-Path $repository.Path "tools/windows_workspace.psm1"
                Pattern = "source repository"
            },
            [pscustomobject]@{
                Label = "another worktree writable root"
                Path = $otherWritableTool
                Pattern = "writable workspace path"
            }
        )) {
            $forbiddenHash = (Get-FileHash -LiteralPath $forbidden.Path -Algorithm SHA256).Hash.ToLowerInvariant()
            $records = [System.Collections.Generic.List[object]]::new()
            foreach ($name in $expectedNames) {
                $path = if ($name -ceq "python") { $forbidden.Path } else { $preparedTool }
                $hash = if ($name -ceq "python") { $forbiddenHash } else { $preparedHash }
                $records.Add([ordered]@{
                    name = $name
                    path = $path
                    sha256 = $hash
                    controlled = $controlledNames -ccontains $name
                }) | Out-Null
            }
            $stamp = New-ContractEnvironmentStamp -Location $location `
                -Fingerprint $fingerprint -Configuration $configuration -Tools $records.ToArray()
            Set-ContractFile -Path $location.StampPath -Value ($stamp | ConvertTo-Json -Depth 32)
            Assert-Throws -Pattern $forbidden.Pattern -Action {
                Invoke-EasyConWindowsVerify -RepositoryRoot $repository `
                    -ConfigurationPath $configurationPath -CacheRoot $cache
            }
        }
    }

    Invoke-ContractCase -Name "damaged-controlled-tool-rejected" -Action {
        $cache = Join-Path $temporaryRoot "artifact cache"
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        $fingerprint = Get-PrivateEnvironmentFingerprint -RepositoryRoot $repository `
            -Configuration $configuration
        $location = Get-PrivateEnvironmentLocation -RepositoryRoot $repository `
            -Fingerprint $fingerprint.Value -Configuration $configuration -CacheRoot $cache
        $tool = Join-Path $location.EnvironmentRoot "tools/cmake.exe"
        Set-ContractFile -Path $tool -Value "damaged"
        $stamp = New-ContractEnvironmentStamp -Location $location `
            -Fingerprint $fingerprint -Configuration $configuration -Tools @(
                [ordered]@{
                name = "cmake"
                path = $tool
                sha256 = ("0" * 64)
                controlled = $true
                }
            )
        Set-ContractFile -Path $location.StampPath -Value ($stamp | ConvertTo-Json -Depth 32)
        Assert-Throws -Pattern "prepared tool cmake is damaged.*Rerun Setup" -Action {
            Invoke-EasyConWindowsVerify -RepositoryRoot $repository `
                -ConfigurationPath $configurationPath -CacheRoot $cache
        }
    }

    Invoke-ContractCase -Name "workspace-plan-does-not-provision" -Action {
        $gates = [System.Collections.Generic.List[object]]::new()
        Invoke-PrivateWorkspaceGates -RepositoryRoot $repository -GateInvoker {
            param($Name, $Program, $Arguments, $Root)
            $gates.Add([pscustomobject]@{
                Name = $Name
                Program = $Program
                Arguments = @($Arguments)
                Root = $Root
            }) | Out-Null
        }
        $cargoGates = @($gates | Where-Object { $_.Name -like "cargo *" })
        Assert-Contract ($cargoGates.Count -eq 4) "Workspace must retain the four Rust gates"
        foreach ($gate in $cargoGates | Where-Object { $_.Name -notlike "cargo fmt*" }) {
            Assert-Contract ($gate.Arguments -ccontains "--locked") `
                "Cargo resolution must use the tracked lockfile"
            Assert-Contract ($gate.Arguments -cnotcontains "--offline") `
                "Workspace duty separation must not be described as an offline contract"
        }
        Assert-Contract (-not @($gates | Where-Object {
            $_.Name -match "install|download|provision|vcpkg"
        })) "Workspace gates must not provision, install, or download"
    }

    Invoke-ContractCase -Name "require-clean-tree-detects-ignored-python-bytecode" -Action {
        $git = (Get-Command git.exe -ErrorAction Stop).Source
        $cleanRepository = Join-Path $temporaryRoot "ignored bytecode clean tree"
        Set-ContractFile -Path (Join-Path $cleanRepository ".gitignore") `
            -Value "__pycache__/`n*.pyc`n"
        Set-ContractFile -Path (Join-Path $cleanRepository "tools/contract.py") `
            -Value "print('contract')`n"
        & $git init --quiet $cleanRepository
        & $git -C $cleanRepository add -- .
        & $git -c user.name=EasyConContract -c user.email=contract@example.invalid `
            -C $cleanRepository commit --quiet -m "contract fixture"
        Assert-Contract ($LASTEXITCODE -eq 0) `
            "the ignored bytecode fixture must start as a clean Git checkout"
        $bytecode = Join-Path $cleanRepository "tools/__pycache__/contract.cpython-312.pyc"
        $gateState = [pscustomobject]@{ Writes = 0 }
        $gateInvoker = {
            param($Name, $Program, $Arguments, $Root)
            $null = $Name, $Program, $Arguments, $Root
            if ($gateState.Writes -eq 0) {
                Set-ContractFile -Path $bytecode -Value "ignored generated bytecode"
                $gateState.Writes++
            }
        }.GetNewClosure()
        Assert-Throws -Pattern "ignored Python bytecode|source tree" -Action {
            Invoke-PrivateWorkspaceGates -RepositoryRoot $cleanRepository `
                -GateInvoker $gateInvoker -RequireCleanTree
        }
        Assert-Contract ($gateState.Writes -eq 1) `
            "the clean-tree fixture must create exactly one ignored bytecode artifact"
    }

    Invoke-ContractCase -Name "pinned-bootstrap-tool-remains-after-download-consumption" -Action {
        $source = Join-Path $temporaryRoot "bootstrap-download.exe"
        $destination = Join-Path $temporaryRoot "tools/7zr-1.0/7zr.exe"
        Set-ContractFile -Path $source -Value "pinned bootstrap executable"
        $sha512 = (Get-FileHash -LiteralPath $source -Algorithm SHA512).Hash.ToLowerInvariant()
        $installed = Install-PrivatePinnedExecutable -Source $source `
            -Destination $destination -Sha512 $sha512 -TrustedRoot $temporaryRoot `
            -Description "contract bootstrap tool"
        Remove-Item -LiteralPath $source -Force
        Assert-Contract (Test-Path -LiteralPath $installed -PathType Leaf) `
            "the stamped tool must survive when vcpkg consumes its download input"
        Assert-Contract (
            (Get-FileHash -LiteralPath $installed -Algorithm SHA512).Hash.ToLowerInvariant() `
                -ceq $sha512
        ) "the persistent tool copy must retain the audited hash"
    }

    Invoke-ContractCase -Name "locked-pinned-download-preserves-primary-failure" -Action {
        $destination = Join-Path $temporaryRoot "downloads/pinned-tool.zip"
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination) | Out-Null
        $payload = [System.Text.Encoding]::UTF8.GetBytes("complete pinned download")
        $expectedHash = [Convert]::ToHexString(
            [System.Security.Cryptography.SHA512]::HashData($payload)
        ).ToLowerInvariant()
        $state = [pscustomobject]@{ Handle = $null; Temporary = $null }
        $failingCapture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Description, $WorkingDirectory, $StreamOutput
            $outputIndex = [Array]::IndexOf([string[]]$Arguments, "--output")
            $state.Temporary = [string]$Arguments[$outputIndex + 1]
            [System.IO.File]::WriteAllBytes($state.Temporary, [byte[]](1, 2, 3))
            $state.Handle = [System.IO.File]::Open(
                $state.Temporary,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::Read,
                [System.IO.FileShare]::None
            )
            throw [System.IO.IOException]::new("synthetic pinned download primary failure")
        }.GetNewClosure()

        $failure = $null
        try {
            try {
                Invoke-PrivateCommandWithNativeCapture `
                    -CommandName "Get-EasyConPinnedDownload" -Parameters @{
                        Destination = $destination
                        Url = "https://example.invalid/pinned-tool.zip"
                        Sha512 = $expectedHash
                        TrustedRoot = $temporaryRoot
                        Description = "contract pinned tool"
                    } -NativeCapture $failingCapture
            }
            catch {
                $failure = $_
            }
            Assert-Contract ($null -ne $failure) "locked pinned download must fail"
            Assert-Contract (
                $failure.Exception.Message -match "synthetic pinned download primary failure"
            ) "pinned download cleanup must not replace the primary failure; got: $($failure.Exception.Message)"
            Assert-Contract (
                $failure.Exception.Message -match "cleanup" -and
                $failure.Exception.Message -match "temporary.*remains|residual"
            ) "pinned download failure must report cleanup and residual state"
            Assert-Contract (-not (Test-Path -LiteralPath $destination)) `
                "a locked temporary must never be accepted as the pinned destination"
            Assert-Contract (Test-Path -LiteralPath $state.Temporary -PathType Leaf) `
                "an externally locked download temporary may remain after cleanup"
        }
        finally {
            if ($null -ne $state.Handle) {
                $state.Handle.Dispose()
            }
        }

        $successfulCapture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Description, $WorkingDirectory, $StreamOutput
            $outputIndex = [Array]::IndexOf([string[]]$Arguments, "--output")
            [System.IO.File]::WriteAllBytes(
                [string]$Arguments[$outputIndex + 1],
                $payload
            )
        }.GetNewClosure()
        $downloaded = Invoke-PrivateCommandWithNativeCapture `
            -CommandName "Get-EasyConPinnedDownload" -Parameters @{
                Destination = $destination
                Url = "https://example.invalid/pinned-tool.zip"
                Sha512 = $expectedHash
                TrustedRoot = $temporaryRoot
                Description = "contract pinned tool"
            } -NativeCapture $successfulCapture
        Assert-Contract ($downloaded -ceq $destination) `
            "a later pinned download must recover after the handle is released"
        Assert-Contract (
            (Get-FileHash -LiteralPath $destination -Algorithm SHA512).Hash.ToLowerInvariant() `
                -ceq $expectedHash
        ) "the recovered pinned download must pass its frozen hash"
        Remove-Item -LiteralPath $state.Temporary -Force
    }

    Invoke-ContractCase -Name "locked-vcpkg-asset-download-preserves-primary-failure" -Action {
        $cache = Join-Path $temporaryRoot "vcpkg asset cache"
        $payload = [System.Text.Encoding]::UTF8.GetBytes("complete vcpkg asset")
        $expectedHash = [Convert]::ToHexString(
            [System.Security.Cryptography.SHA256]::HashData($payload)
        ).ToLowerInvariant()
        $configuration = [pscustomobject]@{
            vcpkg = [pscustomobject]@{
                toolRelease = "contract"
                windowsAsset = [pscustomobject]@{
                    url = "https://example.invalid/vcpkg.exe"
                    bytes = $payload.Length
                    sha256 = $expectedHash
                }
            }
        }
        $state = [pscustomobject]@{ Handle = $null; Temporary = $null }
        $failingCapture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Description, $WorkingDirectory, $StreamOutput
            $outputIndex = [Array]::IndexOf([string[]]$Arguments, "--output")
            $state.Temporary = [string]$Arguments[$outputIndex + 1]
            [System.IO.File]::WriteAllBytes($state.Temporary, [byte[]](4, 5, 6))
            $state.Handle = [System.IO.File]::Open(
                $state.Temporary,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::Read,
                [System.IO.FileShare]::None
            )
            throw [System.IO.IOException]::new("synthetic vcpkg asset primary failure")
        }.GetNewClosure()

        $failure = $null
        try {
            try {
                Invoke-PrivateCommandWithNativeCapture -CommandName "Get-EasyConVcpkgAsset" `
                    -Parameters @{
                        CacheRoot = $cache
                        Configuration = $configuration
                    } -NativeCapture $failingCapture
            }
            catch {
                $failure = $_
            }
            Assert-Contract ($null -ne $failure) "locked vcpkg asset download must fail"
            Assert-Contract (
                $failure.Exception.Message -match "synthetic vcpkg asset primary failure"
            ) "vcpkg asset cleanup must not replace the primary failure; got: $($failure.Exception.Message)"
            Assert-Contract (
                $failure.Exception.Message -match "cleanup" -and
                $failure.Exception.Message -match "temporary.*remains|residual"
            ) "vcpkg asset failure must report cleanup and residual state"
            $cachedAsset = Join-Path $cache "downloads/vcpkg-contract-windows.exe"
            Assert-Contract (-not (Test-Path -LiteralPath $cachedAsset)) `
                "a locked vcpkg temporary must never be accepted as the cached asset"
            Assert-Contract (Test-Path -LiteralPath $state.Temporary -PathType Leaf) `
                "an externally locked vcpkg temporary may remain after cleanup"
        }
        finally {
            if ($null -ne $state.Handle) {
                $state.Handle.Dispose()
            }
        }

        $successfulCapture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Description, $WorkingDirectory, $StreamOutput
            $outputIndex = [Array]::IndexOf([string[]]$Arguments, "--output")
            [System.IO.File]::WriteAllBytes(
                [string]$Arguments[$outputIndex + 1],
                $payload
            )
        }.GetNewClosure()
        $asset = Invoke-PrivateCommandWithNativeCapture -CommandName "Get-EasyConVcpkgAsset" `
            -Parameters @{
                CacheRoot = $cache
                Configuration = $configuration
            } -NativeCapture $successfulCapture
        Assert-Contract (
            (Get-FileHash -LiteralPath $asset -Algorithm SHA256).Hash.ToLowerInvariant() `
                -ceq $expectedHash
        ) "a later vcpkg asset download must recover with the frozen hash"
        Remove-Item -LiteralPath $state.Temporary -Force
    }

    Invoke-ContractCase -Name "locked-stamp-temporary-preserves-primary-failure" -Action {
        $environment = Join-Path $temporaryRoot "stamp publish environment"
        New-Item -ItemType Directory -Force -Path $environment | Out-Null
        $stamp = Join-Path $environment "environment-stamp.json"
        $state = [pscustomobject]@{ Handle = $null; Temporary = $null }
        $failingMove = {
            param($Source, $Destination)
            $null = $Destination
            $state.Temporary = $Source
            $state.Handle = [System.IO.File]::Open(
                $Source,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::Read,
                [System.IO.FileShare]::None
            )
            throw [System.IO.IOException]::new("synthetic stamp publish primary failure")
        }.GetNewClosure()

        $failure = $null
        try {
            try {
                Invoke-PrivateCommand -CommandName "Write-EasyConEnvironmentStamp" `
                    -Parameters @{
                        Path = $stamp
                        Value = [ordered]@{ status = "partial" }
                        EnvironmentRoot = $environment
                        MoveAction = $failingMove
                    }
            }
            catch {
                $failure = $_
            }
            Assert-Contract ($null -ne $failure) "locked stamp publish must fail"
            Assert-Contract (
                $failure.Exception.Message -match "synthetic stamp publish primary failure"
            ) "stamp cleanup must preserve the primary failure; got: $($failure.Exception.Message)"
            Assert-Contract (
                $failure.Exception.Message -match "cleanup" -and
                $failure.Exception.Message -match "temporary.*remains|residual"
            ) "stamp failure must report cleanup and residual state"
            Assert-Contract (-not (Test-Path -LiteralPath $stamp)) `
                "a locked temporary stamp must never become the published Verify input"
            Assert-Contract (Test-Path -LiteralPath $state.Temporary -PathType Leaf) `
                "an externally locked stamp temporary may remain after cleanup"
        }
        finally {
            if ($null -ne $state.Handle) {
                $state.Handle.Dispose()
            }
        }

        Invoke-PrivateCommand -CommandName "Write-EasyConEnvironmentStamp" -Parameters @{
            Path = $stamp
            Value = [ordered]@{ status = "ready" }
            EnvironmentRoot = $environment
        }
        $published = Get-Content -Raw -LiteralPath $stamp | ConvertFrom-Json
        Assert-Contract ([string]$published.status -ceq "ready") `
            "a later stamp write must recover with complete published JSON"
        Remove-Item -LiteralPath $state.Temporary -Force
    }

    Invoke-ContractCase -Name "msvc-developer-shell-reinitializes-after-sanitization" -Action {
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        Initialize-PrivateMsvcEnvironment `
            -MsvcToolsVersion ([string]$configuration.hostTools.msvcToolsVersion) `
            -WindowsSdkVersion ([string]$configuration.hostTools.windowsSdkVersion) | Out-Null
        $systemRoot = [Environment]::GetEnvironmentVariable("SystemRoot", "Process")
        Set-PrivateVerifiedProcessEnvironment `
            -PathDirectories @($PSHOME, $systemRoot, (Join-Path $systemRoot "System32")) `
            -Variables ([ordered]@{ CARGO_INCREMENTAL = "0" })
        $reinitialized = Initialize-PrivateMsvcEnvironment `
            -MsvcToolsVersion ([string]$configuration.hostTools.msvcToolsVersion) `
            -WindowsSdkVersion ([string]$configuration.hostTools.windowsSdkVersion)
        Assert-Contract ($env:VSCMD_ARG_HOST_ARCH -ceq "x64") `
            "sanitization must allow the pinned x64 host Developer Shell to initialize again"
        Assert-Contract ($env:VSCMD_ARG_TGT_ARCH -ceq "x64") `
            "sanitization must allow the pinned x64 target Developer Shell to initialize again"
        Assert-Contract (Test-Path -LiteralPath $reinitialized.Tools.'cl.exe' -PathType Leaf) `
            "reinitialized Developer Shell must resolve the pinned compiler"
    }

    Invoke-ContractCase -Name "verified-child-process-environment-is-sanitized" -Action {
        $poisonedNames = @(
            "RUSTC", "RUSTC_BOOTSTRAP", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTFLAGS",
            "RUSTUP_HOME", "CARGO_ENCODED_RUSTFLAGS", "CARGO_NET_OFFLINE",
            "CARGO_REGISTRIES_CRATES_IO_INDEX", "CC_X86_64_PC_WINDOWS_MSVC",
            "CXX_X86_64_PC_WINDOWS_MSVC", "AR_X86_64_PC_WINDOWS_MSVC", "HOST_CC",
            "TARGET_CXX", "CL", "_CL_", "CMAKE_PREFIX_PATH", "OPENSSL_ROOT_DIR",
            "VCPKG_DEFAULT_TRIPLET", "VSCMD_VER", "__VSCMD_PREINIT_PATH",
            "VCPKG_FORCE_DOWNLOADED_BINARIES", "VCPKG_FORCE_SYSTEM_BINARIES",
            "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "TMPDIR",
            "PYTHONPYCACHEPREFIX", "PYTHONDONTWRITEBYTECODE"
        )
        $controlledValues = [ordered]@{
            AR = "controlled-lib.exe"
            CC = "controlled-cl.exe"
            CARGO_HOME = Join-Path $temporaryRoot "controlled cargo home"
            CARGO_TARGET_DIR = Join-Path $temporaryRoot "controlled target"
            CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = "controlled-link.exe"
            CARGO_INCREMENTAL = "0"
            CXX = "controlled-cl.exe"
            INCLUDE = Join-Path $temporaryRoot "controlled include"
            LIB = Join-Path $temporaryRoot "controlled lib"
            LIBPATH = Join-Path $temporaryRoot "controlled libpath"
            RUSTUP_HOME = Join-Path $temporaryRoot "controlled rustup home"
            RUSTUP_TOOLCHAIN = "1.97.1"
            VCPKG_ROOT = Join-Path $temporaryRoot "controlled vcpkg"
            VCPKG_BINARY_SOURCES = "clear"
            VCPKG_FORCE_DOWNLOADED_BINARIES = "1"
        }
        $saved = @{}
        foreach ($name in @($poisonedNames + @($controlledValues.Keys) + @("PATH")) | Select-Object -Unique) {
            $saved[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        }
        try {
            foreach ($name in $poisonedNames) {
                [Environment]::SetEnvironmentVariable($name, "poison-$name", "Process")
            }
            Set-PrivateVerifiedProcessEnvironment -PathDirectories @($PSHOME) `
                -Variables $controlledValues
            $pwsh = Join-Path $PSHOME "pwsh.exe"
            $childScript = @'
[ordered]@{
    rustc = $env:RUSTC
    rustcBootstrap = $env:RUSTC_BOOTSTRAP
    rustcWrapper = $env:RUSTC_WRAPPER
    workspaceWrapper = $env:RUSTC_WORKSPACE_WRAPPER
    cl = $env:CL
    underscoreCl = $env:_CL_
    cmakePrefix = $env:CMAKE_PREFIX_PATH
    packageRoot = $env:OPENSSL_ROOT_DIR
    rustupHome = $env:RUSTUP_HOME
    cargoNetOffline = $env:CARGO_NET_OFFLINE
    cargoRegistry = $env:CARGO_REGISTRIES_CRATES_IO_INDEX
    targetCc = $env:CC_X86_64_PC_WINDOWS_MSVC
    targetCxx = $env:CXX_X86_64_PC_WINDOWS_MSVC
    targetAr = $env:AR_X86_64_PC_WINDOWS_MSVC
    hostCc = $env:HOST_CC
    targetCxxAlias = $env:TARGET_CXX
    vcpkgTriplet = $env:VCPKG_DEFAULT_TRIPLET
    vcpkgForceDownloaded = $env:VCPKG_FORCE_DOWNLOADED_BINARIES
    vcpkgForceSystem = $env:VCPKG_FORCE_SYSTEM_BINARIES
    vscmdVersion = $env:VSCMD_VER
    vscmdPreinitPath = $env:__VSCMD_PREINIT_PATH
    httpProxy = $env:HTTP_PROXY
    httpsProxy = $env:HTTPS_PROXY
    allProxy = $env:ALL_PROXY
    tmpdir = $env:TMPDIR
    pythonPycachePrefix = $env:PYTHONPYCACHEPREFIX
    pythonDontWriteBytecode = $env:PYTHONDONTWRITEBYTECODE
    ar = $env:AR
    cc = $env:CC
    cargoHome = $env:CARGO_HOME
    cargoTarget = $env:CARGO_TARGET_DIR
    targetLinker = $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER
    cxx = $env:CXX
    include = $env:INCLUDE
    lib = $env:LIB
    libpath = $env:LIBPATH
    path = $env:PATH
} | ConvertTo-Json -Compress
'@
            $child = (& $pwsh -NoLogo -NoProfile -Command $childScript) | ConvertFrom-Json
            foreach ($property in @(
                "rustc", "rustcBootstrap", "rustcWrapper", "workspaceWrapper", "cl",
                "underscoreCl", "cmakePrefix", "packageRoot", "cargoNetOffline",
                "cargoRegistry", "targetCc", "targetCxx", "targetAr", "hostCc", "targetCxxAlias",
                "vcpkgTriplet", "vcpkgForceSystem", "vscmdVersion", "vscmdPreinitPath", "httpProxy", "httpsProxy",
                "allProxy", "tmpdir", "pythonPycachePrefix", "pythonDontWriteBytecode"
            )) {
                Assert-Contract ([string]::IsNullOrEmpty([string]$child.$property)) `
                    "verified child process must not inherit $property"
            }
            foreach ($property in @(
                "ar", "cc", "cargoHome", "cargoTarget", "targetLinker", "cxx",
                "include", "lib", "libpath", "rustupHome", "vcpkgForceDownloaded"
            )) {
                $key = @{
                    ar = "AR"
                    cc = "CC"
                    cargoHome = "CARGO_HOME"
                    cargoTarget = "CARGO_TARGET_DIR"
                    targetLinker = "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"
                    cxx = "CXX"
                    include = "INCLUDE"
                    lib = "LIB"
                    libpath = "LIBPATH"
                    rustupHome = "RUSTUP_HOME"
                    vcpkgForceDownloaded = "VCPKG_FORCE_DOWNLOADED_BINARIES"
                }[$property]
                Assert-Contract ($child.$property -ceq $controlledValues[$key]) `
                    "verified child must receive controlled $key"
            }
            $childPathEntries = @($child.path -split [regex]::Escape(
                [System.IO.Path]::PathSeparator
            ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
            Assert-Contract (-not @($childPathEntries | Where-Object {
                -not $_.Equals($PSHOME, [System.StringComparison]::OrdinalIgnoreCase)
            })) "verified child PATH must contain only explicitly supplied directories"
        }
        finally {
            foreach ($entry in $saved.GetEnumerator()) {
                [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, "Process")
            }
        }
    }

    Invoke-ContractCase -Name "vcpkg-checkout-publish-rolls-back-partial-destination" -Action {
        $source = Join-Path $temporaryRoot "vcpkg publish source"
        $destination = Join-Path $temporaryRoot "vcpkg published"
        Set-ContractFile -Path (Join-Path $source "scripts/buildsystems/vcpkg.cmake") `
            -Value "complete checkout"
        $publishState = [pscustomobject]@{ Attempts = 0 }
        $failingMove = {
            param($PublishSource, $PublishDestination)
            $publishState.Attempts++
            New-Item -ItemType Directory -Force -Path $PublishDestination | Out-Null
            Set-ContractFile -Path (Join-Path $PublishDestination "partial.txt") -Value "partial"
            throw [System.IO.IOException]::new("synthetic Windows sharing violation")
        }.GetNewClosure()
        Assert-Throws -Pattern "sharing violation|publish" -Action {
            Publish-PrivateDirectoryAtomically -Parameters @{
                Source = $source
                Destination = $destination
                TrustedRoot = $temporaryRoot
                MaxAttempts = 2
                RetryMilliseconds = 0
                MoveAction = $failingMove
                RetryAction = { param($Attempt, $Failure) }
            }
        }
        Assert-Contract ($publishState.Attempts -eq 2) `
            "publish must use a finite, deterministic retry budget"
        Assert-Contract (-not (Test-Path -LiteralPath $destination)) `
            "failed publish must remove every partial destination"
        Assert-Contract (Test-Path -LiteralPath (
            Join-Path $source "scripts/buildsystems/vcpkg.cmake"
        ) -PathType Leaf) "failed atomic publish must preserve complete staging for cleanup or retry"

        Publish-PrivateDirectoryAtomically -Parameters @{
            Source = $source
            Destination = $destination
            TrustedRoot = $temporaryRoot
            RetryMilliseconds = 0
        } | Out-Null
        Assert-Contract (-not (Test-Path -LiteralPath $source)) `
            "successful atomic publish must consume staging"
        Assert-Contract (Test-Path -LiteralPath (
            Join-Path $destination "scripts/buildsystems/vcpkg.cmake"
        ) -PathType Leaf) "a later publish must recover with the complete checkout"
    }

    Invoke-ContractCase -Name "vcpkg-locked-partial-preserves-primary-failure" -Action {
        $source = Join-Path $temporaryRoot "vcpkg locked publish source"
        $destination = Join-Path $temporaryRoot "vcpkg locked published"
        Set-ContractFile -Path (Join-Path $source "scripts/buildsystems/vcpkg.cmake") `
            -Value "complete checkout"
        $publishState = [pscustomobject]@{
            Handle = $null
            Attempts = 0
        }
        $failingMove = {
            param($PublishSource, $PublishDestination)
            $publishState.Attempts++
            New-Item -ItemType Directory -Force -Path $PublishDestination | Out-Null
            $partial = Join-Path $PublishDestination "locked-partial.txt"
            Set-ContractFile -Path $partial -Value "partial"
            $publishState.Handle = [System.IO.File]::Open(
                $partial,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::Read,
                [System.IO.FileShare]::None
            )
            throw [System.IO.IOException]::new("synthetic primary publish failure")
        }.GetNewClosure()

        $failure = $null
        try {
            try {
                Publish-PrivateDirectoryAtomically -Parameters @{
                    Source = $source
                    Destination = $destination
                    TrustedRoot = $temporaryRoot
                    MaxAttempts = 2
                    RetryMilliseconds = 0
                    MoveAction = $failingMove
                    RetryAction = { param($Attempt, $Failure) }
                }
            }
            catch {
                $failure = $_
            }
            Assert-Contract ($null -ne $failure) "locked partial publish must fail"
            Assert-Contract ($failure.Exception.Message -match "synthetic primary publish failure") `
                "partial cleanup failure must not replace the primary publish failure"
            Assert-Contract (
                $failure.Exception.Message -match "cleanup" -and
                $failure.Exception.Message -match "destination remains|partial destination"
            ) "locked partial failure must report the incomplete cleanup state"
            Assert-Contract ($publishState.Attempts -eq 1) `
                "publish must stop when a partial destination cannot be removed"
            Assert-Contract (Test-Path -LiteralPath $destination -PathType Container) `
                "an externally locked partial destination may remain after bounded cleanup"
            Assert-Contract (Test-Path -LiteralPath (
                Join-Path $source "scripts/buildsystems/vcpkg.cmake"
            ) -PathType Leaf) "locked cleanup failure must preserve complete staging"
        }
        finally {
            if ($null -ne $publishState.Handle) {
                $publishState.Handle.Dispose()
            }
        }

        Remove-Item -LiteralPath $destination -Recurse -Force
        Publish-PrivateDirectoryAtomically -Parameters @{
            Source = $source
            Destination = $destination
            TrustedRoot = $temporaryRoot
            RetryMilliseconds = 0
        } | Out-Null
        Assert-Contract (Test-Path -LiteralPath (
            Join-Path $destination "scripts/buildsystems/vcpkg.cmake"
        ) -PathType Leaf) "a later Setup can clean the remainder and publish after handle release"
    }

    Invoke-ContractCase -Name "physical-path-reparse-rejected" -Action {
        $trusted = Join-Path $temporaryRoot "trusted root"
        $blocked = Join-Path $trusted "redirect"
        $leaf = Join-Path $blocked "output.bin"
        Assert-Throws -Pattern "reparse point" -Action {
            Assert-PrivatePhysicalPath -Path $leaf -TrustedRoot $trusted `
                -ReparsePointClassifier {
                    param($Candidate)
                    $Candidate.Equals($blocked, [System.StringComparison]::OrdinalIgnoreCase)
                }
        }
        Assert-Throws -Pattern "escaped its trusted root" -Action {
            Assert-PrivatePhysicalPath -Path (Join-Path $temporaryRoot "outside.bin") `
                -TrustedRoot $trusted -ReparsePointClassifier { $false }
        }
    }

    Invoke-ContractCase -Name "vcpkg-transient-cleanup-does-not-follow-reparse" -Action {
        $environment = Join-Path $temporaryRoot "transient cleanup environment"
        $buildtrees = Join-Path $environment "buildtrees"
        $external = Join-Path $temporaryRoot "transient cleanup external"
        New-Item -ItemType Directory -Path $buildtrees, $external | Out-Null
        Set-ContractFile -Path (Join-Path $buildtrees "ordinary/output.bin") -Value "delete"
        $externalMarker = Join-Path $external "keep.txt"
        Set-ContractFile -Path $externalMarker -Value "keep"
        $junction = Join-Path $buildtrees "source-link"
        New-Item -ItemType Junction -Path $junction -Target $external | Out-Null
        $junctionItem = Get-Item -Force -LiteralPath $junction
        Assert-Contract (
            ($junctionItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
        ) "the regression fixture must contain a real reparse point"

        Remove-PrivateTransientBuildTree -Path $buildtrees -TrustedRoot $environment
        Assert-Contract (-not (Test-Path -LiteralPath $buildtrees)) `
            "transient vcpkg buildtrees must be removed after a successful install"
        Assert-Contract (Test-Path -LiteralPath $externalMarker -PathType Leaf) `
            "transient cleanup must remove a junction without following its external target"
    }
}
catch {
    $contractFailure = $_
    throw
}
finally {
    $cleanupFailure = $null
    try {
        Remove-Module windows_workspace -ErrorAction Stop
        $resolvedTemporary = [System.IO.Path]::GetFullPath($temporaryRoot)
        $systemTemporary = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
        Assert-Contract ($resolvedTemporary.StartsWith(
            $systemTemporary, [System.StringComparison]::OrdinalIgnoreCase
        )) "temporary contract root must remain under the system temporary directory"
        Remove-Item -LiteralPath $resolvedTemporary -Recurse -Force -ErrorAction Stop
    }
    catch {
        $cleanupFailure = $_
    }
    if ($null -ne $cleanupFailure) {
        if ($null -ne $contractFailure) {
            $contractFailure.Exception.Data["EasyConContractCleanupFailure"] = `
                $cleanupFailure.Exception.ToString()
        }
        else {
            throw $cleanupFailure
        }
    }
}

Write-Output "Windows workspace contracts passed: 41 cases"

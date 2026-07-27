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

        [scriptblock]$GateInvoker
    )

    & $script:workspaceModule {
        param($Root, $Invoker)
        Invoke-EasyConWindowsWorkspaceGates -RepositoryRoot $Root -GateInvoker $Invoker
    } $RepositoryRoot $GateInvoker
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
        [Parameter(Mandatory)][string]$CacheRoot
    )
    Invoke-PrivateCommand -CommandName "Get-EasyConEnvironmentLocation" -Parameters @{
        RepositoryRoot = $RepositoryRoot
        Fingerprint = $Fingerprint
        CacheRoot = $CacheRoot
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
        Assert-Contract ($configuration.version -eq 3) "environment config schema must be v3"
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
                '"version": 3', '"version": 3, "version": 3'
            )
            "old schema" = $configurationText.Replace('"version": 3', '"version": 2')
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

    Invoke-ContractCase -Name "shared-rust-toolchain-retains-rustup-validation" -Action {
        $pin = [pscustomobject]@{
            Channel = "1.97.1"
            Target = "x86_64-pc-windows-msvc"
            Components = @("clippy", "rustfmt")
        }
        $state = [pscustomobject]@{
            Installs = 0
            FailuresRemaining = 2
            RustcChecks = 0
            TargetChecks = 0
            ComponentChecks = 0
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
        try {
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
                $state.ComponentChecks -eq 2
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

    Invoke-ContractCase -Name "worktree-environment-isolation" -Action {
        $cache = Join-Path $temporaryRoot "shared cache"
        $first = Get-PrivateEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree one") `
            -Fingerprint ("a" * 64) -CacheRoot $cache
        $firstAgain = Get-PrivateEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree one") `
            -Fingerprint ("a" * 64) -CacheRoot $cache
        $second = Get-PrivateEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree two") `
            -Fingerprint ("a" * 64) -CacheRoot $cache
        $changed = Get-PrivateEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree one") `
            -Fingerprint ("b" * 64) -CacheRoot $cache
        Assert-Contract ($first.EnvironmentRoot -ceq $firstAgain.EnvironmentRoot) `
            "one worktree and fingerprint must resolve stably"
        Assert-Contract ($first.EnvironmentRoot -cne $second.EnvironmentRoot) `
            "different worktrees must not share mutable build outputs"
        Assert-Contract ($first.EnvironmentRoot -cne $changed.EnvironmentRoot) `
            "a fingerprint change must select a new environment root"
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
            -Fingerprint $fingerprint.Value -CacheRoot $cache
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
        Assert-Throws -Pattern "does not match.*Rerun Setup" -Action {
            Invoke-EasyConWindowsVerify -RepositoryRoot $repository `
                -ConfigurationPath $configurationPath -CacheRoot $cache
        }
    }

    Invoke-ContractCase -Name "damaged-controlled-tool-rejected" -Action {
        $cache = Join-Path $temporaryRoot "artifact cache"
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        $fingerprint = Get-PrivateEnvironmentFingerprint -RepositoryRoot $repository `
            -Configuration $configuration
        $location = Get-PrivateEnvironmentLocation -RepositoryRoot $repository `
            -Fingerprint $fingerprint.Value -CacheRoot $cache
        $tool = Join-Path $location.EnvironmentRoot "tools/cmake.exe"
        Set-ContractFile -Path $tool -Value "damaged"
        $stamp = [ordered]@{
            schemaVersion = 1
            fingerprint = $fingerprint.Value
            workspaceKey = $location.WorkspaceKey
            environmentRoot = $location.EnvironmentRoot
            target = $configuration.target
            tools = @([ordered]@{
                name = "cmake"
                path = $tool
                sha256 = ("0" * 64)
                controlled = $true
            })
        }
        Set-ContractFile -Path $location.StampPath -Value ($stamp | ConvertTo-Json -Depth 8)
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
            "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"
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
                "allProxy"
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

Write-Output "Windows workspace contracts passed: 32 cases"

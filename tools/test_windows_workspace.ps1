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

try {
    Invoke-ContractCase -Name "strict-environment-configuration" -Action {
        $configurationText = Get-Content -Raw -LiteralPath $configurationPath
        $configuration = Get-EasyConWindowsBuildConfiguration -Path $configurationPath
        Assert-Contract ($configuration.version -eq 3) "environment config schema must be v3"
        Assert-Contract ($configuration.vcpkg.internalTools.Count -eq 4) `
            "CMake, Ninja, 7-Zip, and its 7zr bootstrap must be audited"
        Assert-Contract ($configuration.vcpkg.nativeDependencies.Count -eq 3) `
            "direct native dependencies must be audited"
        Assert-Contract ($configuration.fingerprintInputs.path -ccontains "Cargo.lock") `
            "Cargo.lock must invalidate the prepared dependency environment"
        Assert-Contract ($configuration.fingerprintInputs.path -ccontains "tools/windows_workspace.psm1") `
            "environment implementation changes must invalidate the prepared environment"

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
            "internal tool hash" = $configurationText.Replace(
                '55d3d891e8fc6c8ad7f92e172125319896761e57c5125944613d9bbfa5b9374387e9fc1468ad5bcb31464f43fb1c455ea251343942595f42955dc67090aa12ee',
                ('A' * 128)
            )
            "native tree" = $configurationText.Replace(
                '0e28cd8713d7b810bef28ed0d7859dd761215854',
                ('A' * 40)
            )
        }
        foreach ($entry in $mutations.GetEnumerator()) {
            $path = Join-Path $temporaryRoot ("configuration-{0}.json" -f $entry.Key)
            Set-ContractFile -Path $path -Value $entry.Value
            Assert-Throws -Pattern "config|version|target|fingerprint|SHA|git tree|duplicate" -Action {
                Get-EasyConWindowsBuildConfiguration -Path $path
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
        $publicLifecycleCommands = @(Get-Command -Module windows_workspace | Where-Object {
            $_.Name -match '^Invoke-EasyConWindows'
        } | Select-Object -ExpandProperty Name | Sort-Object)
        $expectedLifecycleCommands = @(
            "Invoke-EasyConWindowsSetup",
            "Invoke-EasyConWindowsVerify",
            "Invoke-EasyConWindowsWorkspace"
        )
        Assert-Contract (
            ($publicLifecycleCommands -join "`n") -ceq
            ($expectedLifecycleCommands -join "`n")
        ) "public Windows lifecycle surface must contain only Setup, Verify, and Workspace"

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
            Assert-Contract (Test-EasyConVcpkgVersionRecord -Record $record -Expected $expected) `
                "vcpkg audit must accept the $field record shape without reading absent fields"
        }
        $incomplete = [pscustomobject]@{ version = "1.2.3" }
        Assert-Contract (-not (Test-EasyConVcpkgVersionRecord `
            -Record $incomplete -Expected $expected)) `
            "vcpkg audit must reject a record without port-version or git-tree"
    }

    Invoke-ContractCase -Name "vcpkg-tool-manifest-record-shapes" -Action {
        $sevenZr = [pscustomobject]@{ name = "7zr"; os = "windows" }
        Assert-Contract (Test-EasyConVcpkgToolManifestRecord `
            -Record $sevenZr -Expected ([pscustomobject]@{ name = "7zr" })) `
            "7zr must be audited without reading an absent architecture"
        Assert-Contract (-not (Test-EasyConVcpkgToolManifestRecord `
            -Record $sevenZr -Expected ([pscustomobject]@{ name = "cmake" }))) `
            "an architecture-less record must not satisfy a normal x64 tool pin"
        $cmake = [pscustomobject]@{ name = "cmake"; os = "windows"; arch = "x64" }
        Assert-Contract (Test-EasyConVcpkgToolManifestRecord `
            -Record $cmake -Expected ([pscustomobject]@{ name = "cmake" })) `
            "ordinary Windows tools must retain an x64 architecture pin"
    }

    Invoke-ContractCase -Name "fingerprint-and-manifest-change" -Action {
        $configuration = Get-EasyConWindowsBuildConfiguration -Path $configurationPath
        $fixture = Join-Path $temporaryRoot "fingerprint repository"
        foreach ($relative in @($configuration.fingerprintInputs.path)) {
            Set-ContractFile -Path (Join-Path $fixture $relative) -Value "fixture:$relative"
        }
        $first = Get-EasyConEnvironmentFingerprint -RepositoryRoot $fixture `
            -Configuration $configuration
        $again = Get-EasyConEnvironmentFingerprint -RepositoryRoot $fixture `
            -Configuration $configuration
        Assert-Contract ($first.Value -ceq $again.Value) "unchanged inputs need one stable fingerprint"
        Add-Content -LiteralPath (Join-Path $fixture "vcpkg.json") -Value "changed" -Encoding utf8NoBOM
        $changed = Get-EasyConEnvironmentFingerprint -RepositoryRoot $fixture `
            -Configuration $configuration
        Assert-Contract ($first.Value -cne $changed.Value) `
            "a frozen manifest change must require a different prepared environment"
        Add-Content -LiteralPath (Join-Path $fixture "Cargo.lock") -Value "changed" -Encoding utf8NoBOM
        $lockChanged = Get-EasyConEnvironmentFingerprint -RepositoryRoot $fixture `
            -Configuration $configuration
        Assert-Contract ($changed.Value -cne $lockChanged.Value) `
            "a Cargo lock change must require a different prepared environment"
    }

    Invoke-ContractCase -Name "text-fingerprint-canonicalizes-checkout-line-endings" -Action {
        $configuration = Get-EasyConWindowsBuildConfiguration -Path $configurationPath
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
            $fingerprints[$fixture.Key] = (Get-EasyConEnvironmentFingerprint `
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
        $changed = Get-EasyConEnvironmentFingerprint -RepositoryRoot $changedRoot `
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
        $binaryCrLf = Get-EasyConEnvironmentFingerprint -RepositoryRoot $binaryRoot `
            -Configuration $binaryConfiguration
        [System.IO.File]::WriteAllBytes($binaryPath, [byte[]](0x61, 0x0a, 0x62))
        $binaryLf = Get-EasyConEnvironmentFingerprint -RepositoryRoot $binaryRoot `
            -Configuration $binaryConfiguration
        Assert-Contract ($binaryCrLf.Value -cne $binaryLf.Value) `
            "binary fingerprint inputs must hash raw bytes without newline normalization"

        [System.IO.File]::WriteAllBytes($binaryPath, [byte[]](0xc3, 0x28))
        $textConfiguration = [pscustomobject]@{
            fingerprintInputs = @([pscustomobject]@{ path = "input.bin"; kind = "text" })
        }
        Assert-Throws -Pattern "valid UTF-8" -Action {
            Get-EasyConEnvironmentFingerprint -RepositoryRoot $binaryRoot `
                -Configuration $textConfiguration
        }
    }

    Invoke-ContractCase -Name "worktree-environment-isolation" -Action {
        $cache = Join-Path $temporaryRoot "shared cache"
        $first = Get-EasyConEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree one") `
            -Fingerprint ("a" * 64) -CacheRoot $cache
        $firstAgain = Get-EasyConEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree one") `
            -Fingerprint ("a" * 64) -CacheRoot $cache
        $second = Get-EasyConEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree two") `
            -Fingerprint ("a" * 64) -CacheRoot $cache
        $changed = Get-EasyConEnvironmentLocation -RepositoryRoot (Join-Path $temporaryRoot "worktree one") `
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
        $configuration = Get-EasyConWindowsBuildConfiguration -Path $configurationPath
        $fingerprint = Get-EasyConEnvironmentFingerprint -RepositoryRoot $repository `
            -Configuration $configuration
        $location = Get-EasyConEnvironmentLocation -RepositoryRoot $repository `
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
        $configuration = Get-EasyConWindowsBuildConfiguration -Path $configurationPath
        $fingerprint = Get-EasyConEnvironmentFingerprint -RepositoryRoot $repository `
            -Configuration $configuration
        $location = Get-EasyConEnvironmentLocation -RepositoryRoot $repository `
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
        $installed = Install-EasyConPinnedExecutable -Source $source `
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

    Invoke-ContractCase -Name "msvc-developer-shell-reinitializes-after-sanitization" -Action {
        $configuration = Get-EasyConWindowsBuildConfiguration -Path $configurationPath
        Initialize-EasyConMsvcEnvironment `
            -MsvcToolsVersion ([string]$configuration.hostTools.msvcToolsVersion) `
            -WindowsSdkVersion ([string]$configuration.hostTools.windowsSdkVersion) | Out-Null
        $systemRoot = [Environment]::GetEnvironmentVariable("SystemRoot", "Process")
        Set-PrivateVerifiedProcessEnvironment `
            -PathDirectories @($PSHOME, $systemRoot, (Join-Path $systemRoot "System32")) `
            -Variables ([ordered]@{ CARGO_INCREMENTAL = "0" })
        $reinitialized = Initialize-EasyConMsvcEnvironment `
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
            RUSTUP_TOOLCHAIN = "1.97.1"
            VCPKG_ROOT = Join-Path $temporaryRoot "controlled vcpkg"
            VCPKG_BINARY_SOURCES = "clear"
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
                "underscoreCl", "cmakePrefix", "packageRoot", "rustupHome", "cargoNetOffline",
                "cargoRegistry", "targetCc", "targetCxx", "targetAr", "hostCc", "targetCxxAlias",
                "vcpkgTriplet", "vscmdVersion", "vscmdPreinitPath", "httpProxy", "httpsProxy",
                "allProxy"
            )) {
                Assert-Contract ([string]::IsNullOrEmpty([string]$child.$property)) `
                    "verified child process must not inherit $property"
            }
            foreach ($property in @(
                "ar", "cc", "cargoHome", "cargoTarget", "targetLinker", "cxx",
                "include", "lib", "libpath"
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
            Assert-EasyConPhysicalPath -Path $leaf -TrustedRoot $trusted `
                -ReparsePointClassifier {
                    param($Candidate)
                    $Candidate.Equals($blocked, [System.StringComparison]::OrdinalIgnoreCase)
                }
        }
        Assert-Throws -Pattern "escaped its trusted root" -Action {
            Assert-EasyConPhysicalPath -Path (Join-Path $temporaryRoot "outside.bin") `
                -TrustedRoot $trusted -ReparsePointClassifier { $false }
        }
    }
}
finally {
    Remove-Module windows_workspace -ErrorAction SilentlyContinue
    $resolvedTemporary = [System.IO.Path]::GetFullPath($temporaryRoot)
    $systemTemporary = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    Assert-Contract ($resolvedTemporary.StartsWith(
        $systemTemporary, [System.StringComparison]::OrdinalIgnoreCase
    )) "temporary contract root must remain under the system temporary directory"
    Remove-Item -LiteralPath $resolvedTemporary -Recurse -Force
}

Write-Output "Windows workspace contracts passed: 16 cases"

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

$modulePath = Join-Path $PSScriptRoot "windows_workspace.psm1"
$repository = Resolve-Path (Join-Path $PSScriptRoot "..")
$configurationPath = Join-Path $PSScriptRoot "windows_build_environment.json"
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
    "easycon environment contract {0}" -f [guid]::NewGuid().ToString("N")
)
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
Import-Module -Name $modulePath -Force

try {
    Invoke-ContractCase -Name "strict-environment-configuration" -Action {
        $configurationText = Get-Content -Raw -LiteralPath $configurationPath
        $configuration = Get-EasyConWindowsBuildConfiguration -Path $configurationPath
        Assert-Contract ($configuration.version -eq 2) "environment config schema must be v2"
        Assert-Contract ($configuration.vcpkg.internalTools.Count -eq 4) `
            "CMake, Ninja, 7-Zip, and its 7zr bootstrap must be audited"
        Assert-Contract ($configuration.vcpkg.nativeDependencies.Count -eq 3) `
            "direct native dependencies must be audited"
        Assert-Contract ($configuration.fingerprintFiles -ccontains "Cargo.lock") `
            "Cargo.lock must invalidate the prepared dependency environment"
        Assert-Contract ($configuration.fingerprintFiles -ccontains "tools/windows_workspace.psm1") `
            "environment implementation changes must invalidate the prepared environment"

        $mutations = [ordered]@{
            "duplicate key" = $configurationText.Replace(
                '"version": 2', '"version": 2, "version": 2'
            )
            "old schema" = $configurationText.Replace('"version": 2', '"version": 1')
            "unfrozen target" = $configurationText.Replace(
                '"x86_64-pc-windows-msvc"', '"x86_64-unknown-linux-gnu"'
            )
            "missing fingerprint input" = $configurationText.Replace(
                '    "tools/provision_vision_test_model.py"',
                '    "../outside.py"'
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
        foreach ($relative in @($configuration.fingerprintFiles)) {
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
        Invoke-EasyConWindowsWorkspaceGates -RepositoryRoot $repository -GateInvoker {
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

Write-Output "Windows workspace contracts passed: 10 cases"

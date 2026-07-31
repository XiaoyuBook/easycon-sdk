Set-StrictMode -Version Latest

function Resolve-EasyConFullPath {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [string]$BasePath = (Get-Location).Path
    )

    $fullPath = if ([System.IO.Path]::IsPathFullyQualified($Path)) {
        [System.IO.Path]::GetFullPath($Path)
    }
    else {
        [System.IO.Path]::GetFullPath((Join-Path $BasePath $Path))
    }
    $pathRoot = [System.IO.Path]::GetPathRoot($fullPath)
    if ($fullPath.Equals($pathRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $fullPath
    }
    return $fullPath.TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
}

function Test-EasyConPathWithin {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$Root
    )

    $resolvedPath = Resolve-EasyConFullPath -Path $Path
    $resolvedRoot = Resolve-EasyConFullPath -Path $Root
    if ($resolvedPath.Equals($resolvedRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $true
    }
    $prefix = $resolvedRoot + [System.IO.Path]::DirectorySeparatorChar
    return $resolvedPath.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)
}

function Assert-EasyConPhysicalPath {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [string]$TrustedRoot,

        [scriptblock]$ReparsePointClassifier
    )

    $resolved = Resolve-EasyConFullPath -Path $Path
    if (-not [string]::IsNullOrWhiteSpace($TrustedRoot)) {
        $trusted = Resolve-EasyConFullPath -Path $TrustedRoot
        if (-not (Test-EasyConPathWithin -Path $resolved -Root $trusted)) {
            throw "physical path escaped its trusted root: $resolved"
        }
    }

    $volumeRoot = [System.IO.Path]::GetPathRoot($resolved)
    $relative = $resolved.Substring($volumeRoot.Length).TrimStart(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
    $components = if ([string]::IsNullOrEmpty($relative)) {
        @()
    }
    else {
        @($relative -split '[\\/]')
    }
    $current = $volumeRoot
    $prefixes = @($volumeRoot)
    foreach ($component in $components) {
        $current = Join-Path $current $component
        $prefixes += $current
    }

    $ancestorMissing = $false
    foreach ($prefix in $prefixes) {
        if ($null -ne $ReparsePointClassifier) {
            if (& $ReparsePointClassifier $prefix) {
                throw "physical path contains a reparse point: $prefix"
            }
            continue
        }
        if ($ancestorMissing) {
            continue
        }
        try {
            $item = Get-Item -Force -LiteralPath $prefix -ErrorAction Stop
        }
        catch [System.Management.Automation.ItemNotFoundException] {
            $ancestorMissing = $true
            continue
        }
        catch {
            throw "cannot classify physical path component ${prefix}: $($_.Exception.Message)"
        }
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "physical path contains a reparse point: $prefix"
        }
    }
    return $resolved
}

function Get-EasyConPhysicalFile {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [string]$TrustedRoot,

        [scriptblock]$ReparsePointClassifier
    )

    $arguments = @{ Path = $Path; ReparsePointClassifier = $ReparsePointClassifier }
    if (-not [string]::IsNullOrWhiteSpace($TrustedRoot)) {
        $arguments.TrustedRoot = $TrustedRoot
    }
    $resolved = Assert-EasyConPhysicalPath @arguments
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
        throw "required physical file is missing: $resolved"
    }
    Assert-EasyConPhysicalPath @arguments | Out-Null
    return $resolved
}

function Read-EasyConPhysicalText {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [string]$TrustedRoot,

        [scriptblock]$ReparsePointClassifier
    )

    $arguments = @{ Path = $Path; ReparsePointClassifier = $ReparsePointClassifier }
    if (-not [string]::IsNullOrWhiteSpace($TrustedRoot)) {
        $arguments.TrustedRoot = $TrustedRoot
    }
    $resolved = Get-EasyConPhysicalFile @arguments
    Assert-EasyConPhysicalPath @arguments | Out-Null
    $text = [System.IO.File]::ReadAllText($resolved, [System.Text.Encoding]::UTF8)
    Assert-EasyConPhysicalPath @arguments | Out-Null
    return $text
}

function New-EasyConSafeDirectory {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$TrustedRoot
    )

    $resolved = Assert-EasyConPhysicalPath -Path $Path -TrustedRoot $TrustedRoot
    [System.IO.Directory]::CreateDirectory($resolved) | Out-Null
    Assert-EasyConPhysicalPath -Path $resolved -TrustedRoot $TrustedRoot | Out-Null
    return $resolved
}

function Assert-EasyConPhysicalTree {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$TrustedRoot,

        [scriptblock]$ReparsePointClassifier
    )

    $resolved = Assert-EasyConPhysicalPath -Path $Path -TrustedRoot $TrustedRoot `
        -ReparsePointClassifier $ReparsePointClassifier
    if (-not (Test-Path -LiteralPath $resolved -PathType Container)) {
        return $resolved
    }

    $pending = [System.Collections.Generic.Queue[string]]::new()
    $pending.Enqueue($resolved)
    while ($pending.Count -ne 0) {
        $directory = $pending.Dequeue()
        foreach ($entry in @(Get-ChildItem -Force -LiteralPath $directory -ErrorAction Stop)) {
            $isReparsePoint = if ($null -ne $ReparsePointClassifier) {
                & $ReparsePointClassifier $entry.FullName
            }
            else {
                ($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
            }
            if ($isReparsePoint) {
                throw "physical tree contains a reparse point: $($entry.FullName)"
            }
            if ($entry.PSIsContainer) {
                $pending.Enqueue($entry.FullName)
            }
        }
    }
    return $resolved
}

function Remove-EasyConSafeTree {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$TrustedRoot
    )

    $resolved = Assert-EasyConPhysicalTree -Path $Path -TrustedRoot $TrustedRoot
    if (Test-Path -LiteralPath $resolved) {
        Assert-EasyConPhysicalTree -Path $resolved -TrustedRoot $TrustedRoot | Out-Null
        Remove-Item -Force -Recurse -LiteralPath $resolved
    }
}

function Get-EasyConWorkspaceTargetDirectory {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$CacheRoot
    )

    $resolvedRepository = Resolve-EasyConFullPath -Path $RepositoryRoot
    $resolvedCache = Resolve-EasyConFullPath -Path $CacheRoot
    $normalized = $resolvedRepository.ToUpperInvariant()
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($normalized)
    $digest = [System.Security.Cryptography.SHA256]::HashData($bytes)
    $hash = [System.Convert]::ToHexString($digest).Substring(0, 16).ToLowerInvariant()
    return Join-Path $resolvedCache (Join-Path "w" $hash)
}

function Assert-EasyConCargoPathBudget {
    param(
        [Parameter(Mandatory)]
        [string]$CargoTargetDirectory
    )

    $projectedObjectDirectory = Join-Path $CargoTargetDirectory (
        "x86_64-pc-windows-msvc/debug/build/easycon-native-sys-0000000000000000/" +
        "out/cmake/CMakeFiles/CMakeScratch/TryCompile-000000/CMakeFiles/cmTC_00000.dir"
    )
    if ($projectedObjectDirectory.Length -gt 220) {
        throw "derived Cargo/CMake path exceeds the conservative Windows path budget; pass a shorter -CacheRoot"
    }
}

function Get-EasyConFileSha256 {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $resolved = Assert-EasyConPhysicalPath -Path $Path
    return (Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Assert-EasyConPinnedFile {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [long]$Bytes,

        [Parameter(Mandatory)]
        [string]$Sha256,

        [Parameter(Mandatory)]
        [string]$Description
    )

    $resolved = Assert-EasyConPhysicalPath -Path $Path
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
        throw "$Description is missing: $Path"
    }
    $item = Get-Item -Force -LiteralPath $resolved
    if ($item.Length -ne $Bytes) {
        throw "$Description has $($item.Length) bytes; expected $Bytes"
    }
    Assert-EasyConPhysicalPath -Path $resolved | Out-Null
    $actualHash = Get-EasyConFileSha256 -Path $resolved
    if ($actualHash -cne $Sha256.ToLowerInvariant()) {
        throw "$Description has SHA-256 $actualHash; expected $($Sha256.ToLowerInvariant())"
    }
}

function Get-EasyConFileHash {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [ValidateSet("SHA256", "SHA512")]
        [string]$Algorithm = "SHA256"
    )

    $resolved = Get-EasyConPhysicalFile -Path $Path
    return (Get-FileHash -LiteralPath $resolved -Algorithm $Algorithm).Hash.ToLowerInvariant()
}

function Remove-EasyConTransientBuildTree {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$TrustedRoot
    )

    $resolved = Assert-EasyConPhysicalPath -Path $Path -TrustedRoot $TrustedRoot
    if (-not (Test-Path -LiteralPath $resolved)) {
        return
    }
    $root = Get-Item -Force -LiteralPath $resolved -ErrorAction Stop
    if (-not $root.PSIsContainer) {
        throw "transient build tree is not a directory: $resolved"
    }

    # PowerShell removes child links themselves rather than traversing their targets. The
    # root and every ancestor remain reparse-free and confined to the trusted environment.
    Remove-Item -Force -Recurse -LiteralPath $resolved -ErrorAction Stop
    if (Test-Path -LiteralPath $resolved) {
        throw "transient build tree remains after cleanup: $resolved"
    }
}

function Get-EasyConKnownVcpkgTransientTrees {
    param(
        [Parameter(Mandatory)]
        [string]$EnvironmentRoot,

        [Parameter(Mandatory)]
        [string]$TrustedRoot
    )

    $environment = Assert-EasyConPhysicalPath -Path $EnvironmentRoot `
        -TrustedRoot $TrustedRoot
    $workspaceRoot = Join-Path $environment "setup/vcpkg"
    return @(
        Resolve-EasyConFullPath -Path (Join-Path $workspaceRoot "buildtrees")
        Resolve-EasyConFullPath -Path (Join-Path $workspaceRoot "packages")
    )
}

function Assert-EasyConContentFile {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [ValidateSet("SHA256", "SHA512")]
        [string]$Algorithm,

        [Parameter(Mandatory)]
        [string]$Hash,

        [long]$Bytes = -1,

        [Parameter(Mandatory)]
        [string]$TrustedRoot,

        [Parameter(Mandatory)]
        [string]$Description
    )

    $resolved = Get-EasyConPhysicalFile -Path $Path -TrustedRoot $TrustedRoot
    if ($Bytes -ge 0) {
        $actualBytes = (Get-Item -Force -LiteralPath $resolved).Length
        if ($actualBytes -ne $Bytes) {
            throw "$Description has $actualBytes bytes; expected $Bytes"
        }
    }
    $actualHash = Get-EasyConFileHash -Path $resolved -Algorithm $Algorithm
    if ($actualHash -cne $Hash) {
        $label = if ($Algorithm -ceq "SHA256") { "SHA-256" } else { "SHA-512" }
        throw "$Description has $label $actualHash; expected $Hash"
    }
    return $resolved
}

function Enter-EasyConExclusiveFileLease {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$TrustedRoot,

        [Parameter(Mandatory)]
        [string]$Description,

        [ValidateRange(0, 7200000)]
        [int]$TimeoutMilliseconds = 1800000,

        [ValidateRange(0, 1000)]
        [int]$RetryMilliseconds = 100
    )

    $trusted = Resolve-EasyConFullPath -Path $TrustedRoot
    New-EasyConSafeDirectory -Path $trusted -TrustedRoot $trusted | Out-Null
    $lockPath = Assert-EasyConPhysicalPath -Path $Path -TrustedRoot $trusted
    New-EasyConSafeDirectory -Path (Split-Path -Parent $lockPath) `
        -TrustedRoot $trusted | Out-Null
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $lastFailure = $null
    while ($true) {
        try {
            return [System.IO.FileStream]::new(
                $lockPath,
                [System.IO.FileMode]::OpenOrCreate,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None
            )
        }
        catch [System.IO.IOException] {
            $lastFailure = $_.Exception
        }
        if ($timer.ElapsedMilliseconds -ge $TimeoutMilliseconds) {
            throw "$Description is busy: $($lastFailure.Message)"
        }
        $remaining = $TimeoutMilliseconds - [int]$timer.ElapsedMilliseconds
        $delay = [Math]::Min($RetryMilliseconds, $remaining)
        if ($delay -gt 0) {
            Start-Sleep -Milliseconds $delay
        }
    }
}

function Enter-EasyConSharedCacheLease {
    param(
        [Parameter(Mandatory)]
        [string]$CacheRoot,

        [ValidateSet("Shared", "Exclusive")]
        [string]$Access,

        [ValidateRange(0, 7200000)]
        [int]$TimeoutMilliseconds = 1800000,

        [ValidateRange(0, 1000)]
        [int]$RetryMilliseconds = 100
    )

    $shared = Resolve-EasyConFullPath -Path (Join-Path $CacheRoot "caches")
    New-EasyConSafeDirectory -Path $shared -TrustedRoot $shared | Out-Null
    $lockPath = Assert-EasyConPhysicalPath `
        -Path (Join-Path $shared "locks/setup-assets.lock") -TrustedRoot $shared
    New-EasyConSafeDirectory -Path (Split-Path -Parent $lockPath) `
        -TrustedRoot $shared | Out-Null
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $lastFailure = $null
    while ($true) {
        try {
            if ($Access -ceq "Shared") {
                if (-not (Test-Path -LiteralPath $lockPath -PathType Leaf)) {
                    $initializer = [System.IO.FileStream]::new(
                        $lockPath,
                        [System.IO.FileMode]::OpenOrCreate,
                        [System.IO.FileAccess]::ReadWrite,
                        [System.IO.FileShare]::ReadWrite
                    )
                    $initializer.Dispose()
                }
                return [System.IO.FileStream]::new(
                    $lockPath,
                    [System.IO.FileMode]::Open,
                    [System.IO.FileAccess]::Read,
                    [System.IO.FileShare]::Read
                )
            }
            return [System.IO.FileStream]::new(
                $lockPath,
                [System.IO.FileMode]::OpenOrCreate,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None
            )
        }
        catch [System.IO.IOException] {
            $lastFailure = $_.Exception
        }
        if ($timer.ElapsedMilliseconds -ge $TimeoutMilliseconds) {
            throw "shared Setup asset cache ownership is busy ($Access): $($lastFailure.Message)"
        }
        $remaining = $TimeoutMilliseconds - [int]$timer.ElapsedMilliseconds
        $delay = [Math]::Min($RetryMilliseconds, $remaining)
        if ($delay -gt 0) {
            Start-Sleep -Milliseconds $delay
        }
    }
}

function Get-EasyConSharedContentAsset {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$SharedCacheRoot,

        [Parameter(Mandatory)]
        [string]$Url,

        [Parameter(Mandatory)]
        [ValidateSet("SHA256", "SHA512")]
        [string]$Algorithm,

        [Parameter(Mandatory)]
        [string]$Hash,

        [long]$Bytes = -1,

        [Parameter(Mandatory)]
        [string]$Description,

        [scriptblock]$DownloadAction
    )

    $expectedLength = if ($Algorithm -ceq "SHA256") { 64 } else { 128 }
    if ($Hash -cnotmatch "^[0-9a-f]{$expectedLength}$") {
        throw "$Description content hash must be $expectedLength lowercase hexadecimal characters"
    }
    $uri = [uri]$Url
    if (-not $uri.IsAbsoluteUri -or $uri.Scheme -cne "https") {
        throw "$Description URL must be absolute HTTPS"
    }

    $shared = Resolve-EasyConFullPath -Path $SharedCacheRoot
    New-EasyConSafeDirectory -Path $shared -TrustedRoot $shared | Out-Null
    $assets = New-EasyConSafeDirectory -Path (Join-Path $shared "assets-v1") `
        -TrustedRoot $shared
    $algorithmName = $Algorithm.ToLowerInvariant()
    $blobs = New-EasyConSafeDirectory -Path (Join-Path $assets "blobs/$algorithmName") `
        -TrustedRoot $shared
    $locks = New-EasyConSafeDirectory -Path (Join-Path $assets "locks") `
        -TrustedRoot $shared
    $quarantine = New-EasyConSafeDirectory -Path (Join-Path $assets "quarantine") `
        -TrustedRoot $shared
    $assetPath = Assert-EasyConPhysicalPath -Path (Join-Path $blobs $Hash) `
        -TrustedRoot $shared
    $lease = Enter-EasyConExclusiveFileLease `
        -Path (Join-Path $locks "$algorithmName-$Hash.lock") -TrustedRoot $shared `
        -Description "$Description shared cache ownership"
    try {
        if (Test-Path -LiteralPath $assetPath -PathType Leaf) {
            try {
                return Assert-EasyConContentFile -Path $assetPath -Algorithm $Algorithm `
                    -Hash $Hash -Bytes $Bytes -TrustedRoot $shared `
                    -Description "$Description shared cache entry"
            }
            catch {
                $damage = $_.Exception
                $quarantined = Join-Path $quarantine (
                    "$algorithmName-$Hash.corrupt-$PID-$([guid]::NewGuid().ToString('N'))"
                )
                try {
                    Assert-EasyConPhysicalPath -Path $quarantined -TrustedRoot $shared | Out-Null
                    [System.IO.File]::Move($assetPath, $quarantined)
                }
                catch {
                    $diagnostic = [System.IO.IOException]::new(
                        "$Description shared cache entry is damaged and could not be isolated: $($damage.Message); $($_.Exception.Message)",
                        $damage
                    )
                    $diagnostic.Data["EasyConSharedAssetIsolationFailure"] = `
                        $_.Exception.ToString()
                    throw $diagnostic
                }
            }
        }

        $temporary = "$assetPath.download-$PID-$([guid]::NewGuid().ToString('N'))"
        Assert-EasyConPhysicalPath -Path $temporary -TrustedRoot $shared | Out-Null
        $primaryFailure = $null
        try {
            if ($null -eq $DownloadAction) {
                $curl = Get-EasyConCommandPath -Name "curl.exe"
                Invoke-EasyConNativeCapture -Program $curl -Arguments @(
                    "--fail", "--location", "--silent", "--show-error",
                    "--retry", "15", "--retry-delay", "1", "--retry-all-errors",
                    "--continue-at", "-",
                    "--speed-limit", "1024", "--speed-time", "60",
                    "--connect-timeout", "20", "--max-time", "120",
                    "--proto", "=https", "--proto-redir", "=https",
                    "--output", $temporary, $Url
                ) -Description "download $Description" | Out-Null
            }
            else {
                & $DownloadAction $Url $temporary
            }
            Assert-EasyConContentFile -Path $temporary -Algorithm $Algorithm `
                -Hash $Hash -Bytes $Bytes -TrustedRoot $shared `
                -Description "$Description download" | Out-Null
            [System.IO.File]::Move($temporary, $assetPath)
        }
        catch {
            $primaryFailure = $_
            throw
        }
        finally {
            Complete-EasyConTemporaryFileCleanup -Path $temporary -TrustedRoot $shared `
                -Description "$Description shared download" -PrimaryFailure $primaryFailure
        }
        return Assert-EasyConContentFile -Path $assetPath -Algorithm $Algorithm `
            -Hash $Hash -Bytes $Bytes -TrustedRoot $shared `
            -Description "$Description shared cache entry"
    }
    finally {
        $lease.Dispose()
    }
}

function Assert-EasyConAtomicFileDestination {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$TrustedRoot,

        [Parameter(Mandatory)]
        [string]$Description
    )

    $resolved = Assert-EasyConPhysicalPath -Path $Path -TrustedRoot $TrustedRoot
    New-EasyConSafeDirectory -Path (Split-Path -Parent $resolved) `
        -TrustedRoot $TrustedRoot | Out-Null
    Assert-EasyConPhysicalPath -Path $resolved -TrustedRoot $TrustedRoot | Out-Null
    if (Test-Path -LiteralPath $resolved) {
        $item = Get-Item -Force -LiteralPath $resolved -ErrorAction Stop
        if ($item.PSIsContainer -or $item -isnot [System.IO.FileInfo]) {
            throw "$Description destination must be a regular file: $resolved"
        }
    }
    return $resolved
}

function Publish-EasyConContentFileAtomically {
    param(
        [Parameter(Mandatory)]
        [string]$Destination,

        [Parameter(Mandatory)]
        [ValidateSet("SHA256", "SHA512")]
        [string]$Algorithm,

        [Parameter(Mandatory)]
        [string]$Hash,

        [Parameter(Mandatory)]
        [long]$Bytes,

        [Parameter(Mandatory)]
        [string]$TrustedRoot,

        [Parameter(Mandatory)]
        [string]$Description,

        [Parameter(Mandatory)]
        [scriptblock]$MaterializeAction,

        [scriptblock]$MoveAction,

        [ValidateRange(1, 10)]
        [int]$CleanupMaxAttempts = 4,

        [ValidateRange(0, 5000)]
        [int]$CleanupRetryMilliseconds = 250,

        [scriptblock]$CleanupRetryAction
    )

    $destinationPath = Assert-EasyConAtomicFileDestination -Path $Destination `
        -TrustedRoot $TrustedRoot -Description $Description
    $temporary = "$destinationPath.publish-$PID-$([guid]::NewGuid().ToString('N'))"
    Assert-EasyConPhysicalPath -Path $temporary -TrustedRoot $TrustedRoot | Out-Null
    $primaryFailure = $null
    try {
        & $MaterializeAction $temporary | Out-Null
        Assert-EasyConContentFile -Path $temporary -Algorithm $Algorithm -Hash $Hash `
            -Bytes $Bytes -TrustedRoot $TrustedRoot `
            -Description "$Description materialized file" | Out-Null
        Assert-EasyConAtomicFileDestination -Path $destinationPath `
            -TrustedRoot $TrustedRoot -Description $Description | Out-Null
        if ($null -eq $MoveAction) {
            [System.IO.File]::Move($temporary, $destinationPath, $true)
        }
        else {
            & $MoveAction $temporary $destinationPath | Out-Null
        }
        Assert-EasyConPhysicalPath -Path $destinationPath -TrustedRoot $TrustedRoot | Out-Null
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        $cleanupParameters = @{
            Path = $temporary
            TrustedRoot = $TrustedRoot
            Description = "$Description atomic publication"
            PrimaryFailure = $primaryFailure
            MaxAttempts = $CleanupMaxAttempts
            RetryMilliseconds = $CleanupRetryMilliseconds
        }
        if ($null -ne $CleanupRetryAction) {
            $cleanupParameters.RetryAction = $CleanupRetryAction
        }
        Complete-EasyConTemporaryFileCleanup @cleanupParameters
    }
    return Assert-EasyConContentFile -Path $destinationPath -Algorithm $Algorithm `
        -Hash $Hash -Bytes $Bytes -TrustedRoot $TrustedRoot `
        -Description "$Description destination"
}

function Write-EasyConUtf8FileAtomically {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string]$Text,

        [Parameter(Mandatory)]
        [string]$TrustedRoot,

        [Parameter(Mandatory)]
        [string]$Description
    )

    $content = [System.Text.UTF8Encoding]::new($false, $true).GetBytes($Text)
    $hash = [Convert]::ToHexString(
        [System.Security.Cryptography.SHA256]::HashData($content)
    ).ToLowerInvariant()
    $materialize = {
        param($Temporary)
        $stream = [System.IO.FileStream]::new(
            $Temporary,
            [System.IO.FileMode]::CreateNew,
            [System.IO.FileAccess]::Write,
            [System.IO.FileShare]::None
        )
        try {
            $stream.Write($content, 0, $content.Length)
            $stream.Flush($true)
        }
        finally {
            $stream.Dispose()
        }
    }.GetNewClosure()
    return Publish-EasyConContentFileAtomically -Destination $Path -Algorithm SHA256 `
        -Hash $hash -Bytes $content.Length -TrustedRoot $TrustedRoot `
        -Description $Description -MaterializeAction $materialize
}

function Copy-EasyConContentFileAtomically {
    param(
        [Parameter(Mandatory)]
        [string]$Source,

        [Parameter(Mandatory)]
        [string]$Destination,

        [Parameter(Mandatory)]
        [ValidateSet("SHA256", "SHA512")]
        [string]$Algorithm,

        [Parameter(Mandatory)]
        [string]$Hash,

        [long]$Bytes = -1,

        [Parameter(Mandatory)]
        [string]$SourceTrustedRoot,

        [Parameter(Mandatory)]
        [string]$DestinationTrustedRoot,

        [Parameter(Mandatory)]
        [string]$Description,

        [switch]$ReplaceExisting
    )

    $sourcePath = Assert-EasyConContentFile -Path $Source -Algorithm $Algorithm `
        -Hash $Hash -Bytes $Bytes -TrustedRoot $SourceTrustedRoot `
        -Description "$Description source"
    $destinationPath = Assert-EasyConAtomicFileDestination -Path $Destination `
        -TrustedRoot $DestinationTrustedRoot -Description $Description
    if ($sourcePath.Equals($destinationPath, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "$Description source and destination must be different files"
    }
    if (-not $ReplaceExisting -and (Test-Path -LiteralPath $destinationPath -PathType Leaf)) {
        try {
            return Assert-EasyConContentFile -Path $destinationPath -Algorithm $Algorithm `
                -Hash $Hash -Bytes $Bytes -TrustedRoot $DestinationTrustedRoot `
                -Description "$Description destination"
        }
        catch {
            Remove-Item -Force -LiteralPath $destinationPath -ErrorAction Stop
            if (Test-Path -LiteralPath $destinationPath) {
                throw "$Description damaged destination remains after removal: $destinationPath"
            }
        }
    }

    $materialize = {
        param($Temporary)
        $sourceStream = $null
        $destinationStream = $null
        try {
            $sourceStream = [System.IO.FileStream]::new(
                $sourcePath,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::Read,
                [System.IO.FileShare]::Read
            )
            $destinationStream = [System.IO.FileStream]::new(
                $Temporary,
                [System.IO.FileMode]::CreateNew,
                [System.IO.FileAccess]::Write,
                [System.IO.FileShare]::None
            )
            $sourceStream.CopyTo($destinationStream, 65536)
            $destinationStream.Flush($true)
        }
        finally {
            if ($null -ne $destinationStream) {
                $destinationStream.Dispose()
            }
            if ($null -ne $sourceStream) {
                $sourceStream.Dispose()
            }
        }
    }.GetNewClosure()
    return Publish-EasyConContentFileAtomically -Destination $destinationPath `
        -Algorithm $Algorithm -Hash $Hash -Bytes $Bytes `
        -TrustedRoot $DestinationTrustedRoot -Description $Description `
        -MaterializeAction $materialize
}

function Get-EasyConFingerprintInputHash {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [ValidateSet("text", "binary")]
        [string]$Kind
    )

    $resolved = Get-EasyConPhysicalFile -Path $Path
    if ($Kind -ceq "binary") {
        return Get-EasyConFileHash -Path $resolved -Algorithm SHA256
    }

    try {
        $bytes = [System.IO.File]::ReadAllBytes($resolved)
        $strictUtf8 = [System.Text.UTF8Encoding]::new($false, $true)
        $text = $strictUtf8.GetString($bytes)
    }
    catch {
        throw "text fingerprint input must be valid UTF-8: $resolved ($($_.Exception.Message))"
    }
    $canonical = $text.Replace("`r`n", "`n").Replace("`r", "`n")
    $canonicalBytes = $strictUtf8.GetBytes($canonical)
    $digest = [System.Security.Cryptography.SHA256]::HashData($canonicalBytes)
    return [System.Convert]::ToHexString($digest).ToLowerInvariant()
}

function Complete-EasyConTemporaryFileCleanup {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$TrustedRoot,

        [Parameter(Mandatory)]
        [string]$Description,

        [object]$PrimaryFailure,

        [ValidateRange(1, 10)]
        [int]$MaxAttempts = 4,

        [ValidateRange(0, 5000)]
        [int]$RetryMilliseconds = 250,

        [scriptblock]$RetryAction
    )

    if ($null -eq $RetryAction) {
        $RetryAction = {
            param($Attempt, $Failure)
            $null = $Attempt, $Failure
            if ($RetryMilliseconds -gt 0) {
                Start-Sleep -Milliseconds $RetryMilliseconds
            }
        }.GetNewClosure()
    }

    $cleanupFailure = $null
    $cleanupAttempts = 0
    for ($attempt = 1; $attempt -le $MaxAttempts; $attempt++) {
        $cleanupAttempts = $attempt
        try {
            $temporary = Assert-EasyConPhysicalPath -Path $Path -TrustedRoot $TrustedRoot
            if (-not (Test-Path -LiteralPath $temporary)) {
                return
            }
            Remove-Item -LiteralPath $temporary -Force -ErrorAction Stop
            if (Test-Path -LiteralPath $temporary) {
                throw [System.IO.IOException]::new(
                    "temporary file still exists after cleanup"
                )
            }
            return
        }
        catch {
            $cleanupFailure = $_.Exception
            if (-not (Test-Path -LiteralPath $Path)) {
                return
            }
            if ($attempt -lt $MaxAttempts) {
                try {
                    & $RetryAction $attempt $cleanupFailure
                }
                catch {
                    $cleanupFailure = $_.Exception
                    break
                }
            }
        }
    }

    $cleanupMessage = (
        "temporary file cleanup failed after $cleanupAttempts attempt(s): " +
        "$($cleanupFailure.Message); temporary file remains incomplete and must not be used; " +
        "release external handles and rerun Setup"
    )
    if ($null -eq $PrimaryFailure) {
        throw [System.IO.IOException]::new("$Description $cleanupMessage", $cleanupFailure)
    }

    $primaryException = if ($PrimaryFailure -is [System.Management.Automation.ErrorRecord]) {
        $PrimaryFailure.Exception
    }
    elseif ($PrimaryFailure -is [System.Exception]) {
        $PrimaryFailure
    }
    else {
        [System.Exception]::new([string]$PrimaryFailure)
    }
    $diagnostic = [System.IO.IOException]::new(
        "$Description failed: $($primaryException.Message); $cleanupMessage",
        $primaryException
    )
    $diagnostic.Data["EasyConTemporaryCleanupFailure"] = $cleanupFailure.ToString()
    $diagnostic.Data["EasyConResidualTemporary"] = $Path
    throw $diagnostic
}

function Get-EasyConPinnedDownload {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Destination,

        [Parameter(Mandatory)]
        [string]$Url,

        [Parameter(Mandatory)]
        [string]$Sha512,

        [Parameter(Mandatory)]
        [string]$TrustedRoot,

        [Parameter(Mandatory)]
        [string]$Description,

        [string]$SharedCacheRoot,

        [scriptblock]$DownloadAction
    )

    $destinationPath = Assert-EasyConPhysicalPath -Path $Destination -TrustedRoot $TrustedRoot
    if (-not [string]::IsNullOrWhiteSpace($SharedCacheRoot)) {
        $sharedAsset = Get-EasyConSharedContentAsset -SharedCacheRoot $SharedCacheRoot `
            -Url $Url -Algorithm SHA512 -Hash $Sha512 -Description $Description `
            -DownloadAction $DownloadAction
        return Copy-EasyConContentFileAtomically -Source $sharedAsset `
            -Destination $destinationPath -Algorithm SHA512 -Hash $Sha512 `
            -SourceTrustedRoot $SharedCacheRoot -DestinationTrustedRoot $TrustedRoot `
            -Description $Description
    }
    if (Test-Path -LiteralPath $destinationPath -PathType Leaf) {
        $actual = Get-EasyConFileHash -Path $destinationPath -Algorithm SHA512
        if ($actual -ceq $Sha512) {
            return $destinationPath
        }
        throw "$Description cache is damaged: SHA-512 $actual does not match $Sha512; rerun Setup with a clean controlled environment root"
    }

    $temporary = "$destinationPath.download-$PID-$([guid]::NewGuid().ToString('N'))"
    Assert-EasyConPhysicalPath -Path $temporary -TrustedRoot $TrustedRoot | Out-Null
    $primaryFailure = $null
    try {
        $curl = Get-EasyConCommandPath -Name "curl.exe"
        Invoke-EasyConNativeCapture -Program $curl -Arguments @(
            "--fail", "--location", "--silent", "--show-error",
            "--retry", "5", "--retry-all-errors",
            "--connect-timeout", "20", "--max-time", "600",
            "--proto", "=https", "--proto-redir", "=https",
            "--output", $temporary, $Url
        ) -Description "download $Description" | Out-Null
        $actual = Get-EasyConFileHash -Path $temporary -Algorithm SHA512
        if ($actual -cne $Sha512) {
            throw "$Description download has SHA-512 $actual; expected $Sha512"
        }
        Move-Item -LiteralPath $temporary -Destination $destinationPath
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        Complete-EasyConTemporaryFileCleanup -Path $temporary -TrustedRoot $TrustedRoot `
            -Description "$Description download" -PrimaryFailure $primaryFailure
    }
    return $destinationPath
}

function Install-EasyConPinnedExecutable {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Source,

        [Parameter(Mandatory)]
        [string]$Destination,

        [Parameter(Mandatory)]
        [string]$Sha512,

        [Parameter(Mandatory)]
        [string]$TrustedRoot,

        [Parameter(Mandatory)]
        [string]$Description
    )

    $trusted = Assert-EasyConPhysicalPath -Path $TrustedRoot
    $sourcePath = Get-EasyConPhysicalFile -Path $Source -TrustedRoot $trusted
    $sourceHash = Get-EasyConFileHash -Path $sourcePath -Algorithm SHA512
    if ($sourceHash -cne $Sha512) {
        throw "$Description source has SHA-512 $sourceHash; expected $Sha512"
    }

    $destinationPath = Assert-EasyConPhysicalPath -Path $Destination -TrustedRoot $trusted
    New-EasyConSafeDirectory -Path (Split-Path -Parent $destinationPath) `
        -TrustedRoot $trusted | Out-Null
    if (-not (Test-Path -LiteralPath $destinationPath -PathType Leaf)) {
        Copy-Item -LiteralPath $sourcePath -Destination $destinationPath
    }
    $installed = Get-EasyConPhysicalFile -Path $destinationPath -TrustedRoot $trusted
    $installedHash = Get-EasyConFileHash -Path $installed -Algorithm SHA512
    if ($installedHash -cne $Sha512) {
        throw "$Description installed copy has SHA-512 $installedHash; expected $Sha512"
    }
    return $installed
}

function Publish-EasyConDirectoryAtomically {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Source,

        [Parameter(Mandatory)]
        [string]$Destination,

        [Parameter(Mandatory)]
        [string]$TrustedRoot,

        [ValidateRange(1, 10)]
        [int]$MaxAttempts = 4,

        [ValidateRange(0, 5000)]
        [int]$RetryMilliseconds = 250,

        [scriptblock]$MoveAction,

        [scriptblock]$RetryAction
    )

    $trusted = Assert-EasyConPhysicalPath -Path $TrustedRoot
    $sourcePath = Assert-EasyConPhysicalTree -Path $Source -TrustedRoot $trusted
    $destinationPath = Assert-EasyConPhysicalPath -Path $Destination -TrustedRoot $trusted
    if (-not (Test-Path -LiteralPath $sourcePath -PathType Container)) {
        throw "atomic directory publish source is missing: $sourcePath"
    }
    if (Test-Path -LiteralPath $destinationPath) {
        throw "atomic directory publish refuses an existing destination: $destinationPath"
    }
    if ($null -eq $MoveAction) {
        $MoveAction = {
            param($PublishSource, $PublishDestination)
            [System.IO.Directory]::Move($PublishSource, $PublishDestination)
        }
    }
    if ($null -eq $RetryAction) {
        $RetryAction = {
            param($Attempt, $Failure)
            $null = $Attempt
            $null = $Failure
            if ($RetryMilliseconds -gt 0) {
                Start-Sleep -Milliseconds $RetryMilliseconds
            }
        }.GetNewClosure()
    }

    for ($attempt = 1; $attempt -le $MaxAttempts; $attempt++) {
        try {
            & $MoveAction $sourcePath $destinationPath
            if (
                (Test-Path -LiteralPath $sourcePath) -or
                -not (Test-Path -LiteralPath $destinationPath -PathType Container)
            ) {
                throw [System.IO.IOException]::new(
                    "atomic directory rename returned an incomplete publish state"
                )
            }
            Assert-EasyConPhysicalTree -Path $destinationPath -TrustedRoot $trusted | Out-Null
            return $destinationPath
        }
        catch {
            $failure = $_.Exception
            $cleanupFailure = $null
            $cleanupAttempts = 0
            if (Test-Path -LiteralPath $destinationPath) {
                for ($cleanupAttempt = 1; $cleanupAttempt -le $MaxAttempts; $cleanupAttempt++) {
                    $cleanupAttempts = $cleanupAttempt
                    try {
                        Remove-EasyConSafeTree -Path $destinationPath -TrustedRoot $trusted
                        if (Test-Path -LiteralPath $destinationPath) {
                            throw [System.IO.IOException]::new(
                                "partial destination still exists after cleanup"
                            )
                        }
                        $cleanupFailure = $null
                        break
                    }
                    catch {
                        $cleanupFailure = $_.Exception
                        if (-not (Test-Path -LiteralPath $destinationPath)) {
                            $cleanupFailure = $null
                            break
                        }
                        if ($cleanupAttempt -lt $MaxAttempts) {
                            try {
                                & $RetryAction $cleanupAttempt $cleanupFailure
                            }
                            catch {
                                $cleanupFailure = $_.Exception
                                break
                            }
                        }
                    }
                }
            }
            if ($null -ne $cleanupFailure) {
                $diagnostic = [System.IO.IOException]::new(
                    (
                        "atomic directory publish failed after $attempt attempt(s): " +
                        "$($failure.Message); partial destination cleanup failed after " +
                        "$cleanupAttempts attempt(s): $($cleanupFailure.Message); " +
                        "destination remains incomplete and must not be used; release external " +
                        "handles and rerun Setup"
                    ),
                    $failure
                )
                $diagnostic.Data["EasyConPublishCleanupFailure"] = $cleanupFailure.ToString()
                $diagnostic.Data["EasyConPartialDestination"] = $destinationPath
                throw $diagnostic
            }
            if (-not (Test-Path -LiteralPath $sourcePath -PathType Container)) {
                throw [System.IO.IOException]::new(
                    "atomic directory publish lost its complete staging tree: $($failure.Message)",
                    $failure
                )
            }
            if ($failure -isnot [System.IO.IOException] -or $attempt -eq $MaxAttempts) {
                throw [System.IO.IOException]::new(
                    "atomic directory publish failed after $attempt attempt(s): $($failure.Message)",
                    $failure
                )
            }
            & $RetryAction $attempt $failure
        }
    }
}

function Get-EasyConTreeFingerprint {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$TrustedRoot
    )

    $root = Assert-EasyConPhysicalTree -Path $Path -TrustedRoot $TrustedRoot
    if (-not (Test-Path -LiteralPath $root -PathType Container)) {
        throw "required environment tree is missing: $root"
    }
    $builder = [System.Text.StringBuilder]::new()
    $files = @(Get-ChildItem -LiteralPath $root -Recurse -Force -File | Sort-Object FullName)
    foreach ($file in $files) {
        Assert-EasyConPhysicalPath -Path $file.FullName -TrustedRoot $root | Out-Null
        $relative = [System.IO.Path]::GetRelativePath($root, $file.FullName).Replace('\', '/')
        $hash = Get-EasyConFileHash -Path $file.FullName -Algorithm SHA256
        [void]$builder.Append($relative).Append("`0").Append($file.Length).Append("`0").Append($hash).Append("`n")
    }
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($builder.ToString())
    $digest = [System.Security.Cryptography.SHA256]::HashData($bytes)
    return [pscustomobject]@{
        Files = $files.Count
        Sha256 = [System.Convert]::ToHexString($digest).ToLowerInvariant()
    }
}

function Initialize-EasyConPreparedArtifactAuditor {
    if ($null -ne ("EasyCon.WindowsWorkspace.PreparedArtifactAuditor" -as [type])) {
        return
    }

    Add-Type -Language CSharp -TypeDefinition @'
using System;
using System.IO;
using System.Text;
using System.Text.Json;

namespace EasyCon.WindowsWorkspace
{
    public sealed class PreparedArtifactAuditResult
    {
        public bool IsText { get; internal set; }
        public bool IsJson { get; internal set; }
        public string RawMatchedLabel { get; internal set; }
        public string StructuredMatchedLabel { get; internal set; }
        public string TextDamage { get; internal set; }
        public string TextDamageKind { get; internal set; }
        public string JsonError { get; internal set; }
        public string ResourceFailure { get; internal set; }
        public bool JsonLimitExceeded { get; internal set; }
        public long BytesRead { get; internal set; }
        public int MaximumJsonWindowBytes { get; internal set; }
    }

    public static class PreparedArtifactAuditor
    {
        public const int JsonWindowLimitBytes = 65536;
        private const int EncodingProbeBytes = 512;
        private const int TextWindowCharacters = 16384;
        private const int MaximumJsonDepth = 256;

        private static readonly Encoding StrictUtf8 = new UTF8Encoding(false, true);
        private static readonly Encoding StrictUtf16Le = new UnicodeEncoding(false, false, true);
        private static readonly Encoding StrictUtf16Be = new UnicodeEncoding(true, false, true);

        private sealed class DetectedEncoding
        {
            internal Encoding Encoding;
            internal int PreambleLength;
            internal bool ProtectedText;
            internal string DamageKind;
        }

        public static PreparedArtifactAuditResult Audit(
            string path,
            string[] forbiddenLabels,
            string[] forbiddenReferences)
        {
            if (forbiddenLabels == null || forbiddenReferences == null ||
                forbiddenLabels.Length != forbiddenReferences.Length)
            {
                throw new ArgumentException("forbidden labels and references must have equal length");
            }

            PreparedArtifactAuditResult result = new PreparedArtifactAuditResult();
            try
            {
                using (FileStream stream = new FileStream(
                    path,
                    FileMode.Open,
                    FileAccess.Read,
                    FileShare.Read,
                    4096,
                    FileOptions.SequentialScan))
                {
                    result.BytesRead = stream.Length;
                    DetectedEncoding encoding = DetectEncoding(stream);
                    ScanDecodedText(
                        stream,
                        encoding,
                        forbiddenLabels,
                        forbiddenReferences,
                        result);
                    if (result.IsText && result.TextDamage == null)
                    {
                        ScanStructuredJson(
                            stream,
                            encoding,
                            forbiddenLabels,
                            forbiddenReferences,
                            result);
                    }
                }
            }
            catch (OutOfMemoryException exception)
            {
                result.ResourceFailure = "memory allocation failed: " + exception.Message;
            }
            catch (IOException exception)
            {
                result.ResourceFailure = "file read failed: " + exception.Message;
            }
            catch (UnauthorizedAccessException exception)
            {
                result.ResourceFailure = "file access failed: " + exception.Message;
            }
            return result;
        }

        private static DetectedEncoding DetectEncoding(FileStream stream)
        {
            byte[] probe = new byte[EncodingProbeBytes];
            int length = 0;
            while (length < probe.Length)
            {
                int read = stream.Read(probe, length, probe.Length - length);
                if (read == 0)
                {
                    break;
                }
                length += read;
            }
            stream.Position = 0;

            if (length >= 3 && probe[0] == 0xef && probe[1] == 0xbb && probe[2] == 0xbf)
            {
                return NewEncoding(StrictUtf8, 3, true, "BOM");
            }
            if (length >= 2 && probe[0] == 0xff && probe[1] == 0xfe)
            {
                return NewEncoding(StrictUtf16Le, 2, true, "BOM");
            }
            if (length >= 2 && probe[0] == 0xfe && probe[1] == 0xff)
            {
                return NewEncoding(StrictUtf16Be, 2, true, "BOM");
            }
            if (LooksLikeUtf16(probe, length, true))
            {
                return NewEncoding(StrictUtf16Le, 0, true, "inferred UTF-16LE");
            }
            if (LooksLikeUtf16(probe, length, false))
            {
                return NewEncoding(StrictUtf16Be, 0, true, "inferred UTF-16BE");
            }
            return NewEncoding(StrictUtf8, 0, false, "UTF-8");
        }

        private static DetectedEncoding NewEncoding(
            Encoding encoding,
            int preambleLength,
            bool protectedText,
            string damageKind)
        {
            return new DetectedEncoding
            {
                Encoding = encoding,
                PreambleLength = preambleLength,
                ProtectedText = protectedText,
                DamageKind = damageKind
            };
        }

        private static bool LooksLikeUtf16(byte[] probe, int length, bool littleEndian)
        {
            int pairs = length / 2;
            if (pairs < 2)
            {
                return false;
            }

            int compatible = 0;
            int nullLane = 0;
            for (int index = 0; index < pairs; index++)
            {
                byte text = probe[(index * 2) + (littleEndian ? 0 : 1)];
                byte expectedNull = probe[(index * 2) + (littleEndian ? 1 : 0)];
                if (expectedNull == 0)
                {
                    nullLane++;
                    if (text == 0x09 || text == 0x0a || text == 0x0d ||
                        (text >= 0x20 && text <= 0x7e))
                    {
                        compatible++;
                    }
                }
            }
            return nullLane >= 2 &&
                nullLane * 100 >= pairs * 75 &&
                compatible * 100 >= pairs * 75;
        }

        private static void ScanDecodedText(
            FileStream stream,
            DetectedEncoding encoding,
            string[] labels,
            string[] references,
            PreparedArtifactAuditResult result)
        {
            stream.Position = encoding.PreambleLength;
            int maximumReferenceLength = 0;
            foreach (string reference in references)
            {
                maximumReferenceLength = Math.Max(maximumReferenceLength, reference.Length);
            }

            result.IsText = true;
            char[] buffer = new char[TextWindowCharacters];
            string tail = String.Empty;
            try
            {
                using (StreamReader reader = new StreamReader(
                    stream,
                    encoding.Encoding,
                    false,
                    TextWindowCharacters,
                    true))
                {
                    int read;
                    while ((read = reader.Read(buffer, 0, buffer.Length)) > 0)
                    {
                        string chunk = new string(buffer, 0, read);
                        if (chunk.IndexOf('\0') >= 0)
                        {
                            if (encoding.ProtectedText)
                            {
                                result.TextDamage = "contains a NUL character";
                                result.TextDamageKind = encoding.DamageKind;
                            }
                            else
                            {
                                result.IsText = false;
                            }
                            return;
                        }

                        if (result.RawMatchedLabel == null)
                        {
                            string search = tail + chunk;
                            result.RawMatchedLabel = FindReference(search, labels, references);
                            if (maximumReferenceLength > 1)
                            {
                                int tailLength = Math.Min(maximumReferenceLength - 1, search.Length);
                                tail = search.Substring(search.Length - tailLength);
                            }
                        }
                    }
                }
            }
            catch (DecoderFallbackException exception)
            {
                if (encoding.ProtectedText)
                {
                    result.TextDamage = exception.Message;
                    result.TextDamageKind = encoding.DamageKind;
                }
                else
                {
                    result.IsText = false;
                }
            }
        }

        private static void ScanStructuredJson(
            FileStream stream,
            DetectedEncoding encoding,
            string[] labels,
            string[] references,
            PreparedArtifactAuditResult result)
        {
            stream.Position = encoding.PreambleLength;
            byte[] buffer = new byte[JsonWindowLimitBytes];
            JsonReaderOptions options = new JsonReaderOptions
            {
                AllowTrailingCommas = false,
                CommentHandling = JsonCommentHandling.Disallow,
                MaxDepth = MaximumJsonDepth
            };
            JsonReaderState state = new JsonReaderState(options);
            int buffered = 0;
            bool sawToken = false;
            string matchedLabel = null;

            try
            {
                using (Stream jsonStream = Encoding.CreateTranscodingStream(
                    stream,
                    encoding.Encoding,
                    StrictUtf8,
                    true))
                {
                    while (true)
                    {
                        int read = jsonStream.Read(buffer, buffered, buffer.Length - buffered);
                        int available = buffered + read;
                        bool isFinalBlock = read == 0;
                        result.MaximumJsonWindowBytes = Math.Max(
                            result.MaximumJsonWindowBytes,
                            available);

                        Utf8JsonReader reader = new Utf8JsonReader(
                            new ReadOnlySpan<byte>(buffer, 0, available),
                            isFinalBlock,
                            state);
                        try
                        {
                            while (reader.Read())
                            {
                                sawToken = true;
                                if (matchedLabel == null &&
                                    (reader.TokenType == JsonTokenType.PropertyName ||
                                     reader.TokenType == JsonTokenType.String))
                                {
                                    matchedLabel = FindReference(
                                        reader.GetString(),
                                        labels,
                                        references);
                                }
                            }
                        }
                        catch (JsonException exception)
                        {
                            if (reader.CurrentDepth >= MaximumJsonDepth - 1 ||
                                exception.Message.IndexOf(
                                    "maximum depth",
                                    StringComparison.OrdinalIgnoreCase) >= 0)
                            {
                                result.JsonLimitExceeded = true;
                            }
                            result.JsonError = exception.Message;
                            return;
                        }

                        int consumed = checked((int)reader.BytesConsumed);
                        state = reader.CurrentState;
                        int remaining = available - consumed;
                        if (remaining > 0 && consumed > 0)
                        {
                            Buffer.BlockCopy(buffer, consumed, buffer, 0, remaining);
                        }
                        buffered = remaining;

                        if (isFinalBlock)
                        {
                            result.IsJson = sawToken;
                            return;
                        }
                        if (buffered == buffer.Length)
                        {
                            result.JsonLimitExceeded = true;
                            result.JsonError =
                                "a JSON token exceeds the bounded " +
                                JsonWindowLimitBytes + " byte audit window";
                            return;
                        }
                    }
                }
            }
            finally
            {
                result.StructuredMatchedLabel = matchedLabel;
            }
        }

        private static string FindReference(
            string text,
            string[] labels,
            string[] references)
        {
            if (text == null)
            {
                return null;
            }
            for (int index = 0; index < references.Length; index++)
            {
                if (text.IndexOf(references[index], StringComparison.OrdinalIgnoreCase) >= 0)
                {
                    return labels[index];
                }
            }
            return null;
        }
    }
}
'@
}

function Assert-EasyConPreparedTextArtifactDoesNotReferenceRoots {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string[]]$ForbiddenLabels,

        [Parameter(Mandatory)]
        [string[]]$ForbiddenReferences,

        [Parameter(Mandatory)]
        [string]$Description
    )

    Initialize-EasyConPreparedArtifactAuditor
    try {
        $audit = [EasyCon.WindowsWorkspace.PreparedArtifactAuditor]::Audit(
            $Path,
            $ForbiddenLabels,
            $ForbiddenReferences
        )
    }
    catch [System.OutOfMemoryException] {
        throw "$Description audit failed closed for ${Path}: memory allocation failed"
    }
    catch {
        throw "$Description audit failed closed for ${Path}: $($_.Exception.Message)"
    }

    if ($audit.IsText -and -not [string]::IsNullOrWhiteSpace($audit.RawMatchedLabel)) {
        throw "$Description contains a $($audit.RawMatchedLabel) absolute path: $Path"
    }
    if (-not [string]::IsNullOrWhiteSpace($audit.StructuredMatchedLabel)) {
        throw "$Description contains a $($audit.StructuredMatchedLabel) absolute path: $Path"
    }
    if (-not [string]::IsNullOrWhiteSpace($audit.ResourceFailure)) {
        throw "$Description audit failed closed for ${Path}: $($audit.ResourceFailure)"
    }
    if (-not [string]::IsNullOrWhiteSpace($audit.TextDamage)) {
        $damageDescription = if ($audit.TextDamageKind -ceq "BOM") {
            "damaged BOM text artifact"
        }
        else {
            "damaged $($audit.TextDamageKind) text artifact"
        }
        throw "$Description contains $damageDescription ${Path}: $($audit.TextDamage)"
    }
    if ($audit.JsonLimitExceeded) {
        throw "$Description structured JSON audit failed closed for ${Path}: $($audit.JsonError)"
    }

    $requiresJson = [System.IO.Path]::GetExtension($Path).Equals(
        ".json",
        [System.StringComparison]::OrdinalIgnoreCase
    )
    if ($requiresJson -and (-not $audit.IsText -or -not $audit.IsJson)) {
        $jsonDamage = if ([string]::IsNullOrWhiteSpace($audit.JsonError)) {
            "the file is not strict supported text JSON"
        }
        else {
            $audit.JsonError
        }
        throw "$Description contains damaged structured JSON ${Path}: $jsonDamage"
    }
    return $audit
}

function Assert-EasyConPreparedTreeDoesNotReferenceRepository {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$TrustedRoot,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [string]$WritableRoot,

        [Parameter(Mandatory)]
        [string]$Description,

        [switch]$PassThru
    )

    $tree = Assert-EasyConPhysicalPath -Path $Path -TrustedRoot $TrustedRoot
    if (-not (Test-Path -LiteralPath $tree -PathType Container)) {
        throw "required environment tree is missing: $tree"
    }
    $forbiddenRoots = [System.Collections.Generic.List[object]]::new()
    $repository = Resolve-EasyConFullPath -Path $RepositoryRoot
    $forbiddenRoots.Add([pscustomobject]@{
        Label = "source worktree"
        References = @($repository, $repository.Replace("\", "/")) | Select-Object -Unique
    }) | Out-Null
    if (-not [string]::IsNullOrWhiteSpace($WritableRoot)) {
        $writable = Resolve-EasyConFullPath -Path $WritableRoot
        $forbiddenRoots.Add([pscustomobject]@{
            Label = "writable"
            References = @($writable, $writable.Replace("\", "/")) | Select-Object -Unique
        }) | Out-Null
    }

    $forbiddenLabels = [System.Collections.Generic.List[string]]::new()
    $forbiddenReferences = [System.Collections.Generic.List[string]]::new()
    foreach ($root in $forbiddenRoots) {
        foreach ($reference in @($root.References)) {
            $forbiddenLabels.Add($root.Label) | Out-Null
            $forbiddenReferences.Add($reference) | Out-Null
        }
    }

    Initialize-EasyConPreparedArtifactAuditor
    $summary = [ordered]@{
        FilesScanned = 0L
        TextFilesScanned = 0L
        JsonDocumentsScanned = 0L
        TotalBytesRead = 0L
        MaximumJsonWindowBytes = 0
        JsonWindowLimitBytes = `
            [EasyCon.WindowsWorkspace.PreparedArtifactAuditor]::JsonWindowLimitBytes
    }
    $enumerators = [System.Collections.Generic.Stack[System.Collections.IEnumerator]]::new()
    try {
        $enumerators.Push(
            [System.IO.Directory]::EnumerateFileSystemEntries($tree).GetEnumerator()
        )
        while ($enumerators.Count -gt 0) {
            $enumerator = $enumerators.Peek()
            if (-not $enumerator.MoveNext()) {
                $completed = $enumerators.Pop()
                if ($completed -is [System.IDisposable]) {
                    $completed.Dispose()
                }
                continue
            }

            $entryPath = [string]$enumerator.Current
            Assert-EasyConPhysicalPath -Path $entryPath -TrustedRoot $tree | Out-Null
            $attributes = [System.IO.File]::GetAttributes($entryPath)
            if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "physical tree contains a reparse point: $entryPath"
            }
            if (($attributes -band [System.IO.FileAttributes]::Directory) -ne 0) {
                $enumerators.Push(
                    [System.IO.Directory]::EnumerateFileSystemEntries($entryPath).GetEnumerator()
                )
                continue
            }

            $audit = Assert-EasyConPreparedTextArtifactDoesNotReferenceRoots `
                -Path $entryPath -ForbiddenLabels $forbiddenLabels.ToArray() `
                -ForbiddenReferences $forbiddenReferences.ToArray() -Description $Description
            Assert-EasyConPhysicalPath -Path $entryPath -TrustedRoot $tree | Out-Null
            $summary.FilesScanned++
            $summary.TotalBytesRead += $audit.BytesRead
            if ($audit.IsText) {
                $summary.TextFilesScanned++
            }
            if ($audit.IsJson) {
                $summary.JsonDocumentsScanned++
            }
            $summary.MaximumJsonWindowBytes = [Math]::Max(
                $summary.MaximumJsonWindowBytes,
                $audit.MaximumJsonWindowBytes
            )
        }
    }
    catch [System.OutOfMemoryException] {
        throw "$Description tree audit failed closed: memory allocation failed"
    }
    finally {
        while ($enumerators.Count -gt 0) {
            $remaining = $enumerators.Pop()
            if ($remaining -is [System.IDisposable]) {
                $remaining.Dispose()
            }
        }
    }

    if ($PassThru) {
        return [pscustomobject]$summary
    }
}

function Assert-EasyConSharedToolPathBoundary {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$WritableRoot,

        [Parameter(Mandatory)]
        [string]$Description
    )

    if (Test-EasyConPathWithin -Path $Path -Root $RepositoryRoot) {
        throw "$Description is inside the source repository"
    }
    if (Test-EasyConPathWithin -Path $Path -Root $WritableRoot) {
        throw "$Description is bound to a writable workspace path"
    }
}

function ConvertTo-EasyConStrictVersion {
    param(
        [Parameter(Mandatory)]
        [string]$Value,

        [Parameter(Mandatory)]
        [string]$Description
    )

    if ($Value -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
        throw "$Description must be an exact ASCII semantic version"
    }
    $components = @()
    foreach ($component in $Value.Split('.')) {
        $parsed = 0
        if (-not [int]::TryParse(
            $component,
            [Globalization.NumberStyles]::None,
            [Globalization.CultureInfo]::InvariantCulture,
            [ref]$parsed
        )) {
            throw "$Description component is outside the supported System.Version range"
        }
        $components += $parsed
    }
    return [version]::new($components[0], $components[1], $components[2])
}

function Get-EasyConWindowsBuildConfiguration {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [scriptblock]$ReparsePointClassifier
    )

    $resolved = Get-EasyConPhysicalFile -Path $Path `
        -ReparsePointClassifier $ReparsePointClassifier
    try {
        $json = Read-EasyConPhysicalText -Path $resolved `
            -ReparsePointClassifier $ReparsePointClassifier
        $documentOptions = [System.Text.Json.JsonDocumentOptions]::new()
        $documentOptions.AllowTrailingCommas = $false
        $documentOptions.CommentHandling = [System.Text.Json.JsonCommentHandling]::Disallow
        $document = [System.Text.Json.JsonDocument]::Parse($json, $documentOptions)
    }
    catch {
        throw "Windows build environment config is invalid: $($_.Exception.Message)"
    }
    $primaryFailure = $null
    try {
        $getObject = {
            param(
                [System.Text.Json.JsonElement]$Element,
                [string[]]$ExpectedKeys,
                [string]$Description
            )
            if ($Element.ValueKind -ne [System.Text.Json.JsonValueKind]::Object) {
                throw "$Description must be a JSON object"
            }
            $properties = @{}
            foreach ($property in $Element.EnumerateObject()) {
                if ($properties.ContainsKey($property.Name)) {
                    throw "$Description contains duplicate key '$($property.Name)'"
                }
                $properties[$property.Name] = $property.Value.Clone()
            }
            $actualKeys = @($properties.Keys | Sort-Object)
            $requiredKeys = @($ExpectedKeys | Sort-Object)
            if (($actualKeys -join "`0") -cne ($requiredKeys -join "`0")) {
                throw "$Description keys must be exactly: $($requiredKeys -join ', ')"
            }
            return $properties
        }
        $getString = {
            param([System.Text.Json.JsonElement]$Element, [string]$Description)
            if ($Element.ValueKind -ne [System.Text.Json.JsonValueKind]::String) {
                throw "$Description must be a JSON string"
            }
            return $Element.GetString()
        }

        $root = & $getObject $document.RootElement @(
            "version",
            "target",
            "fingerprintInputs",
            "visionModelDirectory",
            "hostTools",
            "vcpkg"
        ) "Windows build environment root"
        $version = 0
        if (
            $root.version.ValueKind -ne [System.Text.Json.JsonValueKind]::Number -or
            -not $root.version.TryGetInt32([ref]$version) -or
            $version -ne 4
        ) {
            throw "Windows build environment version must be the JSON integer 4"
        }
        $target = & $getString $root.target "Windows build target"
        if ($target -cne "x86_64-pc-windows-msvc") {
            throw "Windows build target must remain x86_64-pc-windows-msvc"
        }
        if ($root.fingerprintInputs.ValueKind -ne [System.Text.Json.JsonValueKind]::Array) {
            throw "fingerprintInputs must be a JSON array"
        }
        $expectedFingerprintPaths = @(
            "tools/windows_build_environment.json",
            "tools/windows_workspace.psm1",
            "tools/run_windows_workspace.ps1",
            "rust-toolchain.toml",
            "Cargo.toml",
            "Cargo.lock",
            "crates/easycon-model/Cargo.toml",
            "crates/easycon-runtime/Cargo.toml",
            "crates/easycon-controller/Cargo.toml",
            "crates/easycon-ecs/Cargo.toml",
            "crates/easycon-serial/Cargo.toml",
            "crates/easycon-native-sys/Cargo.toml",
            "crates/easycon-vision/Cargo.toml",
            "tests/support/Cargo.toml",
            "vcpkg.json",
            "vcpkg-configuration.json",
            "cmake/triplets/x64-windows-static-md.cmake",
            "CMakePresets.json",
            "spec/fixtures/vision/ocr-model.json",
            "tools/provision_vision_test_model.py"
        )
        $fingerprintPaths = [System.Collections.Generic.HashSet[string]]::new(
            [System.StringComparer]::OrdinalIgnoreCase
        )
        $fingerprintIndex = 0
        foreach ($element in $root.fingerprintInputs.EnumerateArray()) {
            $input = & $getObject $element @("path", "kind") "fingerprint input"
            $relative = & $getString $input.path "fingerprint input path"
            $components = @($relative.Split('/'))
            if (
                $relative -cnotmatch '^[A-Za-z0-9._/-]+$' -or
                $relative.StartsWith('/') -or
                $relative.Contains('//') -or
                $components -ccontains '.' -or
                $components -ccontains '..' -or
                $components -contains ''
            ) {
                throw "fingerprint input must be one normalized repository-relative path"
            }
            if (-not $fingerprintPaths.Add($relative)) {
                throw "fingerprintInputs contains duplicate Windows path identity '$relative'"
            }
            $kind = & $getString $input.kind "fingerprint input kind"
            if ($kind -cnotin @("text", "binary")) {
                throw "fingerprint input kind must be exactly text or binary"
            }
            if (
                $fingerprintIndex -ge $expectedFingerprintPaths.Count -or
                $relative -cne $expectedFingerprintPaths[$fingerprintIndex] -or
                $kind -cne "text"
            ) {
                throw "Windows environment fingerprint input set, kind, or order changed"
            }
            $fingerprintIndex++
        }
        if ($fingerprintIndex -ne $expectedFingerprintPaths.Count) {
            throw "Windows environment fingerprint input set, kind, or order changed"
        }
        $visionDirectory = & $getString $root.visionModelDirectory "OCR model directory"
        if ($visionDirectory -notmatch '^[A-Za-z0-9._-]+$') {
            throw "OCR model directory must be one portable path component"
        }

        $hostTools = & $getObject $root.hostTools @(
            "pythonMinimumVersion",
            "visualStudioMajorVersion",
            "msvcToolsVersion",
            "windowsSdkVersion"
        ) "host tool configuration"
        foreach ($entry in @(
            @("Python minimum version", (& $getString $hostTools.pythonMinimumVersion "Python minimum version")),
            @("MSVC tools version", (& $getString $hostTools.msvcToolsVersion "MSVC tools version"))
        )) {
            ConvertTo-EasyConStrictVersion -Value ([string]$entry[1]) `
                -Description ([string]$entry[0]) | Out-Null
        }
        $vsMajor = & $getString $hostTools.visualStudioMajorVersion "Visual Studio major version"
        if ($vsMajor -cnotmatch '^(0|[1-9][0-9]*)$') {
            throw "Visual Studio major version must be ASCII digits without leading zeroes"
        }
        $windowsSdk = & $getString $hostTools.windowsSdkVersion "Windows SDK version"
        if ($windowsSdk -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
            throw "Windows SDK version must contain four exact ASCII numeric components"
        }

        $vcpkg = & $getObject $root.vcpkg @(
            "scriptsRepository",
            "scriptsCommit",
            "registryBaseline",
            "toolRelease",
            "toolCommit",
            "windowsAsset",
            "toolsManifest",
            "internalTools",
            "nativeDependencies"
        ) "vcpkg configuration"
        $scriptsRepository = & $getString $vcpkg.scriptsRepository "vcpkg scripts repository"
        if ($scriptsRepository -cne "https://github.com/microsoft/vcpkg.git") {
            throw "vcpkg scripts repository URL is not exactly canonical"
        }
        $scriptsCommit = & $getString $vcpkg.scriptsCommit "vcpkg scripts commit"
        $toolCommit = & $getString $vcpkg.toolCommit "vcpkg tool commit"
        $registryBaseline = & $getString $vcpkg.registryBaseline "vcpkg registry baseline"
        foreach ($entry in @(
            @("scripts commit", $scriptsCommit),
            @("registry baseline", $registryBaseline),
            @("tool commit", $toolCommit)
        )) {
            if ([string]$entry[1] -cnotmatch '^[0-9a-f]{40}$') {
                throw "vcpkg $($entry[0]) must be 40 lowercase hexadecimal digits"
            }
        }
        $toolRelease = & $getString $vcpkg.toolRelease "vcpkg tool release"
        $releaseDate = [datetime]::MinValue
        if (
            $toolRelease -cnotmatch '^[0-9]{4}-[0-9]{2}-[0-9]{2}$' -or
            -not [datetime]::TryParseExact(
                $toolRelease,
                "yyyy-MM-dd",
                [Globalization.CultureInfo]::InvariantCulture,
                [Globalization.DateTimeStyles]::None,
                [ref]$releaseDate
            )
        ) {
            throw "vcpkg tool release must be a valid ISO calendar date"
        }

        $asset = & $getObject $vcpkg.windowsAsset @(
            "name", "url", "bytes", "sha256"
        ) "Windows vcpkg asset"
        $assetName = & $getString $asset.name "Windows vcpkg asset name"
        if ($assetName -cne "vcpkg.exe") {
            throw "Windows vcpkg asset name changed"
        }
        $assetUrl = & $getString $asset.url "Windows vcpkg asset URL"
        $expectedAssetUrl = (
            "https://github.com/microsoft/vcpkg-tool/releases/download/" +
            "$toolRelease/vcpkg.exe"
        )
        if ($assetUrl -cne $expectedAssetUrl) {
            throw "Windows vcpkg asset URL is not exactly canonical"
        }
        $assetBytes = 0L
        if (
            $asset.bytes.ValueKind -ne [System.Text.Json.JsonValueKind]::Number -or
            -not $asset.bytes.TryGetInt64([ref]$assetBytes) -or
            $assetBytes -le 0
        ) {
            throw "Windows vcpkg asset bytes must be a positive JSON integer"
        }
        $assetSha256 = & $getString $asset.sha256 "Windows vcpkg asset SHA-256"
        if ($assetSha256 -cnotmatch '^[0-9a-f]{64}$') {
            throw "Windows vcpkg asset SHA-256 must be lowercase hexadecimal"
        }

        $toolsManifest = & $getObject $vcpkg.toolsManifest @(
            "path", "sha256"
        ) "vcpkg tools manifest"
        $toolsManifestPath = & $getString $toolsManifest.path "vcpkg tools manifest path"
        if ($toolsManifestPath -cne "scripts/vcpkg-tools.json") {
            throw "vcpkg tools manifest path changed"
        }
        $toolsManifestSha = & $getString $toolsManifest.sha256 "vcpkg tools manifest SHA-256"
        if ($toolsManifestSha -cnotmatch '^[0-9a-f]{64}$') {
            throw "vcpkg tools manifest SHA-256 must be lowercase hexadecimal"
        }

        if ($vcpkg.internalTools.ValueKind -ne [System.Text.Json.JsonValueKind]::Array) {
            throw "vcpkg internalTools must be a JSON array"
        }
        $toolNames = @()
        foreach ($element in $vcpkg.internalTools.EnumerateArray()) {
            if ($element.ValueKind -ne [System.Text.Json.JsonValueKind]::Object) {
                throw "vcpkg internal tool must be a JSON object"
            }
            $toolName = & $getString $element.GetProperty("name") "vcpkg internal tool name"
            $toolFields = @(
                "name", "version", "url", "archive", "executable", "sha512"
            )
            if ($toolName -ceq "7zip") {
                $toolFields += "executableSha256"
            }
            $tool = & $getObject $element $toolFields "vcpkg internal tool"
            if ($toolName -notin @("cmake", "ninja", "7zip", "7zr") -or $toolNames -ccontains $toolName) {
                throw "vcpkg internal tool names must be exactly cmake, ninja, 7zip, and 7zr"
            }
            $toolNames += $toolName
            $toolVersion = & $getString $tool.version "$toolName version"
            if ($toolVersion -cnotmatch '^[0-9]+(?:\.[0-9]+){1,2}$') {
                throw "$toolName version must be numeric and exact"
            }
            $toolUrl = & $getString $tool.url "$toolName URL"
            $uri = [uri]$toolUrl
            if ($uri.Scheme -cne "https" -or $uri.UserInfo -or $uri.Query -or $uri.Fragment) {
                throw "$toolName URL must be canonical HTTPS without userinfo, query, or fragment"
            }
            foreach ($field in @("archive", "executable")) {
                $value = & $getString $tool.$field "$toolName $field"
                if ([string]::IsNullOrWhiteSpace($value) -or $value.IndexOfAny([char[]]@("`r", "`n")) -ge 0) {
                    throw "$toolName $field is invalid"
                }
            }
            $sha512 = & $getString $tool.sha512 "$toolName SHA-512"
            if ($sha512 -cnotmatch '^[0-9a-f]{128}$') {
                throw "$toolName SHA-512 must be lowercase hexadecimal"
            }
            if ($toolName -ceq "7zip") {
                $executableSha256 = & $getString $tool.executableSha256 `
                    "7zip executable SHA-256"
                if ($executableSha256 -cnotmatch '^[0-9a-f]{64}$') {
                    throw "7zip executable SHA-256 must be lowercase hexadecimal"
                }
            }
        }
        if (($toolNames | Sort-Object) -join ',' -cne '7zip,7zr,cmake,ninja') {
            throw "vcpkg internal tool names must be exactly cmake, ninja, 7zip, and 7zr"
        }
        $sevenZr = @($vcpkg.internalTools.EnumerateArray() | Where-Object {
            $_.GetProperty("name").GetString() -ceq "7zr"
        })[0]
        $sevenZrArchive = $sevenZr.GetProperty("archive").GetString()
        $sevenZrSha512 = $sevenZr.GetProperty("sha512").GetString()
        if ($sevenZrArchive -cne "$($sevenZrSha512.Substring(0, 8))-7zr.exe") {
            throw "7zr local download name must match the vcpkg content hash prefix"
        }

        if ($vcpkg.nativeDependencies.ValueKind -ne [System.Text.Json.JsonValueKind]::Array) {
            throw "vcpkg nativeDependencies must be a JSON array"
        }
        $dependencyNames = @()
        foreach ($element in $vcpkg.nativeDependencies.EnumerateArray()) {
            $dependency = & $getObject $element @(
                "name", "version", "portVersion", "gitTree"
            ) "native dependency"
            $dependencyName = & $getString $dependency.name "native dependency name"
            if (
                $dependencyName -notin @("opencv4", "tesseract", "leptonica") -or
                $dependencyNames -ccontains $dependencyName
            ) {
                throw "native dependencies must be exactly opencv4, tesseract, and leptonica"
            }
            $dependencyNames += $dependencyName
            $dependencyVersion = & $getString $dependency.version "$dependencyName version"
            if ($dependencyVersion -cnotmatch '^[0-9]+(?:\.[0-9]+){1,2}$') {
                throw "$dependencyName version must be numeric and exact"
            }
            $portVersion = 0
            if (
                $dependency.portVersion.ValueKind -ne [System.Text.Json.JsonValueKind]::Number -or
                -not $dependency.portVersion.TryGetInt32([ref]$portVersion) -or
                $portVersion -lt 0
            ) {
                throw "$dependencyName portVersion must be a non-negative JSON integer"
            }
            $gitTree = & $getString $dependency.gitTree "$dependencyName git tree"
            if ($gitTree -cnotmatch '^[0-9a-f]{40}$') {
                throw "$dependencyName git tree must be 40 lowercase hexadecimal digits"
            }
        }
        if (($dependencyNames | Sort-Object) -join ',' -cne 'leptonica,opencv4,tesseract') {
            throw "native dependencies must be exactly opencv4, tesseract, and leptonica"
        }
        return $json | ConvertFrom-Json -Depth 16
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        try {
            $document.Dispose()
        }
        catch {
            if ($null -ne $primaryFailure) {
                $primaryFailure.Exception.Data["EasyConJsonDocumentCleanupFailure"] = `
                    $_.Exception.ToString()
            }
            else {
                throw
            }
        }
    }
}

function Get-EasyConRustToolchainPin {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [scriptblock]$ReparsePointClassifier
    )

    $path = Join-Path $RepositoryRoot "rust-toolchain.toml"
    $text = Read-EasyConPhysicalText -Path $path -TrustedRoot $RepositoryRoot `
        -ReparsePointClassifier $ReparsePointClassifier
    $channelMatches = [regex]::Matches($text, '(?m)^\s*channel\s*=\s*"([^"]+)"\s*$')
    $targetMatches = [regex]::Matches($text, '(?m)^\s*targets\s*=\s*\[\s*"([^"]+)"\s*\]\s*$')
    if ($channelMatches.Count -ne 1 -or $channelMatches[0].Groups[1].Value -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
        throw "rust-toolchain.toml must contain one exact numeric channel"
    }
    if ($targetMatches.Count -ne 1) {
        throw "rust-toolchain.toml must contain one exact target"
    }
    $components = @("clippy", "rustfmt")
    foreach ($component in $components) {
        if ($text -notmatch ('(?m)^\s*components\s*=.*"' + [regex]::Escape($component) + '"')) {
            throw "rust-toolchain.toml must include $component"
        }
    }
    return [pscustomobject]@{
        Channel = $channelMatches[0].Groups[1].Value
        Target = $targetMatches[0].Groups[1].Value
        Components = $components
    }
}

function Get-EasyConCMakeMinimumVersion {
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [scriptblock]$ReparsePointClassifier
    )

    $presets = Read-EasyConPhysicalText `
        -Path (Join-Path $RepositoryRoot "CMakePresets.json") `
        -TrustedRoot $RepositoryRoot -ReparsePointClassifier $ReparsePointClassifier |
        ConvertFrom-Json -Depth 32
    $minimum = $presets.cmakeMinimumRequired
    return [version]::new([int]$minimum.major, [int]$minimum.minor, [int]$minimum.patch)
}

function Get-EasyConCommandPath {
    param(
        [Parameter(Mandatory)]
        [string]$Name
    )

    $command = Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -eq $command) {
        throw "required command is not available on PATH: $Name"
    }
    return Assert-EasyConPhysicalPath -Path $command.Source
}

function Invoke-EasyConNativeCapture {
    param(
        [Parameter(Mandatory)]
        [string]$Program,

        [string[]]$Arguments = @(),

        [Parameter(Mandatory)]
        [string]$Description,

        [string]$WorkingDirectory,

        [switch]$StreamOutput
    )

    $Program = Assert-EasyConPhysicalPath -Path $Program
    $previousLocation = $null
    $primaryFailure = $null
    try {
        if (-not [string]::IsNullOrWhiteSpace($WorkingDirectory)) {
            $previousLocation = Get-Location
            Set-Location -LiteralPath $WorkingDirectory
        }
        $output = @(& $Program @Arguments 2>&1 | ForEach-Object {
            if ($StreamOutput) {
                Write-Host $_
            }
            $_
        })
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            $details = ($output | ForEach-Object { $_.ToString() }) -join `
                [Environment]::NewLine
            throw [System.InvalidOperationException]::new(
                "$Description failed with exit code $exitCode`n$details"
            )
        }
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        if ($null -ne $previousLocation) {
            try {
                Set-Location -LiteralPath $previousLocation.Path
            }
            catch {
                if ($null -ne $primaryFailure) {
                    $primaryFailure.Exception.Data["EasyConLocationCleanupFailure"] = `
                        $_.Exception.ToString()
                }
                else {
                    throw
                }
            }
        }
    }
    return @($output | ForEach-Object { $_.ToString() })
}

function Find-EasyConVisualStudio {
    [CmdletBinding()]
    param(
        [string]$VsWherePath,

        [scriptblock]$ReparsePointClassifier
    )

    if ([string]::IsNullOrWhiteSpace($VsWherePath)) {
        $programFilesX86 = [Environment]::GetEnvironmentVariable("ProgramFiles(x86)", "Process")
        if ([string]::IsNullOrWhiteSpace($programFilesX86)) {
            throw "ProgramFiles(x86) is not set; cannot locate vswhere.exe"
        }
        $VsWherePath = Join-Path $programFilesX86 "Microsoft Visual Studio\Installer\vswhere.exe"
    }
    $VsWherePath = Resolve-EasyConFullPath -Path $VsWherePath
    Assert-EasyConPhysicalPath -Path $VsWherePath `
        -ReparsePointClassifier $ReparsePointClassifier | Out-Null
    if (-not (Test-Path -LiteralPath $VsWherePath -PathType Leaf)) {
        throw "vswhere.exe was not found at $VsWherePath"
    }
    $output = Invoke-EasyConNativeCapture -Program $VsWherePath -Arguments @(
        "-latest",
        "-version",
        "[17.0,18.0)",
        "-products",
        "*",
        "-requires",
        "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
        "-property",
        "installationPath"
    ) -Description "Visual Studio 2022 discovery"
    $installations = @($output | Where-Object {
        -not [string]::IsNullOrWhiteSpace($_)
    })
    if ($installations.Count -eq 0) {
        throw "Visual Studio 2022 with the x64 C++ toolchain was not found"
    }
    $installation = $installations[0].Trim()
    $installation = Resolve-EasyConFullPath -Path $installation
    Assert-EasyConPhysicalPath -Path $installation `
        -ReparsePointClassifier $ReparsePointClassifier | Out-Null
    $devShell = Join-Path $installation "Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
    Assert-EasyConPhysicalPath -Path $devShell -TrustedRoot $installation `
        -ReparsePointClassifier $ReparsePointClassifier | Out-Null
    if (-not (Test-Path -LiteralPath $devShell -PathType Leaf)) {
        throw "Visual Studio was found, but its Developer Shell module is missing: $devShell"
    }
    return $installation
}

function Assert-EasyConMsvcEnvironment {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$InstallationPath,

        [scriptblock]$CommandResolver
    )

    if ($env:VSCMD_ARG_HOST_ARCH -cne "x64" -or $env:VSCMD_ARG_TGT_ARCH -cne "x64") {
        throw "MSVC Developer Shell must be x64-hosted and target x64"
    }
    $variables = @(
        "INCLUDE",
        "LIB",
        "LIBPATH",
        "VCINSTALLDIR",
        "VCToolsInstallDir",
        "VSINSTALLDIR",
        "WindowsSdkDir",
        "WindowsSDKVersion",
        "UniversalCRTSdkDir",
        "UCRTVersion"
    )
    foreach ($name in $variables) {
        $value = [Environment]::GetEnvironmentVariable($name, "Process")
        if ([string]::IsNullOrWhiteSpace($value)) {
            throw "MSVC Developer Shell environment variable is empty: $name"
        }
    }

    $tools = [ordered]@{}
    foreach ($name in @("cl.exe", "link.exe", "lib.exe", "rc.exe", "mt.exe")) {
        $path = if ($null -ne $CommandResolver) {
            & $CommandResolver $name
        }
        else {
            Get-EasyConCommandPath -Name $name
        }
        if ([string]::IsNullOrWhiteSpace([string]$path) -or -not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "MSVC Developer Shell command is missing: $name"
        }
        $tools[$name] = Assert-EasyConPhysicalPath -Path $path
    }
    foreach ($name in @("cl.exe", "link.exe", "lib.exe")) {
        if (-not (Test-EasyConPathWithin -Path $tools[$name] -Root $InstallationPath)) {
            throw "$name did not resolve inside the discovered Visual Studio installation"
        }
        Assert-EasyConPhysicalPath -Path $tools[$name] -TrustedRoot $InstallationPath | Out-Null
    }
    return [pscustomobject]$tools
}

function Initialize-EasyConMsvcEnvironment {
    [CmdletBinding()]
    param(
        [string]$VsWherePath,

        [string]$MsvcToolsVersion,

        [string]$WindowsSdkVersion,

        [scriptblock]$DevShellLoader,

        [scriptblock]$CommandResolver,

        [scriptblock]$ReparsePointClassifier
    )

    $installation = Find-EasyConVisualStudio -VsWherePath $VsWherePath `
        -ReparsePointClassifier $ReparsePointClassifier
    $devShell = Get-EasyConPhysicalFile `
        -Path (Join-Path $installation "Common7\Tools\Microsoft.VisualStudio.DevShell.dll") `
        -TrustedRoot $installation -ReparsePointClassifier $ReparsePointClassifier
    if ($null -ne $DevShellLoader) {
        & $DevShellLoader $installation | Out-Null
    }
    else {
        Assert-EasyConPhysicalPath -Path $devShell -TrustedRoot $installation `
            -ReparsePointClassifier $ReparsePointClassifier | Out-Null
        Import-Module -Name $devShell -ErrorAction Stop | Out-Null
        $developerArguments = @("-arch=x64", "-host_arch=x64")
        if (-not [string]::IsNullOrWhiteSpace($MsvcToolsVersion)) {
            $developerArguments += "-vcvars_ver=$MsvcToolsVersion"
        }
        if (-not [string]::IsNullOrWhiteSpace($WindowsSdkVersion)) {
            $developerArguments += "-winsdk=$WindowsSdkVersion"
        }
        Enter-VsDevShell -VsInstallPath $installation -SkipAutomaticLocation `
            -DevCmdArguments ($developerArguments -join " ") | Out-Null
    }
    $tools = Assert-EasyConMsvcEnvironment -InstallationPath $installation `
        -CommandResolver $CommandResolver
    if (
        -not [string]::IsNullOrWhiteSpace($MsvcToolsVersion) -and
        $env:VCToolsVersion.TrimEnd('\') -cne $MsvcToolsVersion
    ) {
        throw "MSVC tools version $($env:VCToolsVersion) does not match $MsvcToolsVersion; rerun Setup after installing the pinned toolset"
    }
    if (
        -not [string]::IsNullOrWhiteSpace($WindowsSdkVersion) -and
        $env:WindowsSDKVersion.TrimEnd('\') -cne $WindowsSdkVersion
    ) {
        throw "Windows SDK version $($env:WindowsSDKVersion) does not match $WindowsSdkVersion; rerun Setup after installing the pinned SDK"
    }
    return [pscustomobject]@{
        InstallationPath = $installation
        Tools = $tools
    }
}

function Get-EasyConBuildToolVersions {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [version]$CMakeMinimumVersion,

        [Parameter(Mandatory)]
        [version]$NinjaMinimumVersion
    )

    $cmake = Get-EasyConCommandPath -Name "cmake"
    $ninja = Get-EasyConCommandPath -Name "ninja"
    $cmakeOutput = @(Invoke-EasyConNativeCapture -Program $cmake -Arguments @("--version") `
        -Description "CMake version check")
    $ninjaOutput = @(Invoke-EasyConNativeCapture -Program $ninja -Arguments @("--version") `
        -Description "Ninja version check")
    if ($cmakeOutput[0] -notmatch '^cmake version ([0-9]+\.[0-9]+\.[0-9]+)') {
        throw "CMake returned an unrecognized version string"
    }
    $cmakeVersion = ConvertTo-EasyConStrictVersion -Value $Matches[1] `
        -Description "CMake version"
    if ($ninjaOutput[0] -notmatch '^([0-9]+\.[0-9]+\.[0-9]+)') {
        throw "Ninja returned an unrecognized version string"
    }
    $ninjaVersion = ConvertTo-EasyConStrictVersion -Value $Matches[1] `
        -Description "Ninja version"
    if ($cmakeVersion -lt $CMakeMinimumVersion) {
        throw "CMake $cmakeVersion is older than required $CMakeMinimumVersion"
    }
    if ($ninjaVersion -lt $NinjaMinimumVersion) {
        throw "Ninja $ninjaVersion is older than required $NinjaMinimumVersion"
    }
    return [pscustomobject]@{
        CMakePath = $cmake
        CMakeVersion = $cmakeVersion.ToString()
        NinjaPath = $ninja
        NinjaVersion = $ninjaVersion.ToString()
    }
}

function Assert-EasyConCMakeCacheIsolation {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$CargoTargetDirectory,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [scriptblock]$ReparsePointClassifier
    )

    if (-not (Test-Path -LiteralPath $CargoTargetDirectory -PathType Container)) {
        return
    }
    $target = Assert-EasyConPhysicalTree -Path $CargoTargetDirectory `
        -TrustedRoot $CargoTargetDirectory -ReparsePointClassifier $ReparsePointClassifier
    $expectedSource = Assert-EasyConPhysicalPath -Path $RepositoryRoot `
        -ReparsePointClassifier $ReparsePointClassifier
    $caches = Get-ChildItem -LiteralPath $target -Recurse -Force -File `
        -Filter "CMakeCache.txt" -ErrorAction Stop
    foreach ($cache in $caches) {
        $text = Read-EasyConPhysicalText -Path $cache.FullName -TrustedRoot $target `
            -ReparsePointClassifier $ReparsePointClassifier
        if ($text -notmatch '(?m)^CMAKE_PROJECT_NAME:STATIC=easycon_native\r?$') {
            continue
        }
        $match = [regex]::Match($text, '(?m)^CMAKE_HOME_DIRECTORY:INTERNAL=(.+)\r?$')
        if (-not $match.Success) {
            throw "EasyCon CMake cache does not record its source root: $($cache.FullName)"
        }
        $actualSource = Resolve-EasyConFullPath -Path $match.Groups[1].Value.Trim()
        if (-not $actualSource.Equals($expectedSource, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "EasyCon CMake cache belongs to another source root: $actualSource; isolated target is $CargoTargetDirectory"
        }
    }
}

function Assert-EasyConVcpkgCheckout {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$VcpkgRoot,

        [Parameter(Mandatory)]
        [object]$Configuration,

        [Parameter(Mandatory)]
        [string]$VcpkgExecutable
    )

    $root = Assert-EasyConPhysicalPath -Path $VcpkgRoot
    if (-not (Test-Path -LiteralPath $root -PathType Container)) {
        throw "pinned vcpkg checkout is missing: $root"
    }
    Assert-EasyConPhysicalTree -Path $root -TrustedRoot $root | Out-Null
    $requiredFiles = @(
        ".vcpkg-root",
        "bootstrap-vcpkg.bat",
        "bootstrap-vcpkg.sh",
        "scripts/buildsystems/vcpkg.cmake"
    )
    foreach ($relative in $requiredFiles) {
        $requiredPath = Join-Path $root $relative
        Assert-EasyConPhysicalPath -Path $requiredPath -TrustedRoot $root | Out-Null
        if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
            throw "pinned vcpkg checkout is missing $relative"
        }
    }
    $git = Get-EasyConCommandPath -Name "git.exe"
    $head = @(Invoke-EasyConNativeCapture -Program $git -Arguments @(
        "--no-optional-locks", "-c", "core.longpaths=true", "-C", $root,
        "rev-parse", "HEAD"
    ) -Description "vcpkg scripts commit check")[0].Trim()
    if ($head -cne [string]$Configuration.vcpkg.scriptsCommit) {
        throw "vcpkg scripts commit $head does not match the frozen pin"
    }
    foreach ($relative in $requiredFiles) {
        $tracked = @(Invoke-EasyConNativeCapture -Program $git -Arguments @(
            "--no-optional-locks", "-c", "core.longpaths=true", "-C", $root,
            "ls-files", "--error-unmatch", "--", $relative
        ) -Description "vcpkg required tracked file check")
        if ($tracked.Count -ne 1 -or $tracked[0] -cne $relative) {
            throw "pinned vcpkg checkout does not track required file $relative"
        }
    }
    $status = @(Invoke-EasyConNativeCapture -Program $git -Arguments @(
        "--no-optional-locks", "-c", "core.longpaths=true", "-C", $root,
        "status", "--porcelain=v1", "--untracked-files=all", "--ignored=matching"
    ) -Description "vcpkg scripts cleanliness check")
    if ($status.Count -ne 0) {
        throw "pinned vcpkg scripts checkout has tracked, untracked, or ignored content`n$($status -join [Environment]::NewLine)"
    }
    $asset = $Configuration.vcpkg.windowsAsset
    $executable = Get-EasyConPhysicalFile -Path $VcpkgExecutable
    if (Test-EasyConPathWithin -Path $executable -Root $root) {
        throw "vcpkg executable must remain outside the immutable scripts checkout"
    }
    Assert-EasyConPinnedFile -Path $executable -Bytes ([long]$asset.bytes) `
        -Sha256 ([string]$asset.sha256) -Description "vcpkg.exe"
    $versionOutput = @(Invoke-EasyConNativeCapture -Program $executable -Arguments @("version") `
        -Description "vcpkg tool version check")
    $expected = "vcpkg package management program version $($Configuration.vcpkg.toolRelease)-$($Configuration.vcpkg.toolCommit)"
    if ($versionOutput[0].Trim() -cne $expected) {
        throw "unexpected vcpkg tool version: $($versionOutput[0].Trim())"
    }
    $marker = Get-EasyConPhysicalFile -Path (Join-Path $root ".vcpkg-root") `
        -TrustedRoot $root
    $markerBytes = [long](Get-Item -Force -LiteralPath $marker).Length
    $markerSha256 = Get-EasyConFileHash -Path $marker -Algorithm SHA256
    $toolchain = Join-Path $root "scripts/buildsystems/vcpkg.cmake"
    Assert-EasyConPhysicalPath -Path $toolchain -TrustedRoot $root | Out-Null
    return [pscustomobject]@{
        Root = $root
        Executable = $executable
        ExecutableBytes = [long]$asset.bytes
        ExecutableSha256 = ([string]$asset.sha256).ToLowerInvariant()
        Marker = $marker
        MarkerBytes = $markerBytes
        MarkerSha256 = $markerSha256
        Version = $versionOutput[0].Trim()
        Toolchain = $toolchain
    }
}

function Assert-EasyConVcpkgEnvironmentInputs {
    [CmdletBinding()]
    param()

    foreach ($name in @(
        "VCPKG_OVERLAY_PORTS",
        "VCPKG_OVERLAY_TRIPLETS",
        "VCPKG_CHAINLOAD_TOOLCHAIN_FILE",
        "VCPKG_INSTALL_OPTIONS",
        "VCPKG_BOOTSTRAP_OPTIONS"
    )) {
        $value = [Environment]::GetEnvironmentVariable($name, "Process")
        if (-not [string]::IsNullOrWhiteSpace($value)) {
            throw "untrusted vcpkg environment input must be cleared before bootstrap: $name"
        }
        [Environment]::SetEnvironmentVariable($name, $null, "Process")
    }
}

function Clear-EasyConUntrustedBuildEnvironment {
    [CmdletBinding()]
    param(
        [switch]$AllowProxy
    )

    $exactNames = @(
        "AR", "CC", "CFLAGS", "CL", "_CL_", "CXX", "CXXFLAGS", "LINK", "_LINK_",
        "EXTERNAL_INCLUDE", "INCLUDE", "LIB", "LIBPATH", "UCRTCONTENTROOT", "UCRTVERSION",
        "UNIVERSALCRTSDKDIR", "VCINSTALLDIR", "VCTOOLSINSTALLDIR", "VCTOOLSVERSION",
        "VSCMD_ARG_HOST_ARCH", "VSCMD_ARG_TGT_ARCH", "VSINSTALLDIR", "WINDOWSLIBPATH",
        "WINDOWSSDKBINPATH", "WINDOWSSDKDIR", "WINDOWSSDKVERBINPATH", "WINDOWSSDKVERSION",
        "RUSTC", "RUSTC_BOOTSTRAP", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTFLAGS",
        "CARGO_BUILD_TARGET", "CARGO_ENCODED_RUSTFLAGS", "CARGO_HOME", "CARGO_INCREMENTAL",
        "CARGO_TARGET_DIR", "RUSTDOCFLAGS", "RUSTUP_DIST_SERVER", "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN", "RUSTUP_UPDATE_ROOT",
        "TEMP", "TMP", "TMPDIR", "PYTHONPYCACHEPREFIX", "PYTHONDONTWRITEBYTECODE",
        "VCPKG_ROOT", "VCPKG_BINARY_SOURCES", "VCPKG_DOWNLOADS",
        "VCPKG_DISABLE_METRICS", "VCPKG_FEATURE_FLAGS", "VCPKG_OVERLAY_PORTS",
        "VCPKG_OVERLAY_TRIPLETS", "VCPKG_CHAINLOAD_TOOLCHAIN_FILE",
        "VCPKG_INSTALL_OPTIONS", "VCPKG_BOOTSTRAP_OPTIONS",
        "VCPKG_FORCE_DOWNLOADED_BINARIES", "VCPKG_FORCE_SYSTEM_BINARIES"
    )
    if (-not $AllowProxy) {
        $exactNames += @(
            "ALL_PROXY", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY",
            "all_proxy", "http_proxy", "https_proxy", "no_proxy"
        )
    }
    foreach ($entry in @(Get-ChildItem Env:)) {
        $name = [string]$entry.Name
        if (
            $exactNames -contains $name -or
            $name -match '^CARGO_(BUILD|NET|PROFILE|REGISTRIES|TARGET)_' -or
            (-not $AllowProxy -and $name -match '^CARGO_HTTP_') -or
            $name -match '^CMAKE_' -or
            $name -match '^PKG_CONFIG' -or
            $name -match '^RUSTC_' -or
            $name -match '^RUSTDOC' -or
            $name -match '^(__)?VSCMD_' -or
            $name -match '^(HOST_|TARGET_)?(AR|CC|CFLAGS|CXX|CXXFLAGS)(_|$)' -or
            $name -match '^(X_)?VCPKG_' -or
            $name -match '^[A-Z0-9][A-Z0-9_]*_(ROOT|DIR)$'
        ) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        }
    }
}

function Set-EasyConVerifiedProcessEnvironment {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]]$PathDirectories,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary]$Variables
    )

    Clear-EasyConUntrustedBuildEnvironment
    $resolvedDirectories = [System.Collections.Generic.List[string]]::new()
    foreach ($directory in $PathDirectories) {
        $resolved = Assert-EasyConPhysicalPath -Path $directory
        if (-not (Test-Path -LiteralPath $resolved -PathType Container)) {
            throw "verified PATH directory is missing: $resolved"
        }
        if (-not @($resolvedDirectories | Where-Object {
            $_.Equals($resolved, [System.StringComparison]::OrdinalIgnoreCase)
        })) {
            $resolvedDirectories.Add($resolved)
        }
    }
    foreach ($entry in $Variables.GetEnumerator()) {
        $name = [string]$entry.Key
        $value = [string]$entry.Value
        if ($name -cnotmatch '^[A-Z][A-Z0-9_]*$' -or [string]::IsNullOrWhiteSpace($value)) {
            throw "verified environment variables require uppercase names and non-empty values"
        }
        [Environment]::SetEnvironmentVariable($name, $value, "Process")
    }
    [Environment]::SetEnvironmentVariable(
        "PATH",
        ($resolvedDirectories.ToArray() -join [System.IO.Path]::PathSeparator),
        "Process"
    )
}

function Get-EasyConProcessEnvironmentSnapshot {
    $snapshot = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in [Environment]::GetEnvironmentVariables("Process").GetEnumerator()) {
        $snapshot.Add([pscustomobject]@{
            Name = [string]$entry.Key
            Value = [string]$entry.Value
        })
    }
    return $snapshot.ToArray()
}

function Restore-EasyConProcessEnvironment {
    param(
        [Parameter(Mandatory)]
        [object[]]$Snapshot
    )

    foreach ($entry in @([Environment]::GetEnvironmentVariables("Process").GetEnumerator())) {
        Remove-Item -LiteralPath "Env:$([string]$entry.Key)" -ErrorAction SilentlyContinue
    }
    foreach ($entry in $Snapshot) {
        Set-Item -LiteralPath "Env:$($entry.Name)" -Value ([string]$entry.Value)
    }
}

function Get-EasyConVcpkgAsset {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$CacheRoot,

        [Parameter(Mandatory)]
        [object]$Configuration,

        [string]$AssetPath,

        [string]$SharedCacheRoot
    )

    $cache = Assert-EasyConPhysicalPath -Path $CacheRoot
    $downloads = Join-Path $cache "downloads"
    New-EasyConSafeDirectory -Path $downloads -TrustedRoot $cache | Out-Null
    $asset = $Configuration.vcpkg.windowsAsset
    $cachedAsset = Join-Path $downloads "vcpkg-$($Configuration.vcpkg.toolRelease)-windows.exe"
    Assert-EasyConPhysicalPath -Path $cachedAsset -TrustedRoot $cache | Out-Null
    if (-not [string]::IsNullOrWhiteSpace($SharedCacheRoot)) {
        $downloadAction = $null
        if (-not [string]::IsNullOrWhiteSpace($AssetPath)) {
            $source = Assert-EasyConPhysicalPath -Path $AssetPath
            Assert-EasyConPinnedFile -Path $source -Bytes ([long]$asset.bytes) `
                -Sha256 ([string]$asset.sha256) -Description "provided vcpkg.exe"
            $downloadAction = {
                param($Url, $Destination)
                $null = $Url
                Copy-Item -LiteralPath $source -Destination $Destination
            }.GetNewClosure()
        }
        $sharedAsset = Get-EasyConSharedContentAsset -SharedCacheRoot $SharedCacheRoot `
            -Url ([string]$asset.url) -Algorithm SHA256 -Hash ([string]$asset.sha256) `
            -Bytes ([long]$asset.bytes) -Description "pinned vcpkg.exe" `
            -DownloadAction $downloadAction
        return Copy-EasyConContentFileAtomically -Source $sharedAsset `
            -Destination $cachedAsset -Algorithm SHA256 -Hash ([string]$asset.sha256) `
            -Bytes ([long]$asset.bytes) -SourceTrustedRoot $SharedCacheRoot `
            -DestinationTrustedRoot $cache -Description "pinned vcpkg.exe"
    }
    if (Test-Path -LiteralPath $cachedAsset -PathType Leaf) {
        Assert-EasyConPinnedFile -Path $cachedAsset -Bytes ([long]$asset.bytes) `
            -Sha256 ([string]$asset.sha256) -Description "cached vcpkg.exe"
        return $cachedAsset
    }

    if (-not [string]::IsNullOrWhiteSpace($AssetPath)) {
        $source = Assert-EasyConPhysicalPath -Path $AssetPath
        Assert-EasyConPinnedFile -Path $source -Bytes ([long]$asset.bytes) `
            -Sha256 ([string]$asset.sha256) -Description "provided vcpkg.exe"
        Assert-EasyConPhysicalPath -Path $source | Out-Null
        Assert-EasyConPhysicalPath -Path $cachedAsset -TrustedRoot $cache | Out-Null
        Copy-Item -LiteralPath $source -Destination $cachedAsset
        Assert-EasyConPhysicalPath -Path $cachedAsset -TrustedRoot $cache | Out-Null
        Assert-EasyConPinnedFile -Path $cachedAsset -Bytes ([long]$asset.bytes) `
            -Sha256 ([string]$asset.sha256) -Description "cached vcpkg.exe"
        return $cachedAsset
    }

    $temporary = "$cachedAsset.download-$PID-$([guid]::NewGuid().ToString('N'))"
    if (-not (Test-EasyConPathWithin -Path $temporary -Root $cache)) {
        throw "temporary vcpkg download escaped the controlled cache root"
    }
    Assert-EasyConPhysicalPath -Path $temporary -TrustedRoot $cache | Out-Null
    $primaryFailure = $null
    try {
        $curl = Get-EasyConCommandPath -Name "curl.exe"
        Invoke-EasyConNativeCapture -Program $curl -Arguments @(
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--retry",
            "5",
            "--retry-all-errors",
            "--connect-timeout",
            "20",
            "--max-time",
            "180",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--output",
            $temporary,
            [string]$asset.url
        ) -Description "download pinned vcpkg.exe" | Out-Null
        Assert-EasyConPinnedFile -Path $temporary -Bytes ([long]$asset.bytes) `
            -Sha256 ([string]$asset.sha256) -Description "downloaded vcpkg.exe"
        Assert-EasyConPhysicalPath -Path $temporary -TrustedRoot $cache | Out-Null
        Assert-EasyConPhysicalPath -Path $cachedAsset -TrustedRoot $cache | Out-Null
        Move-Item -LiteralPath $temporary -Destination $cachedAsset
        Assert-EasyConPhysicalPath -Path $cachedAsset -TrustedRoot $cache | Out-Null
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        Complete-EasyConTemporaryFileCleanup -Path $temporary -TrustedRoot $cache `
            -Description "vcpkg asset download" -PrimaryFailure $primaryFailure
    }
    Assert-EasyConPinnedFile -Path $cachedAsset -Bytes ([long]$asset.bytes) `
        -Sha256 ([string]$asset.sha256) -Description "cached vcpkg.exe"
    return $cachedAsset
}

function Install-EasyConVcpkgCheckout {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$VcpkgRoot,

        [Parameter(Mandatory)]
        [string]$CacheRoot,

        [Parameter(Mandatory)]
        [object]$Configuration,

        [Parameter(Mandatory)]
        [string]$VcpkgExecutable
    )

    $root = Assert-EasyConPhysicalPath -Path $VcpkgRoot
    $cache = Assert-EasyConPhysicalPath -Path $CacheRoot
    if (-not (Test-EasyConPathWithin -Path $root -Root $cache)) {
        throw "vcpkg installation is restricted to the controlled environment root"
    }
    if (Test-Path -LiteralPath $root) {
        throw "refusing to replace an existing invalid vcpkg checkout: $root"
    }
    $parent = Split-Path -Parent $root
    New-EasyConSafeDirectory -Path $parent -TrustedRoot $cache | Out-Null
    $temporary = "$root.provision-$PID-$([guid]::NewGuid().ToString('N'))"
    if (-not (Test-EasyConPathWithin -Path $temporary -Root $cache)) {
        throw "temporary vcpkg checkout escaped the controlled cache root"
    }
    New-EasyConSafeDirectory -Path $temporary -TrustedRoot $cache | Out-Null
    $completed = $false
    $primaryFailure = $null
    try {
        $git = Get-EasyConCommandPath -Name "git.exe"
        Invoke-EasyConNativeCapture -Program $git -Arguments @(
            "-c", "core.longpaths=true", "-c", "core.symlinks=false", "init", $temporary
        ) `
            -Description "initialize pinned vcpkg checkout" | Out-Null
        Invoke-EasyConNativeCapture -Program $git -Arguments @(
            "-c", "core.longpaths=true", "-c", "core.symlinks=false", "-C", $temporary,
            "remote", "add", "origin",
            [string]$Configuration.vcpkg.scriptsRepository
        ) -Description "set pinned vcpkg origin" | Out-Null
        Invoke-EasyConNativeCapture -Program $git -Arguments @(
            "-c", "http.sslBackend=schannel",
            "-c", "core.longpaths=true", "-c", "core.symlinks=false", "-C", $temporary,
            "fetch", "--depth", "1", "origin",
            [string]$Configuration.vcpkg.scriptsCommit
        ) -Description "fetch pinned vcpkg scripts" | Out-Null
        Invoke-EasyConNativeCapture -Program $git -Arguments @(
            "-c", "core.longpaths=true", "-c", "core.symlinks=false", "-C", $temporary,
            "checkout", "--detach", "FETCH_HEAD"
        ) -Description "check out pinned vcpkg scripts" | Out-Null

        Get-EasyConPhysicalFile -Path $VcpkgExecutable | Out-Null
        Assert-EasyConVcpkgCheckout -VcpkgRoot $temporary `
            -Configuration $Configuration -VcpkgExecutable $VcpkgExecutable | Out-Null
        Assert-EasyConPhysicalTree -Path $temporary -TrustedRoot $cache | Out-Null
        Assert-EasyConPhysicalPath -Path $root -TrustedRoot $cache | Out-Null
        Publish-EasyConDirectoryAtomically -Source $temporary -Destination $root `
            -TrustedRoot $cache | Out-Null
        Assert-EasyConPhysicalPath -Path $root -TrustedRoot $cache | Out-Null
        $completed = $true
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        if (-not $completed -and (Test-Path -LiteralPath $temporary)) {
            try {
                if (-not (Test-EasyConPathWithin -Path $temporary -Root $cache)) {
                    throw "refusing to clean a temporary path outside the controlled cache root"
                }
                Remove-EasyConSafeTree -Path $temporary -TrustedRoot $cache
            }
            catch {
                if ($null -ne $primaryFailure) {
                    $primaryFailure.Exception.Data["EasyConVcpkgStagingCleanupFailure"] = `
                        $_.Exception.ToString()
                    $primaryFailure.Exception.Data["EasyConResidualVcpkgStaging"] = $temporary
                }
                else {
                    throw
                }
            }
        }
    }
}

function Get-EasyConSharedVcpkgCheckout {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$SharedCacheRoot,

        [Parameter(Mandatory)]
        [object]$Configuration,

        [Parameter(Mandatory)]
        [string]$VcpkgExecutable,

        [scriptblock]$ValidateAction,

        [scriptblock]$InstallAction
    )

    $shared = Resolve-EasyConFullPath -Path $SharedCacheRoot
    New-EasyConSafeDirectory -Path $shared -TrustedRoot $shared | Out-Null
    $sources = New-EasyConSafeDirectory -Path (Join-Path $shared "vcpkg-scripts") `
        -TrustedRoot $shared
    $quarantine = New-EasyConSafeDirectory `
        -Path (Join-Path $shared "vcpkg-scripts-quarantine") -TrustedRoot $shared
    $commit = [string]$Configuration.vcpkg.scriptsCommit
    $checkout = Assert-EasyConPhysicalPath -Path (Join-Path $sources $commit) `
        -TrustedRoot $shared
    $lease = Enter-EasyConExclusiveFileLease `
        -Path (Join-Path $shared "locks/vcpkg-scripts-$commit.lock") `
        -TrustedRoot $shared -Description "vcpkg scripts $commit shared cache ownership"
    if ($null -eq $ValidateAction) {
        $assertCheckout = ${function:Assert-EasyConVcpkgCheckout}
        $assertAuditPins = ${function:Assert-EasyConVcpkgAuditPins}
        $ValidateAction = {
            param($Root)
            $verified = & $assertCheckout -VcpkgRoot $Root `
                -Configuration $Configuration -VcpkgExecutable $VcpkgExecutable
            & $assertAuditPins -VcpkgRoot $verified.Root `
                -Configuration $Configuration
            return $verified.Root
        }.GetNewClosure()
    }
    if ($null -eq $InstallAction) {
        $installCheckout = ${function:Install-EasyConVcpkgCheckout}
        $InstallAction = {
            param($Root)
            & $installCheckout -VcpkgRoot $Root -CacheRoot $shared `
                -Configuration $Configuration -VcpkgExecutable $VcpkgExecutable
        }.GetNewClosure()
    }
    try {
        if (Test-Path -LiteralPath $checkout -PathType Container) {
            try {
                return & $ValidateAction $checkout
            }
            catch {
                $damage = $_.Exception
                $quarantined = Join-Path $quarantine (
                    "$commit.corrupt-$PID-$([guid]::NewGuid().ToString('N'))"
                )
                try {
                    Assert-EasyConPhysicalPath -Path $quarantined -TrustedRoot $shared | Out-Null
                    [System.IO.Directory]::Move($checkout, $quarantined)
                }
                catch {
                    $diagnostic = [System.IO.IOException]::new(
                        "cached vcpkg scripts are damaged and could not be isolated: $($damage.Message); $($_.Exception.Message)",
                        $damage
                    )
                    $diagnostic.Data["EasyConVcpkgSourceIsolationFailure"] = `
                        $_.Exception.ToString()
                    throw $diagnostic
                }
            }
        }

        & $InstallAction $checkout
        return & $ValidateAction $checkout
    }
    finally {
        $lease.Dispose()
    }
}

function Install-EasyConVcpkgCheckoutFromShared {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$SourceRoot,

        [Parameter(Mandatory)]
        [string]$VcpkgRoot,

        [Parameter(Mandatory)]
        [string]$EnvironmentRoot,

        [Parameter(Mandatory)]
        [string]$SharedCacheRoot,

        [Parameter(Mandatory)]
        [object]$Configuration,

        [Parameter(Mandatory)]
        [string]$VcpkgExecutable
    )

    $environment = Assert-EasyConPhysicalPath -Path $EnvironmentRoot
    $source = Assert-EasyConPhysicalTree -Path $SourceRoot -TrustedRoot $SharedCacheRoot
    Assert-EasyConVcpkgCheckout -VcpkgRoot $source -Configuration $Configuration `
        -VcpkgExecutable $VcpkgExecutable | Out-Null
    Assert-EasyConVcpkgAuditPins -VcpkgRoot $source -Configuration $Configuration
    $destination = Assert-EasyConPhysicalPath -Path $VcpkgRoot -TrustedRoot $environment
    if (Test-Path -LiteralPath $destination) {
        throw "refusing to replace an existing vcpkg environment checkout"
    }
    New-EasyConSafeDirectory -Path (Split-Path -Parent $destination) `
        -TrustedRoot $environment | Out-Null
    $temporary = "$destination.materialize-$PID-$([guid]::NewGuid().ToString('N'))"
    Assert-EasyConPhysicalPath -Path $temporary -TrustedRoot $environment | Out-Null
    $completed = $false
    $primaryFailure = $null
    try {
        $git = Get-EasyConCommandPath -Name "git.exe"
        Invoke-EasyConNativeCapture -Program $git -Arguments @(
            "-c", "core.longpaths=true", "clone", "--local", "--no-hardlinks", "--no-tags",
            $source, $temporary
        ) -Description "materialize cached vcpkg scripts" | Out-Null
        Invoke-EasyConNativeCapture -Program $git -Arguments @(
            "-c", "core.longpaths=true", "-C", $temporary, "remote", "set-url", "origin",
            [string]$Configuration.vcpkg.scriptsRepository
        ) -Description "restore pinned vcpkg origin" | Out-Null
        Assert-EasyConVcpkgCheckout -VcpkgRoot $temporary -Configuration $Configuration `
            -VcpkgExecutable $VcpkgExecutable | Out-Null
        Assert-EasyConVcpkgAuditPins -VcpkgRoot $temporary -Configuration $Configuration
        Publish-EasyConDirectoryAtomically -Source $temporary -Destination $destination `
            -TrustedRoot $environment | Out-Null
        $completed = $true
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        if (-not $completed -and (Test-Path -LiteralPath $temporary)) {
            try {
                Remove-EasyConSafeTree -Path $temporary -TrustedRoot $environment
            }
            catch {
                if ($null -ne $primaryFailure) {
                    $primaryFailure.Exception.Data["EasyConVcpkgMaterializationCleanupFailure"] = `
                        $_.Exception.ToString()
                    $primaryFailure.Exception.Data["EasyConResidualVcpkgMaterialization"] = `
                        $temporary
                }
                else {
                    throw
                }
            }
        }
    }
}

function Get-EasyConVcpkgRegistryBaseline {
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [scriptblock]$ReparsePointClassifier
    )

    $configuration = Read-EasyConPhysicalText `
        -Path (Join-Path $RepositoryRoot "vcpkg-configuration.json") `
        -TrustedRoot $RepositoryRoot -ReparsePointClassifier $ReparsePointClassifier |
        ConvertFrom-Json -Depth 16
    $baseline = [string]$configuration.'default-registry'.baseline
    if ($baseline -notmatch '^[0-9a-f]{40}$') {
        throw "vcpkg-configuration.json has an invalid registry baseline"
    }
    return $baseline
}

function Get-EasyConEnvironmentFingerprint {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [object]$Configuration
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $records = [System.Collections.Generic.List[object]]::new()
    $builder = [System.Text.StringBuilder]::new()
    foreach ($input in @($Configuration.fingerprintInputs)) {
        $relative = [string]$input.path
        $kind = [string]$input.kind
        $path = Get-EasyConPhysicalFile -Path (Join-Path $repository $relative) `
            -TrustedRoot $repository
        $hash = Get-EasyConFingerprintInputHash -Path $path -Kind $kind
        $records.Add([ordered]@{ path = $relative; kind = $kind; sha256 = $hash })
        [void]$builder.Append($relative)
        [void]$builder.Append("`0")
        [void]$builder.Append($kind)
        [void]$builder.Append("`0")
        [void]$builder.Append($hash)
        [void]$builder.Append("`n")
    }
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($builder.ToString())
    $digest = [System.Security.Cryptography.SHA256]::HashData($bytes)
    return [pscustomobject]@{
        Value = [System.Convert]::ToHexString($digest).ToLowerInvariant()
        Inputs = $records.ToArray()
    }
}

function Get-EasyConEnvironmentLocation {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$Fingerprint,

        [Parameter(Mandatory)]
        [object]$Configuration,

        [string]$CacheRoot
    )

    if ([string]::IsNullOrWhiteSpace($CacheRoot)) {
        $CacheRoot = [Environment]::GetEnvironmentVariable("EASYCON_BUILD_CACHE_ROOT", "Process")
    }
    if ([string]::IsNullOrWhiteSpace($CacheRoot)) {
        $localAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
        if ([string]::IsNullOrWhiteSpace($localAppData)) {
            throw "LOCALAPPDATA is unavailable; pass -CacheRoot explicitly"
        }
        $CacheRoot = Join-Path $localAppData "EasyConSdk/be2"
    }
    $cache = Resolve-EasyConFullPath -Path $CacheRoot
    $repository = Resolve-EasyConFullPath -Path $RepositoryRoot
    if ((Test-EasyConPathWithin -Path $cache -Root $repository) -and -not (
        Test-EasyConPathWithin -Path $cache -Root (Join-Path $repository ".tools")
    )) {
        throw "environment storage inside the repository must remain under ignored .tools storage"
    }
    $workspacePath = Get-EasyConWorkspaceTargetDirectory -RepositoryRoot $repository `
        -CacheRoot $cache
    $workspaceKey = Split-Path -Leaf $workspacePath
    $environmentSchema = [int]$Configuration.version
    $hostTargetIdentity = @(
        [string]$Configuration.target,
        [string]$Configuration.hostTools.visualStudioMajorVersion,
        [string]$Configuration.hostTools.msvcToolsVersion,
        [string]$Configuration.hostTools.windowsSdkVersion
    ) -join "`0"
    $identityBytes = [System.Text.Encoding]::UTF8.GetBytes(
        "$environmentSchema`0$hostTargetIdentity`0$Fingerprint"
    )
    $identityDigest = [System.Security.Cryptography.SHA256]::HashData($identityBytes)
    $identityKey = [System.Convert]::ToHexString($identityDigest).Substring(0, 24).ToLowerInvariant()
    $environmentKey = "v$environmentSchema-$identityKey"
    $environment = Join-Path $cache (Join-Path "e" $environmentKey)
    $writableRoot = Join-Path $cache "w"
    $lockPath = Join-Path $cache (Join-Path "locks" "environment-$environmentKey.lock")
    $workspaceLockPath = Join-Path $cache (Join-Path "locks" "workspace-$workspaceKey.lock")
    return [pscustomobject]@{
        CacheRoot = $cache
        EnvironmentRoot = Resolve-EasyConFullPath -Path $environment
        StampPath = Resolve-EasyConFullPath -Path (Join-Path $environment "environment-stamp.json")
        LockPath = Resolve-EasyConFullPath -Path $lockPath
        IdentityKey = $environmentKey
        EnvironmentSchema = $environmentSchema
        HostTargetIdentity = $hostTargetIdentity
        WorkspaceKey = $workspaceKey
        WritableRoot = Resolve-EasyConFullPath -Path $writableRoot
        WorkspaceRoot = Resolve-EasyConFullPath -Path $workspacePath
        WorkspaceLockPath = Resolve-EasyConFullPath -Path $workspaceLockPath
        CargoTargetDirectory = Resolve-EasyConFullPath -Path (Join-Path $workspacePath "target")
    }
}

function Set-EasyConWorkspaceRuntimeEnvironment {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$WorkspaceRoot,

        [Parameter(Mandatory)]
        [string]$WritableRoot
    )

    $writable = Resolve-EasyConFullPath -Path $WritableRoot
    New-EasyConSafeDirectory -Path $writable -TrustedRoot $writable | Out-Null
    $workspace = New-EasyConSafeDirectory -Path $WorkspaceRoot -TrustedRoot $writable
    $temporary = New-EasyConSafeDirectory -Path (Join-Path $workspace "tmp") `
        -TrustedRoot $writable
    $pythonCache = New-EasyConSafeDirectory -Path (Join-Path $workspace "python-cache") `
        -TrustedRoot $writable

    $env:TEMP = $temporary
    $env:TMP = $temporary
    $env:TMPDIR = $temporary
    $env:PYTHONPYCACHEPREFIX = $pythonCache
    $env:PYTHONDONTWRITEBYTECODE = "1"
    return [pscustomobject]@{
        WorkspaceRoot = $workspace
        Temporary = $temporary
        PythonCache = $pythonCache
    }
}

function Enter-EasyConEnvironmentLease {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Location,

        [ValidateSet("Shared", "Exclusive")]
        [string]$Access,

        [ValidateRange(0, 7200000)]
        [int]$TimeoutMilliseconds = 1800000,

        [ValidateRange(0, 1000)]
        [int]$RetryMilliseconds = 100
    )

    $cache = Resolve-EasyConFullPath -Path ([string]$Location.CacheRoot)
    New-EasyConSafeDirectory -Path $cache -TrustedRoot $cache | Out-Null
    $lockPath = Assert-EasyConPhysicalPath -Path ([string]$Location.LockPath) `
        -TrustedRoot $cache
    New-EasyConSafeDirectory -Path (Split-Path -Parent $lockPath) `
        -TrustedRoot $cache | Out-Null
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $lastFailure = $null
    while ($true) {
        try {
            if ($Access -ceq "Shared") {
                if (-not (Test-Path -LiteralPath $lockPath -PathType Leaf)) {
                    $initializer = [System.IO.FileStream]::new(
                        $lockPath,
                        [System.IO.FileMode]::OpenOrCreate,
                        [System.IO.FileAccess]::ReadWrite,
                        [System.IO.FileShare]::ReadWrite
                    )
                    $initializer.Dispose()
                }
                return [System.IO.FileStream]::new(
                    $lockPath,
                    [System.IO.FileMode]::Open,
                    [System.IO.FileAccess]::Read,
                    [System.IO.FileShare]::Read
                )
            }
            return [System.IO.FileStream]::new(
                $lockPath,
                [System.IO.FileMode]::OpenOrCreate,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None
            )
        }
        catch [System.IO.IOException] {
            $lastFailure = $_.Exception
        }
        if ($timer.ElapsedMilliseconds -ge $TimeoutMilliseconds) {
            throw "Windows build environment ownership is busy for identity $($Location.IdentityKey) ($Access): $($lastFailure.Message)"
        }
        $remaining = $TimeoutMilliseconds - [int]$timer.ElapsedMilliseconds
        $delay = [Math]::Min($RetryMilliseconds, $remaining)
        if ($delay -gt 0) {
            Start-Sleep -Milliseconds $delay
        }
    }
}

function Enter-EasyConWorkspaceLease {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Location,

        [ValidateRange(0, 7200000)]
        [int]$TimeoutMilliseconds = 1800000,

        [ValidateRange(0, 1000)]
        [int]$RetryMilliseconds = 100
    )

    $workspaceLock = $Location.PSObject.Properties["WorkspaceLockPath"]
    $workspaceRoot = $Location.PSObject.Properties["WorkspaceRoot"]
    $workspaceKey = $Location.PSObject.Properties["WorkspaceKey"]
    if (
        $null -eq $workspaceLock -or $null -eq $workspaceRoot -or $null -eq $workspaceKey -or
        [string]::IsNullOrWhiteSpace([string]$workspaceLock.Value) -or
        [string]::IsNullOrWhiteSpace([string]$workspaceRoot.Value) -or
        [string]::IsNullOrWhiteSpace([string]$workspaceKey.Value)
    ) {
        throw "Windows workspace location is missing its isolated writable output identity"
    }
    $cache = Resolve-EasyConFullPath -Path ([string]$Location.CacheRoot)
    $resolvedWorkspace = Assert-EasyConPhysicalPath -Path ([string]$workspaceRoot.Value) `
        -TrustedRoot $cache
    $leaseLocation = [pscustomobject]@{
        CacheRoot = $cache
        LockPath = Resolve-EasyConFullPath -Path ([string]$workspaceLock.Value)
        IdentityKey = "workspace-$([string]$workspaceKey.Value)"
        EnvironmentRoot = $resolvedWorkspace
    }
    return Enter-EasyConEnvironmentLease -Location $leaseLocation -Access Exclusive `
        -TimeoutMilliseconds $TimeoutMilliseconds -RetryMilliseconds $RetryMilliseconds
}

function Test-EasyConVcpkgToolManifestRecord {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Record,

        [Parameter(Mandatory)]
        [object]$Expected
    )

    $name = $Record.PSObject.Properties["name"]
    $operatingSystem = $Record.PSObject.Properties["os"]
    $architecture = $Record.PSObject.Properties["arch"]
    if (
        $null -eq $name -or [string]$name.Value -cne [string]$Expected.name -or
        $null -eq $operatingSystem -or [string]$operatingSystem.Value -cne "windows"
    ) {
        return $false
    }
    if ([string]$Expected.name -ceq "7zr") {
        return $null -eq $architecture
    }
    return $null -ne $architecture -and [string]$architecture.Value -in @("x64", "amd64")
}

function Test-EasyConVcpkgVersionRecord {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Record,

        [Parameter(Mandatory)]
        [object]$Expected
    )

    $versionMatches = $false
    foreach ($field in @("version", "version-semver", "version-string")) {
        $property = $Record.PSObject.Properties[$field]
        if ($null -ne $property -and [string]$property.Value -ceq [string]$Expected.version) {
            $versionMatches = $true
        }
    }
    $portVersion = $Record.PSObject.Properties["port-version"]
    $gitTree = $Record.PSObject.Properties["git-tree"]
    return (
        $versionMatches -and
        $null -ne $portVersion -and [int]$portVersion.Value -eq [int]$Expected.portVersion -and
        $null -ne $gitTree -and [string]$gitTree.Value -ceq [string]$Expected.gitTree
    )
}

function Assert-EasyConVcpkgAuditPins {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$VcpkgRoot,

        [Parameter(Mandatory)]
        [object]$Configuration
    )

    $root = Assert-EasyConPhysicalPath -Path $VcpkgRoot
    $manifest = $Configuration.vcpkg.toolsManifest
    $manifestPath = Get-EasyConPhysicalFile -Path (Join-Path $root $manifest.path) `
        -TrustedRoot $root
    $actualHash = Get-EasyConFileHash -Path $manifestPath -Algorithm SHA256
    if ($actualHash -cne [string]$manifest.sha256) {
        throw "vcpkg internal tools manifest SHA-256 $actualHash does not match the audited pin"
    }
    $document = Read-EasyConPhysicalText -Path $manifestPath -TrustedRoot $root | ConvertFrom-Json -Depth 32
    foreach ($expected in @($Configuration.vcpkg.internalTools)) {
        $actual = @($document.tools | Where-Object {
            Test-EasyConVcpkgToolManifestRecord -Record $_ -Expected $expected
        })
        if ($actual.Count -ne 1) {
            throw "vcpkg internal tools manifest does not contain one Windows x64 $($expected.name) entry"
        }
        $auditFields = @("version", "url", "executable", "sha512")
        if ([string]$expected.name -cne "7zr") {
            $auditFields += "archive"
        }
        foreach ($field in $auditFields) {
            if ([string]$actual[0].$field -cne [string]$expected.$field) {
                throw "vcpkg internal $($expected.name) $field does not match windows_build_environment.json"
            }
        }
        if (
            [string]$expected.name -ceq "7zr" -and
            $null -ne $actual[0].PSObject.Properties["archive"]
        ) {
            throw "vcpkg internal 7zr manifest unexpectedly contains an archive field"
        }
    }

    $baselinePath = Get-EasyConPhysicalFile -Path (Join-Path $root "versions/baseline.json") `
        -TrustedRoot $root
    $baseline = Read-EasyConPhysicalText -Path $baselinePath -TrustedRoot $root |
        ConvertFrom-Json -Depth 128
    foreach ($expected in @($Configuration.vcpkg.nativeDependencies)) {
        $entry = $baseline.default.($expected.name)
        if (
            [string]$entry.baseline -cne [string]$expected.version -or
            [int]$entry.'port-version' -ne [int]$expected.portVersion
        ) {
            throw "vcpkg baseline for $($expected.name) does not match the audited native dependency pin"
        }
        $prefix = switch ([string]$expected.name) {
            "opencv4" { "o-" }
            "tesseract" { "t-" }
            "leptonica" { "l-" }
        }
        $versionsPath = Get-EasyConPhysicalFile `
            -Path (Join-Path $root "versions/$prefix/$($expected.name).json") -TrustedRoot $root
        $versions = (Read-EasyConPhysicalText -Path $versionsPath -TrustedRoot $root |
            ConvertFrom-Json -Depth 64).versions
        $matches = @($versions | Where-Object {
            Test-EasyConVcpkgVersionRecord -Record $_ -Expected $expected
        })
        if ($matches.Count -ne 1) {
            throw "vcpkg versions database does not map $($expected.name) to the audited git tree"
        }
    }
}

function Install-EasyConControlledBuildTools {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$EnvironmentRoot,

        [Parameter(Mandatory)]
        [object]$Configuration,

        [Parameter(Mandatory)]
        [string]$SharedCacheRoot
    )

    $environment = Assert-EasyConPhysicalPath -Path $EnvironmentRoot
    $shared = Resolve-EasyConFullPath -Path $SharedCacheRoot
    New-EasyConSafeDirectory -Path $shared -TrustedRoot $shared | Out-Null
    $downloads = New-EasyConSafeDirectory -Path (Join-Path $shared (Join-Path `
        "vcpkg-downloads" ([string]$Configuration.vcpkg.scriptsCommit))) `
        -TrustedRoot $shared
    $toolsRoot = New-EasyConSafeDirectory -Path (Join-Path $environment "tools") `
        -TrustedRoot $environment
    $result = [ordered]@{}
    foreach ($tool in @($Configuration.vcpkg.internalTools | Where-Object { $_.name -in @("cmake", "ninja") })) {
        $archive = Get-EasyConPinnedDownload -Destination (Join-Path $downloads $tool.archive) `
            -Url ([string]$tool.url) -Sha512 ([string]$tool.sha512) `
            -TrustedRoot $shared -Description "$($tool.name) $($tool.version)" `
            -SharedCacheRoot $SharedCacheRoot
        $destination = Join-Path $toolsRoot "$($tool.name)-$($tool.version)"
        if (-not (Test-Path -LiteralPath $destination -PathType Container)) {
            New-EasyConSafeDirectory -Path $destination -TrustedRoot $environment | Out-Null
            Expand-Archive -LiteralPath $archive -DestinationPath $destination
        }
        $executable = Get-EasyConPhysicalFile -Path (Join-Path $destination $tool.executable) `
            -TrustedRoot $environment
        $result[$tool.name] = $executable
    }
    return [pscustomobject]$result
}

function Write-EasyConEnvironmentStamp {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [object]$Value,

        [Parameter(Mandatory)]
        [string]$EnvironmentRoot,

        [scriptblock]$MoveAction
    )

    $environment = Assert-EasyConPhysicalPath -Path $EnvironmentRoot
    $stamp = Assert-EasyConPhysicalPath -Path $Path -TrustedRoot $environment
    $temporary = "$stamp.write-$PID-$([guid]::NewGuid().ToString('N'))"
    Assert-EasyConPhysicalPath -Path $temporary -TrustedRoot $environment | Out-Null
    if ($null -eq $MoveAction) {
        $MoveAction = {
            param($Source, $Destination)
            Move-Item -Force -LiteralPath $Source -Destination $Destination
        }
    }
    $primaryFailure = $null
    try {
        $json = $Value | ConvertTo-Json -Depth 32
        [System.IO.File]::WriteAllText($temporary, $json + "`n", [System.Text.UTF8Encoding]::new($false))
        & $MoveAction $temporary $stamp
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        Complete-EasyConTemporaryFileCleanup -Path $temporary -TrustedRoot $environment `
            -Description "environment stamp write" -PrimaryFailure $primaryFailure
    }
}

function Assert-EasyConVisionModel {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$ManifestPath,

        [Parameter(Mandatory)]
        [string]$ModelRoot,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [string]$AllowedModelRoot
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $manifestPathResolved = Resolve-EasyConFullPath -Path $ManifestPath -BasePath $repository
    $expectedManifest = Resolve-EasyConFullPath -Path (
        Join-Path $repository "spec/fixtures/vision/ocr-model.json"
    )
    if (-not $manifestPathResolved.Equals($expectedManifest, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "only the tracked OCR test manifest is accepted"
    }
    Assert-EasyConPhysicalPath -Path $manifestPathResolved -TrustedRoot $repository | Out-Null
    $allowedRoot = if ([string]::IsNullOrWhiteSpace($AllowedModelRoot)) {
        Join-Path $repository ".tools/vision-models"
    }
    else {
        Resolve-EasyConFullPath -Path $AllowedModelRoot
    }
    $resolvedModelRoot = Resolve-EasyConFullPath -Path $ModelRoot -BasePath $repository
    if (-not (Test-EasyConPathWithin -Path $resolvedModelRoot -Root $allowedRoot)) {
        throw "OCR test model must remain inside the controlled model storage"
    }
    Assert-EasyConPhysicalPath -Path $resolvedModelRoot -TrustedRoot $allowedRoot | Out-Null
    $manifest = Read-EasyConPhysicalText -Path $manifestPathResolved `
        -TrustedRoot $repository | ConvertFrom-Json -Depth 16
    if (
        $manifest.version -ne 1 -or
        $manifest.purpose -cne "EasyCon SDK Phase 3 OCR component tests only" -or
        [string]$manifest.redistribution -notmatch 'not packaged or tracked'
    ) {
        throw "OCR manifest does not retain its test-only and non-packaged contract"
    }
    foreach ($name in @("model", "license")) {
        $entry = $manifest.$name
        if (
            [string]$entry.path -ne [System.IO.Path]::GetFileName([string]$entry.path) -or
            [long]$entry.bytes -le 0 -or
            [string]$entry.sha256 -notmatch '^[0-9a-f]{64}$'
        ) {
            throw "OCR manifest $name entry is invalid"
        }
        $uri = [uri]$entry.url
        if ($uri.Scheme -cne "https" -or $uri.Host -cne "raw.githubusercontent.com") {
            throw "OCR manifest $name URL is outside the frozen HTTPS host"
        }
        $entryPath = Join-Path $resolvedModelRoot $entry.path
        Assert-EasyConPhysicalPath -Path $entryPath -TrustedRoot $allowedRoot | Out-Null
        Assert-EasyConPinnedFile -Path $entryPath `
            -Bytes ([long]$entry.bytes) -Sha256 ([string]$entry.sha256) `
            -Description "OCR test $name"
    }
    return [pscustomobject]@{
        Root = $resolvedModelRoot
        ModelSha256 = [string]$manifest.model.sha256
        LicenseSha256 = [string]$manifest.license.sha256
    }
}

function Install-EasyConSharedVisionModel {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$ManifestPath,

        [Parameter(Mandatory)]
        [string]$ModelRoot,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$AllowedModelRoot,

        [Parameter(Mandatory)]
        [string]$SharedCacheRoot,

        [Parameter(Mandatory)]
        [string]$PythonPath
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $manifestPathResolved = Get-EasyConPhysicalFile -Path $ManifestPath `
        -TrustedRoot $repository
    Invoke-EasyConNativeCapture -Program $PythonPath -Arguments @(
        "tools/provision_vision_test_model.py",
        "--manifest", "spec/fixtures/vision/ocr-model.json",
        "--output", $ModelRoot,
        "--allowed-root", $AllowedModelRoot,
        "--verify-manifest-only"
    ) -Description "verify frozen OCR test model manifest" `
        -WorkingDirectory $repository | Out-Null
    $manifest = Read-EasyConPhysicalText -Path $manifestPathResolved `
        -TrustedRoot $repository | ConvertFrom-Json -Depth 16
    $allowed = Resolve-EasyConFullPath -Path $AllowedModelRoot
    $model = Resolve-EasyConFullPath -Path $ModelRoot
    if (-not (Test-EasyConPathWithin -Path $model -Root $allowed)) {
        throw "OCR test model must remain inside the controlled model storage"
    }
    New-EasyConSafeDirectory -Path $allowed -TrustedRoot $allowed | Out-Null
    New-EasyConSafeDirectory -Path $model -TrustedRoot $allowed | Out-Null
    foreach ($name in @("model", "license")) {
        $entry = $manifest.$name
        $uri = [uri][string]$entry.url
        if ($uri.Scheme -cne "https" -or $uri.Host -cne "raw.githubusercontent.com") {
            throw "OCR manifest $name URL is outside the frozen HTTPS host"
        }
        $sharedAsset = Get-EasyConSharedContentAsset `
            -SharedCacheRoot $SharedCacheRoot -Url ([string]$entry.url) `
            -Algorithm SHA256 -Hash ([string]$entry.sha256) -Bytes ([long]$entry.bytes) `
            -Description "OCR test $name"
        Copy-EasyConContentFileAtomically -Source $sharedAsset `
            -Destination (Join-Path $model ([string]$entry.path)) -Algorithm SHA256 `
            -Hash ([string]$entry.sha256) -Bytes ([long]$entry.bytes) `
            -SourceTrustedRoot $SharedCacheRoot -DestinationTrustedRoot $allowed `
            -Description "OCR test $name" | Out-Null
    }
    return Assert-EasyConVisionModel -ManifestPath $manifestPathResolved `
        -ModelRoot $model -RepositoryRoot $repository -AllowedModelRoot $allowed
}

function Assert-EasyConRustToolchain {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Pin
    )

    $rustup = Get-EasyConCommandPath -Name "rustup.exe"
    $rustcOutput = @(Invoke-EasyConNativeCapture -Program $rustup -Arguments @(
        "run", [string]$Pin.Channel, "rustc", "--version", "--verbose"
    ) -Description "frozen rustc version check")
    $release = @($rustcOutput | Where-Object { $_ -match '^release: ' })
    $host = @($rustcOutput | Where-Object { $_ -match '^host: ' })
    if ($release.Count -ne 1 -or ($release[0] -split ': ', 2)[1] -cne [string]$Pin.Channel) {
        throw "rustc does not match the frozen channel $($Pin.Channel)"
    }
    if ($host.Count -ne 1 -or ($host[0] -split ': ', 2)[1] -cne [string]$Pin.Target) {
        throw "rustc host does not match the frozen Windows target $($Pin.Target)"
    }
    $targets = @(Invoke-EasyConNativeCapture -Program $rustup -Arguments @(
        "target", "list", "--toolchain", [string]$Pin.Channel, "--installed"
    ) -Description "frozen Rust target check")
    if ($targets -notcontains [string]$Pin.Target) {
        throw "frozen Rust target is not installed: $($Pin.Target)"
    }
    $components = @(Invoke-EasyConNativeCapture -Program $rustup -Arguments @(
        "component", "list", "--toolchain", [string]$Pin.Channel, "--installed"
    ) -Description "frozen Rust component check")
    foreach ($component in $Pin.Components) {
        if (-not @($components | Where-Object { $_ -match ("^" + [regex]::Escape($component) + "-") })) {
            throw "frozen Rust component is not installed: $component"
        }
    }
    $rustupHomeValue = [Environment]::GetEnvironmentVariable("RUSTUP_HOME", "Process")
    if ([string]::IsNullOrWhiteSpace($rustupHomeValue)) {
        throw "RUSTUP_HOME must identify the controlled Rust toolchain root"
    }
    $rustupHome = Assert-EasyConPhysicalPath -Path $rustupHomeValue
    $expectedToolchain = Join-Path $rustupHome (
        "toolchains/$($Pin.Channel)-$($Pin.Target)"
    )
    $expectedCargo = Resolve-EasyConFullPath -Path (Join-Path $expectedToolchain "bin/cargo.exe")
    Assert-EasyConPhysicalPath -Path $expectedCargo -TrustedRoot $rustupHome | Out-Null
    $cargoOutput = @(Invoke-EasyConNativeCapture -Program $rustup -Arguments @(
        "which", "--toolchain", [string]$Pin.Channel, "cargo"
    ) -Description "frozen Cargo path check")
    if (
        $cargoOutput.Count -ne 1 -or
        [string]$cargoOutput[0] -cne ([string]$cargoOutput[0]).Trim() -or
        -not [System.IO.Path]::IsPathFullyQualified([string]$cargoOutput[0])
    ) {
        throw "rustup must report exactly one absolute Cargo path for the frozen toolchain"
    }
    $reportedCargo = Resolve-EasyConFullPath -Path ([string]$cargoOutput[0])
    if (-not $reportedCargo.Equals(
        $expectedCargo, [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw "rustup Cargo path escaped or does not match the frozen toolchain: $reportedCargo"
    }
    $cargo = Get-EasyConPhysicalFile -Path $reportedCargo -TrustedRoot $rustupHome
    $cargoVersion = Get-EasyConCargoVersion -CargoPath $cargo `
        -ExpectedVersion ([string]$Pin.Channel) -ExpectedTarget ([string]$Pin.Target)
    $env:RUSTUP_TOOLCHAIN = [string]$Pin.Channel
    return [pscustomobject]@{
        RustupPath = $rustup
        CargoPath = $cargo
        CargoVersion = $cargoVersion
        Version = [string]$Pin.Channel
        Target = [string]$Pin.Target
    }
}

function Install-EasyConRustToolchain {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Pin,

        [ValidateRange(0, 60000)]
        [int]$RetryDelayMilliseconds = 1000
    )

    $rustup = Get-EasyConCommandPath -Name "rustup.exe"
    $installArguments = @(
        "toolchain",
        "install",
        [string]$Pin.Channel,
        "--profile",
        "minimal",
        "--component",
        "clippy",
        "--component",
        "rustfmt",
        "--target",
        [string]$Pin.Target
    )
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            Invoke-EasyConNativeCapture -Program $rustup -Arguments $installArguments `
                -Description "install frozen Rust toolchain" | Out-Null
            break
        }
        catch {
            if ($attempt -eq 3) {
                throw
            }
            if ($RetryDelayMilliseconds -gt 0) {
                Start-Sleep -Milliseconds $RetryDelayMilliseconds
            }
        }
    }
    return Assert-EasyConRustToolchain -Pin $Pin
}

function ConvertTo-EasyConCMakePathLiteral {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $resolved = Resolve-EasyConFullPath -Path $Path
    if ($resolved.IndexOfAny([char[]]@('"', ';', '$', "`r", "`n")) -ge 0) {
        throw "controlled path cannot be represented safely in a CMake cache literal: $resolved"
    }
    return $resolved.Replace("\", "/")
}

function New-EasyConVcpkgProvisionLayout {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Vcpkg,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$EnvironmentRoot
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $environment = Assert-EasyConPhysicalPath -Path $EnvironmentRoot
    $workspaceRoot = Join-Path $environment "setup/vcpkg"
    $manifestRoot = New-EasyConSafeDirectory -Path (Join-Path $workspaceRoot "manifest") `
        -TrustedRoot $environment
    $buildtrees = New-EasyConSafeDirectory -Path (Join-Path $workspaceRoot "buildtrees") `
        -TrustedRoot $environment
    $packages = New-EasyConSafeDirectory -Path (Join-Path $workspaceRoot "packages") `
        -TrustedRoot $environment
    $installed = New-EasyConSafeDirectory `
        -Path (Join-Path $workspaceRoot "installed") -TrustedRoot $environment

    $manifestEntries = @(Get-ChildItem -Force -LiteralPath $manifestRoot -ErrorAction Stop |
        Where-Object { $_.Name -cne "vcpkg.json" })
    if ($manifestEntries.Count -ne 0) {
        throw "generated vcpkg manifest directory contains unexpected input: $($manifestEntries[0].FullName)"
    }

    $sourceManifest = Join-Path $repository "vcpkg.json"
    $executionManifest = Join-Path $manifestRoot "vcpkg.json"
    Assert-EasyConPhysicalPath -Path $sourceManifest -TrustedRoot $repository | Out-Null
    Assert-EasyConPhysicalPath -Path $executionManifest -TrustedRoot $environment | Out-Null
    $manifestHash = Get-EasyConFileSha256 -Path $sourceManifest
    Copy-Item -Force -LiteralPath $sourceManifest -Destination $executionManifest
    Assert-EasyConPhysicalPath -Path $executionManifest -TrustedRoot $environment | Out-Null
    if ((Get-EasyConFileSha256 -Path $executionManifest) -cne $manifestHash) {
        throw "generated vcpkg manifest does not match the tracked manifest"
    }

    return [pscustomobject]@{
        ManifestRoot = $manifestRoot
        Buildtrees = $buildtrees
        Packages = $packages
        Installed = $installed
    }
}

function New-EasyConVcpkgWorkspaceLayout {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Vcpkg,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$WorkspaceRoot,

        [Parameter(Mandatory)]
        [string]$CacheRoot,

        [Parameter(Mandatory)]
        [string]$EnvironmentRoot,

        [Parameter(Mandatory)]
        [string]$InstalledRoot
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $cache = Assert-EasyConPhysicalPath -Path $CacheRoot
    $workspace = New-EasyConSafeDirectory -Path $WorkspaceRoot -TrustedRoot $cache
    $environment = Assert-EasyConPhysicalPath -Path $EnvironmentRoot -TrustedRoot $EnvironmentRoot
    $installed = Assert-EasyConPhysicalTree -Path $InstalledRoot -TrustedRoot $environment
    $workspaceRoot = New-EasyConSafeDirectory -Path (Join-Path $workspace "vcpkg") `
        -TrustedRoot $cache
    $manifestRoot = New-EasyConSafeDirectory -Path (Join-Path $workspaceRoot "manifest") `
        -TrustedRoot $cache
    $executionRoot = New-EasyConSafeDirectory -Path (Join-Path $workspaceRoot "root") `
        -TrustedRoot $cache
    $scripts = New-EasyConSafeDirectory -Path (Join-Path $executionRoot "scripts") `
        -TrustedRoot $cache
    $buildsystems = New-EasyConSafeDirectory -Path (Join-Path $scripts "buildsystems") `
        -TrustedRoot $cache
    $downloads = New-EasyConSafeDirectory -Path (Join-Path $workspaceRoot "downloads") `
        -TrustedRoot $cache

    foreach ($shape in @(
        [pscustomobject]@{
            Path = $manifestRoot
            Allowed = @("vcpkg.json")
            Description = "generated workspace vcpkg manifest directory"
        },
        [pscustomobject]@{
            Path = $executionRoot
            Allowed = @(".vcpkg-root", "scripts", "vcpkg.exe")
            Description = "generated workspace vcpkg root"
        },
        [pscustomobject]@{
            Path = $scripts
            Allowed = @("buildsystems")
            Description = "generated workspace vcpkg scripts directory"
        },
        [pscustomobject]@{
            Path = $buildsystems
            Allowed = @("vcpkg.cmake")
            Description = "generated workspace vcpkg toolchain directory"
        }
    )) {
        $unexpected = @(Get-ChildItem -Force -LiteralPath $shape.Path -ErrorAction Stop |
            Where-Object { $shape.Allowed -cnotcontains $_.Name })
        if ($unexpected.Count -ne 0) {
            throw "$($shape.Description) contains unexpected input: $($unexpected[0].FullName)"
        }
    }

    $preparedRoot = Assert-EasyConPhysicalPath -Path $Vcpkg.Root -TrustedRoot $environment
    if (-not (Test-Path -LiteralPath $preparedRoot -PathType Container)) {
        throw "prepared vcpkg scripts root is missing: $preparedRoot"
    }
    $actualToolchain = Get-EasyConPhysicalFile -Path $Vcpkg.Toolchain `
        -TrustedRoot $preparedRoot
    $expectedToolchain = Resolve-EasyConFullPath `
        -Path (Join-Path $preparedRoot "scripts/buildsystems/vcpkg.cmake")
    if (-not $actualToolchain.Equals(
        $expectedToolchain,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw "prepared vcpkg toolchain path does not match its fixed scripts checkout"
    }
    $actualMarker = Get-EasyConPhysicalFile -Path $Vcpkg.Marker -TrustedRoot $preparedRoot
    $expectedMarker = Resolve-EasyConFullPath -Path (Join-Path $preparedRoot ".vcpkg-root")
    if (-not $actualMarker.Equals($expectedMarker, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "prepared vcpkg root marker path does not match its fixed scripts checkout"
    }
    $actualExecutable = Get-EasyConPhysicalFile -Path $Vcpkg.Executable `
        -TrustedRoot $environment
    if (Test-EasyConPathWithin -Path $actualExecutable -Root $preparedRoot) {
        throw "vcpkg executable must remain outside the immutable scripts checkout"
    }

    $sourceManifest = Get-EasyConPhysicalFile -Path (Join-Path $repository "vcpkg.json") `
        -TrustedRoot $repository
    $manifestBytes = [long](Get-Item -Force -LiteralPath $sourceManifest).Length
    $manifestHash = Get-EasyConFileHash -Path $sourceManifest -Algorithm SHA256
    $toolchainBytes = [long](Get-Item -Force -LiteralPath $actualToolchain).Length
    $toolchainHash = Get-EasyConFileHash -Path $actualToolchain -Algorithm SHA256
    Assert-EasyConContentFile -Path $actualExecutable -Algorithm SHA256 `
        -Hash ([string]$Vcpkg.ExecutableSha256) -Bytes ([long]$Vcpkg.ExecutableBytes) `
        -TrustedRoot $environment -Description "verified workspace vcpkg.exe source" | Out-Null
    Assert-EasyConContentFile -Path $actualMarker -Algorithm SHA256 `
        -Hash ([string]$Vcpkg.MarkerSha256) -Bytes ([long]$Vcpkg.MarkerBytes) `
        -TrustedRoot $preparedRoot -Description "verified workspace .vcpkg-root source" | Out-Null

    $executionManifest = Join-Path $manifestRoot "vcpkg.json"
    $workspaceExecutable = Join-Path $executionRoot "vcpkg.exe"
    $workspaceMarker = Join-Path $executionRoot ".vcpkg-root"
    $wrapper = Join-Path $buildsystems "vcpkg.cmake"
    foreach ($destination in @(
        [pscustomobject]@{ Path = $executionManifest; Description = "workspace vcpkg manifest" },
        [pscustomobject]@{ Path = $workspaceExecutable; Description = "workspace vcpkg executable" },
        [pscustomobject]@{ Path = $workspaceMarker; Description = "workspace vcpkg root marker" },
        [pscustomobject]@{ Path = $wrapper; Description = "workspace vcpkg toolchain wrapper" }
    )) {
        Assert-EasyConAtomicFileDestination -Path $destination.Path -TrustedRoot $workspace `
            -Description $destination.Description | Out-Null
    }

    Copy-EasyConContentFileAtomically -Source $actualExecutable `
        -Destination $workspaceExecutable -Algorithm SHA256 `
        -Hash ([string]$Vcpkg.ExecutableSha256) -Bytes ([long]$Vcpkg.ExecutableBytes) `
        -SourceTrustedRoot $environment -DestinationTrustedRoot $workspace `
        -Description "workspace vcpkg executable" -ReplaceExisting | Out-Null
    Copy-EasyConContentFileAtomically -Source $actualMarker `
        -Destination $workspaceMarker -Algorithm SHA256 `
        -Hash ([string]$Vcpkg.MarkerSha256) -Bytes ([long]$Vcpkg.MarkerBytes) `
        -SourceTrustedRoot $preparedRoot -DestinationTrustedRoot $workspace `
        -Description "workspace vcpkg root marker" -ReplaceExisting | Out-Null
    Copy-EasyConContentFileAtomically -Source $sourceManifest `
        -Destination $executionManifest -Algorithm SHA256 -Hash $manifestHash `
        -Bytes $manifestBytes -SourceTrustedRoot $repository `
        -DestinationTrustedRoot $workspace -Description "workspace vcpkg manifest" `
        -ReplaceExisting | Out-Null

    $wrapperText = @(
        "# Generated by tools/windows_workspace.psm1; do not edit.",
        "set(Z_VCPKG_ROOT_DIR `"$(ConvertTo-EasyConCMakePathLiteral -Path $executionRoot)`" CACHE INTERNAL `"EasyCon workspace-local vcpkg applocal root`" FORCE)",
        "set(VCPKG_MANIFEST_DIR `"$(ConvertTo-EasyConCMakePathLiteral -Path $manifestRoot)`" CACHE PATH `"EasyCon verified local manifest`" FORCE)",
        "set(VCPKG_MANIFEST_INSTALL OFF CACHE BOOL `"EasyCon Setup owns vcpkg installation`" FORCE)",
        "set(VCPKG_INSTALLED_DIR `"$(ConvertTo-EasyConCMakePathLiteral -Path $installed)`" CACHE PATH `"EasyCon external vcpkg install tree`" FORCE)",
        "include(`"$(ConvertTo-EasyConCMakePathLiteral -Path $actualToolchain)`")",
        ""
    ) -join "`n"
    Write-EasyConUtf8FileAtomically -Path $wrapper -Text $wrapperText `
        -TrustedRoot $workspace -Description "workspace vcpkg toolchain wrapper" | Out-Null
    if ([System.IO.File]::ReadAllText($wrapper, [System.Text.Encoding]::UTF8) -cne $wrapperText) {
        throw "generated workspace vcpkg toolchain wrapper changed during creation"
    }

    Assert-EasyConPhysicalTree -Path $manifestRoot -TrustedRoot $workspace | Out-Null
    Assert-EasyConPhysicalTree -Path $executionRoot -TrustedRoot $workspace | Out-Null
    foreach ($shape in @(
        [pscustomobject]@{ Path = $manifestRoot; Expected = @("vcpkg.json") },
        [pscustomobject]@{ Path = $executionRoot; Expected = @(".vcpkg-root", "scripts", "vcpkg.exe") },
        [pscustomobject]@{ Path = $scripts; Expected = @("buildsystems") },
        [pscustomobject]@{ Path = $buildsystems; Expected = @("vcpkg.cmake") }
    )) {
        $actualEntries = @(Get-ChildItem -Force -LiteralPath $shape.Path -ErrorAction Stop |
            Select-Object -ExpandProperty Name | Sort-Object)
        $expectedEntries = @($shape.Expected | Sort-Object)
        if (($actualEntries -join "`n") -cne ($expectedEntries -join "`n")) {
            throw "generated workspace vcpkg layout is incomplete or contains unexpected entries: $($shape.Path)"
        }
    }

    Assert-EasyConContentFile -Path $actualExecutable -Algorithm SHA256 `
        -Hash ([string]$Vcpkg.ExecutableSha256) -Bytes ([long]$Vcpkg.ExecutableBytes) `
        -TrustedRoot $environment -Description "verified workspace vcpkg.exe source" | Out-Null
    Assert-EasyConContentFile -Path $actualMarker -Algorithm SHA256 `
        -Hash ([string]$Vcpkg.MarkerSha256) -Bytes ([long]$Vcpkg.MarkerBytes) `
        -TrustedRoot $preparedRoot -Description "verified workspace .vcpkg-root source" | Out-Null
    Assert-EasyConContentFile -Path $sourceManifest -Algorithm SHA256 -Hash $manifestHash `
        -Bytes $manifestBytes -TrustedRoot $repository `
        -Description "tracked workspace vcpkg manifest source" | Out-Null
    Assert-EasyConContentFile -Path $actualToolchain -Algorithm SHA256 -Hash $toolchainHash `
        -Bytes $toolchainBytes -TrustedRoot $preparedRoot `
        -Description "prepared workspace vcpkg toolchain source" | Out-Null

    return [pscustomobject]@{
        Root = Assert-EasyConPhysicalPath -Path $executionRoot -TrustedRoot $workspace
        Toolchain = $wrapper
        ManifestRoot = $manifestRoot
        Downloads = $downloads
    }
}

function Install-EasyConVcpkgDependencies {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Vcpkg,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$DownloadsRoot,

        [Parameter(Mandatory)]
        [string]$CacheRoot,

        [string]$DownloadsTrustedRoot,

        [Parameter(Mandatory)]
        [object]$WorkspaceLayout,

        [ValidateRange(0, 60000)]
        [int]$RetryDelayMilliseconds = 1000
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $cache = Assert-EasyConPhysicalPath -Path $CacheRoot
    $downloadsTrustRoot = if ([string]::IsNullOrWhiteSpace($DownloadsTrustedRoot)) {
        $cache
    }
    else {
        Assert-EasyConPhysicalPath -Path $DownloadsTrustedRoot
    }
    $downloads = Assert-EasyConPhysicalPath -Path $DownloadsRoot `
        -TrustedRoot $downloadsTrustRoot
    $buildtrees = Assert-EasyConPhysicalPath -Path $WorkspaceLayout.Buildtrees -TrustedRoot $cache
    $packages = Assert-EasyConPhysicalPath -Path $WorkspaceLayout.Packages -TrustedRoot $cache
    $installed = Assert-EasyConPhysicalPath -Path $WorkspaceLayout.Installed -TrustedRoot $cache
    $manifestRoot = Assert-EasyConPhysicalPath -Path $WorkspaceLayout.ManifestRoot -TrustedRoot $cache
    $triplets = Join-Path $repository "cmake/triplets"
    Assert-EasyConPhysicalTree -Path $triplets -TrustedRoot $repository | Out-Null
    $arguments = @(
        "install",
        "--triplet", "x64-windows-static-md",
        "--vcpkg-root", $Vcpkg.Root,
        "--x-manifest-root=$manifestRoot",
        "--x-install-root=$installed",
        "--x-buildtrees-root=$buildtrees",
        "--x-packages-root=$packages",
        "--downloads-root=$downloads",
        "--overlay-triplets=$triplets"
    )
    $primaryFailure = $null
    try {
        for ($attempt = 1; $attempt -le 3; $attempt++) {
            try {
                Invoke-EasyConNativeCapture -Program $Vcpkg.Executable -Arguments $arguments `
                    -Description "install verified vcpkg dependencies" `
                    -WorkingDirectory $repository -StreamOutput | Out-Null
                break
            }
            catch {
                if ($attempt -eq 3) {
                    throw
                }
                if ($RetryDelayMilliseconds -gt 0) {
                    Start-Sleep -Milliseconds $RetryDelayMilliseconds
                }
            }
        }
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        $cleanupFailures = [System.Collections.Generic.List[object]]::new()
        foreach ($transientTree in @($buildtrees, $packages)) {
            try {
                Remove-EasyConTransientBuildTree -Path $transientTree -TrustedRoot $cache
            }
            catch {
                $cleanupFailures.Add([pscustomobject]@{
                    Failure = $_
                    Path = $transientTree
                    Residual = Test-Path -LiteralPath $transientTree
                })
            }
        }
        if ($cleanupFailures.Count -gt 0) {
            if ($null -ne $primaryFailure) {
                for ($index = 0; $index -lt $cleanupFailures.Count; $index++) {
                    $cleanup = $cleanupFailures[$index]
                    $primaryFailure.Exception.Data["EasyConVcpkgTransientCleanupFailure$index"] = `
                        $cleanup.Failure.Exception.ToString()
                    if ($cleanup.Residual) {
                        $primaryFailure.Exception.Data["EasyConResidualVcpkgTransientTree$index"] = `
                            $cleanup.Path
                    }
                }
            }
            else {
                $first = $cleanupFailures[0]
                $diagnostic = [System.IO.IOException]::new(
                    "vcpkg install succeeded but transient cleanup failed for $($first.Path): $($first.Failure.Exception.Message)",
                    $first.Failure.Exception
                )
                for ($index = 0; $index -lt $cleanupFailures.Count; $index++) {
                    $cleanup = $cleanupFailures[$index]
                    $diagnostic.Data["EasyConVcpkgTransientCleanupFailure$index"] = `
                        $cleanup.Failure.Exception.ToString()
                    if ($cleanup.Residual) {
                        $diagnostic.Data["EasyConResidualVcpkgTransientTree$index"] = `
                            $cleanup.Path
                    }
                }
                throw $diagnostic
            }
        }
    }
}

function Install-EasyConControlledSevenZip {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Vcpkg,

        [Parameter(Mandatory)]
        [string]$DownloadsRoot,

        [Parameter(Mandatory)]
        [string]$CacheRoot,

        [Parameter(Mandatory)]
        [object]$SevenZipPin,

        [Parameter(Mandatory)]
        [object]$SevenZrPin,

        [string]$SharedCacheRoot
    )

    $cache = Assert-EasyConPhysicalPath -Path $CacheRoot
    $downloads = Assert-EasyConPhysicalPath -Path $DownloadsRoot -TrustedRoot $cache
    $sevenZipArchive = Get-EasyConPinnedDownload `
        -Destination (Join-Path $downloads $SevenZipPin.archive) `
        -Url ([string]$SevenZipPin.url) -Sha512 ([string]$SevenZipPin.sha512) `
        -TrustedRoot $cache -Description "7-Zip $($SevenZipPin.version)" `
        -SharedCacheRoot $SharedCacheRoot
    $sevenZrDownload = Get-EasyConPinnedDownload `
        -Destination (Join-Path $downloads $SevenZrPin.archive) `
        -Url ([string]$SevenZrPin.url) -Sha512 ([string]$SevenZrPin.sha512) `
        -TrustedRoot $cache -Description "7zr $($SevenZrPin.version)" `
        -SharedCacheRoot $SharedCacheRoot
    $sevenZr = Install-EasyConPinnedExecutable -Source $sevenZrDownload `
        -Destination (Join-Path $cache (Join-Path `
            "tools/7zr-$($SevenZrPin.version)" ([string]$SevenZrPin.executable))) `
        -Sha512 ([string]$SevenZrPin.sha512) -TrustedRoot $cache `
        -Description "7zr $($SevenZrPin.version)"
    $null = $sevenZipArchive
    $isolatedPath = New-EasyConSafeDirectory `
        -Path (Join-Path $cache "tools/vcpkg-fetch-path") -TrustedRoot $cache
    $isolatedEntries = @(Get-ChildItem -Force -LiteralPath $isolatedPath -ErrorAction Stop)
    if ($isolatedEntries.Count -ne 0) {
        throw "controlled vcpkg fetch PATH must remain empty"
    }
    $expectedSevenZip = Resolve-EasyConFullPath -Path (Join-Path $downloads (
        "tools/7zip-$($SevenZipPin.version)-windows/$($SevenZipPin.executable)"
    ))
    Assert-EasyConPhysicalPath -Path $expectedSevenZip -TrustedRoot $cache | Out-Null

    $originalPath = [Environment]::GetEnvironmentVariable("PATH", "Process")
    $originalForceDownloaded = [Environment]::GetEnvironmentVariable(
        "VCPKG_FORCE_DOWNLOADED_BINARIES", "Process"
    )
    try {
        [Environment]::SetEnvironmentVariable("PATH", $isolatedPath, "Process")
        [Environment]::SetEnvironmentVariable(
            "VCPKG_FORCE_DOWNLOADED_BINARIES", "1", "Process"
        )
        $sevenZipOutput = @(Invoke-EasyConNativeCapture -Program $Vcpkg.Executable -Arguments @(
            "fetch", "7zip", "--vcpkg-root", $Vcpkg.Root, "--downloads-root=$downloads"
        ) -Description "prepare audited vcpkg 7-Zip tool")
    }
    finally {
        [Environment]::SetEnvironmentVariable("PATH", $originalPath, "Process")
        [Environment]::SetEnvironmentVariable(
            "VCPKG_FORCE_DOWNLOADED_BINARIES", $originalForceDownloaded, "Process"
        )
    }

    if ($sevenZipOutput.Count -eq 0) {
        throw "vcpkg 7-Zip fetch returned no audited tool path"
    }
    $pathLines = @($sevenZipOutput | ForEach-Object {
        $line = [string]$_
        $trimmed = $line.Trim()
        if (-not [string]::IsNullOrWhiteSpace($trimmed) -and [System.IO.Path]::IsPathFullyQualified($trimmed)) {
            $trimmed
        }
    })
    $reportedLine = [string]$sevenZipOutput[-1]
    if (
        $reportedLine -cne $reportedLine.Trim() -or
        $pathLines.Count -ne 1 -or
        $pathLines[0] -cne $reportedLine
    ) {
        throw "vcpkg 7-Zip fetch output must end with exactly one absolute audited tool path"
    }
    $reportedSevenZip = Resolve-EasyConFullPath -Path $reportedLine
    if (-not $reportedSevenZip.Equals(
        $expectedSevenZip, [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw "vcpkg 7-Zip fetch output did not select the controlled path: $reportedSevenZip"
    }
    $sevenZip = Get-EasyConPhysicalFile -Path $expectedSevenZip -TrustedRoot $cache
    $sevenZipHash = Get-EasyConFileHash -Path $sevenZip -Algorithm SHA256
    if ($sevenZipHash -cne [string]$SevenZipPin.executableSha256) {
        throw "controlled 7-Zip executable has SHA-256 $sevenZipHash; expected $($SevenZipPin.executableSha256)"
    }
    $versionOutput = @(Invoke-EasyConNativeCapture -Program $sevenZip -Arguments @("i") `
        -Description "verify controlled 7-Zip version")
    $expectedVersionPattern = '^7-Zip ' + [regex]::Escape([string]$SevenZipPin.version) +
        ' \(x64\) : .+$'
    $versionLines = @($versionOutput | Where-Object { [string]$_ -cmatch $expectedVersionPattern })
    if ($versionLines.Count -ne 1) {
        throw "controlled 7-Zip did not report the exact x64 version $($SevenZipPin.version)"
    }
    return [pscustomobject]@{
        SevenZipPath = $sevenZip
        SevenZipVersion = [string]$SevenZipPin.version
        SevenZrPath = $sevenZr
    }
}

function Set-EasyConCargoNativeLinkSearch {
    param(
        [Parameter(Mandatory)]
        [string]$InstalledRoot,

        [Parameter(Mandatory)]
        [string]$CacheRoot
    )

    $cache = Assert-EasyConPhysicalPath -Path $CacheRoot
    $installed = Assert-EasyConPhysicalPath -Path $InstalledRoot -TrustedRoot $cache
    $library = Assert-EasyConPhysicalPath `
        -Path (Join-Path $installed "x64-windows-static-md/lib") -TrustedRoot $cache
    if (-not (Test-Path -LiteralPath $library -PathType Container)) {
        throw "external vcpkg install tree is missing its native library directory"
    }
    Assert-EasyConPhysicalPath -Path $library -TrustedRoot $cache | Out-Null
    $env:CARGO_ENCODED_RUSTFLAGS = "-L$([char]0x1f)native=$library"
    return $library
}

function Get-EasyConCargoVersion {
    param(
        [Parameter(Mandatory)]
        [string]$CargoPath,

        [Parameter(Mandatory)]
        [string]$ExpectedVersion,

        [string]$ExpectedTarget
    )

    $output = @(Invoke-EasyConNativeCapture -Program $CargoPath -Arguments @("--version", "--verbose") `
        -Description "frozen Cargo version check")
    if (
        $output.Count -eq 0 -or
        $output[0] -notmatch '^cargo ([0-9]+\.[0-9]+\.[0-9]+) ' -or
        $Matches[1] -cne $ExpectedVersion
    ) {
        throw "Cargo does not match the frozen Rust version $ExpectedVersion"
    }
    $version = $Matches[1]
    $release = @($output | Where-Object { $_ -cmatch '^release: ' })
    if ($release.Count -ne 1 -or ($release[0] -split ': ', 2)[1] -cne $ExpectedVersion) {
        throw "Cargo release does not match the frozen Rust version $ExpectedVersion"
    }
    if (-not [string]::IsNullOrWhiteSpace($ExpectedTarget)) {
        $host = @($output | Where-Object { $_ -cmatch '^host: ' })
        if ($host.Count -ne 1 -or ($host[0] -split ': ', 2)[1] -cne $ExpectedTarget) {
            throw "Cargo host does not match the frozen Rust target $ExpectedTarget"
        }
    }
    return $version
}

function Install-EasyConCargoSources {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$CargoPath,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$EnvironmentRoot,

        [Parameter(Mandatory)]
        [string]$DownloadCacheRoot
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot -TrustedRoot $RepositoryRoot
    $environment = Assert-EasyConPhysicalTree -Path $EnvironmentRoot -TrustedRoot $EnvironmentRoot
    $downloadCache = Resolve-EasyConFullPath -Path $DownloadCacheRoot
    New-EasyConSafeDirectory -Path $downloadCache -TrustedRoot $downloadCache | Out-Null
    $vendorRoot = Join-Path $environment "cargo-vendor"
    if (Test-Path -LiteralPath $vendorRoot) {
        throw "refusing to replace an existing Cargo vendor tree during Setup"
    }
    Assert-EasyConPhysicalPath -Path $vendorRoot -TrustedRoot $environment | Out-Null

    $previousCargoHome = [Environment]::GetEnvironmentVariable("CARGO_HOME", "Process")
    $primaryFailure = $null
    try {
        $env:CARGO_HOME = $downloadCache
        Invoke-EasyConNativeCapture -Program $CargoPath -Arguments @(
            "vendor", "--locked", "--versioned-dirs", $vendorRoot
        ) -Description "install locked Cargo sources" -WorkingDirectory $repository `
            -StreamOutput | Out-Null
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        try {
            [Environment]::SetEnvironmentVariable("CARGO_HOME", $previousCargoHome, "Process")
        }
        catch {
            if ($null -ne $primaryFailure) {
                $primaryFailure.Exception.Data["EasyConCargoHomeCleanupFailure"] = `
                    $_.Exception.ToString()
            }
            else {
                throw
            }
        }
    }

    $vendor = Assert-EasyConPhysicalTree -Path $vendorRoot -TrustedRoot $environment
    return [pscustomobject]@{
        VendorRoot = $vendor
        VendorTree = Get-EasyConTreeFingerprint -Path $vendor -TrustedRoot $environment
    }
}

function New-EasyConCargoWorkspaceLayout {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$WorkspaceRoot,

        [Parameter(Mandatory)]
        [string]$CacheRoot,

        [Parameter(Mandatory)]
        [string]$VendorRoot,

        [Parameter(Mandatory)]
        [string]$EnvironmentRoot
    )

    $cache = Assert-EasyConPhysicalPath -Path $CacheRoot
    $workspace = New-EasyConSafeDirectory -Path $WorkspaceRoot -TrustedRoot $cache
    $environment = Assert-EasyConPhysicalPath -Path $EnvironmentRoot -TrustedRoot $EnvironmentRoot
    $vendor = Assert-EasyConPhysicalTree -Path $VendorRoot -TrustedRoot $environment
    $cargoHome = New-EasyConSafeDirectory -Path (Join-Path $workspace "cargo-home") `
        -TrustedRoot $cache
    $configurationPath = Join-Path $cargoHome "config.toml"
    Assert-EasyConPhysicalPath -Path $configurationPath -TrustedRoot $workspace | Out-Null
    $vendorLiteral = $vendor.Replace("\", "/")
    if ($vendorLiteral.IndexOfAny([char[]]@('"', "`r", "`n")) -ge 0) {
        throw "Cargo vendor path cannot be represented safely in config.toml"
    }
    $configurationText = @(
        "# Generated by tools/windows_workspace.psm1; do not edit.",
        "[source.crates-io]",
        'replace-with = "easycon-vendored-sources"',
        "",
        "[source.easycon-vendored-sources]",
        "directory = `"$vendorLiteral`"",
        ""
    ) -join "`n"
    [System.IO.File]::WriteAllText(
        $configurationPath,
        $configurationText,
        [System.Text.UTF8Encoding]::new($false)
    )
    $configuration = Get-EasyConPhysicalFile -Path $configurationPath -TrustedRoot $workspace
    if ([System.IO.File]::ReadAllText($configuration, [System.Text.Encoding]::UTF8) -cne $configurationText) {
        throw "generated Cargo source configuration changed during creation"
    }
    return [pscustomobject]@{
        CargoHome = $cargoHome
        Configuration = $configuration
        ConfigurationSha256 = Get-EasyConFileHash -Path $configuration -Algorithm SHA256
    }
}

function ConvertFrom-EasyConPythonVersionOutput {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]]$Output,

        [Parameter(Mandatory)]
        [string]$Description
    )

    if ($Output.Count -ne 1) {
        throw "$Description returned an unrecognized version response; expected exactly one line"
    }
    $line = [string]$Output[0]
    if ($line -cnotmatch '^Python ([0-9]+)\.([0-9]+)\.([0-9]+)$') {
        throw "$Description returned an unrecognized version string"
    }
    return [version]::new([int]$Matches[1], [int]$Matches[2], [int]$Matches[3])
}

function Get-EasyConPythonVersion {
    $python = Get-EasyConCommandPath -Name "python.exe"
    $output = @(Invoke-EasyConNativeCapture -Program $python -Arguments @("--version") `
        -Description "Python version check")
    $version = ConvertFrom-EasyConPythonVersionOutput -Output $output `
        -Description "Python"
    if ($version -lt [version]"3.8.0") {
        throw "Python $version is older than required 3.8.0"
    }
    return [pscustomobject]@{
        Path = $python
        Version = $version
    }
}

function Write-EasyConStructuredRecord {
    param(
        [Parameter(Mandatory)]
        [string]$Kind,

        [Parameter(Mandatory)]
        [object]$Value
    )

    Write-Host ("EASYCON_{0} {1}" -f $Kind.ToUpperInvariant(), ($Value | ConvertTo-Json -Compress -Depth 12))
}

function Install-EasyConWindowsEnvironment {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$ConfigurationPath,

        [Parameter(Mandatory)]
        [string]$EnvironmentRoot,

        [Parameter(Mandatory)]
        [string]$WorkspaceRoot,

        [Parameter(Mandatory)]
        [string]$WritableRoot,

        [string]$SharedCacheRoot,

        [string]$VsWherePath
    )

    $started = [System.Diagnostics.Stopwatch]::StartNew()
    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $configurationFile = Resolve-EasyConFullPath -Path $ConfigurationPath -BasePath $repository
    $expectedConfiguration = Resolve-EasyConFullPath -Path (
        Join-Path $repository "tools/windows_build_environment.json"
    )
    if (-not $configurationFile.Equals($expectedConfiguration, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "only the tracked Windows build environment config is accepted"
    }
    Assert-EasyConPhysicalPath -Path $configurationFile -TrustedRoot $repository | Out-Null
    $configuration = Get-EasyConWindowsBuildConfiguration -Path $configurationFile
    $rustPin = Get-EasyConRustToolchainPin -RepositoryRoot $repository
    if ($rustPin.Target -cne [string]$configuration.target) {
        throw "the Windows workspace target must remain x86_64-pc-windows-msvc"
    }
    $registryBaseline = Get-EasyConVcpkgRegistryBaseline -RepositoryRoot $repository
    if (
        $registryBaseline -cne [string]$configuration.vcpkg.registryBaseline -or
        $registryBaseline -cne [string]$configuration.vcpkg.scriptsCommit
    ) {
        throw "the tracked registry baseline, audited registry pin, and vcpkg scripts pin diverged"
    }

    $cache = Resolve-EasyConFullPath -Path $EnvironmentRoot
    New-EasyConSafeDirectory -Path $cache -TrustedRoot $cache | Out-Null
    $cacheStorage = if ([string]::IsNullOrWhiteSpace($SharedCacheRoot)) {
        $cache
    }
    else {
        Resolve-EasyConFullPath -Path $SharedCacheRoot
    }
    New-EasyConSafeDirectory -Path $cacheStorage -TrustedRoot $cacheStorage | Out-Null
    Assert-EasyConVcpkgEnvironmentInputs
    Clear-EasyConUntrustedBuildEnvironment -AllowProxy
    $runtime = Set-EasyConWorkspaceRuntimeEnvironment -WorkspaceRoot $WorkspaceRoot `
        -WritableRoot $WritableRoot
    $setupTemporary = $runtime.Temporary

    $msvc = Initialize-EasyConMsvcEnvironment -VsWherePath $VsWherePath `
        -MsvcToolsVersion ([string]$configuration.hostTools.msvcToolsVersion) `
        -WindowsSdkVersion ([string]$configuration.hostTools.windowsSdkVersion)
    $controlledTools = Install-EasyConControlledBuildTools -EnvironmentRoot $cache `
        -Configuration $configuration -SharedCacheRoot $cacheStorage
    $controlledPath = @(
        Split-Path -Parent $controlledTools.cmake
        Split-Path -Parent $controlledTools.ninja
    )
    $env:PATH = (($controlledPath + @($env:PATH)) -join [System.IO.Path]::PathSeparator)
    $cmakePin = @($configuration.vcpkg.internalTools | Where-Object { $_.name -ceq "cmake" })[0]
    $ninjaPin = @($configuration.vcpkg.internalTools | Where-Object { $_.name -ceq "ninja" })[0]
    $buildTools = Get-EasyConBuildToolVersions `
        -CMakeMinimumVersion (ConvertTo-EasyConStrictVersion `
            -Value ([string]$cmakePin.version) -Description "CMake version") `
        -NinjaMinimumVersion (ConvertTo-EasyConStrictVersion `
            -Value ([string]$ninjaPin.version) -Description "Ninja version")
    if (
        $buildTools.CMakeVersion -cne [string]$cmakePin.version -or
        $buildTools.NinjaVersion -cne [string]$ninjaPin.version
    ) {
        throw "controlled CMake and Ninja must exactly match windows_build_environment.json"
    }
    $python = Get-EasyConPythonVersion
    $pythonMinimumVersion = ConvertTo-EasyConStrictVersion `
        -Value ([string]$configuration.hostTools.pythonMinimumVersion) `
        -Description "Python minimum version"
    if ($python.Version -lt $pythonMinimumVersion) {
        throw "Python $($python.Version) is older than the pinned minimum $($configuration.hostTools.pythonMinimumVersion)"
    }
    $git = Get-EasyConCommandPath -Name "git.exe"
    $pwsh = Get-EasyConCommandPath -Name "pwsh.exe"

    $resolvedVcpkgRoot = Resolve-EasyConFullPath `
        -Path (Join-Path $cache (Join-Path "vcpkg" $configuration.vcpkg.scriptsCommit))
    if (-not (Test-EasyConPathWithin -Path $resolvedVcpkgRoot -Root $cache)) {
        throw "VCPKG_ROOT must remain inside the controlled build cache"
    }
    $vcpkgExecutable = Get-EasyConVcpkgAsset -CacheRoot $cache `
        -Configuration $configuration -SharedCacheRoot $cacheStorage
    if (-not (Test-Path -LiteralPath $resolvedVcpkgRoot -PathType Container)) {
        $sharedVcpkgRoot = Get-EasyConSharedVcpkgCheckout `
            -SharedCacheRoot $cacheStorage -Configuration $configuration `
            -VcpkgExecutable $vcpkgExecutable
        Install-EasyConVcpkgCheckoutFromShared -SourceRoot $sharedVcpkgRoot `
            -VcpkgRoot $resolvedVcpkgRoot -EnvironmentRoot $cache `
            -SharedCacheRoot $cacheStorage -Configuration $configuration `
            -VcpkgExecutable $vcpkgExecutable
    }
    $vcpkg = Assert-EasyConVcpkgCheckout -VcpkgRoot $resolvedVcpkgRoot `
        -Configuration $configuration -VcpkgExecutable $vcpkgExecutable
    Assert-EasyConVcpkgAuditPins -VcpkgRoot $resolvedVcpkgRoot `
        -Configuration $configuration

    $modelManifest = Join-Path $repository "spec/fixtures/vision/ocr-model.json"
    $allowedModelRoot = Resolve-EasyConFullPath -Path (Join-Path $cache "vision-models")
    $resolvedModelRoot = Resolve-EasyConFullPath `
        -Path (Join-Path $allowedModelRoot $configuration.visionModelDirectory)
    Assert-EasyConPhysicalPath -Path $resolvedModelRoot -TrustedRoot $allowedModelRoot | Out-Null
    $vision = Install-EasyConSharedVisionModel -ManifestPath $modelManifest `
        -ModelRoot $resolvedModelRoot -RepositoryRoot $repository `
        -AllowedModelRoot $allowedModelRoot -SharedCacheRoot $cacheStorage `
        -PythonPath $python.Path

    $rustupHome = New-EasyConSafeDirectory -Path (Join-Path $cacheStorage "rustup-home") `
        -TrustedRoot $cacheStorage
    $env:RUSTUP_HOME = $rustupHome
    $rust = Install-EasyConRustToolchain -Pin $rustPin

    $binaryCache = Join-Path $cacheStorage "vcpkg-binary-cache"
    if ($binaryCache.IndexOfAny([char[]]",;") -ge 0) {
        throw "vcpkg binary cache path cannot contain comma or semicolon"
    }
    New-EasyConSafeDirectory -Path $binaryCache -TrustedRoot $cacheStorage | Out-Null

    $cargoSources = Install-EasyConCargoSources -CargoPath $rust.CargoPath `
        -RepositoryRoot $repository -EnvironmentRoot $cache `
        -DownloadCacheRoot (Join-Path $cacheStorage "cargo-download-cache")

    $externalDownloads = Join-Path $cacheStorage (
        Join-Path "vcpkg-downloads" ([string]$configuration.vcpkg.scriptsCommit)
    )
    $setupVcpkgDownloads = New-EasyConSafeDirectory -Path $externalDownloads `
        -TrustedRoot $cacheStorage

    $vcpkgLayout = New-EasyConVcpkgProvisionLayout -Vcpkg $vcpkg `
        -RepositoryRoot $repository -EnvironmentRoot $cache

    $env:VCPKG_ROOT = $vcpkg.Root
    $env:VCPKG_BINARY_SOURCES = "clear;files,$binaryCache,readwrite"
    $env:VCPKG_DOWNLOADS = $setupVcpkgDownloads
    $env:VCPKG_DISABLE_METRICS = "1"
    $env:VCPKG_FEATURE_FLAGS = "manifests,registries,versions"
    $env:VCPKG_FORCE_DOWNLOADED_BINARIES = "1"
    $env:CXX = [string]$msvc.Tools.'cl.exe'
    $env:CARGO_INCREMENTAL = "0"
    $env:EASYCON_VISION_TEST_TESSDATA = $vision.Root

    $sevenZipPin = @($configuration.vcpkg.internalTools | Where-Object { $_.name -ceq "7zip" })[0]
    $sevenZrPin = @($configuration.vcpkg.internalTools | Where-Object { $_.name -ceq "7zr" })[0]
    $sevenZipTools = Install-EasyConControlledSevenZip -Vcpkg $vcpkg `
        -DownloadsRoot $setupVcpkgDownloads -CacheRoot $cacheStorage `
        -SevenZipPin $sevenZipPin -SevenZrPin $sevenZrPin `
        -SharedCacheRoot $cacheStorage
    $sevenZip = Copy-EasyConContentFileAtomically `
        -Source $sevenZipTools.SevenZipPath `
        -Destination (Join-Path $cache "tools/7zip-$($sevenZipPin.version)/7z.exe") `
        -Algorithm SHA256 -Hash ([string]$sevenZipPin.executableSha256) `
        -SourceTrustedRoot $cacheStorage -DestinationTrustedRoot $cache `
        -Description "controlled 7-Zip executable"
    $sevenZr = Copy-EasyConContentFileAtomically `
        -Source $sevenZipTools.SevenZrPath `
        -Destination (Join-Path $cache "tools/7zr-$($sevenZrPin.version)/7zr.exe") `
        -Algorithm SHA512 -Hash ([string]$sevenZrPin.sha512) `
        -SourceTrustedRoot $cacheStorage -DestinationTrustedRoot $cache `
        -Description "controlled 7zr executable"
    $env:PATH = (@(
        Split-Path -Parent $sevenZip
        Split-Path -Parent $sevenZr
        $env:PATH
    ) -join [System.IO.Path]::PathSeparator)

    Install-EasyConVcpkgDependencies -Vcpkg $vcpkg -RepositoryRoot $repository `
        -DownloadsRoot $setupVcpkgDownloads -DownloadsTrustedRoot $cacheStorage `
        -CacheRoot $cache -WorkspaceLayout $vcpkgLayout

    $nativeTree = Get-EasyConTreeFingerprint -Path $vcpkgLayout.Installed -TrustedRoot $cache
    Assert-EasyConPreparedTreeDoesNotReferenceRepository -Path $cargoSources.VendorRoot `
        -TrustedRoot $cache -RepositoryRoot $repository -WritableRoot $WritableRoot `
        -Description "prepared Cargo vendor tree"
    Assert-EasyConPreparedTreeDoesNotReferenceRepository -Path $vcpkgLayout.Installed `
        -TrustedRoot $cache -RepositoryRoot $repository -WritableRoot $WritableRoot `
        -Description "prepared vcpkg installed tree"
    Remove-EasyConSafeTree -Path $setupTemporary -TrustedRoot $WritableRoot

    $cargoVersion = $rust.CargoVersion
    $started.Stop()
    $summary = [ordered]@{
        status = "installed"
        durationMs = $started.ElapsedMilliseconds
        repository = $repository
        target = $rust.Target
        visualStudio = $msvc.InstallationPath
        msvcToolsVersion = $env:VCToolsVersion.TrimEnd('\')
        windowsSdkVersion = $env:WindowsSDKVersion.TrimEnd('\')
        cmakePath = $buildTools.CMakePath
        cmake = $buildTools.CMakeVersion
        ninjaPath = $buildTools.NinjaPath
        ninja = $buildTools.NinjaVersion
        sevenZipPath = $sevenZip
        sevenZipVersion = $sevenZipTools.SevenZipVersion
        sevenZrPath = $sevenZr
        rust = $rust.Version
        rustupPath = $rust.RustupPath
        cargoPath = $rust.CargoPath
        cargo = $cargoVersion
        cargoVendor = $cargoSources.VendorRoot
        cargoVendorFiles = $cargoSources.VendorTree.Files
        cargoVendorSha256 = $cargoSources.VendorTree.Sha256
        pythonPath = $python.Path
        python = $python.Version.ToString()
        gitPath = $git
        pwshPath = $pwsh
        msvcTools = $msvc.Tools
        vcpkgScripts = [string]$configuration.vcpkg.scriptsCommit
        vcpkgTool = [string]$configuration.vcpkg.toolRelease
        vcpkgRoot = $vcpkg.Root
        vcpkgExecutable = $vcpkg.Executable
        vcpkgInstalled = $vcpkgLayout.Installed
        vcpkgBinaryCache = $binaryCache
        ocrModel = $vision.Root
        nativeTreeFiles = $nativeTree.Files
        nativeTreeSha256 = $nativeTree.Sha256
    }
    Write-EasyConStructuredRecord -Kind "setup" -Value $summary
    return [pscustomobject]$summary
}

function Invoke-EasyConWindowsSetupCore {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$ConfigurationPath,

        [string]$CacheRoot,

        [string]$VsWherePath,

        [object]$Context
    )

    if ($null -eq $Context) {
        $Context = Get-EasyConWindowsEnvironmentContext -RepositoryRoot $RepositoryRoot `
            -ConfigurationPath $ConfigurationPath -CacheRoot $CacheRoot
    }
    Assert-EasyConWindowsEnvironmentContextCurrent -Context $Context
    $repository = $Context.Repository
    $configuration = $Context.Configuration
    $fingerprint = $Context.Fingerprint
    $location = $Context.Location

    $sharedCacheRoot = Resolve-EasyConFullPath -Path (Join-Path $location.CacheRoot "caches")
    $sharedLease = Enter-EasyConSharedCacheLease -CacheRoot $location.CacheRoot `
        -Access Exclusive
    try {
        $installed = Install-EasyConWindowsEnvironment -RepositoryRoot $repository `
            -ConfigurationPath $ConfigurationPath -EnvironmentRoot $location.EnvironmentRoot `
            -WorkspaceRoot $location.WorkspaceRoot -WritableRoot $location.WritableRoot `
            -SharedCacheRoot $sharedCacheRoot -VsWherePath $VsWherePath
    }
    finally {
        $sharedLease.Dispose()
    }
    $toolRecords = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in @(
        @("cmake", $installed.cmakePath, $true),
        @("ninja", $installed.ninjaPath, $true),
        @("7zip", $installed.sevenZipPath, $true),
        @("7zr", $installed.sevenZrPath, $true),
        @("vcpkg", $installed.vcpkgExecutable, $true),
        @("rustup", $installed.rustupPath, $false),
        @("cargo", $installed.cargoPath, $false),
        @("python", $installed.pythonPath, $false),
        @("git", $installed.gitPath, $false),
        @("pwsh", $installed.pwshPath, $false),
        @("cl", $installed.msvcTools.'cl.exe', $false),
        @("link", $installed.msvcTools.'link.exe', $false),
        @("lib", $installed.msvcTools.'lib.exe', $false),
        @("rc", $installed.msvcTools.'rc.exe', $false),
        @("mt", $installed.msvcTools.'mt.exe', $false)
    )) {
        $path = Get-EasyConPhysicalFile -Path ([string]$entry[1])
        if ([bool]$entry[2] -and -not (
            Test-EasyConPathWithin -Path $path -Root $location.EnvironmentRoot
        )) {
            throw "controlled tool $($entry[0]) escaped the prepared environment root"
        }
        Assert-EasyConSharedToolPathBoundary -Path $path -RepositoryRoot $repository `
            -WritableRoot $location.WritableRoot -Description "prepared tool $($entry[0])"
        $toolRecords.Add([ordered]@{
            name = [string]$entry[0]
            path = $path
            sha256 = Get-EasyConFileHash -Path $path -Algorithm SHA256
            controlled = [bool]$entry[2]
        })
    }

    $stamp = [ordered]@{
        schemaVersion = 2
        environmentSchema = $location.EnvironmentSchema
        hostTargetIdentity = $location.HostTargetIdentity
        fingerprint = $fingerprint.Value
        fingerprintInputs = $fingerprint.Inputs
        createdUtc = [datetime]::UtcNow.ToString("o", [Globalization.CultureInfo]::InvariantCulture)
        environmentRoot = $location.EnvironmentRoot
        target = [string]$configuration.target
        tools = $toolRecords.ToArray()
        versions = [ordered]@{
            rust = $installed.rust
            cargo = $installed.cargo
            python = $installed.python
            cmake = $installed.cmake
            ninja = $installed.ninja
            sevenZip = $installed.sevenZipVersion
            msvcTools = $installed.msvcToolsVersion
            windowsSdk = $installed.windowsSdkVersion
            vcpkgScripts = $installed.vcpkgScripts
            vcpkgTool = $installed.vcpkgTool
        }
        paths = [ordered]@{
            cargoVendor = $installed.cargoVendor
            vcpkgScriptsRoot = $installed.vcpkgRoot
            vcpkgInstalled = $installed.vcpkgInstalled
            ocrModel = $installed.ocrModel
        }
        nativeTree = [ordered]@{
            files = $installed.nativeTreeFiles
            sha256 = $installed.nativeTreeSha256
        }
        cargoSources = [ordered]@{
            files = $installed.cargoVendorFiles
            sha256 = $installed.cargoVendorSha256
        }
    }
    Assert-EasyConWindowsEnvironmentContextCurrent -Context $Context
    Write-EasyConEnvironmentStamp -Path $location.StampPath -Value $stamp `
        -EnvironmentRoot $location.EnvironmentRoot
    return [pscustomobject]@{
        status = "installed"
        fingerprint = $fingerprint.Value
        environmentRoot = $location.EnvironmentRoot
    }
}

function Get-EasyConStrictJsonObject {
    param(
        [Parameter(Mandatory)]
        [System.Text.Json.JsonElement]$Element,

        [Parameter(Mandatory)]
        [string[]]$ExpectedKeys,

        [Parameter(Mandatory)]
        [string]$Description
    )

    if ($Element.ValueKind -ne [System.Text.Json.JsonValueKind]::Object) {
        throw "$Description must be a JSON object"
    }
    $properties = @{}
    foreach ($property in $Element.EnumerateObject()) {
        if ($properties.ContainsKey($property.Name)) {
            throw "$Description contains duplicate key '$($property.Name)'"
        }
        $properties[$property.Name] = $property.Value.Clone()
    }
    $actual = @($properties.Keys | Sort-Object)
    $expected = @($ExpectedKeys | Sort-Object)
    if (($actual -join "`0") -cne ($expected -join "`0")) {
        throw "$Description keys must be exactly: $($expected -join ', ')"
    }
    return $properties
}

function Get-EasyConStrictJsonString {
    param(
        [Parameter(Mandatory)]
        [System.Text.Json.JsonElement]$Element,

        [Parameter(Mandatory)]
        [string]$Description
    )

    if ($Element.ValueKind -ne [System.Text.Json.JsonValueKind]::String) {
        throw "$Description must be a JSON string"
    }
    return $Element.GetString()
}

function Get-EasyConStrictJsonUnsignedInteger {
    param(
        [Parameter(Mandatory)]
        [System.Text.Json.JsonElement]$Element,

        [Parameter(Mandatory)]
        [string]$Description,

        [long]$Minimum = 0
    )

    $value = 0L
    if (
        $Element.ValueKind -ne [System.Text.Json.JsonValueKind]::Number -or
        $Element.GetRawText() -cnotmatch '^(0|[1-9][0-9]*)$' -or
        -not $Element.TryGetInt64([ref]$value) -or
        $value -lt $Minimum
    ) {
        $qualification = if ($Minimum -gt 0) { "positive" } else { "non-negative" }
        throw "$Description must be a $qualification JSON integer"
    }
    return $value
}

function Get-EasyConStrictJsonBoolean {
    param(
        [Parameter(Mandatory)]
        [System.Text.Json.JsonElement]$Element,

        [Parameter(Mandatory)]
        [string]$Description
    )

    if ($Element.ValueKind -eq [System.Text.Json.JsonValueKind]::True) {
        return $true
    }
    if ($Element.ValueKind -eq [System.Text.Json.JsonValueKind]::False) {
        return $false
    }
    throw "$Description must be a JSON Boolean"
}

function Read-EasyConEnvironmentStamp {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$EnvironmentRoot
    )

    $document = $null
    try {
        $stampText = Read-EasyConPhysicalText -Path $Path -TrustedRoot $EnvironmentRoot
        $options = [System.Text.Json.JsonDocumentOptions]::new()
        $options.AllowTrailingCommas = $false
        $options.CommentHandling = [System.Text.Json.JsonCommentHandling]::Disallow
        $document = [System.Text.Json.JsonDocument]::Parse($stampText, $options)
    }
    catch {
        throw "Windows build environment stamp is damaged. Rerun Setup: $($_.Exception.Message)"
    }

    try {
        $root = Get-EasyConStrictJsonObject -Element $document.RootElement -ExpectedKeys @(
            "schemaVersion", "environmentSchema", "hostTargetIdentity", "fingerprint",
            "fingerprintInputs", "createdUtc", "environmentRoot", "target", "tools",
            "versions", "paths", "nativeTree", "cargoSources"
        ) -Description "stamp root"
        $schemaVersion = Get-EasyConStrictJsonUnsignedInteger `
            -Element $root.schemaVersion -Description "stamp schemaVersion"
        if ($schemaVersion -ne 2) {
            throw "stamp schemaVersion must be the JSON integer 2"
        }
        $environmentSchema = Get-EasyConStrictJsonUnsignedInteger `
            -Element $root.environmentSchema -Description "stamp environmentSchema" -Minimum 1
        if ($environmentSchema -gt [int]::MaxValue) {
            throw "stamp environmentSchema is outside the supported integer range"
        }
        $hostTargetIdentity = Get-EasyConStrictJsonString `
            -Element $root.hostTargetIdentity -Description "stamp hostTargetIdentity"
        $fingerprint = Get-EasyConStrictJsonString `
            -Element $root.fingerprint -Description "stamp fingerprint"
        if ($fingerprint -cnotmatch '^[0-9a-f]{64}$') {
            throw "stamp fingerprint must be 64 lowercase hexadecimal digits"
        }
        $createdUtc = Get-EasyConStrictJsonString `
            -Element $root.createdUtc -Description "stamp createdUtc"
        $created = [datetime]::MinValue
        if (-not [datetime]::TryParseExact(
            $createdUtc,
            "yyyy-MM-dd'T'HH:mm:ss.fffffff'Z'",
            [Globalization.CultureInfo]::InvariantCulture,
            [Globalization.DateTimeStyles]::AssumeUniversal -bor
                [Globalization.DateTimeStyles]::AdjustToUniversal,
            [ref]$created
        )) {
            throw "stamp createdUtc must be one exact UTC timestamp"
        }
        $environmentRoot = Get-EasyConStrictJsonString `
            -Element $root.environmentRoot -Description "stamp environmentRoot"
        if (-not [System.IO.Path]::IsPathFullyQualified($environmentRoot)) {
            throw "stamp environmentRoot must be an absolute path"
        }
        $target = Get-EasyConStrictJsonString -Element $root.target -Description "stamp target"
        if ([string]::IsNullOrWhiteSpace($hostTargetIdentity) -or [string]::IsNullOrWhiteSpace($target)) {
            throw "stamp identity strings must not be empty"
        }

        if ($root.fingerprintInputs.ValueKind -ne [System.Text.Json.JsonValueKind]::Array) {
            throw "stamp fingerprintInputs must be a JSON array"
        }
        $inputRecords = [System.Collections.Generic.List[object]]::new()
        $inputPaths = [System.Collections.Generic.HashSet[string]]::new(
            [System.StringComparer]::OrdinalIgnoreCase
        )
        foreach ($element in $root.fingerprintInputs.EnumerateArray()) {
            $input = Get-EasyConStrictJsonObject -Element $element `
                -ExpectedKeys @("path", "kind", "sha256") -Description "stamp fingerprint input"
            $relative = Get-EasyConStrictJsonString `
                -Element $input.path -Description "stamp fingerprint input path"
            $components = @($relative.Split('/'))
            if (
                $relative -cnotmatch '^[A-Za-z0-9._/-]+$' -or
                $relative.StartsWith('/') -or
                $relative.Contains('//') -or
                $components -ccontains '.' -or
                $components -ccontains '..' -or
                $components -contains '' -or
                -not $inputPaths.Add($relative)
            ) {
                throw "stamp fingerprint input paths must be unique normalized repository-relative paths"
            }
            $kind = Get-EasyConStrictJsonString `
                -Element $input.kind -Description "stamp fingerprint input kind"
            if ($kind -cnotin @("text", "binary")) {
                throw "stamp fingerprint input kind must be exactly text or binary"
            }
            $sha256 = Get-EasyConStrictJsonString `
                -Element $input.sha256 -Description "stamp fingerprint input SHA-256"
            if ($sha256 -cnotmatch '^[0-9a-f]{64}$') {
                throw "stamp fingerprint input SHA-256 must be lowercase hexadecimal"
            }
            $inputRecords.Add([pscustomobject][ordered]@{
                path = $relative
                kind = $kind
                sha256 = $sha256
            }) | Out-Null
        }
        if ($inputRecords.Count -eq 0) {
            throw "stamp fingerprintInputs must not be empty"
        }

        if ($root.tools.ValueKind -ne [System.Text.Json.JsonValueKind]::Array) {
            throw "stamp tools must be a JSON array"
        }
        $toolRecords = [System.Collections.Generic.List[object]]::new()
        $toolNames = [System.Collections.Generic.HashSet[string]]::new(
            [System.StringComparer]::Ordinal
        )
        foreach ($element in $root.tools.EnumerateArray()) {
            $tool = Get-EasyConStrictJsonObject -Element $element `
                -ExpectedKeys @("name", "path", "sha256", "controlled") `
                -Description "stamp tool"
            $name = Get-EasyConStrictJsonString -Element $tool.name -Description "stamp tool name"
            if ($name -cnotmatch '^[a-z0-9]+$' -or -not $toolNames.Add($name)) {
                throw "stamp tool names must be unique lowercase identifiers"
            }
            $toolPath = Get-EasyConStrictJsonString `
                -Element $tool.path -Description "stamp tool path"
            if (-not [System.IO.Path]::IsPathFullyQualified($toolPath)) {
                throw "stamp tool path must be absolute"
            }
            $toolSha256 = Get-EasyConStrictJsonString `
                -Element $tool.sha256 -Description "stamp tool SHA-256"
            if ($toolSha256 -cnotmatch '^[0-9a-f]{64}$') {
                throw "stamp tool SHA-256 must be lowercase hexadecimal"
            }
            $controlled = Get-EasyConStrictJsonBoolean `
                -Element $tool.controlled -Description "stamp tool controlled"
            $toolRecords.Add([pscustomobject][ordered]@{
                name = $name
                path = $toolPath
                sha256 = $toolSha256
                controlled = $controlled
            }) | Out-Null
        }
        if ($toolRecords.Count -eq 0) {
            throw "stamp tools must not be empty"
        }

        $versions = Get-EasyConStrictJsonObject -Element $root.versions -ExpectedKeys @(
            "rust", "cargo", "python", "cmake", "ninja", "sevenZip", "msvcTools",
            "windowsSdk", "vcpkgScripts", "vcpkgTool"
        ) -Description "stamp versions"
        $versionRecord = [ordered]@{}
        foreach ($name in @(
            "rust", "cargo", "python", "cmake", "ninja", "sevenZip", "msvcTools",
            "windowsSdk", "vcpkgScripts", "vcpkgTool"
        )) {
            $value = Get-EasyConStrictJsonString `
                -Element $versions[$name] -Description "stamp versions $name"
            if ([string]::IsNullOrWhiteSpace($value)) {
                throw "stamp versions $name must not be empty"
            }
            $versionRecord[$name] = $value
        }

        $paths = Get-EasyConStrictJsonObject -Element $root.paths -ExpectedKeys @(
            "cargoVendor", "vcpkgScriptsRoot", "vcpkgInstalled", "ocrModel"
        ) -Description "stamp paths"
        $pathRecord = [ordered]@{}
        foreach ($name in @("cargoVendor", "vcpkgScriptsRoot", "vcpkgInstalled", "ocrModel")) {
            $value = Get-EasyConStrictJsonString `
                -Element $paths[$name] -Description "stamp paths $name"
            if (-not [System.IO.Path]::IsPathFullyQualified($value)) {
                throw "stamp paths $name must be absolute"
            }
            $pathRecord[$name] = $value
        }

        $readTree = {
            param([System.Text.Json.JsonElement]$Element, [string]$Description)
            $tree = Get-EasyConStrictJsonObject -Element $Element `
                -ExpectedKeys @("files", "sha256") -Description $Description
            $files = Get-EasyConStrictJsonUnsignedInteger -Element $tree.files `
                -Description "$Description files" -Minimum 1
            if ($files -gt [int]::MaxValue) {
                throw "$Description files is outside the supported integer range"
            }
            $sha256 = Get-EasyConStrictJsonString `
                -Element $tree.sha256 -Description "$Description SHA-256"
            if ($sha256 -cnotmatch '^[0-9a-f]{64}$') {
                throw "$Description SHA-256 must be lowercase hexadecimal"
            }
            return [pscustomobject][ordered]@{ files = [int]$files; sha256 = $sha256 }
        }
        $nativeTree = & $readTree $root.nativeTree "stamp nativeTree"
        $cargoSources = & $readTree $root.cargoSources "stamp cargoSources"

        return [pscustomobject][ordered]@{
            schemaVersion = [int]$schemaVersion
            environmentSchema = [int]$environmentSchema
            hostTargetIdentity = $hostTargetIdentity
            fingerprint = $fingerprint
            fingerprintInputs = $inputRecords.ToArray()
            createdUtc = $createdUtc
            environmentRoot = $environmentRoot
            target = $target
            tools = $toolRecords.ToArray()
            versions = [pscustomobject]$versionRecord
            paths = [pscustomobject]$pathRecord
            nativeTree = $nativeTree
            cargoSources = $cargoSources
        }
    }
    catch {
        throw "Windows build environment stamp is damaged. Rerun Setup: $($_.Exception.Message)"
    }
    finally {
        $document.Dispose()
    }
}

function Assert-EasyConPreparedPythonVersion {
    param(
        [Parameter(Mandatory)]
        [string]$PythonPath,

        [Parameter(Mandatory)]
        [string]$ExpectedVersion,

        [Parameter(Mandatory)]
        [version]$MinimumVersion
    )

    $expected = ConvertTo-EasyConStrictVersion -Value $ExpectedVersion `
        -Description "prepared Python version"
    $output = @(Invoke-EasyConNativeCapture -Program $PythonPath -Arguments @("--version") `
        -Description "prepared Python version check")
    $actual = ConvertFrom-EasyConPythonVersionOutput -Output $output `
        -Description "prepared Python"
    if ($actual -lt $MinimumVersion) {
        throw "Python $actual is older than required $MinimumVersion"
    }
    if ($actual -ne $expected) {
        throw "Python version changed since Setup. Rerun Setup."
    }
    return $actual
}

function Invoke-EasyConWindowsVerifyCore {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$ConfigurationPath,

        [string]$CacheRoot,

        [string]$VsWherePath,

        [switch]$AllowProxy,

        [object]$Context
    )

    Clear-EasyConUntrustedBuildEnvironment -AllowProxy:$AllowProxy
    $started = [System.Diagnostics.Stopwatch]::StartNew()
    if ($null -eq $Context) {
        $Context = Get-EasyConWindowsEnvironmentContext -RepositoryRoot $RepositoryRoot `
            -ConfigurationPath $ConfigurationPath -CacheRoot $CacheRoot
    }
    Assert-EasyConWindowsEnvironmentContextCurrent -Context $Context
    $repository = $Context.Repository
    $configuration = $Context.Configuration
    $fingerprint = $Context.Fingerprint
    $location = $Context.Location
    $runtime = Set-EasyConWorkspaceRuntimeEnvironment `
        -WorkspaceRoot $location.WorkspaceRoot -WritableRoot $location.WritableRoot
    if (-not (Test-Path -LiteralPath $location.StampPath -PathType Leaf)) {
        throw "Windows build environment is not prepared for fingerprint $($fingerprint.Value). Run: pwsh -NoProfile -File tools/run_windows_workspace.ps1 -Mode Setup"
    }
    $stamp = Read-EasyConEnvironmentStamp -Path $location.StampPath `
        -EnvironmentRoot $location.EnvironmentRoot
    if (
        $stamp.environmentSchema -ne [int]$location.EnvironmentSchema -or
        $stamp.hostTargetIdentity -cne [string]$location.HostTargetIdentity -or
        $stamp.fingerprint -cne $fingerprint.Value -or
        $stamp.environmentRoot -cne $location.EnvironmentRoot -or
        $stamp.target -cne [string]$configuration.target
    ) {
        throw "Windows build environment stamp does not match the current shared identity. Rerun Setup."
    }
    $expectedInputs = @($fingerprint.Inputs)
    $actualInputs = @($stamp.fingerprintInputs)
    if ($actualInputs.Count -ne $expectedInputs.Count) {
        throw "Windows build environment stamp fingerprint inputs do not match. Rerun Setup."
    }
    for ($index = 0; $index -lt $expectedInputs.Count; $index++) {
        if (
            $actualInputs[$index].path -cne [string]$expectedInputs[$index].path -or
            $actualInputs[$index].kind -cne [string]$expectedInputs[$index].kind -or
            $actualInputs[$index].sha256 -cne [string]$expectedInputs[$index].sha256
        ) {
            throw "Windows build environment stamp fingerprint inputs do not match. Rerun Setup."
        }
    }

    $tools = @{}
    $controlledToolNames = @("7zip", "7zr", "cmake", "ninja", "vcpkg")
    foreach ($record in @($stamp.tools)) {
        $name = $record.name
        $expectedControlled = $controlledToolNames -ccontains $name
        if ($record.controlled -ne $expectedControlled) {
            throw "Windows build environment stamp tool $name has an invalid controlled classification. Rerun Setup."
        }
        $path = Get-EasyConPhysicalFile -Path $record.path
        if ($record.controlled -and -not (
            Test-EasyConPathWithin -Path $path -Root $location.EnvironmentRoot
        )) {
            throw "prepared tool $name escaped the controlled environment root. Rerun Setup."
        }
        Assert-EasyConSharedToolPathBoundary -Path $path -RepositoryRoot $repository `
            -WritableRoot $location.WritableRoot -Description "prepared tool $name"
        $actualHash = Get-EasyConFileHash -Path $path -Algorithm SHA256
        if ($actualHash -cne $record.sha256) {
            throw "prepared tool $name is damaged: SHA-256 $actualHash does not match the Setup stamp. Rerun Setup."
        }
        if ($name -ceq "7zip") {
            $sevenZipPin = @($configuration.vcpkg.internalTools | Where-Object {
                $_.name -ceq "7zip"
            })[0]
            if ($actualHash -cne [string]$sevenZipPin.executableSha256) {
                throw "prepared 7-Zip does not match the pinned executable SHA-256. Rerun Setup."
            }
        }
        $tools[$name] = $path
    }
    $expectedToolNames = @(
        "7zip", "7zr", "cargo", "cl", "cmake", "git", "lib", "link", "mt", "ninja",
        "pwsh", "python", "rc", "rustup", "vcpkg"
    )
    if ((@($tools.Keys | Sort-Object) -join ',') -cne ($expectedToolNames -join ',')) {
        throw "Windows build environment stamp tool set is incomplete. Rerun Setup."
    }
    $rustPin = Get-EasyConRustToolchainPin -RepositoryRoot $repository
    $cmakePin = @($configuration.vcpkg.internalTools | Where-Object {
        $_.name -ceq "cmake"
    })[0]
    $ninjaPin = @($configuration.vcpkg.internalTools | Where-Object {
        $_.name -ceq "ninja"
    })[0]
    $sevenZipPin = @($configuration.vcpkg.internalTools | Where-Object {
        $_.name -ceq "7zip"
    })[0]
    if (
        $stamp.versions.rust -cne [string]$rustPin.Channel -or
        $stamp.versions.cargo -cne [string]$rustPin.Channel -or
        $stamp.versions.cmake -cne [string]$cmakePin.version -or
        $stamp.versions.ninja -cne [string]$ninjaPin.version -or
        $stamp.versions.sevenZip -cne [string]$sevenZipPin.version -or
        $stamp.versions.msvcTools -cne [string]$configuration.hostTools.msvcToolsVersion -or
        $stamp.versions.windowsSdk -cne [string]$configuration.hostTools.windowsSdkVersion -or
        $stamp.versions.vcpkgScripts -cne [string]$configuration.vcpkg.scriptsCommit -or
        $stamp.versions.vcpkgTool -cne [string]$configuration.vcpkg.toolRelease
    ) {
        throw "Windows build environment stamp versions do not match the fixed configuration. Rerun Setup."
    }

    $preparedPaths = @{}
    try {
        $sharedCacheRoot = Assert-EasyConPhysicalTree `
            -Path (Join-Path $location.CacheRoot "caches") -TrustedRoot $location.CacheRoot
        $preparedPaths.rustupHome = Assert-EasyConPhysicalTree `
            -Path (Join-Path $sharedCacheRoot "rustup-home") -TrustedRoot $sharedCacheRoot
        foreach ($name in @(
            "cargoVendor", "vcpkgScriptsRoot", "vcpkgInstalled", "ocrModel"
        )) {
            $preparedPaths[$name] = Assert-EasyConPhysicalTree `
                -Path ([string]$stamp.paths.$name) -TrustedRoot $location.EnvironmentRoot
        }
    }
    catch {
        throw "Windows build environment contains a missing or unsafe prepared path. Rerun Setup: $($_.Exception.Message)"
    }
    $cargoVendorTree = Get-EasyConTreeFingerprint -Path $preparedPaths.cargoVendor `
        -TrustedRoot $location.EnvironmentRoot
    if (
        $cargoVendorTree.Files -ne [int]$stamp.cargoSources.files -or
        $cargoVendorTree.Sha256 -cne [string]$stamp.cargoSources.sha256
    ) {
        throw "prepared Cargo vendor tree is missing or damaged. Rerun Setup."
    }

    $msvc = Initialize-EasyConMsvcEnvironment -VsWherePath $VsWherePath `
        -MsvcToolsVersion ([string]$configuration.hostTools.msvcToolsVersion) `
        -WindowsSdkVersion ([string]$configuration.hostTools.windowsSdkVersion)
    foreach ($name in @("cl", "link", "lib", "rc", "mt")) {
        $actual = [string]$msvc.Tools."$name.exe"
        if (-not $actual.Equals($tools[$name], [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "MSVC $name path changed since Setup. Rerun Setup."
        }
    }
    $controlledMsvcEnvironment = [ordered]@{}
    foreach ($name in @(
        "EXTERNAL_INCLUDE", "INCLUDE", "LIB", "LIBPATH", "UCRTCONTENTROOT", "UCRTVERSION",
        "UNIVERSALCRTSDKDIR", "VCINSTALLDIR", "VCTOOLSINSTALLDIR", "VCTOOLSVERSION",
        "VSCMD_ARG_HOST_ARCH", "VSCMD_ARG_TGT_ARCH", "VSINSTALLDIR", "WINDOWSLIBPATH",
        "WINDOWSSDKBINPATH", "WINDOWSSDKDIR", "WINDOWSSDKVERBINPATH", "WINDOWSSDKVERSION"
    )) {
        $value = [Environment]::GetEnvironmentVariable($name, "Process")
        if (-not [string]::IsNullOrWhiteSpace($value)) {
            $controlledMsvcEnvironment[$name] = $value
        }
    }

    $pathDirectories = @(
        Split-Path -Parent $tools.cmake
        Split-Path -Parent $tools.ninja
        Split-Path -Parent $tools.vcpkg
        Split-Path -Parent $tools.'7zip'
        Split-Path -Parent $tools.'7zr'
        Split-Path -Parent $tools.rustup
        Split-Path -Parent $tools.cargo
        Split-Path -Parent $tools.python
        Split-Path -Parent $tools.git
        Split-Path -Parent $tools.pwsh
        Split-Path -Parent $tools.cl
        Split-Path -Parent $tools.link
        Split-Path -Parent $tools.lib
        Split-Path -Parent $tools.rc
        Split-Path -Parent $tools.mt
    ) | Select-Object -Unique
    $systemRoot = [Environment]::GetEnvironmentVariable("SystemRoot", "Process")
    if ([string]::IsNullOrWhiteSpace($systemRoot)) {
        throw "SystemRoot is unavailable; cannot construct the verified process PATH"
    }
    $pathDirectories += @($systemRoot, (Join-Path $systemRoot "System32"))
    $workspaceCargoHome = Join-Path $location.WorkspaceRoot "cargo-home"
    $workspaceDownloads = Join-Path $location.WorkspaceRoot "vcpkg/downloads"
    $verifiedVariables = [ordered]@{
        AR = $tools.lib
        CC = $tools.cl
        CXX = $tools.cl
        CARGO_HOME = $workspaceCargoHome
        CARGO_INCREMENTAL = "0"
        CARGO_TARGET_DIR = $location.CargoTargetDirectory
        CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = $tools.link
        EASYCON_VISION_TEST_TESSDATA = $preparedPaths.ocrModel
        RUSTUP_HOME = $preparedPaths.rustupHome
        RUSTUP_TOOLCHAIN = [string]$rustPin.Channel
        TEMP = $runtime.Temporary
        TMP = $runtime.Temporary
        TMPDIR = $runtime.Temporary
        PYTHONPYCACHEPREFIX = $runtime.PythonCache
        PYTHONDONTWRITEBYTECODE = "1"
        VCPKG_BINARY_SOURCES = "clear"
        VCPKG_DISABLE_METRICS = "1"
        VCPKG_DOWNLOADS = $workspaceDownloads
        VCPKG_FEATURE_FLAGS = "manifests,registries,versions"
        VCPKG_FORCE_DOWNLOADED_BINARIES = "1"
        VCPKG_ROOT = $preparedPaths.vcpkgScriptsRoot
    }
    foreach ($entry in $controlledMsvcEnvironment.GetEnumerator()) {
        $verifiedVariables[$entry.Key] = $entry.Value
    }
    Set-EasyConVerifiedProcessEnvironment -PathDirectories $pathDirectories `
        -Variables $verifiedVariables
    $rust = Assert-EasyConRustToolchain -Pin $rustPin
    if (
        -not $rust.RustupPath.Equals($tools.rustup, [System.StringComparison]::OrdinalIgnoreCase) -or
        -not $rust.CargoPath.Equals($tools.cargo, [System.StringComparison]::OrdinalIgnoreCase)
    ) {
        throw "Rust tool paths changed since Setup. Rerun Setup."
    }
    $cargoVersion = $rust.CargoVersion
    $buildTools = Get-EasyConBuildToolVersions `
        -CMakeMinimumVersion ([version]$stamp.versions.cmake) `
        -NinjaMinimumVersion ([version]$stamp.versions.ninja)
    if (
        -not $buildTools.CMakePath.Equals($tools.cmake, [System.StringComparison]::OrdinalIgnoreCase) -or
        -not $buildTools.NinjaPath.Equals($tools.ninja, [System.StringComparison]::OrdinalIgnoreCase) -or
        $buildTools.CMakeVersion -cne [string]$stamp.versions.cmake -or
        $buildTools.NinjaVersion -cne [string]$stamp.versions.ninja -or
        [string]$stamp.versions.sevenZip -cne [string]$sevenZipPin.version
    ) {
        throw "controlled build tool version changed since Setup. Rerun Setup."
    }
    $pythonMinimumVersion = ConvertTo-EasyConStrictVersion `
        -Value ([string]$configuration.hostTools.pythonMinimumVersion) `
        -Description "Python minimum version"
    Assert-EasyConPreparedPythonVersion -PythonPath $tools.python `
        -ExpectedVersion ([string]$stamp.versions.python) `
        -MinimumVersion $pythonMinimumVersion | Out-Null

    $vcpkg = Assert-EasyConVcpkgCheckout -VcpkgRoot $preparedPaths.vcpkgScriptsRoot `
        -Configuration $configuration -VcpkgExecutable $tools.vcpkg
    Assert-EasyConVcpkgAuditPins -VcpkgRoot $vcpkg.Root -Configuration $configuration
    $allowedModelRoot = Join-Path $location.EnvironmentRoot "vision-models"
    $vision = Assert-EasyConVisionModel `
        -ManifestPath (Join-Path $repository "spec/fixtures/vision/ocr-model.json") `
        -ModelRoot $preparedPaths.ocrModel -RepositoryRoot $repository `
        -AllowedModelRoot $allowedModelRoot
    $nativeTree = Get-EasyConTreeFingerprint -Path $preparedPaths.vcpkgInstalled `
        -TrustedRoot $location.EnvironmentRoot
    if (
        $nativeTree.Files -ne [int]$stamp.nativeTree.files -or
        $nativeTree.Sha256 -cne [string]$stamp.nativeTree.sha256
    ) {
        throw "prepared native dependency tree is missing or damaged. Rerun Setup."
    }
    Assert-EasyConPreparedTreeDoesNotReferenceRepository -Path $preparedPaths.cargoVendor `
        -TrustedRoot $location.EnvironmentRoot -RepositoryRoot $repository `
        -WritableRoot $location.WritableRoot `
        -Description "prepared Cargo vendor tree"
    Assert-EasyConPreparedTreeDoesNotReferenceRepository -Path $preparedPaths.vcpkgInstalled `
        -TrustedRoot $location.EnvironmentRoot -RepositoryRoot $repository `
        -WritableRoot $location.WritableRoot `
        -Description "prepared vcpkg installed tree"

    $workspaceCargo = New-EasyConCargoWorkspaceLayout `
        -WorkspaceRoot $location.WorkspaceRoot -CacheRoot $location.CacheRoot `
        -VendorRoot $preparedPaths.cargoVendor -EnvironmentRoot $location.EnvironmentRoot
    $workspaceTarget = New-EasyConSafeDirectory -Path $location.CargoTargetDirectory `
        -TrustedRoot $location.WorkspaceRoot
    Assert-EasyConCargoPathBudget -CargoTargetDirectory $workspaceTarget
    $workspaceTemporary = $runtime.Temporary
    $workspaceVcpkg = New-EasyConVcpkgWorkspaceLayout -Vcpkg $vcpkg `
        -RepositoryRoot $repository -WorkspaceRoot $location.WorkspaceRoot `
        -CacheRoot $location.CacheRoot -EnvironmentRoot $location.EnvironmentRoot `
        -InstalledRoot $preparedPaths.vcpkgInstalled
    Assert-EasyConCMakeCacheIsolation -CargoTargetDirectory $workspaceTarget `
        -RepositoryRoot $repository

    $verifiedVariables.CARGO_HOME = $workspaceCargo.CargoHome
    $verifiedVariables.CARGO_TARGET_DIR = $workspaceTarget
    $verifiedVariables.EASYCON_VISION_TEST_TESSDATA = $vision.Root
    $verifiedVariables.TEMP = $workspaceTemporary
    $verifiedVariables.TMP = $workspaceTemporary
    $verifiedVariables.VCPKG_DOWNLOADS = $workspaceVcpkg.Downloads
    $verifiedVariables.VCPKG_ROOT = $workspaceVcpkg.Root
    Set-EasyConVerifiedProcessEnvironment -PathDirectories $pathDirectories `
        -Variables $verifiedVariables
    $null = Set-EasyConCargoNativeLinkSearch `
        -InstalledRoot $preparedPaths.vcpkgInstalled `
        -CacheRoot $location.EnvironmentRoot

    Assert-EasyConWindowsEnvironmentContextCurrent -Context $Context
    $started.Stop()
    $summary = [ordered]@{
        status = "ready"
        durationMs = $started.ElapsedMilliseconds
        fingerprint = $fingerprint.Value
        environmentRoot = $location.EnvironmentRoot
        target = [string]$configuration.target
        cmake = $buildTools.CMakeVersion
        ninja = $buildTools.NinjaVersion
        sevenZip = [string]$sevenZipPin.version
        rust = $rustPin.Channel
        cargo = $cargoVersion
        vcpkgScripts = [string]$configuration.vcpkg.scriptsCommit
        vcpkgTool = [string]$configuration.vcpkg.toolRelease
        nativeTreeSha256 = $nativeTree.Sha256
        ocrModel = $vision.Root
        workspaceRoot = $location.WorkspaceRoot
        cargoTarget = $workspaceTarget
    }
    Write-EasyConStructuredRecord -Kind "verify" -Value $summary
    return [pscustomobject]$summary
}

function Invoke-EasyConEnvironmentLifecycle {
    [CmdletBinding()]
    param(
        [ValidateSet("Setup", "Verify", "Workspace")]
        [string]$Mode = "Workspace",

        [Parameter(Mandatory)]
        [object]$Location,

        [Parameter(Mandatory)]
        [scriptblock]$SetupAction,

        [Parameter(Mandatory)]
        [scriptblock]$VerifyAction,

        [Parameter(Mandatory)]
        [scriptblock]$WorkspaceAction,

        [ValidateRange(0, 7200000)]
        [int]$LeaseTimeoutMilliseconds = 1800000
    )

    $environmentSnapshot = Get-EasyConProcessEnvironmentSnapshot
    $lease = $null
    $workspaceLease = $null
    $sharedCacheLease = $null
    $primaryFailure = $null
    try {
        $access = if ($Mode -ceq "Setup") { "Exclusive" } else { "Shared" }
        $lease = Enter-EasyConEnvironmentLease -Location $Location -Access $access `
            -TimeoutMilliseconds $LeaseTimeoutMilliseconds
        $workspaceLease = Enter-EasyConWorkspaceLease -Location $Location `
            -TimeoutMilliseconds $LeaseTimeoutMilliseconds
        if ($Mode -cne "Setup") {
            $sharedCacheLease = Enter-EasyConSharedCacheLease `
                -CacheRoot ([string]$Location.CacheRoot) -Access Shared `
                -TimeoutMilliseconds $LeaseTimeoutMilliseconds
        }
        if ($Mode -ceq "Setup") {
            $setupVerificationLease = Enter-EasyConSharedCacheLease `
                -CacheRoot ([string]$Location.CacheRoot) -Access Shared `
                -TimeoutMilliseconds $LeaseTimeoutMilliseconds
            $verificationFailure = $null
            try {
                try {
                    $verified = & $VerifyAction
                }
                catch {
                    $verificationFailure = $_
                }
            }
            finally {
                $setupVerificationLease.Dispose()
            }
            if ($null -eq $verificationFailure) {
                Write-EasyConStructuredRecord -Kind "setup" -Value ([ordered]@{
                    status = "already-ready"
                    identity = [string]$Location.IdentityKey
                    environmentRoot = [string]$Location.EnvironmentRoot
                })
                return $verified
            }
            if (Test-Path -LiteralPath $Location.EnvironmentRoot) {
                Write-Host "Existing prepared environment failed verification and will be rebuilt by Setup: $($verificationFailure.Exception.Message)"
            }

            New-EasyConSafeDirectory -Path $Location.CacheRoot `
                -TrustedRoot $Location.CacheRoot | Out-Null
            if (Test-Path -LiteralPath $Location.EnvironmentRoot) {
                if ($Location.EnvironmentRoot.Equals(
                    $Location.CacheRoot,
                    [System.StringComparison]::OrdinalIgnoreCase
                )) {
                    throw "refusing to rebuild the broad environment cache root"
                }
                $recoveryCleanupFailures = [System.Collections.Generic.List[object]]::new()
                foreach ($transientTree in @(Get-EasyConKnownVcpkgTransientTrees `
                    -EnvironmentRoot $Location.EnvironmentRoot `
                    -TrustedRoot $Location.CacheRoot)) {
                    try {
                        Remove-EasyConTransientBuildTree -Path $transientTree `
                            -TrustedRoot $Location.EnvironmentRoot
                    }
                    catch {
                        $recoveryCleanupFailures.Add([pscustomobject]@{
                            Failure = $_
                            Path = $transientTree
                            Residual = Test-Path -LiteralPath $transientTree
                        })
                    }
                }
                if ($recoveryCleanupFailures.Count -gt 0) {
                    for ($index = 0; $index -lt $recoveryCleanupFailures.Count; $index++) {
                        $cleanup = $recoveryCleanupFailures[$index]
                        $verificationFailure.Exception.Data[
                            "EasyConVcpkgRecoveryCleanupFailure$index"
                        ] = $cleanup.Failure.Exception.ToString()
                        if ($cleanup.Residual) {
                            $verificationFailure.Exception.Data[
                                "EasyConResidualVcpkgTransientTree$index"
                            ] = $cleanup.Path
                        }
                    }
                    throw $verificationFailure
                }
                Remove-EasyConSafeTree -Path $Location.EnvironmentRoot `
                    -TrustedRoot $Location.CacheRoot
            }
            New-EasyConSafeDirectory -Path $Location.EnvironmentRoot `
                -TrustedRoot $Location.CacheRoot | Out-Null
            & $SetupAction | Out-Null
            $finalVerificationLease = Enter-EasyConSharedCacheLease `
                -CacheRoot ([string]$Location.CacheRoot) -Access Shared `
                -TimeoutMilliseconds $LeaseTimeoutMilliseconds
            try {
                return & $VerifyAction
            }
            finally {
                $finalVerificationLease.Dispose()
            }
        }

        $summary = & $VerifyAction
        if ($Mode -ceq "Workspace") {
            & $WorkspaceAction $summary
        }
        return $summary
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        $cleanupFailures = [System.Collections.Generic.List[object]]::new()
        if ($null -ne $sharedCacheLease) {
            try {
                $sharedCacheLease.Dispose()
            }
            catch {
                $cleanupFailures.Add($_)
            }
        }
        if ($null -ne $workspaceLease) {
            try {
                $workspaceLease.Dispose()
            }
            catch {
                $cleanupFailures.Add($_)
            }
        }
        if ($null -ne $lease) {
            try {
                $lease.Dispose()
            }
            catch {
                $cleanupFailures.Add($_)
            }
        }
        try {
            Restore-EasyConProcessEnvironment -Snapshot $environmentSnapshot
        }
        catch {
            $cleanupFailures.Add($_)
        }
        if ($cleanupFailures.Count -gt 0) {
            if ($null -ne $primaryFailure) {
                for ($index = 0; $index -lt $cleanupFailures.Count; $index++) {
                    $primaryFailure.Exception.Data["EasyConCleanupFailure$index"] = `
                        $cleanupFailures[$index].Exception.ToString()
                }
            }
            else {
                for ($index = 1; $index -lt $cleanupFailures.Count; $index++) {
                    $cleanupFailures[0].Exception.Data["EasyConCleanupFailure$index"] = `
                        $cleanupFailures[$index].Exception.ToString()
                }
                throw $cleanupFailures[0]
            }
        }
    }
}

function Get-EasyConWindowsEnvironmentContext {
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$ConfigurationPath,

        [string]$CacheRoot
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $configurationFile = Resolve-EasyConFullPath -Path $ConfigurationPath -BasePath $repository
    $configuration = Get-EasyConWindowsBuildConfiguration -Path $configurationFile
    $fingerprint = Get-EasyConEnvironmentFingerprint -RepositoryRoot $repository `
        -Configuration $configuration
    $location = Get-EasyConEnvironmentLocation -RepositoryRoot $repository `
        -Fingerprint $fingerprint.Value -Configuration $configuration -CacheRoot $CacheRoot
    return [pscustomobject]@{
        Repository = $repository
        Configuration = $configuration
        ConfigurationPath = $configurationFile
        Fingerprint = $fingerprint
        Location = $location
    }
}

function Assert-EasyConWindowsEnvironmentContextCurrent {
    param(
        [Parameter(Mandatory)]
        [object]$Context
    )

    $current = Get-EasyConEnvironmentFingerprint -RepositoryRoot $Context.Repository `
        -Configuration $Context.Configuration
    if ($current.Value -cne $Context.Fingerprint.Value) {
        throw "Windows build environment fingerprint inputs changed while the identity was owned; rerun the command"
    }
}

function Invoke-EasyConWindowsSetup {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$ConfigurationPath,

        [string]$CacheRoot,

        [string]$VsWherePath,

        [ValidateRange(0, 7200000)]
        [int]$LeaseTimeoutMilliseconds = 1800000
    )

    $context = Get-EasyConWindowsEnvironmentContext -RepositoryRoot $RepositoryRoot `
        -ConfigurationPath $ConfigurationPath -CacheRoot $CacheRoot
    $parameters = @{
        RepositoryRoot = $context.Repository
        ConfigurationPath = $context.ConfigurationPath
        CacheRoot = $context.Location.CacheRoot
        VsWherePath = $VsWherePath
        Context = $context
    }
    $setupCore = ${function:Invoke-EasyConWindowsSetupCore}
    $verifyCore = ${function:Invoke-EasyConWindowsVerifyCore}
    $setupAction = { & $setupCore @parameters }.GetNewClosure()
    $verifyAction = {
        & $verifyCore @parameters -AllowProxy
    }.GetNewClosure()
    return Invoke-EasyConEnvironmentLifecycle -Mode Setup -Location $context.Location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction { param($Summary) $null = $Summary } `
        -LeaseTimeoutMilliseconds $LeaseTimeoutMilliseconds
}

function Invoke-EasyConWindowsVerify {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$ConfigurationPath,

        [string]$CacheRoot,

        [string]$VsWherePath,

        [ValidateRange(0, 7200000)]
        [int]$LeaseTimeoutMilliseconds = 1800000
    )

    $context = Get-EasyConWindowsEnvironmentContext -RepositoryRoot $RepositoryRoot `
        -ConfigurationPath $ConfigurationPath -CacheRoot $CacheRoot
    $parameters = @{
        RepositoryRoot = $context.Repository
        ConfigurationPath = $context.ConfigurationPath
        CacheRoot = $context.Location.CacheRoot
        VsWherePath = $VsWherePath
        Context = $context
    }
    $verifyCore = ${function:Invoke-EasyConWindowsVerifyCore}
    $verifyAction = { & $verifyCore @parameters }.GetNewClosure()
    return Invoke-EasyConEnvironmentLifecycle -Mode Verify -Location $context.Location `
        -SetupAction { throw "Verify cannot prepare the Windows build environment" } `
        -VerifyAction $verifyAction -WorkspaceAction { param($Summary) $null = $Summary } `
        -LeaseTimeoutMilliseconds $LeaseTimeoutMilliseconds
}

function Invoke-EasyConWindowsWorkspace {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$ConfigurationPath,

        [string]$CacheRoot,

        [string]$VsWherePath,

        [string]$BaseSha,

        [switch]$RequireCleanTree,

        [scriptblock]$GateInvoker,

        [ValidateRange(0, 7200000)]
        [int]$LeaseTimeoutMilliseconds = 1800000
    )

    $context = Get-EasyConWindowsEnvironmentContext -RepositoryRoot $RepositoryRoot `
        -ConfigurationPath $ConfigurationPath -CacheRoot $CacheRoot
    $parameters = @{
        RepositoryRoot = $context.Repository
        ConfigurationPath = $context.ConfigurationPath
        CacheRoot = $context.Location.CacheRoot
        VsWherePath = $VsWherePath
        Context = $context
    }
    $verifyCore = ${function:Invoke-EasyConWindowsVerifyCore}
    $workspaceGates = ${function:Invoke-EasyConWindowsWorkspaceGates}
    $verifyAction = { & $verifyCore @parameters }.GetNewClosure()
    $workspaceAction = {
        param($Summary)
        $null = $Summary
        & $workspaceGates -RepositoryRoot $context.Repository `
            -BaseSha $BaseSha -RequireCleanTree:$RequireCleanTree `
            -GateInvoker $GateInvoker
    }.GetNewClosure()
    return Invoke-EasyConEnvironmentLifecycle -Mode Workspace -Location $context.Location `
        -SetupAction { throw "Workspace cannot prepare the Windows build environment" } `
        -VerifyAction $verifyAction -WorkspaceAction $workspaceAction `
        -LeaseTimeoutMilliseconds $LeaseTimeoutMilliseconds
}

function Invoke-EasyConGate {
    param(
        [Parameter(Mandatory)]
        [string]$Name,

        [Parameter(Mandatory)]
        [string]$Program,

        [string[]]$Arguments = @(),

        [Parameter(Mandatory)]
        [string]$RepositoryRoot
    )

    $Program = Assert-EasyConPhysicalPath -Path $Program
    $RepositoryRoot = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $previous = Get-Location
    $primaryFailure = $null
    try {
        Set-Location -LiteralPath $RepositoryRoot
        & $Program @Arguments
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            throw [System.InvalidOperationException]::new(
                "gate failed with exit code ${exitCode}: $Name"
            )
        }
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        try {
            Set-Location -LiteralPath $previous.Path
        }
        catch {
            if ($null -ne $primaryFailure) {
                $primaryFailure.Exception.Data["EasyConGateLocationCleanupFailure"] = `
                    $_.Exception.ToString()
            }
            else {
                throw
            }
        }
    }
    $timer.Stop()
    Write-EasyConStructuredRecord -Kind "gate" -Value ([ordered]@{
        gate = $Name
        status = "passed"
        durationMs = $timer.ElapsedMilliseconds
    })
}

function Get-EasyConIgnoredPythonBytecodeSnapshot {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $git = Get-EasyConCommandPath -Name "git.exe"
    $paths = @(Invoke-EasyConNativeCapture -Program $git -Arguments @(
        "--no-optional-locks", "-c", "core.quotePath=false", "ls-files",
        "--others", "--ignored", "--exclude-standard", "--", "*.pyc", "*.pyo"
    ) -Description "ignored Python bytecode snapshot" -WorkingDirectory $repository)
    $builder = [System.Text.StringBuilder]::new()
    foreach ($relative in @($paths | Sort-Object)) {
        if (
            [string]::IsNullOrEmpty($relative) -or
            [System.IO.Path]::IsPathFullyQualified($relative) -or
            @($relative -split '[\\/]') -contains '..'
        ) {
            throw "ignored Python bytecode snapshot returned an unsafe repository path"
        }
        $path = Get-EasyConPhysicalFile -Path (Join-Path $repository $relative) `
            -TrustedRoot $repository
        $file = Get-Item -Force -LiteralPath $path
        $hash = Get-EasyConFileHash -Path $path -Algorithm SHA256
        [void]$builder.Append($relative.Replace("\", "/"))
        [void]$builder.Append("`0").Append($file.Length).Append("`0").Append($hash).Append("`n")
    }
    return $builder.ToString()
}

function Assert-EasyConGitWorkingTreeClean {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $git = Get-EasyConCommandPath -Name "git.exe"
    $status = @(Invoke-EasyConNativeCapture -Program $git -Arguments @(
        "--no-optional-locks", "status", "--porcelain=v1", "--untracked-files=all"
    ) -Description "workspace cleanliness check" -WorkingDirectory $repository)
    if ($status.Count -ne 0) {
        throw "workspace gates left tracked or unignored output`n$($status -join [Environment]::NewLine)"
    }
}

function Invoke-EasyConWindowsWorkspaceGates {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [string]$BaseSha,

        [switch]$RequireCleanTree,

        [scriptblock]$GateInvoker
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $cargo = Get-EasyConCommandPath -Name "cargo.exe"
    $python = Get-EasyConCommandPath -Name "python.exe"
    $pwsh = Get-EasyConCommandPath -Name "pwsh.exe"
    $git = Get-EasyConCommandPath -Name "git.exe"
    $cargoResolution = @("--locked")
    $checkArguments = @("check") + $cargoResolution + @("--workspace", "--all-targets")
    $clippyArguments = @("clippy") + $cargoResolution + @(
        "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"
    )
    $testArguments = @("test") + $cargoResolution + @("--workspace", "--all-features")
    $gates = @(
        @("cargo fmt --all --check", $cargo, @("fmt", "--all", "--check")),
        @(
            "cargo check --locked --workspace --all-targets",
            $cargo,
            $checkArguments
        ),
        @(
            "cargo clippy --locked --workspace --all-targets --all-features -- -D warnings",
            $cargo,
            $clippyArguments
        ),
        @(
            "cargo test --locked --workspace --all-features",
            $cargo,
            $testArguments
        ),
        @("python tools/run_runtime_models.py", $python, @("tools/run_runtime_models.py")),
        @("python tools/validate_specs.py", $python, @("tools/validate_specs.py")),
        @("python tools/check_markdown_links.py", $python, @("tools/check_markdown_links.py")),
        @("python tools/check_repository_guards.py", $python, @("tools/check_repository_guards.py")),
        @(
            "python tools/test_windows_bootstrap_contracts.py",
            $python,
            @("tools/test_windows_bootstrap_contracts.py")
        ),
        @(
            "pwsh tools/test_windows_workspace.ps1",
            $pwsh,
            @("-NoLogo", "-NoProfile", "-File", "tools/test_windows_workspace.ps1")
        ),
        @(
            "pwsh tools/test_windows_environment_lifecycle.ps1",
            $pwsh,
            @("-NoLogo", "-NoProfile", "-File", "tools/test_windows_environment_lifecycle.ps1")
        ),
        @("git diff --check", $git, @("diff", "--check"))
    )
    $ignoredPythonBytecodeBefore = $null
    if ($RequireCleanTree) {
        Assert-EasyConGitWorkingTreeClean -RepositoryRoot $repository
        $ignoredPythonBytecodeBefore = Get-EasyConIgnoredPythonBytecodeSnapshot `
            -RepositoryRoot $repository
    }
    foreach ($gate in $gates) {
        if ($null -eq $GateInvoker) {
            Invoke-EasyConGate -Name $gate[0] -Program $gate[1] -Arguments $gate[2] `
                -RepositoryRoot $repository
        }
        else {
            & $GateInvoker $gate[0] $gate[1] $gate[2] $repository
        }
    }
    if (-not [string]::IsNullOrWhiteSpace($BaseSha) -and $BaseSha -notmatch '^0+$') {
        Invoke-EasyConGate -Name "git cat-file base commit" -Program $git `
            -Arguments @("cat-file", "-e", "$BaseSha^{commit}") -RepositoryRoot $repository
        Invoke-EasyConGate -Name "git diff base...HEAD --check" -Program $git `
            -Arguments @("diff", "--check", "$BaseSha...HEAD") -RepositoryRoot $repository
    }
    if ($RequireCleanTree) {
        Assert-EasyConGitWorkingTreeClean -RepositoryRoot $repository
        $ignoredPythonBytecodeAfter = Get-EasyConIgnoredPythonBytecodeSnapshot `
            -RepositoryRoot $repository
        if ($ignoredPythonBytecodeAfter -cne $ignoredPythonBytecodeBefore) {
            throw "workspace gates changed ignored Python bytecode inside the source tree"
        }
    }
}

Export-ModuleMember -Function @(
    "Invoke-EasyConWindowsSetup",
    "Invoke-EasyConWindowsVerify",
    "Invoke-EasyConWindowsWorkspace"
)

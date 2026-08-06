# Gate and candidate policy is loaded into the private module scope.
$script:EasyConGatePolicyScriptPath = $PSCommandPath

function Get-EasyConExpectedWindowsGatePolicy {
    return @(
        [pscustomobject]@{ Name = "cargo fmt --all --check"; Tool = "cargo"; Arguments = @("fmt", "--all", "--check") },
        [pscustomobject]@{ Name = "cargo check --locked --jobs 4 --workspace --all-targets"; Tool = "cargo"; Arguments = @("check", "--locked", "--workspace", "--all-targets") },
        [pscustomobject]@{ Name = "cargo clippy --locked --jobs 4 --workspace --all-targets --all-features -- -D warnings"; Tool = "cargo"; Arguments = @("clippy", "--locked", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings") },
        [pscustomobject]@{ Name = "cargo test --locked --jobs 4 --workspace --all-features"; Tool = "cargo"; Arguments = @("test", "--locked", "--workspace", "--all-features") },
        [pscustomobject]@{ Name = "python tools/run_runtime_models.py"; Tool = "python"; Arguments = @("tools/run_runtime_models.py") },
        [pscustomobject]@{ Name = "python tools/validate_specs.py"; Tool = "python"; Arguments = @("tools/validate_specs.py") },
        [pscustomobject]@{ Name = "python tools/check_markdown_links.py"; Tool = "python"; Arguments = @("tools/check_markdown_links.py") },
        [pscustomobject]@{ Name = "python tools/check_repository_guards.py"; Tool = "python"; Arguments = @("tools/check_repository_guards.py") },
        [pscustomobject]@{ Name = "python tools/test_windows_bootstrap_contracts.py"; Tool = "python"; Arguments = @("tools/test_windows_bootstrap_contracts.py") },
        [pscustomobject]@{ Name = "pwsh tools/test_windows_workspace.ps1"; Tool = "pwsh"; Arguments = @("-NoLogo", "-NoProfile", "-File", "tools/test_windows_workspace.ps1") },
        [pscustomobject]@{ Name = "pwsh tools/test_windows_environment_lifecycle.ps1"; Tool = "pwsh"; Arguments = @("-NoLogo", "-NoProfile", "-File", "tools/test_windows_environment_lifecycle.ps1") },
        [pscustomobject]@{ Name = "git diff --check"; Tool = "git"; Arguments = @("diff", "--check") }
    )
}
function Get-EasyConGatePolicyStrictObject {
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
    $names = [System.Collections.Generic.List[string]]::new()
    $seen = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($property in $Element.EnumerateObject()) {
        if (-not $seen.Add($property.Name)) {
            throw "$Description contains duplicate key '$($property.Name)'"
        }
        $names.Add($property.Name)
        $properties[$property.Name] = $property.Value.Clone()
    }
    if (($names -join "`0") -cne ($ExpectedKeys -join "`0")) {
        throw "$Description keys and order must be exactly: $($ExpectedKeys -join ', ')"
    }
    return $properties
}
function Get-EasyConGatePolicyStrictString {
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

function Get-EasyConWindowsGatePolicy {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $policyPath = Get-EasyConPhysicalFile -Path (Join-Path $repository "tools/windows_gate_policy.json") `
        -TrustedRoot $repository
    try {
        $json = Read-EasyConPhysicalText -Path $policyPath -TrustedRoot $repository
        $options = [System.Text.Json.JsonDocumentOptions]::new()
        $options.AllowTrailingCommas = $false
        $options.CommentHandling = [System.Text.Json.JsonCommentHandling]::Disallow
        $document = [System.Text.Json.JsonDocument]::Parse($json, $options)
    }
    catch {
        throw "Windows gate policy is invalid: $($_.Exception.Message)"
    }

    $primaryFailure = $null
    try {
        $root = Get-EasyConGatePolicyStrictObject -Element $document.RootElement `
            -ExpectedKeys @("version", "cargoJobs", "gates") `
            -Description "Windows gate policy root"
        $version = 0
        if (
            $root.version.ValueKind -ne [System.Text.Json.JsonValueKind]::Number -or
            -not $root.version.TryGetInt32([ref]$version) -or
            $version -ne 1
        ) {
            throw "Windows gate policy version must be the JSON integer 1"
        }
        $cargoJobs = 0
        if (
            $root.cargoJobs.ValueKind -ne [System.Text.Json.JsonValueKind]::Number -or
            -not $root.cargoJobs.TryGetInt32([ref]$cargoJobs) -or
            $cargoJobs -ne 4
        ) {
            throw "Windows gate policy cargoJobs must be the JSON integer 4"
        }
        if ($root.gates.ValueKind -ne [System.Text.Json.JsonValueKind]::Array) {
            throw "Windows gate policy gates must be a JSON array"
        }

        $expectedGates = @(Get-EasyConExpectedWindowsGatePolicy)
        $actualGates = @($root.gates.EnumerateArray())
        if ($actualGates.Count -ne $expectedGates.Count) {
            throw "Windows gate policy gate set or order changed"
        }
        $gateNames = [System.Collections.Generic.HashSet[string]]::new(
            [System.StringComparer]::OrdinalIgnoreCase
        )
        $parsed = [System.Collections.Generic.List[object]]::new()
        for ($index = 0; $index -lt $actualGates.Count; $index++) {
            $gate = Get-EasyConGatePolicyStrictObject -Element $actualGates[$index] `
                -ExpectedKeys @("name", "tool", "arguments") `
                -Description "Windows gate policy gate $index"
            $name = Get-EasyConGatePolicyStrictString -Element $gate.name `
                -Description "Windows gate policy gate name"
            $tool = Get-EasyConGatePolicyStrictString -Element $gate.tool `
                -Description "Windows gate policy gate tool"
            if (-not $gateNames.Add($name)) {
                throw "Windows gate policy contains duplicate Windows gate identity '$name'"
            }
            if ($gate.arguments.ValueKind -ne [System.Text.Json.JsonValueKind]::Array) {
                throw "Windows gate policy gate arguments must be a JSON array"
            }
            $arguments = [System.Collections.Generic.List[string]]::new()
            foreach ($argument in $gate.arguments.EnumerateArray()) {
                $arguments.Add((Get-EasyConGatePolicyStrictString -Element $argument `
                    -Description "Windows gate policy argument"))
            }
            $expected = $expectedGates[$index]
            if (
                $name -cne $expected.Name -or
                $tool -cne $expected.Tool -or
                (($arguments.ToArray() -join "`0") -cne ($expected.Arguments -join "`0"))
            ) {
                throw "Windows gate policy gate set, path, case, or order changed"
            }
            $parsed.Add([pscustomobject]@{
                Name = $name
                Tool = $tool
                Arguments = $arguments.ToArray()
            }) | Out-Null
        }
        return [pscustomobject]@{
            Version = $version
            CargoJobs = $cargoJobs
            Gates = $parsed.ToArray()
            PolicyPath = $policyPath
        }
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
                $primaryFailure.Exception.Data["EasyConGatePolicyJsonCleanupFailure"] = $_.Exception.ToString()
            }
            else {
                throw
            }
        }
    }
}

function Get-EasyConGatePolicyHash {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $records = [System.Collections.Generic.List[object]]::new()
    $builder = [System.Text.StringBuilder]::new()
    foreach ($relative in @(
        "tools/windows_gate_policy.json",
        "tools/windows_gate_policy.ps1",
        "tools/run_windows_workspace.ps1"
    )) {
        $path = Get-EasyConPhysicalFile -Path (Join-Path $repository $relative) `
            -TrustedRoot $repository
        $hash = Get-EasyConFingerprintInputHash -Path $path -Kind "text"
        $records.Add([ordered]@{ path = $relative; sha256 = $hash }) | Out-Null
        [void]$builder.Append($relative).Append("`0").Append($hash).Append("`n")
    }
    $digest = [System.Security.Cryptography.SHA256]::HashData(
        [System.Text.Encoding]::UTF8.GetBytes($builder.ToString())
    )
    return [pscustomobject]@{
        Value = [System.Convert]::ToHexString($digest).ToLowerInvariant()
        Inputs = $records.ToArray()
    }
}

function Get-EasyConGatePolicyContext {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $loadedScript = Get-EasyConPhysicalFile -Path $script:EasyConGatePolicyScriptPath `
        -TrustedRoot $repository
    $expectedScript = Get-EasyConPhysicalFile -Path (Join-Path $repository "tools/windows_gate_policy.ps1") `
        -TrustedRoot $repository
    if ($loadedScript -cne $expectedScript) {
        throw "Windows gate policy script must be loaded from the repository policy path"
    }
    return [pscustomobject]@{
        Policy = Get-EasyConWindowsGatePolicy -RepositoryRoot $repository
        Hash = Get-EasyConGatePolicyHash -RepositoryRoot $repository
    }
}

function Assert-EasyConGatePolicyContextCurrent {
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [object]$PolicyContext,

        [Parameter(Mandatory)]
        [string]$Boundary
    )

    $current = Get-EasyConGatePolicyHash -RepositoryRoot $RepositoryRoot
    if ($current.Value -cne [string]$PolicyContext.Hash.Value) {
        throw "Windows gate policy inputs changed $Boundary; rerun the command"
    }
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

        [ValidateSet("Workspace", "Targeted", IgnoreCase = $false)]
        [string]$GateMode = "Workspace",

        [string]$BaseSha,

        [switch]$RequireCleanTree,

        [switch]$RequireStagedCandidate,

        [ValidateSet("check", "clippy", "test")]
        [string]$TargetedCargoCommand,

        [string[]]$TargetedCargoArguments = @(),

        [ValidateRange(0, 7200000)]
        [int]$LeaseTimeoutMilliseconds = 1800000
    )

    if ($GateMode -cnotin @("Workspace", "Targeted")) {
        throw "-GateMode must use one exact supported case: Workspace or Targeted"
    }
    $targetedParametersProvided = (
        $PSBoundParameters.ContainsKey("TargetedCargoCommand") -or
        $PSBoundParameters.ContainsKey("TargetedCargoArguments")
    )
    if ($RequireCleanTree -and $RequireStagedCandidate) {
        throw "-RequireCleanTree and -RequireStagedCandidate are mutually exclusive"
    }
    if ($GateMode -cne "Targeted" -and $targetedParametersProvided) {
        throw "Targeted Cargo parameters require -GateMode Targeted"
    }
    if ($GateMode -ceq "Targeted") {
        if ($RequireCleanTree -or $RequireStagedCandidate) {
            throw "Targeted mode cannot use Workspace candidate requirements"
        }
        if ([string]::IsNullOrWhiteSpace($TargetedCargoCommand)) {
            throw "Targeted Cargo command is required"
        }
    }

    $context = Get-EasyConWindowsEnvironmentContext -RepositoryRoot $RepositoryRoot `
        -ConfigurationPath $ConfigurationPath -CacheRoot $CacheRoot
    $policyContext = Get-EasyConGatePolicyContext -RepositoryRoot $context.Repository
    $parameters = @{
        RepositoryRoot = $context.Repository
        ConfigurationPath = $context.ConfigurationPath
        CacheRoot = $context.Location.CacheRoot
        VsWherePath = $VsWherePath
        Context = $context
    }
    $verifyCore = ${function:Invoke-EasyConWindowsVerifyCore}
    $workspaceGates = ${function:Invoke-EasyConWindowsWorkspaceGates}
    $targetedCargoGate = ${function:Invoke-EasyConWindowsTargetedCargoGate}
    $publishWorkspaceEvidence = ${function:Publish-EasyConWorkspaceEvidence}
    $writeStructuredRecord = ${function:Write-EasyConStructuredRecord}
    $assertGatePolicyContextCurrent = ${function:Assert-EasyConGatePolicyContextCurrent}
    $assertWorkspaceCandidateBindingCurrent = ${function:Assert-EasyConWorkspaceCandidateBindingCurrent}
    $newWorkspaceEvidenceRecord = ${function:New-EasyConWorkspaceEvidenceRecord}
    $workspaceStartedUtc = [DateTimeOffset]::UtcNow
    $workspaceTimer = [System.Diagnostics.Stopwatch]::StartNew()
    $timing = [pscustomobject]@{ VerifyDurationMilliseconds = 0L }
    $verifyAction = {
        $verifyTimer = [System.Diagnostics.Stopwatch]::StartNew()
        try {
            return & $verifyCore @parameters
        }
        finally {
            $verifyTimer.Stop()
            $timing.VerifyDurationMilliseconds = [long]$verifyTimer.ElapsedMilliseconds
        }
    }.GetNewClosure()
    $workspaceAction = {
        param($Summary)
        if ($GateMode -ceq "Targeted") {
            & $assertGatePolicyContextCurrent -RepositoryRoot $context.Repository `
                -PolicyContext $policyContext -Boundary "after Verify and before the first gate"
            & $targetedCargoGate -RepositoryRoot $context.Repository `
                -CargoCommand $TargetedCargoCommand -CargoArguments $TargetedCargoArguments `
                -CargoJobs ([int]$policyContext.Policy.CargoJobs)
            & $assertGatePolicyContextCurrent -RepositoryRoot $context.Repository `
                -PolicyContext $policyContext -Boundary "after the final gate"
            return
        }

        & $assertGatePolicyContextCurrent -RepositoryRoot $context.Repository `
            -PolicyContext $policyContext -Boundary "after Verify and before the first gate"
        $outcome = & $workspaceGates -RepositoryRoot $context.Repository `
            -BaseSha $BaseSha -RequireCleanTree:$RequireCleanTree `
            -RequireStagedCandidate:$RequireStagedCandidate -Policy $policyContext.Policy
        & $assertGatePolicyContextCurrent -RepositoryRoot $context.Repository `
            -PolicyContext $policyContext -Boundary "after the final gate"

        $candidate = $null
        $baseCommit = $null
        $gates = @()
        $git = $null
        if ($null -ne $outcome) {
            if ($outcome.PSObject.Properties.Name -ccontains "Candidate") {
                $candidate = $outcome.Candidate
                $baseCommit = [string]$outcome.BaseCommit
                $gates = @($outcome.Gates)
                $git = [string]$outcome.Git
            }
            else {
                # Private test helpers may return a candidate directly, but the public path never injects gates.
                $candidate = $outcome
            }
        }
        if ($null -ne $candidate -and -not [string]::IsNullOrWhiteSpace($git)) {
            $candidate = & $assertWorkspaceCandidateBindingCurrent `
                -RepositoryRoot $context.Repository -Git $git -Candidate $candidate
        }
        $completedUtc = [DateTimeOffset]::UtcNow
        $runId = [guid]::NewGuid().ToString("N").ToLowerInvariant()
        if ($null -ne $candidate) {
            & $publishWorkspaceEvidence -Context $context -Candidate $candidate `
                -BaseCommit $baseCommit -GatePolicyHash ([string]$policyContext.Hash.Value) `
                -Gates $gates -VerifyDurationMilliseconds ([long]$timing.VerifyDurationMilliseconds) `
                -RunId $runId `
                -StartedUtc $workspaceStartedUtc -CompletedUtc $completedUtc `
                -TotalDurationMilliseconds ([long]$workspaceTimer.ElapsedMilliseconds) | Out-Null
            return
        }

        $record = & $newWorkspaceEvidenceRecord `
            -Context $context -Candidate $null -BaseCommit $baseCommit `
            -GatePolicyHash ([string]$policyContext.Hash.Value) -Gates $gates `
            -VerifyDurationMilliseconds ([long]$timing.VerifyDurationMilliseconds) -RunId $runId `
            -StartedUtc $workspaceStartedUtc -CompletedUtc $completedUtc `
            -TotalDurationMilliseconds ([long]$workspaceTimer.ElapsedMilliseconds) `
            -Credential "none" -EvidenceFile $null
        & $writeStructuredRecord -Kind "workspace" -Value $record
    }.GetNewClosure()
    try {
        return Invoke-EasyConEnvironmentLifecycle -Mode Workspace -Location $context.Location `
            -SetupAction { throw "Workspace cannot prepare the Windows build environment" } `
            -VerifyAction $verifyAction -WorkspaceAction $workspaceAction `
            -LeaseTimeoutMilliseconds $LeaseTimeoutMilliseconds
    }
    finally {
        $workspaceTimer.Stop()
    }
}

function Invoke-EasyConGate {
    param(
        [Parameter(Mandatory)]
        [string]$Name,

        [Parameter(Mandatory)]
        [string]$Program,

        [string[]]$Arguments = @(),

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [switch]$SuppressStructuredRecord
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
    if (-not $SuppressStructuredRecord) {
        Write-EasyConStructuredRecord -Kind "gate" -Value ([ordered]@{
            gate = $Name
            status = "passed"
            durationMs = $timer.ElapsedMilliseconds
        })
    }
}

function Invoke-EasyConTimedPolicyGate {
    param(
        [Parameter(Mandatory)]
        [string]$Name,

        [Parameter(Mandatory)]
        [string]$Program,

        [string[]]$Arguments = @(),

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [scriptblock]$GateInvoker
    )

    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        if ($null -eq $GateInvoker) {
            Invoke-EasyConGate -Name $Name -Program $Program -Arguments $Arguments `
                -RepositoryRoot $RepositoryRoot -SuppressStructuredRecord | Out-Host
        }
        else {
            & $GateInvoker $Name $Program $Arguments $RepositoryRoot | Out-Host
        }
    }
    finally {
        $timer.Stop()
    }
    $record = [pscustomobject]@{
        name = $Name
        status = "passed"
        durationMs = [long]$timer.ElapsedMilliseconds
    }
    Write-EasyConStructuredRecord -Kind "gate" -Value $record
    return $record
}

function Get-EasyConTargetedCargoArguments {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [ValidateSet("check", "clippy", "test")]
        [string]$CargoCommand,

        [Parameter(Mandatory)]
        [ValidateRange(1, 64)]
        [int]$CargoJobs,

        [string[]]$CargoArguments = @()
    )

    $arguments = @($CargoArguments)
    $forbidden = @(
        "--workspace",
        "--all",
        "--manifest-path",
        "--target-dir",
        "--config",
        "--target",
        "--offline",
        "--frozen"
    )
    $clippySourceWriting = @("--fix")
    $hasPackage = $false
    $cargoOptionSection = $true
    for ($index = 0; $index -lt $arguments.Count; $index++) {
        $argument = [string]$arguments[$index]
        if ([string]::IsNullOrWhiteSpace($argument)) {
            throw "Targeted Cargo arguments must not contain empty tokens"
        }
        if ($argument -ceq "--") {
            $cargoOptionSection = $false
            continue
        }
        if (-not $cargoOptionSection) {
            continue
        }
        foreach ($blocked in $forbidden) {
            if ($argument -ieq $blocked -or $argument.StartsWith("$blocked=")) {
                throw "Targeted Cargo argument is not permitted: $argument"
            }
        }
        if (
            $argument -ceq "--jobs" -or
            $argument.StartsWith("--jobs=") -or
            $argument -ceq "-j" -or
            ($argument.StartsWith("-j") -and $argument.Length -gt 2)
        ) {
            throw "Targeted Cargo argument is not permitted: $argument"
        }
        if ($argument -ceq "-m" -or ($argument.StartsWith("-m") -and $argument.Length -gt 2)) {
            throw "Targeted Cargo manifest-path argument is not permitted: $argument"
        }
        if ($CargoCommand -ieq "clippy") {
            foreach ($blocked in $clippySourceWriting) {
                if ($argument -ieq $blocked -or $argument.StartsWith("$blocked=")) {
                    throw "Targeted Cargo argument is not permitted: $argument"
                }
            }
        }
        if ($argument -ceq "-p" -or $argument -ceq "--package") {
            if ($index + 1 -ge $arguments.Count) {
                throw "Targeted Cargo package selection requires a package value"
            }
            $package = [string]$arguments[$index + 1]
            if ([string]::IsNullOrWhiteSpace($package) -or $package.StartsWith("-")) {
                throw "Targeted Cargo package selection requires a package value"
            }
            $hasPackage = $true
            $index++
            continue
        }
        if ($argument.StartsWith("--package=")) {
            $package = $argument.Substring("--package=".Length)
            if ([string]::IsNullOrWhiteSpace($package)) {
                throw "Targeted Cargo package selection requires a package value"
            }
            $hasPackage = $true
            continue
        }
        if ($argument.StartsWith("-p") -and $argument.Length -gt 2) {
            $package = $argument.Substring(2)
            if ([string]::IsNullOrWhiteSpace($package) -or $package.StartsWith("=")) {
                throw "Targeted Cargo package selection requires a package value"
            }
            $hasPackage = $true
        }
    }
    if (-not $hasPackage) {
        throw "Targeted Cargo command requires an explicit -p or --package selection"
    }
    return @($CargoCommand, "--locked", "--jobs", [string]$CargoJobs) + $arguments
}

function Invoke-EasyConWindowsTargetedCargoGate {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [ValidateSet("check", "clippy", "test")]
        [string]$CargoCommand,

        [Parameter(Mandatory)]
        [ValidateRange(1, 64)]
        [int]$CargoJobs,

        [string[]]$CargoArguments = @(),

        [scriptblock]$GateInvoker
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    $cargo = Get-EasyConCommandPath -Name "cargo.exe"
    $arguments = @(Get-EasyConTargetedCargoArguments -CargoCommand $CargoCommand `
        -CargoJobs $CargoJobs -CargoArguments $CargoArguments)
    $name = "cargo " + ($arguments -join " ")
    if ($null -eq $GateInvoker) {
        Invoke-EasyConGate -Name $name -Program $cargo -Arguments $arguments `
            -RepositoryRoot $repository
    }
    else {
        & $GateInvoker $name $cargo $arguments $repository
    }
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

function Get-EasyConGitObjectId {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Git,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string[]]$Arguments,

        [Parameter(Mandatory)]
        [string]$Description
    )

    $output = @(Invoke-EasyConNativeCapture -Program $Git -Arguments $Arguments `
        -Description $Description -WorkingDirectory $RepositoryRoot)
    if ($output.Count -ne 1) {
        throw "$Description did not return exactly one Git object ID"
    }
    $value = ([string]$output[0]).Trim()
    if ($value -cnotmatch '^[0-9a-f]{40}$') {
        throw "$Description returned an invalid Git object ID"
    }
    return $value
}

function Resolve-EasyConWorkspaceBaseCommit {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Git,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [string]$BaseSha
    )

    if ([string]::IsNullOrWhiteSpace($BaseSha) -or $BaseSha -match '^0+$') {
        return $null
    }
    return Get-EasyConGitObjectId -Git $Git -RepositoryRoot $RepositoryRoot `
        -Arguments @("--no-optional-locks", "rev-parse", "--verify", "--end-of-options", "$BaseSha^{commit}") `
        -Description "workspace base commit"
}

function Get-EasyConGitWorkingTreeStatus {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Git,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot
    )

    return @(Invoke-EasyConNativeCapture -Program $Git -Arguments @(
        "--no-optional-locks", "status", "--porcelain=v1", "--untracked-files=all"
    ) -Description "workspace candidate status check" -WorkingDirectory $RepositoryRoot)
}

function Assert-EasyConGitStagedCandidate {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Git,

        [Parameter(Mandatory)]
        [string]$RepositoryRoot
    )

    $status = @(Get-EasyConGitWorkingTreeStatus -Git $Git -RepositoryRoot $RepositoryRoot)
    foreach ($line in $status) {
        $entry = [string]$line
        if ($entry.Length -lt 2) {
            throw "workspace candidate status is malformed"
        }
        $indexState = $entry[0]
        $worktreeState = $entry[1]
        if ($indexState -eq '?' -and $worktreeState -eq '?') {
            throw "workspace candidate has untracked content"
        }
        if ($worktreeState -ne ' ') {
            throw "workspace candidate has unstaged content"
        }
        if ($indexState -eq ' ') {
            throw "workspace candidate status is malformed"
        }
    }
    return [pscustomobject]@{
        Snapshot = $status -join "`0"
    }
}

function New-EasyConWorkspaceCandidateBinding {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$Git,

        [switch]$RequireCleanTree,

        [switch]$RequireStagedCandidate
    )

    if ($RequireCleanTree -and $RequireStagedCandidate) {
        throw "-RequireCleanTree and -RequireStagedCandidate are mutually exclusive"
    }
    if (-not $RequireCleanTree -and -not $RequireStagedCandidate) {
        return
    }

    $head = Get-EasyConGitObjectId -Git $Git -RepositoryRoot $RepositoryRoot `
        -Arguments @("--no-optional-locks", "rev-parse", "--verify", "HEAD^{commit}") `
        -Description "workspace candidate HEAD commit"
    if ($RequireCleanTree) {
        Assert-EasyConGitWorkingTreeClean -RepositoryRoot $RepositoryRoot
        $tree = Get-EasyConGitObjectId -Git $Git -RepositoryRoot $RepositoryRoot `
            -Arguments @("--no-optional-locks", "rev-parse", "--verify", "HEAD^{tree}") `
            -Description "workspace clean-tree candidate"
        return [pscustomobject]@{
            CandidateMode = "clean-tree"
            HeadCommit = $head
            Tree = $tree
            StatusSnapshot = $null
            IgnoredPythonBytecodeSnapshot = Get-EasyConIgnoredPythonBytecodeSnapshot `
                -RepositoryRoot $RepositoryRoot
        }
    }

    $status = Assert-EasyConGitStagedCandidate -Git $Git -RepositoryRoot $RepositoryRoot
    $tree = Get-EasyConGitObjectId -Git $Git -RepositoryRoot $RepositoryRoot `
        -Arguments @("--no-optional-locks", "write-tree") `
        -Description "workspace staged candidate tree"
    return [pscustomobject]@{
        CandidateMode = "staged-candidate"
        HeadCommit = $head
        Tree = $tree
        StatusSnapshot = $status.Snapshot
        IgnoredPythonBytecodeSnapshot = $null
    }
}

function Assert-EasyConWorkspaceCandidateBindingCurrent {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$Git,

        [Parameter(Mandatory)]
        [object]$Candidate
    )

    $head = Get-EasyConGitObjectId -Git $Git -RepositoryRoot $RepositoryRoot `
        -Arguments @("--no-optional-locks", "rev-parse", "--verify", "HEAD^{commit}") `
        -Description "workspace candidate HEAD commit after gates"
    if ($head -cne [string]$Candidate.HeadCommit) {
        throw "workspace candidate HEAD changed during gates"
    }

    $tree = $null
    switch ([string]$Candidate.CandidateMode) {
        "clean-tree" {
            Assert-EasyConGitWorkingTreeClean -RepositoryRoot $RepositoryRoot
            $ignoredPythonBytecodeAfter = Get-EasyConIgnoredPythonBytecodeSnapshot `
                -RepositoryRoot $RepositoryRoot
            if ($ignoredPythonBytecodeAfter -cne [string]$Candidate.IgnoredPythonBytecodeSnapshot) {
                throw "workspace gates changed ignored Python bytecode inside the source tree"
            }
            $tree = Get-EasyConGitObjectId -Git $Git -RepositoryRoot $RepositoryRoot `
                -Arguments @("--no-optional-locks", "rev-parse", "--verify", "HEAD^{tree}") `
                -Description "workspace clean-tree candidate after gates"
        }
        "staged-candidate" {
            $status = Assert-EasyConGitStagedCandidate -Git $Git -RepositoryRoot $RepositoryRoot
            if ($status.Snapshot -cne [string]$Candidate.StatusSnapshot) {
                throw "workspace candidate source/index state changed during gates"
            }
            $tree = Get-EasyConGitObjectId -Git $Git -RepositoryRoot $RepositoryRoot `
                -Arguments @("--no-optional-locks", "write-tree") `
                -Description "workspace staged candidate tree after gates"
        }
        default {
            throw "workspace candidate binding has an invalid mode"
        }
    }
    if ($tree -cne [string]$Candidate.Tree) {
        throw "workspace candidate tree changed during gates"
    }
    return $Candidate
}

function New-EasyConWorkspaceEvidenceRecord {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Context,

        [object]$Candidate,

        [string]$BaseCommit,

        [Parameter(Mandatory)]
        [string]$GatePolicyHash,

        [object[]]$Gates = @(),

        [Parameter(Mandatory)]
        [long]$VerifyDurationMilliseconds,

        [Parameter(Mandatory)]
        [string]$RunId,

        [Parameter(Mandatory)]
        [DateTimeOffset]$StartedUtc,

        [Parameter(Mandatory)]
        [DateTimeOffset]$CompletedUtc,

        [Parameter(Mandatory)]
        [long]$TotalDurationMilliseconds,

        [Parameter(Mandatory)]
        [ValidateSet("none", "tree")]
        [string]$Credential,

        [AllowNull()]
        [string]$EvidenceFile
    )

    if ($RunId -cnotmatch '^[0-9a-f]{32}$') {
        throw "workspace evidence runId must be 32 lowercase hexadecimal digits"
    }
    if ($GatePolicyHash -cnotmatch '^[0-9a-f]{64}$') {
        throw "workspace evidence requires a valid gate policy hash"
    }
    if ($VerifyDurationMilliseconds -lt 0 -or $TotalDurationMilliseconds -lt 0) {
        throw "workspace evidence durations must be non-negative"
    }
    if (-not [string]::IsNullOrWhiteSpace($BaseCommit) -and (
        $BaseCommit -cnotmatch '^[0-9a-f]{40}$' -or $BaseCommit -match '^0+$'
    )) {
        throw "workspace evidence requires an immutable full base commit"
    }

    $candidateMode = $null
    $head = $null
    $tree = $null
    if ($null -ne $Candidate) {
        $candidateMode = [string]$Candidate.CandidateMode
        $head = [string]$Candidate.HeadCommit
        $tree = [string]$Candidate.Tree
        if ($candidateMode -cnotin @("clean-tree", "staged-candidate")) {
            throw "workspace evidence requires a validated candidate mode"
        }
        if ($head -cnotmatch '^[0-9a-f]{40}$' -or $tree -cnotmatch '^[0-9a-f]{40}$') {
            throw "workspace evidence requires a validated Git tree and HEAD commit"
        }
        if ($Credential -cne "tree") {
            throw "workspace candidate evidence requires a tree credential"
        }
    }
    elseif ($Credential -cne "none") {
        throw "ordinary workspace evidence must use the none credential"
    }

    $gateRecords = [System.Collections.Generic.List[object]]::new()
    foreach ($gate in @($Gates)) {
        $name = [string]$gate.name
        $status = [string]$gate.status
        $duration = [long]$gate.durationMs
        if ([string]::IsNullOrWhiteSpace($name) -or $status -cne "passed" -or $duration -lt 0) {
            throw "workspace evidence gate records must be ordered passed gate timings"
        }
        $gateRecords.Add([ordered]@{
            name = $name
            status = "passed"
            durationMs = $duration
        }) | Out-Null
    }

    return [ordered]@{
        schemaVersion = 2
        status = "passed"
        mode = "workspace"
        runId = $RunId
        credential = $Credential
        candidateMode = $candidateMode
        baseCommit = if ([string]::IsNullOrWhiteSpace($BaseCommit)) { $null } else { [string]$BaseCommit }
        headCommit = $head
        tree = $tree
        environmentFingerprint = [string]$Context.Fingerprint.Value
        gatePolicyHash = $GatePolicyHash
        verifyDurationMs = $VerifyDurationMilliseconds
        gates = $gateRecords.ToArray()
        target = [string]$Context.Configuration.target
        environmentIdentity = [string]$Context.Location.IdentityKey
        workspaceIdentity = [string]$Context.Location.WorkspaceKey
        startedUtc = $StartedUtc.ToString("O", [Globalization.CultureInfo]::InvariantCulture)
        completedUtc = $CompletedUtc.ToString("O", [Globalization.CultureInfo]::InvariantCulture)
        totalDurationMs = $TotalDurationMilliseconds
        durationMs = $TotalDurationMilliseconds
        evidenceFile = $EvidenceFile
    }
}

function Publish-EasyConWorkspaceEvidenceNoReplace {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$Text,

        [Parameter(Mandatory)]
        [string]$TrustedRoot
    )

    $destination = Assert-EasyConAtomicFileDestination -Path $Path -TrustedRoot $TrustedRoot `
        -Description "workspace evidence"
    if (Test-Path -LiteralPath $destination -PathType Leaf) {
        throw "workspace evidence destination already exists and will not be replaced"
    }
    $temporary = "$destination.write-$PID-$([guid]::NewGuid().ToString('N'))"
    Assert-EasyConPhysicalPath -Path $temporary -TrustedRoot $TrustedRoot | Out-Null
    $primaryFailure = $null
    try {
        $bytes = [System.Text.UTF8Encoding]::new($false, $true).GetBytes($Text)
        $stream = [System.IO.FileStream]::new(
            $temporary,
            [System.IO.FileMode]::CreateNew,
            [System.IO.FileAccess]::Write,
            [System.IO.FileShare]::None
        )
        try {
            $stream.Write($bytes, 0, $bytes.Length)
            $stream.Flush($true)
        }
        finally {
            $stream.Dispose()
        }
        Assert-EasyConAtomicFileDestination -Path $destination -TrustedRoot $TrustedRoot `
            -Description "workspace evidence" | Out-Null
        try {
            [System.IO.File]::Move($temporary, $destination)
        }
        catch [System.IO.IOException] {
            throw "workspace evidence destination already exists and will not be replaced"
        }
        Assert-EasyConPhysicalPath -Path $destination -TrustedRoot $TrustedRoot | Out-Null
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        Complete-EasyConTemporaryFileCleanup -Path $temporary -TrustedRoot $TrustedRoot `
            -Description "workspace evidence no-replace publication" -PrimaryFailure $primaryFailure
    }
}

function Publish-EasyConWorkspaceEvidence {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [object]$Context,

        [Parameter(Mandatory)]
        [object]$Candidate,

        [string]$BaseCommit,

        [Parameter(Mandatory)]
        [string]$GatePolicyHash,

        [object[]]$Gates = @(),

        [Parameter(Mandatory)]
        [long]$VerifyDurationMilliseconds,

        [Parameter(Mandatory)]
        [string]$RunId,

        [Parameter(Mandatory)]
        [DateTimeOffset]$StartedUtc,

        [Parameter(Mandatory)]
        [DateTimeOffset]$CompletedUtc,

        [Parameter(Mandatory)]
        [long]$TotalDurationMilliseconds
    )

    $tree = [string]$Candidate.Tree
    $candidateMode = [string]$Candidate.CandidateMode
    if ($candidateMode -cnotin @("clean-tree", "staged-candidate") -or $tree -cnotmatch '^[0-9a-f]{40}$') {
        throw "workspace evidence requires a validated candidate tree"
    }
    $workspaceRoot = Assert-EasyConPhysicalPath -Path ([string]$Context.Location.WorkspaceRoot)
    $evidenceFile = "evidence/v2/workspace-$candidateMode-$tree-$RunId.json"
    $record = New-EasyConWorkspaceEvidenceRecord -Context $Context -Candidate $Candidate `
        -BaseCommit $BaseCommit -GatePolicyHash $GatePolicyHash -Gates $Gates `
        -VerifyDurationMilliseconds $VerifyDurationMilliseconds -RunId $RunId `
        -StartedUtc $StartedUtc -CompletedUtc $CompletedUtc `
        -TotalDurationMilliseconds $TotalDurationMilliseconds -Credential "tree" `
        -EvidenceFile $evidenceFile
    $destination = Join-Path $workspaceRoot $evidenceFile
    $text = ($record | ConvertTo-Json -Depth 16) + [Environment]::NewLine
    Publish-EasyConWorkspaceEvidenceNoReplace -Path $destination -Text $text `
        -TrustedRoot $workspaceRoot | Out-Null
    Write-EasyConStructuredRecord -Kind "workspace" -Value $record
    return [pscustomobject]$record
}

function Invoke-EasyConWindowsWorkspaceGates {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [string]$BaseSha,

        [switch]$RequireCleanTree,

        [switch]$RequireStagedCandidate,

        [Parameter(Mandatory)]
        [object]$Policy,

        [scriptblock]$GateInvoker
    )

    $repository = Assert-EasyConPhysicalPath -Path $RepositoryRoot
    if ($RequireCleanTree -and $RequireStagedCandidate) {
        throw "-RequireCleanTree and -RequireStagedCandidate are mutually exclusive"
    }
    $cargo = Get-EasyConCommandPath -Name "cargo.exe"
    $python = Get-EasyConCommandPath -Name "python.exe"
    $pwsh = Get-EasyConCommandPath -Name "pwsh.exe"
    $git = Get-EasyConCommandPath -Name "git.exe"
    $baseCommit = Resolve-EasyConWorkspaceBaseCommit -Git $git -RepositoryRoot $repository `
        -BaseSha $BaseSha
    $cargoJobs = [int]$Policy.CargoJobs
    if ($cargoJobs -ne 4) {
        throw "Windows gate policy cargoJobs must remain 4"
    }
    $candidate = New-EasyConWorkspaceCandidateBinding -RepositoryRoot $repository `
        -Git $git -RequireCleanTree:$RequireCleanTree `
        -RequireStagedCandidate:$RequireStagedCandidate
    $programs = @{
        cargo = $cargo
        python = $python
        pwsh = $pwsh
        git = $git
    }
    $records = [System.Collections.Generic.List[object]]::new()
    foreach ($gate in @($Policy.Gates)) {
        $tool = [string]$gate.Tool
        if (-not $programs.ContainsKey($tool)) {
            throw "Windows gate policy selected an unsupported tool '$tool'"
        }
        $arguments = @($gate.Arguments)
        if ($tool -ceq "cargo" -and $arguments[0] -cne "fmt") {
            $arguments = @($arguments[0], "--jobs", [string]$cargoJobs) + @(
                $arguments | Select-Object -Skip 1
            )
        }
        $records.Add((Invoke-EasyConTimedPolicyGate -Name ([string]$gate.Name) `
            -Program ([string]$programs[$tool]) -Arguments $arguments `
            -RepositoryRoot $repository -GateInvoker $GateInvoker)) | Out-Null
    }
    if ($RequireStagedCandidate) {
        $records.Add((Invoke-EasyConTimedPolicyGate -Name "git diff --cached --check" `
            -Program $git -Arguments @("diff", "--cached", "--check") `
            -RepositoryRoot $repository -GateInvoker $GateInvoker)) | Out-Null
    }
    if ($null -ne $baseCommit) {
        $records.Add((Invoke-EasyConTimedPolicyGate -Name "git diff base...HEAD --check" `
            -Program $git -Arguments @("diff", "--check", "$baseCommit...HEAD") `
            -RepositoryRoot $repository -GateInvoker $GateInvoker)) | Out-Null
    }
    if ($null -ne $candidate) {
        $candidate = Assert-EasyConWorkspaceCandidateBindingCurrent `
            -RepositoryRoot $repository -Git $git -Candidate $candidate
    }
    return [pscustomobject]@{
        Candidate = $candidate
        BaseCommit = $baseCommit
        Gates = $records.ToArray()
        Git = $git
    }
}

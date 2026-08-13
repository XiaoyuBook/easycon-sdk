[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet("Fast", "Qualification", IgnoreCase = $false)]
    [string]$Mode,

    [string]$ExactCase
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest

if ($Mode -cnotin @("Fast", "Qualification")) {
    throw "-Mode must use one exact supported case: Fast or Qualification"
}

$script:repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$script:configurationPath = Join-Path $PSScriptRoot "windows_build_environment.json"
$script:modulePath = Join-Path $PSScriptRoot "windows_workspace.psm1"
$script:policyPath = Join-Path $PSScriptRoot "windows_gate_policy.json"
$script:temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
    "ec-contract-{0}" -f [guid]::NewGuid().ToString("N").Substring(0, 12)
)
[System.IO.Directory]::CreateDirectory($script:temporaryRoot) | Out-Null
$modules = @(Import-Module -Name $script:modulePath -Force -PassThru)
if ($modules.Count -ne 1) {
    throw "Windows workspace contract must import exactly one module"
}
$script:workspaceModule = $modules[0]
$script:contractCases = [System.Collections.Generic.List[object]]::new()
$script:contractCaseNames = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::Ordinal
)
$script:exactCaseRequested = $PSBoundParameters.ContainsKey("ExactCase")

function Assert-Contract {
    param(
        [Parameter(Mandatory)][bool]$Condition,
        [Parameter(Mandatory)][string]$Message
    )

    if (-not $Condition) {
        throw "contract failed: $Message"
    }
}

function Assert-Throws {
    param(
        [Parameter(Mandatory)][scriptblock]$Action,
        [Parameter(Mandatory)][string]$Pattern
    )

    try {
        & $Action | Out-Null
    }
    catch {
        Assert-Contract ($_.Exception.Message -match $Pattern) `
            "failure '$($_.Exception.Message)' must match '$Pattern'"
        return $_
    }
    throw "contract failed: expected failure matching '$Pattern'"
}

function Assert-Sequence {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Actual,
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Expected,
        [Parameter(Mandatory)][string]$Description
    )

    Assert-Contract (($Actual -join "`0") -ceq ($Expected -join "`0")) `
        "$Description; actual=$($Actual -join ',') expected=$($Expected -join ',')"
}

function Add-ContractCase {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)]
        [ValidateSet("Fast", "Qualification", IgnoreCase = $false)]
        [string]$CaseMode,
        [Parameter(Mandatory)][scriptblock]$Action
    )

    if ($CaseMode -cnotin @("Fast", "Qualification")) {
        throw "contract case '$Name' has an invalid mode"
    }
    if ([string]::IsNullOrWhiteSpace($Name) -or $Name -cnotmatch '^[a-z0-9]+(?:-[a-z0-9]+)*$') {
        throw "contract case name must be one normalized lowercase identifier"
    }
    if (-not $script:contractCaseNames.Add($Name)) {
        throw "contract case registry contains duplicate exact name '$Name'"
    }
    $script:contractCases.Add([pscustomobject]@{
        Name = $Name
        Mode = $CaseMode
        Action = $Action
    }) | Out-Null
}

function Invoke-PrivateCommand {
    param(
        [Parameter(Mandatory)][string]$Name,
        [hashtable]$Parameters = @{}
    )

    & $script:workspaceModule {
        param($CommandName, $Arguments)
        & $CommandName @Arguments
    } $Name $Parameters
}

function Invoke-PrivateCommandWithOverride {
    param(
        [Parameter(Mandatory)][string]$Name,
        [hashtable]$Parameters = @{},
        [Parameter(Mandatory)][string]$OverrideName,
        [Parameter(Mandatory)][scriptblock]$Override
    )

    & $script:workspaceModule {
        param($CommandName, $Arguments, $PrivateName, $Replacement)
        $path = "Function:\$PrivateName"
        $originalCommands = @(
            Get-Command -Name $PrivateName -CommandType Function -ErrorAction Stop
        )
        if ($originalCommands.Count -ne 1 -or $null -eq $originalCommands[0].ScriptBlock) {
            throw "private override target must resolve exactly one PowerShell function: $PrivateName"
        }
        $original = $originalCommands[0].ScriptBlock
        try {
            Set-Item -LiteralPath $path -Value $Replacement -ErrorAction Stop
            $active = Get-Command -Name $PrivateName -CommandType Function -ErrorAction Stop
            if (-not [object]::ReferenceEquals($active.ScriptBlock, $Replacement)) {
                throw "private command override was not installed: $PrivateName"
            }
            & $CommandName @Arguments
        }
        finally {
            Set-Item -LiteralPath $path -Value $original -ErrorAction Stop
            $restored = Get-Command -Name $PrivateName -CommandType Function -ErrorAction Stop
            if (-not [object]::ReferenceEquals($restored.ScriptBlock, $original)) {
                throw "private command override was not restored: $PrivateName"
            }
        }
    } $Name $Parameters $OverrideName $Override
}

function Set-ContractText {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][AllowEmptyString()][string]$Value
    )

    $parent = [System.IO.Path]::GetDirectoryName([System.IO.Path]::GetFullPath($Path))
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
    [System.IO.File]::WriteAllText(
        $Path,
        $Value,
        [System.Text.UTF8Encoding]::new($false, $true)
    )
}

function Get-ContractFileSha256 {
    param([Parameter(Mandatory)][string]$Path)

    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-ContractApplicationPath {
    param([Parameter(Mandatory)][string]$Name)

    $commands = @(Get-Command -Name $Name -CommandType Application -All -ErrorAction Stop)
    if ($commands.Count -eq 0) {
        throw "contract application '$Name' is unavailable"
    }
    $path = [string]$commands[0].Source
    if (
        -not [System.IO.Path]::IsPathFullyQualified($path) -or
        -not (Test-Path -LiteralPath $path -PathType Leaf)
    ) {
        throw "contract application '$Name' did not resolve to one existing absolute path"
    }
    return $path
}

function Invoke-ContractGit {
    param(
        [Parameter(Mandatory)][string]$WorkingDirectory,
        [Parameter(Mandatory)][string[]]$Arguments,
        [switch]$AllowFailure
    )

    $git = Get-ContractApplicationPath -Name "git.exe"
    $output = @(& $git -C $WorkingDirectory @Arguments 2>&1 | ForEach-Object {
        $_.ToString()
    })
    $exitCode = $LASTEXITCODE
    if (-not $AllowFailure -and $exitCode -ne 0) {
        throw "contract Git failed with exit code ${exitCode}: git $($Arguments -join ' ')`n$($output -join [Environment]::NewLine)"
    }
    return [pscustomobject]@{
        ExitCode = $exitCode
        Output = $output
    }
}

function New-ContractGitRepository {
    param(
        [Parameter(Mandatory)][string]$Path,
        [hashtable]$Files = @{ "tracked.txt" = "base`n" }
    )

    [System.IO.Directory]::CreateDirectory($Path) | Out-Null
    Invoke-ContractGit -WorkingDirectory $Path -Arguments @("init", "--quiet") | Out-Null
    Invoke-ContractGit -WorkingDirectory $Path `
        -Arguments @("config", "user.email", "contract@example.invalid") | Out-Null
    Invoke-ContractGit -WorkingDirectory $Path `
        -Arguments @("config", "user.name", "EasyCon Contract") | Out-Null
    foreach ($entry in $Files.GetEnumerator()) {
        Set-ContractText -Path (Join-Path $Path ([string]$entry.Key)) `
            -Value ([string]$entry.Value)
    }
    Invoke-ContractGit -WorkingDirectory $Path -Arguments @("add", "--all") | Out-Null
    Invoke-ContractGit -WorkingDirectory $Path `
        -Arguments @("commit", "--quiet", "-m", "contract fixture") | Out-Null
    $head = (Invoke-ContractGit -WorkingDirectory $Path `
        -Arguments @("rev-parse", "HEAD")).Output[0].Trim()
    $tree = (Invoke-ContractGit -WorkingDirectory $Path `
        -Arguments @("rev-parse", "HEAD^{tree}")).Output[0].Trim()
    Assert-Contract (
        $head -cmatch '^[0-9a-f]{40}$' -and $tree -cmatch '^[0-9a-f]{40}$'
    ) "contract Git fixture must resolve immutable HEAD and tree identities"
    return [pscustomobject]@{ Path = $Path; Head = $head; Tree = $tree }
}

function Get-ContractPrivatePolicyContext {
    return Invoke-PrivateCommand -Name "Get-EasyConGatePolicyContext" `
        -Parameters @{ RepositoryRoot = $script:repositoryRoot }
}

function Get-ContractEnvironmentLocation {
    param(
        [Parameter(Mandatory)][string]$RepositoryRoot,
        [Parameter(Mandatory)][string]$CacheRoot,
        [string]$Fingerprint = ("a" * 64)
    )

    $configuration = Invoke-PrivateCommand -Name "Get-EasyConWindowsBuildConfiguration" `
        -Parameters @{ Path = $script:configurationPath }
    return Invoke-PrivateCommand -Name "Get-EasyConEnvironmentLocation" -Parameters @{
        RepositoryRoot = $RepositoryRoot
        Fingerprint = $Fingerprint
        Configuration = $configuration
        CacheRoot = $CacheRoot
    }
}

function Get-ContractEnvironmentSnapshot {
    $snapshot = [System.Collections.Generic.Dictionary[string,object]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($entry in [Environment]::GetEnvironmentVariables("Process").GetEnumerator()) {
        $snapshot[[string]$entry.Key] = [pscustomobject]@{
            Name = [string]$entry.Key
            Value = [string]$entry.Value
        }
    }
    return $snapshot
}

function Restore-ContractEnvironment {
    param([Parameter(Mandatory)][object]$Snapshot)

    foreach ($entry in [Environment]::GetEnvironmentVariables("Process").GetEnumerator()) {
        if (-not $Snapshot.ContainsKey([string]$entry.Key)) {
            [Environment]::SetEnvironmentVariable([string]$entry.Key, $null, "Process")
        }
    }
    foreach ($entry in $Snapshot.Values) {
        [Environment]::SetEnvironmentVariable($entry.Name, $entry.Value, "Process")
    }
}

function Assert-ContractEnvironmentMatches {
    param(
        [Parameter(Mandatory)][object]$Expected,
        [Parameter(Mandatory)][string]$Description
    )

    $actual = Get-ContractEnvironmentSnapshot
    Assert-Contract ($actual.Count -eq $Expected.Count) `
        "$Description must restore the process environment entry count"
    foreach ($entry in $Expected.Values) {
        Assert-Contract (
            $actual.ContainsKey($entry.Name) -and
            $actual[$entry.Name].Name -ceq $entry.Name -and
            $actual[$entry.Name].Value -ceq $entry.Value
        ) "$Description must restore '$($entry.Name)' exactly"
    }
}

function Wait-ContractFile {
    param(
        [Parameter(Mandatory)][string]$Path,
        [ValidateRange(1, 300000)][int]$TimeoutMilliseconds = 30000
    )

    $resolved = [System.IO.Path]::GetFullPath($Path)
    $parent = [System.IO.Path]::GetDirectoryName($resolved)
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $watcher = [System.IO.FileSystemWatcher]::new($parent, [System.IO.Path]::GetFileName($resolved))
    $watcher.NotifyFilter = [System.IO.NotifyFilters]::FileName -bor `
        [System.IO.NotifyFilters]::LastWrite
    try {
        $watcher.EnableRaisingEvents = $true
        while (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
            $remaining = [int]([long]$TimeoutMilliseconds - $timer.ElapsedMilliseconds)
            if ($remaining -le 0) {
                throw "timed out waiting for contract marker: $resolved"
            }
            $change = $watcher.WaitForChanged(
                [System.IO.WatcherChangeTypes]::Created -bor
                    [System.IO.WatcherChangeTypes]::Changed -bor
                    [System.IO.WatcherChangeTypes]::Renamed,
                $remaining
            )
            if ($change.TimedOut) {
                throw "timed out waiting for contract marker: $resolved"
            }
        }
    }
    finally {
        $watcher.Dispose()
        $timer.Stop()
    }
}

function Start-ContractProcess {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$ScriptPath,
        [string[]]$Arguments = @()
    )

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = Join-Path $PSHOME "pwsh.exe"
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in @("-NoLogo", "-NoProfile", "-File", $ScriptPath) + $Arguments) {
        $startInfo.ArgumentList.Add([string]$argument)
    }
    $process = [System.Diagnostics.Process]::Start($startInfo)
    if ($null -eq $process) {
        throw "failed to start contract process '$Name'"
    }
    return [pscustomobject]@{
        Name = $Name
        Process = $process
        OutputTask = $process.StandardOutput.ReadToEndAsync()
        ErrorTask = $process.StandardError.ReadToEndAsync()
        Output = $null
        Error = $null
    }
}

function Complete-ContractProcessOutput {
    param([Parameter(Mandatory)][object]$Probe)

    if ($null -eq $Probe.Output) {
        $Probe.Output = $Probe.OutputTask.GetAwaiter().GetResult()
    }
    if ($null -eq $Probe.Error) {
        $Probe.Error = $Probe.ErrorTask.GetAwaiter().GetResult()
    }
}

function Wait-ContractProcessesExited {
    param(
        [Parameter(Mandatory)][object[]]$Probe,
        [ValidateRange(1, 300000)][int]$TimeoutMilliseconds = 30000
    )

    $deadline = [DateTimeOffset]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    foreach ($entry in $Probe) {
        $remaining = [int][Math]::Max(
            0,
            ($deadline - [DateTimeOffset]::UtcNow).TotalMilliseconds
        )
        if (-not $entry.Process.WaitForExit($remaining)) {
            throw "contract process '$($entry.Name)' did not exit within the shared deadline"
        }
        Complete-ContractProcessOutput -Probe $entry
    }
}

function Get-ContractProcessDiagnostics {
    param([Parameter(Mandatory)][object[]]$Probe)

    return (@($Probe | ForEach-Object {
        $exit = if ($_.Process.HasExited) { [string]$_.Process.ExitCode } else { "running" }
        $output = if ($null -eq $_.Output) { "<pending>" } else { $_.Output.Trim() }
        $error = if ($null -eq $_.Error) { "<pending>" } else { $_.Error.Trim() }
        "name=$($_.Name) exit=$exit stdout=<$output> stderr=<$error>"
    }) -join [Environment]::NewLine)
}

function Stop-ContractProcesses {
    param(
        [Parameter(Mandatory)][object[]]$Probe,
        [ValidateRange(1, 300000)][int]$TimeoutMilliseconds = 30000
    )

    $issues = [System.Collections.Generic.List[string]]::new()
    $live = @($Probe | Where-Object {
        $null -ne $_.Process -and -not $_.Process.HasExited
    })
    foreach ($entry in $live) {
        try {
            $entry.Process.Kill($true)
        }
        catch {
            $issues.Add("kill $($entry.Name): $($_.Exception.Message)") | Out-Null
        }
    }
    $deadline = [DateTimeOffset]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    foreach ($entry in $Probe) {
        try {
            if (-not $entry.Process.HasExited) {
                $remaining = [int][Math]::Max(
                    0,
                    ($deadline - [DateTimeOffset]::UtcNow).TotalMilliseconds
                )
                if (-not $entry.Process.WaitForExit($remaining)) {
                    $issues.Add("wait $($entry.Name): process remained live") | Out-Null
                    continue
                }
            }
            Complete-ContractProcessOutput -Probe $entry
        }
        catch {
            $issues.Add("drain $($entry.Name): $($_.Exception.Message)") | Out-Null
        }
        finally {
            try {
                $entry.Process.Dispose()
            }
            catch {
                $issues.Add("dispose $($entry.Name): $($_.Exception.Message)") | Out-Null
            }
        }
    }
    if ($issues.Count -ne 0) {
        throw "contract process teardown failed after all-live-first termination: $($issues -join '; ')"
    }
}

function Invoke-BootstrapContracts {
    $python = Get-ContractApplicationPath -Name "python.exe"
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    & $python -B (Join-Path $PSScriptRoot "test_windows_bootstrap_contracts.py")
    $exitCode = $LASTEXITCODE
    $timer.Stop()
    if ($exitCode -ne 0) {
        throw "Windows bootstrap contracts failed with exit code $exitCode"
    }
    Write-Output (
        "CONTRACT_BOOTSTRAP_PASS mode=Fast durationMs={0}" -f $timer.ElapsedMilliseconds
    )
}

# Fast groups use production parsers and private seams directly. Synonymous mutations stay
# table-driven inside a domain group rather than becoming separate registered cases.
Add-ContractCase -Name "configuration-fingerprint" -CaseMode "Fast" -Action {
    $configuration = Invoke-PrivateCommand -Name "Get-EasyConWindowsBuildConfiguration" `
        -Parameters @{ Path = $script:configurationPath }
    Assert-Contract (
        $configuration.version -eq 5 -and
        $configuration.target -ceq "x86_64-pc-windows-msvc" -and
        @($configuration.fingerprintInputs).Count -ge 20
    ) "the production parser must accept the current strict v5 configuration"

    $configurationText = Get-Content -Raw -LiteralPath $script:configurationPath
    $mutatedConfiguration = $configurationText | ConvertFrom-Json -Depth 64
    $reversed = @($mutatedConfiguration.fingerprintInputs)
    [array]::Reverse($reversed)
    $mutatedConfiguration.fingerprintInputs = $reversed
    $mutationPath = Join-Path $script:temporaryRoot "config/reversed.json"
    Set-ContractText -Path $mutationPath `
        -Value ($mutatedConfiguration | ConvertTo-Json -Depth 64)
    Assert-Throws -Pattern "input set, kind, or order changed" -Action {
        Invoke-PrivateCommand -Name "Get-EasyConWindowsBuildConfiguration" `
            -Parameters @{ Path = $mutationPath }
    } | Out-Null

    $textRoot = Join-Path $script:temporaryRoot "fingerprint/text"
    $textPath = Join-Path $textRoot "input.txt"
    Set-ContractText -Path $textPath -Value "alpha`r`nbeta`r`n"
    $textConfiguration = [pscustomobject]@{
        fingerprintInputs = @([pscustomobject]@{ path = "input.txt"; kind = "text" })
    }
    $crlf = Invoke-PrivateCommand -Name "Get-EasyConEnvironmentFingerprint" `
        -Parameters @{ RepositoryRoot = $textRoot; Configuration = $textConfiguration }
    Set-ContractText -Path $textPath -Value "alpha`nbeta`n"
    $lf = Invoke-PrivateCommand -Name "Get-EasyConEnvironmentFingerprint" `
        -Parameters @{ RepositoryRoot = $textRoot; Configuration = $textConfiguration }
    Assert-Contract ($crlf.Value -ceq $lf.Value) `
        "text fingerprint inputs must canonicalize checkout line endings"

    $binaryConfiguration = [pscustomobject]@{
        fingerprintInputs = @([pscustomobject]@{ path = "input.txt"; kind = "binary" })
    }
    [System.IO.File]::WriteAllBytes($textPath, [byte[]](0x61, 0x0d, 0x0a, 0x62))
    $binaryCrLf = Invoke-PrivateCommand -Name "Get-EasyConEnvironmentFingerprint" `
        -Parameters @{ RepositoryRoot = $textRoot; Configuration = $binaryConfiguration }
    [System.IO.File]::WriteAllBytes($textPath, [byte[]](0x61, 0x0a, 0x62))
    $binaryLf = Invoke-PrivateCommand -Name "Get-EasyConEnvironmentFingerprint" `
        -Parameters @{ RepositoryRoot = $textRoot; Configuration = $binaryConfiguration }
    Assert-Contract ($binaryCrLf.Value -cne $binaryLf.Value) `
        "binary fingerprint inputs must preserve raw byte identity"

    $cache = Join-Path $script:temporaryRoot "fingerprint/cache"
    $one = Get-ContractEnvironmentLocation -RepositoryRoot (
        Join-Path $script:temporaryRoot "fingerprint/worktree-one"
    ) -CacheRoot $cache -Fingerprint $lf.Value
    $two = Get-ContractEnvironmentLocation -RepositoryRoot (
        Join-Path $script:temporaryRoot "fingerprint/worktree-two"
    ) -CacheRoot $cache -Fingerprint $lf.Value
    Assert-Contract (
        $one.EnvironmentRoot -ceq $two.EnvironmentRoot -and
        $one.LockPath -ceq $two.LockPath -and
        $one.WorkspaceRoot -cne $two.WorkspaceRoot
    ) "one fingerprint must share prepared identity and isolate writable worktrees"
}

Add-ContractCase -Name "tool-version-pin-parsers" -CaseMode "Fast" -Action {
    $configuration = Invoke-PrivateCommand -Name "Get-EasyConWindowsBuildConfiguration" `
        -Parameters @{ Path = $script:configurationPath }
    $rust = Invoke-PrivateCommand -Name "Get-EasyConRustToolchainPin" `
        -Parameters @{ RepositoryRoot = $script:repositoryRoot }
    Assert-Contract (
        $rust.Channel -ceq "1.97.1" -and
        $rust.Target -ceq "x86_64-pc-windows-msvc" -and
        (@($rust.Components | Sort-Object) -join "`0") -ceq "clippy`0rustfmt"
    ) "the Rust toolchain parser must retain the exact release, target, and components"

    foreach ($expected in @($configuration.vcpkg.nativeDependencies)) {
        $record = [pscustomobject]@{
            version = [string]$expected.version
            'port-version' = [int]$expected.portVersion
            'git-tree' = [string]$expected.gitTree
        }
        $matches = Invoke-PrivateCommand -Name "Test-EasyConVcpkgVersionRecord" `
            -Parameters @{ Record = $record; Expected = $expected }
        Assert-Contract $matches "the vcpkg version parser must accept the frozen $($expected.name) record"
        $record.'git-tree' = "0" * 40
        $matches = Invoke-PrivateCommand -Name "Test-EasyConVcpkgVersionRecord" `
            -Parameters @{ Record = $record; Expected = $expected }
        Assert-Contract (-not $matches) `
            "the vcpkg version parser must reject a changed $($expected.name) tree"
    }

    foreach ($expected in @($configuration.vcpkg.internalTools)) {
        $record = if ([string]$expected.name -ceq "7zr") {
            [pscustomobject]@{ name = $expected.name; os = "windows" }
        }
        else {
            [pscustomobject]@{ name = $expected.name; os = "windows"; arch = "x64" }
        }
        $matches = Invoke-PrivateCommand -Name "Test-EasyConVcpkgToolManifestRecord" `
            -Parameters @{ Record = $record; Expected = $expected }
        Assert-Contract $matches "the tool manifest parser must accept the frozen $($expected.name) record"
    }

    $python = Invoke-PrivateCommand -Name "ConvertFrom-EasyConPythonVersionOutput" `
        -Parameters @{ Output = @("Python 3.12.10"); Description = "contract Python" }
    Assert-Contract ($python -eq [version]"3.12.10") `
        "Python versions must use numeric System.Version semantics"
    foreach ($invalid in @(@(), @("Python 3.12.10", "extra"), @("Python 3.12"))) {
        Assert-Throws -Pattern "exactly one line|unrecognized" -Action {
            Invoke-PrivateCommand -Name "ConvertFrom-EasyConPythonVersionOutput" `
                -Parameters @{ Output = $invalid; Description = "contract Python" }
        } | Out-Null
    }
}

Add-ContractCase -Name "module-wrapper-contracts" -CaseMode "Fast" -Action {
    $exports = @($script:workspaceModule.ExportedCommands.Keys | Sort-Object)
    Assert-Sequence -Actual $exports -Expected @(
        "Invoke-EasyConWindowsSetup",
        "Invoke-EasyConWindowsVerify",
        "Invoke-EasyConWindowsWorkspace"
    ) -Description "the public module surface must expose only controlled lifecycle wrappers"

    $context = Get-ContractPrivatePolicyContext
    Assert-Contract (
        $context.Hash.Value -cmatch '^[0-9a-f]{64}$' -and
        @($context.Hash.Inputs).Count -eq 3 -and
        (@($context.Hash.Inputs.path) -join "`0") -ceq (
            "tools/windows_gate_policy.json`0" +
            "tools/windows_gate_policy.ps1`0" +
            "tools/run_windows_workspace.ps1"
        )
    ) "runner, policy parser, and JSON must remain one byte-bound policy identity"

    $tokens = $null
    $errors = $null
    $runnerAst = [System.Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot "run_windows_workspace.ps1"),
        [ref]$tokens,
        [ref]$errors
    )
    Assert-Contract ($errors.Count -eq 0 -and $null -ne $runnerAst.ParamBlock) `
        "the controlled runner must have a valid parameterized PowerShell AST"
    $validateSets = @($runnerAst.FindAll({
        param($node)
        $node -is [System.Management.Automation.Language.AttributeAst] -and
        $node.TypeName.FullName -ceq "ValidateSet"
    }, $true))
    Assert-Contract (
        @($validateSets | Where-Object {
            $_.Extent.Text -match 'Setup' -and $_.Extent.Text -match 'Targeted'
        }).Count -eq 1
    ) "the runner mode wrapper must retain one explicit exact ValidateSet"
}

Add-ContractCase -Name "plan-targeted-arguments" -CaseMode "Fast" -Action {
    $allowed = @(
        [pscustomobject]@{
            Command = "test"
            Arguments = @("-p", "easycon-model", "--", "--exact")
        },
        [pscustomobject]@{
            Command = "check"
            Arguments = @("--package=easycon-runtime", "--all-targets")
        },
        [pscustomobject]@{
            Command = "clippy"
            Arguments = @("-peasycon-controller", "--all-targets")
        }
    )
    foreach ($entry in $allowed) {
        $actual = @(Invoke-PrivateCommand -Name "Get-EasyConTargetedCargoArguments" `
            -Parameters @{
                CargoCommand = $entry.Command
                CargoJobs = 4
                CargoArguments = $entry.Arguments
            })
        $expected = @($entry.Command, "--locked", "--jobs", "4") + @($entry.Arguments)
        Assert-Sequence -Actual $actual -Expected $expected `
            -Description "Targeted Cargo must forward tokenized $($entry.Command) arguments"
    }

    $forbidden = @(
        @("test", @("--workspace", "-p", "easycon-model")),
        @("test", @("--manifest-path=other/Cargo.toml", "-p", "easycon-model")),
        @("test", @("-mother/Cargo.toml", "-p", "easycon-model")),
        @("test", @("--jobs=8", "-p", "easycon-model")),
        @("clippy", @("--fix", "-p", "easycon-model")),
        @("check", @("--all", "-p", "easycon-model")),
        @("test", @("--target-dir", "other", "-p", "easycon-model")),
        @("test", @("-p", "easycon-model", "--", "-m", "binary-filter"))
    )
    for ($index = 0; $index -lt $forbidden.Count - 1; $index++) {
        $entry = $forbidden[$index]
        Assert-Throws -Pattern "not permitted|requires an explicit" -Action {
            Invoke-PrivateCommand -Name "Get-EasyConTargetedCargoArguments" `
                -Parameters @{
                    CargoCommand = [string]$entry[0]
                    CargoJobs = 4
                    CargoArguments = [string[]]$entry[1]
                }
        } | Out-Null
    }
    $binaryInput = [string[]]$forbidden[-1][1]
    $separatorIndex = [Array]::IndexOf($binaryInput, "--")
    Assert-Sequence -Actual $binaryInput[0..($separatorIndex - 1)] `
        -Expected @("-p", "easycon-model") `
        -Description "binary arguments must retain an explicit package before --"
    Assert-Sequence -Actual $binaryInput[($separatorIndex + 1)..($binaryInput.Count - 1)] `
        -Expected @("-m", "binary-filter") `
        -Description "binary arguments must reach the test binary after --"
    $binaryArguments = @(Invoke-PrivateCommand -Name "Get-EasyConTargetedCargoArguments" `
        -Parameters @{
            CargoCommand = "test"
            CargoJobs = 4
            CargoArguments = $binaryInput
        })
    Assert-Sequence -Actual $binaryArguments `
        -Expected @("test", "--locked", "--jobs", "4", "-p", "easycon-model", "--", "-m", "binary-filter") `
        -Description "arguments after -- must remain test-binary arguments"
}

Add-ContractCase -Name "candidate-binding" -CaseMode "Fast" -Action {
    $fixture = New-ContractGitRepository -Path (
        Join-Path $script:temporaryRoot "candidate/repository"
    )
    $git = Get-ContractApplicationPath -Name "git.exe"
    Set-ContractText -Path (Join-Path $fixture.Path "tracked.txt") -Value "staged`n"
    Invoke-ContractGit -WorkingDirectory $fixture.Path `
        -Arguments @("add", "--", "tracked.txt") | Out-Null
    $candidate = Invoke-PrivateCommand -Name "New-EasyConWorkspaceCandidateBinding" `
        -Parameters @{
            RepositoryRoot = $fixture.Path
            Git = $git
            RequireStagedCandidate = $true
        }
    $indexTree = (Invoke-ContractGit -WorkingDirectory $fixture.Path `
        -Arguments @("write-tree")).Output[0].Trim()
    Assert-Contract (
        $candidate.CandidateMode -ceq "staged-candidate" -and
        $candidate.HeadCommit -ceq $fixture.Head -and
        $candidate.Tree -ceq $indexTree
    ) "staged candidate binding must fix HEAD and the current index tree"

    Set-ContractText -Path (Join-Path $fixture.Path "tracked.txt") -Value "unstaged`n"
    Assert-Throws -Pattern "unstaged|state changed" -Action {
        Invoke-PrivateCommand -Name "Assert-EasyConWorkspaceCandidateBindingCurrent" `
            -Parameters @{ RepositoryRoot = $fixture.Path; Git = $git; Candidate = $candidate }
    } | Out-Null
    Set-ContractText -Path (Join-Path $fixture.Path "tracked.txt") -Value "staged`n"
    Set-ContractText -Path (Join-Path $fixture.Path "untracked.txt") -Value "untracked`n"
    Assert-Throws -Pattern "untracked" -Action {
        Invoke-PrivateCommand -Name "New-EasyConWorkspaceCandidateBinding" `
            -Parameters @{
                RepositoryRoot = $fixture.Path
                Git = $git
                RequireStagedCandidate = $true
            }
    } | Out-Null
    Remove-Item -LiteralPath (Join-Path $fixture.Path "untracked.txt") -Force
    $base = Invoke-PrivateCommand -Name "Resolve-EasyConWorkspaceBaseCommit" `
        -Parameters @{ Git = $git; RepositoryRoot = $fixture.Path; BaseSha = "HEAD" }
    Assert-Contract ($base -ceq $fixture.Head) `
        "base refs must resolve once to one immutable full commit"
}

Add-ContractCase -Name "evidence-output" -CaseMode "Fast" -Action {
    $evidenceRoot = Join-Path $script:temporaryRoot "evidence"
    [System.IO.Directory]::CreateDirectory($evidenceRoot) | Out-Null
    $context = [pscustomobject]@{
        Fingerprint = [pscustomobject]@{ Value = "a" * 64 }
        Configuration = [pscustomobject]@{ target = "x86_64-pc-windows-msvc" }
        Location = [pscustomobject]@{
            IdentityKey = "v5-contract"
            WorkspaceKey = "contract-workspace"
            WorkspaceRoot = $evidenceRoot
        }
    }
    $candidate = [pscustomobject]@{
        CandidateMode = "staged-candidate"
        HeadCommit = "b" * 40
        Tree = "c" * 40
    }
    $gateRecords = @(
        [pscustomobject]@{ name = "first"; status = "passed"; durationMs = 1 },
        [pscustomobject]@{ name = "second"; status = "passed"; durationMs = 2 }
    )
    $started = [DateTimeOffset]::UtcNow
    $record = Invoke-PrivateCommand -Name "New-EasyConWorkspaceEvidenceRecord" `
        -Parameters @{
            Context = $context
            Candidate = $candidate
            BaseCommit = "d" * 40
            GatePolicyHash = "e" * 64
            Gates = $gateRecords
            VerifyDurationMilliseconds = 3L
            RunId = "f" * 32
            StartedUtc = $started
            CompletedUtc = $started.AddMilliseconds(5)
            TotalDurationMilliseconds = 5L
            Credential = "tree"
            EvidenceFile = "evidence/v2/contract.json"
        }
    Assert-Contract (
        $record.schemaVersion -eq 2 -and
        $record.status -ceq "passed" -and
        $record.tree -ceq $candidate.Tree -and
        $record.baseCommit -ceq ("d" * 40) -and
        (@($record.gates.name) -join "`0") -ceq "first`0second"
    ) "schema-v2 evidence must retain ordered tree-bound candidate identity"

    $destination = Join-Path $evidenceRoot "no-replace.json"
    $text = ($record | ConvertTo-Json -Depth 16) + "`n"
    Invoke-PrivateCommand -Name "Publish-EasyConWorkspaceEvidenceNoReplace" `
        -Parameters @{ Path = $destination; Text = $text; TrustedRoot = $evidenceRoot } | Out-Null
    Assert-Throws -Pattern "already exists|not be replaced" -Action {
        Invoke-PrivateCommand -Name "Publish-EasyConWorkspaceEvidenceNoReplace" `
            -Parameters @{ Path = $destination; Text = $text; TrustedRoot = $evidenceRoot }
    } | Out-Null
    Assert-Contract (
        (Get-Content -Raw -LiteralPath $destination) -ceq $text -and
        @(Get-ChildItem -Force -LiteralPath $evidenceRoot | Where-Object {
            $_.Name -like '*.write-*'
        }).Count -eq 0
    ) "evidence publication must be atomic, no-replace, and leave no temporary"
}

Add-ContractCase -Name "policy-snapshot-revalidation" -CaseMode "Fast" -Action {
    $context = Get-ContractPrivatePolicyContext
    $gateNames = @($context.Policy.Gates.Name)
    $policyDocument = Get-Content -Raw -LiteralPath $script:policyPath | `
        ConvertFrom-Json -Depth 32
    Assert-Contract (
        $gateNames.Count -eq @($policyDocument.gates).Count -and
        $gateNames.Count -gt 0 -and
        @($gateNames | Where-Object {
            $_ -match 'bootstrap|test_windows_workspace|environment_lifecycle'
        }).Count -eq 0
    ) "A policy must follow its JSON gate data and contain zero PowerShell infra contracts"

    $policyText = Get-Content -Raw -LiteralPath $script:policyPath
    $lfSnapshot = Invoke-PrivateCommand `
        -Name "New-EasyConGatePolicyStrictUtf8TextSnapshotFromText" -Parameters @{
            Path = $script:policyPath
            RelativePath = "tools/windows_gate_policy.json"
            Text = $policyText
            TrustedRoot = $script:repositoryRoot
            Description = "contract policy JSON"
        }
    $crlfSnapshot = Invoke-PrivateCommand `
        -Name "New-EasyConGatePolicyStrictUtf8TextSnapshotFromText" -Parameters @{
            Path = $script:policyPath
            RelativePath = "tools/windows_gate_policy.json"
            Text = ($policyText -replace "(?<!`r)`n", "`r`n")
            TrustedRoot = $script:repositoryRoot
            Description = "contract policy JSON"
        }
    Assert-Contract ($lfSnapshot.Sha256 -cne $crlfSnapshot.Sha256) `
        "policy identity must bind exact source bytes without newline normalization"
    Assert-Throws -Pattern "BOM" -Action {
        Invoke-PrivateCommand -Name "New-EasyConGatePolicyStrictUtf8TextSnapshotFromText" `
            -Parameters @{
                Path = $script:policyPath
                RelativePath = "tools/windows_gate_policy.json"
                Text = ([char]0xfeff + $policyText)
                TrustedRoot = $script:repositoryRoot
                Description = "contract policy JSON"
            }
    } | Out-Null

    foreach ($mutation in @(
        "duplicate",
        "unsupported-tool",
        "dot-path-segment",
        "empty-path-segment",
        "expanded-git",
        "missing-cargo-property",
        "missing-python-property",
        "all-features-check",
        "package-scope",
        "filtered-test",
        "weakened-clippy",
        "python-extra"
    )) {
        $value = $policyText | ConvertFrom-Json -Depth 32
        switch ($mutation) {
            "duplicate" {
                $value.gates[1].name = $value.gates[0].name
            }
            "unsupported-tool" {
                $value.gates[0].tool = "cmd"
            }
            "dot-path-segment" {
                $value.gates[4].name = "python tools/./run_runtime_models.py"
                $value.gates[4].arguments[0] = "tools/./run_runtime_models.py"
            }
            "empty-path-segment" {
                $value.gates[5].name = "python tools//validate_specs.py"
                $value.gates[5].arguments[0] = "tools//validate_specs.py"
            }
            "expanded-git" {
                $value.gates[8].name = "git diff --check --stat"
                $value.gates[8].arguments = @("diff", "--check", "--stat")
            }
            "missing-cargo-property" {
                $value.gates = @($value.gates | Where-Object {
                    $_.arguments[0] -cne "fmt"
                })
            }
            "missing-python-property" {
                $value.gates = @($value.gates | Where-Object {
                    $_.arguments[0] -cne "tools/run_runtime_models.py"
                })
            }
            "all-features-check" {
                $value.gates[1].name = `
                    "cargo check --locked --jobs 4 --workspace --all-targets --all-features"
                $value.gates[1].arguments = `
                    @("check", "--locked", "--workspace", "--all-targets", "--all-features")
            }
            "package-scope" {
                $value.gates[3].name += " -p easycon-runtime"
                $value.gates[3].arguments = @($value.gates[3].arguments) + `
                    @("-p", "easycon-runtime")
            }
            "filtered-test" {
                $value.gates[3].name += " -- exact_filter"
                $value.gates[3].arguments = @($value.gates[3].arguments) + `
                    @("--", "exact_filter")
            }
            "weakened-clippy" {
                $value.gates[2].name = $value.gates[2].name.Replace(
                    "-- -D warnings",
                    "-- --cap-lints allow -D warnings"
                )
                $value.gates[2].arguments = @($value.gates[2].arguments)[0..5] + `
                    @("--", "--cap-lints", "allow", "-D", "warnings")
            }
            "python-extra" {
                $value.gates[4].name += " --quiet"
                $value.gates[4].arguments = @($value.gates[4].arguments) + @("--quiet")
            }
        }
        $snapshot = Invoke-PrivateCommand `
            -Name "New-EasyConGatePolicyStrictUtf8TextSnapshotFromText" -Parameters @{
                Path = $script:policyPath
                RelativePath = "tools/windows_gate_policy.json"
                Text = ($value | ConvertTo-Json -Depth 32)
                TrustedRoot = $script:repositoryRoot
                Description = "contract policy JSON"
            }
        Assert-Throws `
            -Pattern "duplicate|unsupported|not permitted|normalized|git diff --check|retain|misses|required|default-feature|scope|test-binary|Clippy|extra arguments" `
            -Action {
            Invoke-PrivateCommand -Name "Get-EasyConWindowsGatePolicy" -Parameters @{
                RepositoryRoot = $script:repositoryRoot
                PolicySnapshot = $snapshot
            }
        } | Out-Null
    }

    $stale = [pscustomobject]@{ Hash = [pscustomobject]@{ Value = "0" * 64 } }
    Assert-Throws -Pattern "changed.*contract boundary" -Action {
        Invoke-PrivateCommand -Name "Assert-EasyConGatePolicyContextCurrent" -Parameters @{
            RepositoryRoot = $script:repositoryRoot
            PolicyContext = $stale
            Boundary = "at the contract boundary"
        }
    } | Out-Null
}

Add-ContractCase -Name "stamp-tool-damage" -CaseMode "Fast" -Action {
    $root = Join-Path $script:temporaryRoot "stamp"
    [System.IO.Directory]::CreateDirectory($root) | Out-Null
    $stampPath = Join-Path $root "environment-stamp.json"
    $absolute = Join-Path $root "prepared"
    $stamp = [ordered]@{
        schemaVersion = 2
        environmentSchema = 5
        hostTargetIdentity = "contract-host-target"
        fingerprint = "a" * 64
        fingerprintInputs = @([ordered]@{
            path = "Cargo.toml"; kind = "text"; sha256 = "b" * 64
        })
        createdUtc = "2026-08-12T00:00:00.0000000Z"
        environmentRoot = $root
        target = "x86_64-pc-windows-msvc"
        tools = @([ordered]@{
            name = "cargo"; path = (Join-Path $root "cargo.exe")
            sha256 = "c" * 64; controlled = $true
        })
        versions = [ordered]@{
            rust = "1.97.1"; cargo = "1.97.1"; python = "3.12.10"
            cmake = "4.3.3"; ninja = "1.13.2"; sevenZip = "26.01"
            msvcTools = "14.44.35207"; windowsSdk = "10.0.26100.0"
            vcpkgScripts = "d" * 40; vcpkgTool = "2026-07-13"
        }
        paths = [ordered]@{
            cargoVendor = (Join-Path $absolute "cargo")
            vcpkgScriptsRoot = (Join-Path $absolute "vcpkg")
            vcpkgInstalled = (Join-Path $absolute "installed")
            ocrModel = (Join-Path $absolute "ocr")
        }
        nativeTree = [ordered]@{ files = 1; sha256 = "e" * 64 }
        cargoSources = [ordered]@{ files = 1; sha256 = "f" * 64 }
    }
    Set-ContractText -Path $stampPath -Value ($stamp | ConvertTo-Json -Depth 16)
    $parsed = Invoke-PrivateCommand -Name "Read-EasyConEnvironmentStamp" `
        -Parameters @{ Path = $stampPath; EnvironmentRoot = $root }
    Assert-Contract (
        $parsed.schemaVersion -eq 2 -and $parsed.environmentSchema -eq 5
    ) "the stamp parser must accept one complete schema-v2 record"

    foreach ($mutation in @("unknown", "string-integer", "damaged")) {
        $value = $stamp | ConvertTo-Json -Depth 16 | ConvertFrom-Json -Depth 16
        switch ($mutation) {
            "unknown" { Add-Member -InputObject $value -NotePropertyName unknown -NotePropertyValue 1 }
            "string-integer" { $value.schemaVersion = "2" }
            "damaged" {
                Set-ContractText -Path $stampPath -Value "{"
                continue
            }
        }
        Set-ContractText -Path $stampPath -Value ($value | ConvertTo-Json -Depth 16)
        Assert-Throws -Pattern "damaged|keys|integer" -Action {
            Invoke-PrivateCommand -Name "Read-EasyConEnvironmentStamp" `
                -Parameters @{ Path = $stampPath; EnvironmentRoot = $root }
        } | Out-Null
    }
    Assert-Throws -Pattern "damaged" -Action {
        Invoke-PrivateCommand -Name "Read-EasyConEnvironmentStamp" `
            -Parameters @{ Path = $stampPath; EnvironmentRoot = $root }
    } | Out-Null

    $tool = Join-Path $root "controlled-tool.exe"
    Set-ContractText -Path $tool -Value "verified-tool"
    $bytes = (Get-Item -LiteralPath $tool).Length
    $hash = Get-ContractFileSha256 -Path $tool
    Invoke-PrivateCommand -Name "Assert-EasyConPinnedFile" -Parameters @{
        Path = $tool; Bytes = $bytes; Sha256 = $hash; Description = "contract tool"
    }
    Set-ContractText -Path $tool -Value "damaged-tool"
    Assert-Throws -Pattern "bytes|SHA-256" -Action {
        Invoke-PrivateCommand -Name "Assert-EasyConPinnedFile" -Parameters @{
            Path = $tool; Bytes = $bytes; Sha256 = $hash; Description = "contract tool"
        }
    } | Out-Null
}

Add-ContractCase -Name "atomic-publication" -CaseMode "Fast" -Action {
    $root = Join-Path $script:temporaryRoot "atomic"
    [System.IO.Directory]::CreateDirectory($root) | Out-Null
    $destination = Join-Path $root "tool.exe"
    Set-ContractText -Path $destination -Value "complete-original"
    $originalHash = Get-ContractFileSha256 -Path $destination
    $content = [System.Text.UTF8Encoding]::new($false).GetBytes("verified-replacement")
    $hash = [Convert]::ToHexString(
        [System.Security.Cryptography.SHA256]::HashData($content)
    ).ToLowerInvariant()
    $materialize = {
        param($temporary)
        [System.IO.File]::WriteAllBytes($temporary, $content)
    }.GetNewClosure()
    Assert-Throws -Pattern "synthetic publication failure" -Action {
        Invoke-PrivateCommand -Name "Publish-EasyConContentFileAtomically" -Parameters @{
            Destination = $destination
            Algorithm = "SHA256"
            Hash = $hash
            Bytes = [long]$content.Length
            TrustedRoot = $root
            Description = "contract atomic file"
            MaterializeAction = $materialize
            MoveAction = { throw [System.IO.IOException]::new("synthetic publication failure") }
            CleanupMaxAttempts = 1
            CleanupRetryMilliseconds = 0
        }
    } | Out-Null
    Assert-Contract (
        (Get-ContractFileSha256 -Path $destination) -ceq $originalHash -and
        @(Get-ChildItem -Force -LiteralPath $root | Where-Object {
            $_.Name -like '*.publish-*'
        }).Count -eq 0
    ) "failed atomic file publication must preserve final content and remove its temporary"

    Invoke-PrivateCommand -Name "Publish-EasyConContentFileAtomically" -Parameters @{
        Destination = $destination
        Algorithm = "SHA256"
        Hash = $hash
        Bytes = [long]$content.Length
        TrustedRoot = $root
        Description = "contract atomic file"
        MaterializeAction = $materialize
        CleanupMaxAttempts = 1
        CleanupRetryMilliseconds = 0
    } | Out-Null
    Assert-Contract ((Get-ContractFileSha256 -Path $destination) -ceq $hash) `
        "successful atomic file publication must expose only verified replacement content"

    $source = Join-Path $root "directory-source"
    $published = Join-Path $root "directory-published"
    [System.IO.Directory]::CreateDirectory($source) | Out-Null
    Set-ContractText -Path (Join-Path $source "complete.txt") -Value "complete"
    Assert-Throws -Pattern "synthetic directory publish" -Action {
        Invoke-PrivateCommand -Name "Publish-EasyConDirectoryAtomically" -Parameters @{
            Source = $source
            Destination = $published
            TrustedRoot = $root
            MaxAttempts = 1
            RetryMilliseconds = 0
            MoveAction = {
                param($publishSource, $publishDestination)
                $null = $publishSource
                [System.IO.Directory]::CreateDirectory($publishDestination) | Out-Null
                Set-Content -LiteralPath (Join-Path $publishDestination "partial.txt") `
                    -Value "partial" -NoNewline
                throw [System.IO.IOException]::new("synthetic directory publish")
            }
        }
    } | Out-Null
    Assert-Contract (
        (Test-Path -LiteralPath $source -PathType Container) -and
        -not (Test-Path -LiteralPath $published)
    ) "directory publication failure must retain staging and roll back partial destination"
}

Add-ContractCase -Name "transport-policy" -CaseMode "Fast" -Action {
    $root = Join-Path $script:temporaryRoot "transport/cache"
    $content = [System.Text.UTF8Encoding]::new($false).GetBytes("transport-content")
    $hash = [Convert]::ToHexString(
        [System.Security.Cryptography.SHA256]::HashData($content)
    ).ToLowerInvariant()
    $state = [pscustomobject]@{ Downloads = 0 }
    $download = {
        param($url, $destination)
        Assert-Contract ($url -ceq "https://example.invalid/contract.bin") `
            "transport download seam must receive the fixed HTTPS URL"
        $state.Downloads++
        [System.IO.File]::WriteAllBytes($destination, $content)
    }.GetNewClosure()
    $parameters = @{
        SharedCacheRoot = $root
        Url = "https://example.invalid/contract.bin"
        Algorithm = "SHA256"
        Hash = $hash
        Bytes = [long]$content.Length
        Description = "contract transport asset"
        DownloadAction = $download
    }
    $first = Invoke-PrivateCommand -Name "Get-EasyConSharedContentAsset" `
        -Parameters $parameters
    $blocked = $parameters.Clone()
    $blocked.DownloadAction = { throw "cache hit must not invoke transport" }
    $second = Invoke-PrivateCommand -Name "Get-EasyConSharedContentAsset" `
        -Parameters $blocked
    Assert-Contract (
        $first -ceq $second -and $state.Downloads -eq 1
    ) "verified content cache hits must not invoke transport"
    Set-ContractText -Path $first -Value "damaged"
    $third = Invoke-PrivateCommand -Name "Get-EasyConSharedContentAsset" `
        -Parameters $parameters
    Assert-Contract (
        $third -ceq $first -and $state.Downloads -eq 2 -and
        (Get-ContractFileSha256 -Path $third) -ceq $hash
    ) "damaged cache content must be isolated and recovered through the fixed transport seam"
    $insecure = $parameters.Clone()
    $insecure.Url = "http://example.invalid/contract.bin"
    Assert-Throws -Pattern "absolute HTTPS" -Action {
        Invoke-PrivateCommand -Name "Get-EasyConSharedContentAsset" -Parameters $insecure
    } | Out-Null
}

Add-ContractCase -Name "msvc-environment-seam" -CaseMode "Fast" -Action {
    $snapshot = Get-ContractEnvironmentSnapshot
    try {
        $installation = Join-Path $script:temporaryRoot "msvc/VisualStudio"
        $vswhere = Join-Path $script:temporaryRoot "msvc/vswhere.exe"
        Set-ContractText -Path $vswhere -Value "fixture"
        Set-ContractText -Path (
            Join-Path $installation "Common7/Tools/Microsoft.VisualStudio.DevShell.dll"
        ) -Value "fixture"
        foreach ($name in @("cl.exe", "link.exe", "lib.exe", "rc.exe", "mt.exe")) {
            Set-ContractText -Path (Join-Path $installation $name) -Value $name
        }
        $env:EASYCON_CONTRACT_MSVC_INSTALLATION = $installation
        $env:EASYCON_CONTRACT_MSVC_CAPTURES = "0"
        $env:EASYCON_CONTRACT_MSVC_LOADS = "0"
        $loader = {
            param($root)
            Assert-Contract ($root -ceq $env:EASYCON_CONTRACT_MSVC_INSTALLATION) `
                "MSVC loader must receive the discovered installation"
            $env:EASYCON_CONTRACT_MSVC_LOADS = [string]([int]$env:EASYCON_CONTRACT_MSVC_LOADS + 1)
            foreach ($name in @(
                "INCLUDE", "LIB", "LIBPATH", "VCINSTALLDIR", "VCToolsInstallDir",
                "VSINSTALLDIR", "WindowsSdkDir", "UniversalCRTSdkDir", "UCRTVersion"
            )) {
                [Environment]::SetEnvironmentVariable($name, $root, "Process")
            }
            $env:WindowsSDKVersion = "10.0.26100.0\"
            $env:VCToolsVersion = "14.44.35207\"
            $env:VSCMD_ARG_HOST_ARCH = "x64"
            $env:VSCMD_ARG_TGT_ARCH = "x64"
        }
        $resolver = {
            param($name)
            return Join-Path $env:EASYCON_CONTRACT_MSVC_INSTALLATION $name
        }
        $capture = {
            param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)
            $null = $Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput
            $env:EASYCON_CONTRACT_MSVC_CAPTURES = [string](
                [int]$env:EASYCON_CONTRACT_MSVC_CAPTURES + 1
            )
            return @($env:EASYCON_CONTRACT_MSVC_INSTALLATION)
        }
        $parameters = @{
            VsWherePath = $vswhere
            MsvcToolsVersion = "14.44.35207"
            WindowsSdkVersion = "10.0.26100.0"
            DevShellLoader = $loader
            CommandResolver = $resolver
        }
        $first = Invoke-PrivateCommandWithOverride -Name "Initialize-EasyConMsvcEnvironment" `
            -Parameters $parameters -OverrideName "Invoke-EasyConNativeCapture" `
            -Override $capture
        Invoke-PrivateCommand -Name "Set-EasyConVerifiedProcessEnvironment" -Parameters @{
            PathDirectories = @($installation)
            Variables = [ordered]@{ EASYCON_CONTRACT_CONTROLLED = "1" }
        }
        $second = Invoke-PrivateCommandWithOverride -Name "Initialize-EasyConMsvcEnvironment" `
            -Parameters $parameters -OverrideName "Invoke-EasyConNativeCapture" `
            -Override $capture
        Assert-Contract (
            $first.InstallationPath -ceq $installation -and
            $second.InstallationPath -ceq $installation -and
            [int]$env:EASYCON_CONTRACT_MSVC_CAPTURES -eq 2 -and
            [int]$env:EASYCON_CONTRACT_MSVC_LOADS -eq 2 -and
            $env:VSCMD_ARG_TGT_ARCH -ceq "x64"
        ) "MSVC discovery must deterministically rebuild the pinned x64 environment after sanitization"
    }
    finally {
        Restore-ContractEnvironment -Snapshot $snapshot
    }
}

Add-ContractCase -Name "lifecycle-state-restore" -CaseMode "Fast" -Action {
    $cache = Join-Path $script:temporaryRoot "lifecycle/cache"
    $location = Get-ContractEnvironmentLocation -RepositoryRoot $script:repositoryRoot `
        -CacheRoot $cache
    $marker = Join-Path $location.EnvironmentRoot "contract-ready.txt"
    $events = [System.Collections.Generic.List[string]]::new()
    $setup = {
        $events.Add("setup") | Out-Null
        [System.IO.Directory]::CreateDirectory($location.EnvironmentRoot) | Out-Null
        Set-ContractText -Path $marker -Value "ready"
    }.GetNewClosure()
    $verify = {
        $events.Add("verify") | Out-Null
        $env:EASYCON_CONTRACT_LIFECYCLE_MUTATION = "verify"
        if (
            -not (Test-Path -LiteralPath $marker -PathType Leaf) -or
            (Get-Content -Raw -LiteralPath $marker) -cne "ready"
        ) {
            throw "synthetic environment mismatch"
        }
        return [pscustomobject]@{ status = "ready" }
    }.GetNewClosure()
    $workspace = {
        param($summary)
        Assert-Contract ($summary.status -ceq "ready") `
            "Workspace action must receive the verified summary"
        $events.Add("workspace") | Out-Null
        $env:EASYCON_CONTRACT_LIFECYCLE_MUTATION = "workspace"
    }.GetNewClosure()
    $snapshot = Get-ContractEnvironmentSnapshot

    Invoke-PrivateCommand -Name "Invoke-EasyConEnvironmentLifecycle" -Parameters @{
        Mode = "Setup"; Location = $location; SetupAction = $setup
        VerifyAction = $verify; WorkspaceAction = $workspace; LeaseTimeoutMilliseconds = 0
    } | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "setup", "verify") `
        -Description "first Setup must verify, prepare, and verify the published state"
    Assert-ContractEnvironmentMatches -Expected $snapshot `
        -Description "successful Setup lifecycle"

    $events.Clear()
    Invoke-PrivateCommand -Name "Invoke-EasyConEnvironmentLifecycle" -Parameters @{
        Mode = "Setup"; Location = $location; SetupAction = $setup
        VerifyAction = $verify; WorkspaceAction = $workspace; LeaseTimeoutMilliseconds = 0
    } | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify") `
        -Description "already-ready Setup must not provision"

    $events.Clear()
    Invoke-PrivateCommand -Name "Invoke-EasyConEnvironmentLifecycle" -Parameters @{
        Mode = "Workspace"; Location = $location; SetupAction = $setup
        VerifyAction = $verify; WorkspaceAction = $workspace; LeaseTimeoutMilliseconds = 0
    } | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "workspace") `
        -Description "Workspace must verify before its action"
    Assert-ContractEnvironmentMatches -Expected $snapshot `
        -Description "successful Workspace lifecycle"

    Assert-Throws -Pattern "synthetic lifecycle failure" -Action {
        Invoke-PrivateCommand -Name "Invoke-EasyConEnvironmentLifecycle" -Parameters @{
            Mode = "Verify"
            Location = $location
            SetupAction = $setup
            VerifyAction = {
                $env:EASYCON_CONTRACT_LIFECYCLE_MUTATION = "failure"
                throw "synthetic lifecycle failure"
            }
            WorkspaceAction = $workspace
            LeaseTimeoutMilliseconds = 0
        }
    } | Out-Null
    Assert-ContractEnvironmentMatches -Expected $snapshot `
        -Description "failing Verify lifecycle"

    $lease = Invoke-PrivateCommand -Name "Enter-EasyConEnvironmentLease" -Parameters @{
        Location = $location; Access = "Exclusive"; TimeoutMilliseconds = 0; RetryMilliseconds = 0
    }
    try {
        $calls = [pscustomobject]@{ Verify = 0 }
        Assert-Throws -Pattern "busy|ownership" -Action {
            Invoke-PrivateCommand -Name "Invoke-EasyConEnvironmentLifecycle" -Parameters @{
                Mode = "Verify"
                Location = $location
                SetupAction = $setup
                VerifyAction = { $calls.Verify++; return "unexpected" }.GetNewClosure()
                WorkspaceAction = $workspace
                LeaseTimeoutMilliseconds = 0
            }
        } | Out-Null
        Assert-Contract ($calls.Verify -eq 0) `
            "lifecycle ownership contention must fail before verification"
    }
    finally {
        $lease.Dispose()
    }
}

# Qualification groups are registered below. Their names are the only public selection surface.
Add-ContractCase -Name "real-worktree-concurrency" -CaseMode "Qualification" -Action {
    $git = Get-ContractApplicationPath -Name "git.exe"
    $fixtureVolume = [System.IO.Path]::GetPathRoot($script:temporaryRoot)
    $fixtureRoot = Join-Path $fixtureVolume (
        "ecq-{0}" -f [guid]::NewGuid().ToString("N").Substring(0, 8)
    )
    $checkoutRoot = Join-Path $fixtureRoot "g"
    $cache = Join-Path $fixtureRoot "c"
    $probeRoot = Join-Path $fixtureRoot "p"
    $firstWorktree = Join-Path $checkoutRoot "1"
    $secondWorktree = Join-Path $checkoutRoot "2"
    $fixedSha = (Invoke-ContractGit -WorkingDirectory $script:repositoryRoot `
        -Arguments @("rev-parse", "HEAD")).Output[0].Trim()
    $fixedTree = (Invoke-ContractGit -WorkingDirectory $script:repositoryRoot `
        -Arguments @("rev-parse", "HEAD^{tree}")).Output[0].Trim()
    $createdWorktrees = [System.Collections.Generic.List[string]]::new()
    $probes = [System.Collections.Generic.List[object]]::new()
    $primaryFailure = $null
    try {
        [System.IO.Directory]::CreateDirectory($checkoutRoot) | Out-Null
        foreach ($worktree in @($firstWorktree, $secondWorktree)) {
            & $git -C $script:repositoryRoot worktree add --detach $worktree $fixedSha | Out-Null
            Assert-Contract ($LASTEXITCODE -eq 0) `
                "qualification must create each detached fixed-SHA worktree"
            $createdWorktrees.Add($worktree) | Out-Null
            $head = (Invoke-ContractGit -WorkingDirectory $worktree `
                -Arguments @("rev-parse", "HEAD")).Output[0].Trim()
            $status = (Invoke-ContractGit -WorkingDirectory $worktree `
                -Arguments @("status", "--porcelain=v1", "--untracked-files=all")).Output
            Assert-Contract ($head -ceq $fixedSha -and $status.Count -eq 0) `
                "each qualification worktree must begin as one clean fixed-SHA checkout"
        }

        $configuration = Invoke-PrivateCommand -Name "Get-EasyConWindowsBuildConfiguration" `
            -Parameters @{ Path = $script:configurationPath }
        $firstFingerprint = Invoke-PrivateCommand -Name "Get-EasyConEnvironmentFingerprint" `
            -Parameters @{ RepositoryRoot = $firstWorktree; Configuration = $configuration }
        $secondFingerprint = Invoke-PrivateCommand -Name "Get-EasyConEnvironmentFingerprint" `
            -Parameters @{ RepositoryRoot = $secondWorktree; Configuration = $configuration }
        Assert-Contract ($firstFingerprint.Value -ceq $secondFingerprint.Value) `
            "clean fixed-SHA worktrees must calculate one environment fingerprint"
        $firstLocation = Invoke-PrivateCommand -Name "Get-EasyConEnvironmentLocation" `
            -Parameters @{
                RepositoryRoot = $firstWorktree
                Fingerprint = $firstFingerprint.Value
                Configuration = $configuration
                CacheRoot = $cache
            }
        $secondLocation = Invoke-PrivateCommand -Name "Get-EasyConEnvironmentLocation" `
            -Parameters @{
                RepositoryRoot = $secondWorktree
                Fingerprint = $secondFingerprint.Value
                Configuration = $configuration
                CacheRoot = $cache
            }
        Assert-Contract (
            $firstLocation.EnvironmentRoot -ceq $secondLocation.EnvironmentRoot -and
            $firstLocation.LockPath -ceq $secondLocation.LockPath -and
            $firstLocation.WorkspaceRoot -cne $secondLocation.WorkspaceRoot
        ) "qualification worktrees must share prepared identity and isolate writable roots"

        $preparedRoot = Join-Path $firstLocation.EnvironmentRoot "setup/vcpkg/scripts"
        $preparedTool = Join-Path $firstLocation.EnvironmentRoot "tools/vcpkg.cmd"
        $installedRoot = Join-Path $firstLocation.EnvironmentRoot "setup/vcpkg/installed"
        foreach ($relative in @(
            ".vcpkg-root",
            "bootstrap-vcpkg.bat",
            "bootstrap-vcpkg.sh",
            "scripts/buildsystems/vcpkg.cmake"
        )) {
            Set-ContractText -Path (Join-Path $preparedRoot $relative) `
                -Value "qualification $relative`n"
        }
        Invoke-ContractGit -WorkingDirectory $preparedRoot -Arguments @("init", "--quiet") | Out-Null
        Invoke-ContractGit -WorkingDirectory $preparedRoot `
            -Arguments @("config", "user.email", "contract@example.invalid") | Out-Null
        Invoke-ContractGit -WorkingDirectory $preparedRoot `
            -Arguments @("config", "user.name", "EasyCon Qualification") | Out-Null
        Invoke-ContractGit -WorkingDirectory $preparedRoot -Arguments @("add", "--all") | Out-Null
        Invoke-ContractGit -WorkingDirectory $preparedRoot `
            -Arguments @("commit", "--quiet", "-m", "prepared vcpkg fixture") | Out-Null
        $preparedCommit = (Invoke-ContractGit -WorkingDirectory $preparedRoot `
            -Arguments @("rev-parse", "HEAD")).Output[0].Trim()
        $toolRelease = "2026-01-01"
        $toolCommit = "2" * 40
        Set-ContractText -Path $preparedTool -Value (
            "@echo off`r`necho vcpkg package management program version " +
            "$toolRelease-$toolCommit`r`n"
        )
        [System.IO.Directory]::CreateDirectory($installedRoot) | Out-Null
        Set-ContractText -Path (Join-Path $installedRoot "installed.txt") -Value "installed`n"
        $toolItem = Get-Item -LiteralPath $preparedTool
        $contractConfiguration = $configuration | ConvertTo-Json -Depth 64 | `
            ConvertFrom-Json -Depth 64
        $contractConfiguration.vcpkg.scriptsCommit = $preparedCommit
        $contractConfiguration.vcpkg.toolRelease = $toolRelease
        $contractConfiguration.vcpkg.toolCommit = $toolCommit
        $contractConfiguration.vcpkg.windowsAsset.bytes = [long]$toolItem.Length
        $contractConfiguration.vcpkg.windowsAsset.sha256 = `
            Get-ContractFileSha256 -Path $preparedTool
        $contractConfigurationPath = Join-Path $probeRoot "configuration.json"
        Set-ContractText -Path $contractConfigurationPath `
            -Value ($contractConfiguration | ConvertTo-Json -Depth 64)

        $probeScript = Join-Path $probeRoot "verify-probe.ps1"
        Set-ContractText -Path $probeScript -Value @'
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ModulePath,
    [Parameter(Mandatory)][string]$RepositoryRoot,
    [Parameter(Mandatory)][string]$LocationPath,
    [Parameter(Mandatory)][string]$ConfigurationPath,
    [Parameter(Mandatory)][string]$VcpkgRoot,
    [Parameter(Mandatory)][string]$VcpkgExecutable,
    [Parameter(Mandatory)][string]$InstalledRoot,
    [Parameter(Mandatory)][string]$FixedSha,
    [Parameter(Mandatory)][string]$AcquiredMarker,
    [Parameter(Mandatory)][string]$StartMarker,
    [Parameter(Mandatory)][string]$ReadyMarker,
    [Parameter(Mandatory)][string]$ReleaseMarker,
    [Parameter(Mandatory)][string]$TracePath,
    [Parameter(Mandatory)][string]$ResultPath
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
function Write-Trace([string]$Milestone) {
    [System.IO.File]::AppendAllText(
        $TracePath,
        ([DateTimeOffset]::UtcNow.ToString("O") + "|" + $Milestone + "`n"),
        [System.Text.UTF8Encoding]::new($false)
    )
}
function Wait-Marker([string]$Path) {
    $parent = [System.IO.Path]::GetDirectoryName([System.IO.Path]::GetFullPath($Path))
    $watcher = [System.IO.FileSystemWatcher]::new($parent, [System.IO.Path]::GetFileName($Path))
    try {
        $watcher.EnableRaisingEvents = $true
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
            $change = $watcher.WaitForChanged(
                [System.IO.WatcherChangeTypes]::Created -bor
                    [System.IO.WatcherChangeTypes]::Changed -bor
                    [System.IO.WatcherChangeTypes]::Renamed,
                60000
            )
            if ($change.TimedOut -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
                throw "qualification marker timed out: $Path"
            }
        }
    }
    finally { $watcher.Dispose() }
}
$module = @(Import-Module -Name $ModulePath -Force -PassThru)
if ($module.Count -ne 1) { throw "probe must import exactly one workspace module" }
$location = Get-Content -Raw -LiteralPath $LocationPath | ConvertFrom-Json -Depth 32
$configuration = Get-Content -Raw -LiteralPath $ConfigurationPath | ConvertFrom-Json -Depth 64
$verify = {
    [System.IO.File]::WriteAllText(
        $AcquiredMarker,
        "acquired",
        [System.Text.UTF8Encoding]::new($false)
    )
    Write-Trace "leases.acquired"
    Wait-Marker $StartMarker
    Write-Trace "start.observed"
    $git = [string](
        Get-Command git.exe -CommandType Application -All -ErrorAction Stop |
            Select-Object -First 1 -ExpandProperty Source
    )
    if (
        -not [System.IO.Path]::IsPathFullyQualified($git) -or
        -not (Test-Path -LiteralPath $git -PathType Leaf)
    ) {
        throw "qualification child Git did not resolve to one existing absolute path"
    }
    $head = @(& $git -C $RepositoryRoot rev-parse HEAD)
    $status = @(& $git -C $RepositoryRoot status --porcelain=v1 --untracked-files=all)
    if ($LASTEXITCODE -ne 0 -or $head.Count -ne 1 -or $head[0] -cne $FixedSha -or $status.Count -ne 0) {
        throw "real Verify checkout changed before layout verification"
    }
    $verifiedVcpkg = & $module[0] {
        param($Root, $Config, $Executable, $TrustedRoot)
        Assert-EasyConVcpkgCheckout -VcpkgRoot $Root -Configuration $Config `
            -VcpkgExecutable $Executable -TrustedRoot $TrustedRoot
    } $VcpkgRoot $configuration $VcpkgExecutable $location.EnvironmentRoot
    $layout = & $module[0] {
        param($Vcpkg, $Repository, $Location, $Installed)
        New-EasyConVcpkgWorkspaceLayout -Vcpkg $Vcpkg `
            -RepositoryRoot $Repository -WorkspaceRoot $Location.WorkspaceRoot `
            -CacheRoot $Location.CacheRoot -EnvironmentRoot $Location.EnvironmentRoot `
            -InstalledRoot $Installed
    } $verifiedVcpkg $RepositoryRoot $location $InstalledRoot
    Write-Trace "verification.completed"
    [System.IO.File]::WriteAllText(
        $ReadyMarker,
        "ready",
        [System.Text.UTF8Encoding]::new($false)
    )
    Write-Trace "ready.written"
    Wait-Marker $ReleaseMarker
    Write-Trace "release.observed"
    return [pscustomobject]@{
        status = "ready"
        environmentRoot = $location.EnvironmentRoot
        workspaceRoot = $location.WorkspaceRoot
        vcpkgRoot = $layout.Root
        toolchain = $layout.Toolchain
        manifestRoot = $layout.ManifestRoot
    }
}.GetNewClosure()
$result = & $module[0] {
    param($OwnedLocation, $VerifyAction)
    Invoke-EasyConEnvironmentLifecycle -Mode Verify -Location $OwnedLocation `
        -SetupAction { throw "Verify cannot prepare" } -VerifyAction $VerifyAction `
        -WorkspaceAction { param($Summary) $null = $Summary } `
        -LeaseTimeoutMilliseconds 30000
} $location $verify
[System.IO.File]::WriteAllText(
    $ResultPath,
    ($result | ConvertTo-Json -Depth 16),
    [System.Text.UTF8Encoding]::new($false)
)
Write-Trace "result.written"
'@

        foreach ($entry in @(
            [pscustomobject]@{
                Name = "one"; Repository = $firstWorktree; Location = $firstLocation
            },
            [pscustomobject]@{
                Name = "two"; Repository = $secondWorktree; Location = $secondLocation
            }
        )) {
            $locationPath = Join-Path $probeRoot "$($entry.Name)-location.json"
            Set-ContractText -Path $locationPath `
                -Value ($entry.Location | ConvertTo-Json -Depth 16)
            $acquired = Join-Path $probeRoot "$($entry.Name)-acquired.txt"
            $start = Join-Path $probeRoot "$($entry.Name)-start.txt"
            $ready = Join-Path $probeRoot "$($entry.Name)-ready.txt"
            $trace = Join-Path $probeRoot "$($entry.Name)-trace.txt"
            $result = Join-Path $probeRoot "$($entry.Name)-result.json"
            $probe = Start-ContractProcess -Name $entry.Name -ScriptPath $probeScript `
                -Arguments @(
                    "-ModulePath", $script:modulePath,
                    "-RepositoryRoot", $entry.Repository,
                    "-LocationPath", $locationPath,
                    "-ConfigurationPath", $contractConfigurationPath,
                    "-VcpkgRoot", $preparedRoot,
                    "-VcpkgExecutable", $preparedTool,
                    "-InstalledRoot", $installedRoot,
                    "-FixedSha", $fixedSha,
                    "-AcquiredMarker", $acquired,
                    "-StartMarker", $start,
                    "-ReadyMarker", $ready,
                    "-ReleaseMarker", (Join-Path $probeRoot "release.txt"),
                    "-TracePath", $trace,
                    "-ResultPath", $result
                )
            Add-Member -InputObject $probe -NotePropertyName Location `
                -NotePropertyValue $entry.Location
            Add-Member -InputObject $probe -NotePropertyName Acquired `
                -NotePropertyValue $acquired
            Add-Member -InputObject $probe -NotePropertyName Start `
                -NotePropertyValue $start
            Add-Member -InputObject $probe -NotePropertyName Ready `
                -NotePropertyValue $ready
            Add-Member -InputObject $probe -NotePropertyName Trace `
                -NotePropertyValue $trace
            Add-Member -InputObject $probe -NotePropertyName Result `
                -NotePropertyValue $result
            $probes.Add($probe) | Out-Null
        }

        foreach ($probe in $probes) {
            Wait-ContractFile -Path $probe.Acquired -TimeoutMilliseconds 30000
        }
        $readinessTimer = [System.Diagnostics.Stopwatch]::StartNew()
        Set-ContractText -Path $probes[0].Start -Value "start"
        Assert-Contract (
            -not (Test-Path -LiteralPath $probes[1].Start) -and
            -not (Test-Path -LiteralPath $probes[1].Ready)
        ) "startup-skew probe two must remain parent-gated before its start marker"
        Set-ContractText -Path $probes[1].Start -Value "start"
        foreach ($probe in $probes) {
            Wait-ContractFile -Path $probe.Ready -TimeoutMilliseconds 60000
        }
        $readinessTimer.Stop()

        foreach ($probe in $probes) {
            $milestones = @(Get-Content -LiteralPath $probe.Trace | ForEach-Object {
                if ($_ -match '^[^|]+\|(?<Milestone>.+)$') { $Matches.Milestone }
            })
            $verificationIndex = [array]::IndexOf($milestones, "verification.completed")
            $readyIndex = [array]::IndexOf($milestones, "ready.written")
            Assert-Contract (
                $verificationIndex -ge 0 -and
                $readyIndex -gt $verificationIndex -and
                $milestones -cnotcontains "release.observed" -and
                -not $probe.Process.HasExited
            ) (
                "concurrent Verify Ready must follow checkout/layout verification and precede " +
                "release; probe=$($probe.Name) milestones=$($milestones -join ',')"
            )
        }

        $environmentLeaseFailure = Assert-Throws -Pattern "busy|ownership" -Action {
            Invoke-PrivateCommand -Name "Enter-EasyConEnvironmentLease" -Parameters @{
                Location = $firstLocation
                Access = "Exclusive"
                TimeoutMilliseconds = 0
                RetryMilliseconds = 0
            }
        }
        Assert-Contract ($null -ne $environmentLeaseFailure) `
            "ready probes must keep the shared environment lease occupied"
        foreach ($probe in $probes) {
            Assert-Throws -Pattern "busy|ownership" -Action {
                Invoke-PrivateCommand -Name "Enter-EasyConWorkspaceLease" -Parameters @{
                    Location = $probe.Location
                    TimeoutMilliseconds = 0
                    RetryMilliseconds = 0
                }
            } | Out-Null
        }
        Assert-Throws -Pattern "busy|ownership" -Action {
            Invoke-PrivateCommand -Name "Enter-EasyConSharedCacheLease" -Parameters @{
                CacheRoot = $cache
                Access = "Exclusive"
                TimeoutMilliseconds = 0
                RetryMilliseconds = 0
            }
        } | Out-Null

        Set-ContractText -Path (Join-Path $probeRoot "release.txt") -Value "release"
        Wait-ContractProcessesExited -Probe $probes.ToArray() -TimeoutMilliseconds 60000
        foreach ($probe in $probes) {
            Assert-Contract (
                $probe.Process.ExitCode -eq 0 -and
                (Test-Path -LiteralPath $probe.Result -PathType Leaf)
            ) (
                "real Verify probe must exit successfully after release; " +
                (Get-ContractProcessDiagnostics -Probe $probes.ToArray())
            )
            $result = Get-Content -Raw -LiteralPath $probe.Result | ConvertFrom-Json -Depth 16
            $milestones = @(Get-Content -LiteralPath $probe.Trace | ForEach-Object {
                if ($_ -match '^[^|]+\|(?<Milestone>.+)$') { $Matches.Milestone }
            })
            Assert-Contract (
                $result.status -ceq "ready" -and
                $result.environmentRoot -ceq $firstLocation.EnvironmentRoot -and
                $result.workspaceRoot -ceq $probe.Location.WorkspaceRoot -and
                (Test-Path -LiteralPath $result.vcpkgRoot -PathType Container) -and
                (Test-Path -LiteralPath $result.toolchain -PathType Leaf) -and
                $milestones -ccontains "release.observed"
            ) "released Verify probe must report its verified, isolated workspace layout"
        }
        Assert-Contract (
            $probes[0].Location.WorkspaceRoot -cne $probes[1].Location.WorkspaceRoot -and
            $readinessTimer.ElapsedMilliseconds -lt 60000
        ) "real concurrent Verify must retain isolated layouts within the bounded observation"

        foreach ($worktree in @($firstWorktree, $secondWorktree)) {
            $head = (Invoke-ContractGit -WorkingDirectory $worktree `
                -Arguments @("rev-parse", "HEAD")).Output[0].Trim()
            $tree = (Invoke-ContractGit -WorkingDirectory $worktree `
                -Arguments @("rev-parse", "HEAD^{tree}")).Output[0].Trim()
            $status = (Invoke-ContractGit -WorkingDirectory $worktree `
                -Arguments @("status", "--porcelain=v1", "--untracked-files=all")).Output
            Assert-Contract (
                $head -ceq $fixedSha -and $tree -ceq $fixedTree -and $status.Count -eq 0
            ) "qualification layout creation must leave each fixed-SHA checkout clean"
        }
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        $cleanupIssues = [System.Collections.Generic.List[string]]::new()
        try {
            Stop-ContractProcesses -Probe $probes.ToArray() -TimeoutMilliseconds 30000
        }
        catch {
            $cleanupIssues.Add($_.Exception.Message) | Out-Null
        }
        $cleanupWorktrees = @($createdWorktrees.ToArray())
        [array]::Reverse($cleanupWorktrees)
        foreach ($worktree in $cleanupWorktrees) {
            try {
                & $git -C $script:repositoryRoot worktree remove $worktree | Out-Null
                if ($LASTEXITCODE -ne 0) {
                    throw "Git worktree remove failed with exit code $LASTEXITCODE"
                }
            }
            catch {
                $cleanupIssues.Add("worktree ${worktree}: $($_.Exception.Message)") | Out-Null
            }
        }
        try {
            $resolvedRoot = [System.IO.Path]::GetFullPath($fixtureRoot)
            $parent = [System.IO.Path]::GetDirectoryName($resolvedRoot)
            Assert-Contract (
                $parent.TrimEnd('\') -ceq $fixtureVolume.TrimEnd('\') -and
                [System.IO.Path]::GetFileName($resolvedRoot) -cmatch '^ecq-[0-9a-f]{8}$'
            ) "qualification fixture root must remain a direct named child of its volume root"
            if (Test-Path -LiteralPath $resolvedRoot) {
                $item = Get-Item -Force -LiteralPath $resolvedRoot
                Assert-Contract (
                    ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0
                ) "qualification fixture root must not be a reparse point"
                Remove-Item -LiteralPath $resolvedRoot -Recurse -Force -ErrorAction Stop
            }
        }
        catch {
            $cleanupIssues.Add("fixture root: $($_.Exception.Message)") | Out-Null
        }
        if ($cleanupIssues.Count -ne 0) {
            if ($null -ne $primaryFailure) {
                $primaryFailure.Exception.Data["EasyConQualificationCleanupFailure"] = `
                    ($cleanupIssues -join [Environment]::NewLine)
            }
            else {
                throw "qualification cleanup failed: $($cleanupIssues -join '; ')"
            }
        }
    }
}

Add-ContractCase -Name "process-lifecycle" -CaseMode "Qualification" -Action {
    $root = Join-Path $script:temporaryRoot "process-lifecycle"
    $probeScript = Join-Path $root "environment-probe.ps1"
    Set-ContractText -Path $probeScript -Value @'
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ModulePath,
    [Parameter(Mandatory)][string]$ResultPath,
    [Parameter(Mandatory)][ValidateSet("Sanitized", "EarlyExit")][string]$ProbeMode
)
$ErrorActionPreference = "Stop"
if ($ProbeMode -ceq "EarlyExit") {
    [Console]::Error.WriteLine("qualification early exit before result")
    exit 41
}
$module = @(Import-Module -Name $ModulePath -Force -PassThru)
if ($module.Count -ne 1) { throw "probe must import exactly one module" }
$systemRoot = [Environment]::GetEnvironmentVariable("SystemRoot", "Process")
& $module[0] {
    param($Root)
    Set-EasyConVerifiedProcessEnvironment `
        -PathDirectories @($Root, (Join-Path $Root "System32")) `
        -Variables ([ordered]@{
            CC = (Join-Path $Root "System32/cmd.exe")
            EASYCON_CONTRACT_CHILD = "controlled"
        })
} $systemRoot
$record = [ordered]@{
    child = $env:EASYCON_CONTRACT_CHILD
    cc = $env:CC
    rustcWrapper = $env:RUSTC_WRAPPER
    proxy = $env:HTTPS_PROXY
    path = $env:PATH
}
[System.IO.File]::WriteAllText(
    $ResultPath,
    ($record | ConvertTo-Json -Compress),
    [System.Text.UTF8Encoding]::new($false)
)
'@
    $resultPath = Join-Path $root "sanitized.json"
    $environmentSnapshot = Get-ContractEnvironmentSnapshot
    $env:RUSTC_WRAPPER = "poisoned-wrapper"
    $env:HTTPS_PROXY = "https://poisoned.invalid"
    $probes = [System.Collections.Generic.List[object]]::new()
    $primaryFailure = $null
    try {
        $probes.Add((Start-ContractProcess -Name "sanitized" -ScriptPath $probeScript `
            -Arguments @(
                "-ModulePath", $script:modulePath,
                "-ResultPath", $resultPath,
                "-ProbeMode", "Sanitized"
            ))) | Out-Null
        $probes.Add((Start-ContractProcess -Name "early-exit" -ScriptPath $probeScript `
            -Arguments @(
                "-ModulePath", $script:modulePath,
                "-ResultPath", (Join-Path $root "must-not-exist.json"),
                "-ProbeMode", "EarlyExit"
            ))) | Out-Null
        Wait-ContractProcessesExited -Probe $probes.ToArray() -TimeoutMilliseconds 30000
        Assert-Contract (
            $probes[0].Process.ExitCode -eq 0 -and
            $probes[1].Process.ExitCode -eq 41 -and
            $probes[1].Error -match "early exit before result" -and
            -not (Test-Path -LiteralPath (Join-Path $root "must-not-exist.json"))
        ) (
            "qualification must preserve sanitized success and early-exit diagnostics; " +
            (Get-ContractProcessDiagnostics -Probe $probes.ToArray())
        )
        $record = Get-Content -Raw -LiteralPath $resultPath | ConvertFrom-Json
        Assert-Contract (
            $record.child -ceq "controlled" -and
            $record.cc -match 'System32[\\/]cmd\.exe$' -and
            [string]::IsNullOrEmpty([string]$record.rustcWrapper) -and
            [string]::IsNullOrEmpty([string]$record.proxy) -and
            $record.path -notmatch 'poisoned'
        ) "real child verification must remove ambient compiler and transport contamination"
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        $cleanupIssues = [System.Collections.Generic.List[string]]::new()
        try {
            if ($probes.Count -ne 0) {
                Stop-ContractProcesses -Probe $probes.ToArray() -TimeoutMilliseconds 30000
            }
        }
        catch {
            $cleanupIssues.Add("processes: $($_.Exception.Message)") | Out-Null
        }
        try {
            Restore-ContractEnvironment -Snapshot $environmentSnapshot
        }
        catch {
            $cleanupIssues.Add("environment: $($_.Exception.Message)") | Out-Null
        }
        if ($cleanupIssues.Count -ne 0) {
            if ($null -ne $primaryFailure) {
                $primaryFailure.Exception.Data["EasyConQualificationProcessCleanupFailure"] = `
                    ($cleanupIssues -join [Environment]::NewLine)
            }
            else {
                throw "qualification process cleanup failed: $($cleanupIssues -join '; ')"
            }
        }
    }
}

Add-ContractCase -Name "cache-lock-download" -CaseMode "Qualification" -Action {
    $root = Join-Path $script:temporaryRoot "cache-lock-download"
    $holderScript = Join-Path $root "lease-holder.ps1"
    $ready = Join-Path $root "ready.txt"
    $release = Join-Path $root "release.txt"
    Set-ContractText -Path $holderScript -Value @'
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ModulePath,
    [Parameter(Mandatory)][string]$CacheRoot,
    [Parameter(Mandatory)][string]$ReadyMarker,
    [Parameter(Mandatory)][string]$ReleaseMarker
)
$ErrorActionPreference = "Stop"
$module = @(Import-Module -Name $ModulePath -Force -PassThru)
$lease = & $module[0] {
    param($Root)
    Enter-EasyConSharedCacheLease -CacheRoot $Root -Access Exclusive `
        -TimeoutMilliseconds 0 -RetryMilliseconds 0
} $CacheRoot
try {
    [System.IO.File]::WriteAllText($ReadyMarker, "ready", [System.Text.UTF8Encoding]::new($false))
    $parent = [System.IO.Path]::GetDirectoryName($ReleaseMarker)
    $watcher = [System.IO.FileSystemWatcher]::new($parent, [System.IO.Path]::GetFileName($ReleaseMarker))
    try {
        $watcher.EnableRaisingEvents = $true
        if (-not (Test-Path -LiteralPath $ReleaseMarker -PathType Leaf)) {
            $change = $watcher.WaitForChanged(
                [System.IO.WatcherChangeTypes]::Created -bor
                    [System.IO.WatcherChangeTypes]::Changed -bor
                    [System.IO.WatcherChangeTypes]::Renamed,
                30000
            )
            if ($change.TimedOut) { throw "lease holder release timed out" }
        }
    }
    finally { $watcher.Dispose() }
}
finally { $lease.Dispose() }
'@
    $probe = Start-ContractProcess -Name "shared-cache-writer" -ScriptPath $holderScript `
        -Arguments @(
            "-ModulePath", $script:modulePath,
            "-CacheRoot", $root,
            "-ReadyMarker", $ready,
            "-ReleaseMarker", $release
        )
    $primaryFailure = $null
    try {
        Wait-ContractFile -Path $ready -TimeoutMilliseconds 30000
        Assert-Throws -Pattern "busy|ownership" -Action {
            Invoke-PrivateCommand -Name "Enter-EasyConSharedCacheLease" -Parameters @{
                CacheRoot = $root
                Access = "Shared"
                TimeoutMilliseconds = 0
                RetryMilliseconds = 0
            }
        } | Out-Null
        Set-ContractText -Path $release -Value "release"
        Wait-ContractProcessesExited -Probe @($probe) -TimeoutMilliseconds 30000
        Assert-Contract ($probe.Process.ExitCode -eq 0) (
            "cross-process shared cache writer must release cleanly; " +
            (Get-ContractProcessDiagnostics -Probe @($probe))
        )

        $content = [System.Text.UTF8Encoding]::new($false).GetBytes("qualification-download")
        $hash = [Convert]::ToHexString(
            [System.Security.Cryptography.SHA256]::HashData($content)
        ).ToLowerInvariant()
        $state = [pscustomobject]@{ Downloads = 0 }
        $download = {
            param($url, $destination)
            Assert-Contract ($url -ceq "https://example.invalid/qualification.bin") `
                "qualification download must keep the fixed HTTPS transport policy"
            $state.Downloads++
            [System.IO.File]::WriteAllBytes($destination, $content)
        }.GetNewClosure()
        $parameters = @{
            SharedCacheRoot = (Join-Path $root "assets")
            Url = "https://example.invalid/qualification.bin"
            Algorithm = "SHA256"
            Hash = $hash
            Bytes = [long]$content.Length
            Description = "qualification download"
            DownloadAction = $download
        }
        $asset = Invoke-PrivateCommand -Name "Get-EasyConSharedContentAsset" `
            -Parameters $parameters
        Set-ContractText -Path $asset -Value "damaged"
        $recovered = Invoke-PrivateCommand -Name "Get-EasyConSharedContentAsset" `
            -Parameters $parameters
        Assert-Contract (
            $asset -ceq $recovered -and $state.Downloads -eq 2 -and
            (Get-ContractFileSha256 -Path $recovered) -ceq $hash
        ) "qualification cache must isolate damage and republish one verified download"
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        $cleanupIssues = [System.Collections.Generic.List[string]]::new()
        try {
            if (-not (Test-Path -LiteralPath $release)) {
                Set-ContractText -Path $release -Value "release"
            }
        }
        catch {
            $cleanupIssues.Add("release: $($_.Exception.Message)") | Out-Null
        }
        try {
            Stop-ContractProcesses -Probe @($probe) -TimeoutMilliseconds 30000
        }
        catch {
            $cleanupIssues.Add("process: $($_.Exception.Message)") | Out-Null
        }
        if ($cleanupIssues.Count -ne 0) {
            if ($null -ne $primaryFailure) {
                $primaryFailure.Exception.Data["EasyConQualificationCacheCleanupFailure"] = `
                    ($cleanupIssues -join [Environment]::NewLine)
            }
            else {
                throw "qualification cache cleanup failed: $($cleanupIssues -join '; ')"
            }
        }
    }
}

Add-ContractCase -Name "junction-cleanup" -CaseMode "Qualification" -Action {
    $root = Join-Path $script:temporaryRoot "junction-cleanup"
    $external = Join-Path $script:temporaryRoot "junction-external"
    $tree = Join-Path $root "buildtrees"
    $junction = Join-Path $tree "port-source"
    [System.IO.Directory]::CreateDirectory($tree) | Out-Null
    [System.IO.Directory]::CreateDirectory($external) | Out-Null
    $externalMarker = Join-Path $external "preserve.txt"
    Set-ContractText -Path $externalMarker -Value "preserve"
    New-Item -ItemType Junction -Path $junction -Target $external | Out-Null
    Invoke-PrivateCommand -Name "Remove-EasyConTransientBuildTree" -Parameters @{
        Path = $tree; TrustedRoot = $root
    }
    Assert-Contract (
        -not (Test-Path -LiteralPath $tree) -and
        (Test-Path -LiteralPath $externalMarker -PathType Leaf)
    ) "transient cleanup must remove a junction without following its external target"

    $lockedTree = Join-Path $root "packages"
    $lockedFile = Join-Path $lockedTree "locked.bin"
    Set-ContractText -Path $lockedFile -Value "locked"
    $handle = [System.IO.File]::Open(
        $lockedFile,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::None
    )
    try {
        Assert-Throws -Pattern "used by another process|cannot access|being used" -Action {
            Invoke-PrivateCommand -Name "Remove-EasyConTransientBuildTree" -Parameters @{
                Path = $lockedTree; TrustedRoot = $root
            }
        } | Out-Null
        Assert-Contract (Test-Path -LiteralPath $lockedTree -PathType Container) `
            "locked cleanup failure must leave explicit residual state"
    }
    finally { $handle.Dispose() }
    Invoke-PrivateCommand -Name "Remove-EasyConTransientBuildTree" -Parameters @{
        Path = $lockedTree; TrustedRoot = $root
    }
    Assert-Contract (-not (Test-Path -LiteralPath $lockedTree)) `
        "cleanup must recover after the real locking handle is released"

    $unknown = Join-Path $root "unknown-reparse"
    New-Item -ItemType Junction -Path $unknown -Target $external | Out-Null
    Assert-Throws -Pattern "reparse point" -Action {
        Invoke-PrivateCommand -Name "Assert-EasyConPhysicalTree" -Parameters @{
            Path = $unknown; TrustedRoot = $root
        }
    } | Out-Null
    Assert-Contract (Test-Path -LiteralPath $externalMarker -PathType Leaf) `
        "unknown reparse rejection must preserve the external target"
    Remove-Item -LiteralPath $unknown -Force
}

$script:contractFailure = $null
try {
    $registered = @($script:contractCases | Where-Object { $_.Mode -ceq $Mode })
    $registeredNames = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($case in $registered) {
        if (-not $registeredNames.Add([string]$case.Name)) {
            throw "selected contract registry contains duplicate exact name '$($case.Name)'"
        }
    }
    $expectedRegistered = if ($Mode -ceq "Fast") { 12 } else { 4 }
    Assert-Contract (
        $registered.Count -eq $expectedRegistered -and
        $registeredNames.Count -eq $expectedRegistered -and
        $script:contractCases.Count -eq 16 -and
        $script:contractCaseNames.Count -eq 16
    ) (
        "contract registry counts must remain fail closed; mode=$Mode " +
        "registered=$($registered.Count) unique=$($registeredNames.Count) " +
        "total=$($script:contractCases.Count) expected=$expectedRegistered"
    )

    if ($script:exactCaseRequested) {
        if ([string]::IsNullOrWhiteSpace($ExactCase)) {
            throw "-ExactCase requires one complete contract group name"
        }
        if (-not $script:contractCaseNames.Contains($ExactCase)) {
            throw "unknown exact contract group '$ExactCase'"
        }
        $registered = @($registered | Where-Object { $_.Name -ceq $ExactCase })
        Assert-Contract ($registered.Count -eq 1) `
            "exact contract group must belong to the selected mode"
    }

    $suiteTimer = [System.Diagnostics.Stopwatch]::StartNew()
    $bootstrapExecuted = $false
    if ($Mode -ceq "Fast" -and -not $script:exactCaseRequested) {
        Invoke-BootstrapContracts
        $bootstrapExecuted = $true
    }
    $executed = 0
    foreach ($case in $registered) {
        $caseTimer = [System.Diagnostics.Stopwatch]::StartNew()
        & $case.Action
        $caseTimer.Stop()
        $executed++
        Write-Output (
            "CONTRACT_CASE_PASS mode={0} name={1} durationMs={2}" -f
                $Mode, $case.Name, $caseTimer.ElapsedMilliseconds
        )
    }
    $suiteTimer.Stop()
    $expectedExecuted = if ($script:exactCaseRequested) { 1 } else { $expectedRegistered }
    Assert-Contract ($executed -eq $expectedExecuted) (
        "contract execution count must match selection; mode=$Mode executed=$executed " +
        "expected=$expectedExecuted"
    )
    $selection = if ($script:exactCaseRequested) { "exact" } else { "all" }
    Write-Output (
        (
            "CONTRACT_SUMMARY mode={0} selection={1} registered={2} unique={3} " +
            "executed={4} bootstrap={5} durationMs={6}"
        ) -f
            $Mode,
            $selection,
            $expectedRegistered,
            $registeredNames.Count,
            $executed,
            $bootstrapExecuted.ToString().ToLowerInvariant(),
            $suiteTimer.ElapsedMilliseconds
    )
}
catch {
    $script:contractFailure = $_
    throw
}
finally {
    $cleanupIssues = [System.Collections.Generic.List[string]]::new()
    try {
        Remove-Module -ModuleInfo $script:workspaceModule -Force -ErrorAction Stop
    }
    catch {
        $cleanupIssues.Add("module: $($_.Exception.Message)") | Out-Null
    }
    try {
        $resolvedTemporary = [System.IO.Path]::GetFullPath($script:temporaryRoot)
        $systemTemporary = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
        Assert-Contract (
            $resolvedTemporary.StartsWith(
                $systemTemporary,
                [System.StringComparison]::OrdinalIgnoreCase
            ) -and
            ([System.IO.Path]::GetFileName($resolvedTemporary) -cmatch '^ec-contract-[0-9a-f]{12}$')
        ) "contract temporary root must remain one direct, named child of the system temporary root"
        if (Test-Path -LiteralPath $resolvedTemporary) {
            $item = Get-Item -Force -LiteralPath $resolvedTemporary
            Assert-Contract (
                ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0
            ) "contract temporary root must not be a reparse point"
            Remove-Item -LiteralPath $resolvedTemporary -Recurse -Force -ErrorAction Stop
        }
    }
    catch {
        $cleanupIssues.Add("temporary root: $($_.Exception.Message)") | Out-Null
    }
    if ($cleanupIssues.Count -ne 0) {
        if ($null -ne $script:contractFailure) {
            $script:contractFailure.Exception.Data["EasyConContractCleanupFailure"] = `
                ($cleanupIssues -join [Environment]::NewLine)
        }
        else {
            throw "contract cleanup failed: $($cleanupIssues -join '; ')"
        }
    }
}

[CmdletBinding()]
param(
    [string]$ExactCase
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest
$script:contractProbeCleanupBlocked = $false

$script:contractCases = [System.Collections.Generic.List[object]]::new()
$script:contractCaseNames = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::Ordinal
)
$script:exactCaseRequested = $PSBoundParameters.ContainsKey("ExactCase")
$script:executedContractCases = 0

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

    if (-not $script:contractCaseNames.Add($Name)) {
        throw "contract case registry contains duplicate exact name '$Name'"
    }
    $script:contractCases.Add([pscustomobject]@{
        Name = $Name
        Action = $Action
    }) | Out-Null
}

function Invoke-SelectedContractCases {
    if ($script:exactCaseRequested) {
        if ([string]::IsNullOrWhiteSpace($ExactCase)) {
            throw "-ExactCase requires one complete exact case name"
        }
        if (-not $script:contractCaseNames.Contains($ExactCase)) {
            throw "unknown exact contract case '$ExactCase'"
        }
        $selected = @($script:contractCases | Where-Object { $_.Name -ceq $ExactCase })
        Assert-Contract ($selected.Count -eq 1) `
            "exact contract case selection must resolve one case"
        $timer = [System.Diagnostics.Stopwatch]::StartNew()
        & $selected[0].Action
        $timer.Stop()
        $script:executedContractCases++
        Write-Output ("CONTRACT_PASS name={0} durationMs={1}" -f $ExactCase, $timer.ElapsedMilliseconds)
        return
    }

    foreach ($case in $script:contractCases) {
        $timer = [System.Diagnostics.Stopwatch]::StartNew()
        & $case.Action
        $timer.Stop()
        $script:executedContractCases++
        Write-Output ("CONTRACT_PASS name={0} durationMs={1}" -f $case.Name, $timer.ElapsedMilliseconds)
    }
}

function Wait-ContractFileCreated {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [int]$TimeoutMilliseconds = 15000,

        [scriptblock]$BeforeWaitRegistration
    )

    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $invokeSeam = $null -ne $BeforeWaitRegistration
    try {
        while ($true) {
            if (Test-Path -LiteralPath $Path -PathType Leaf) {
                return
            }
            if ($invokeSeam) {
                $invokeSeam = $false
                & $BeforeWaitRegistration
                continue
            }
            $remaining = [long]$TimeoutMilliseconds - [long]$timer.ElapsedMilliseconds
            Assert-Contract ($remaining -gt 0) `
                "timed out waiting for synchronized contract file $Path"
            [System.Threading.Thread]::Sleep([int][Math]::Min(20L, $remaining))
        }
    }
    finally {
        $timer.Stop()
    }
}

function Wait-ContractFilesCreated {
    param(
        [Parameter(Mandatory)]
        [string[]]$Path,

        [int]$TimeoutMilliseconds = 15000
    )

    Assert-Contract ($Path.Count -gt 0) `
        "synchronized contract file set must not be empty"
    $fullPaths = @($Path | ForEach-Object { [System.IO.Path]::GetFullPath($_) })
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        while ($true) {
            $missing = @($fullPaths | Where-Object {
                -not (Test-Path -LiteralPath $_ -PathType Leaf)
            })
            if ($missing.Count -eq 0) {
                return
            }

            $remaining = [long]$TimeoutMilliseconds - [long]$timer.ElapsedMilliseconds
            Assert-Contract ($remaining -gt 0) (
                "timed out waiting for synchronized contract files: {0}" -f
                    ($missing -join ', ')
            )
            [System.Threading.Thread]::Sleep([int][Math]::Min(20L, $remaining))
        }
    }
    finally {
        $timer.Stop()
    }
}

function Set-ContractProbeState {
    param(
        [Parameter(Mandatory)][object]$Probe,
        [Parameter(Mandatory)]
        [ValidateSet("Started", "Ready", "ExitedEarly", "Released", "Exited", "ResultValidated")]
        [string]$State,
        [Parameter(Mandatory)][string]$Milestone
    )

    $allowed = switch ([string]$Probe.State) {
        "Created" { @("Started") }
        "Started" { @("Ready", "ExitedEarly") }
        "Ready" { @("Released", "ExitedEarly") }
        "Released" { @("Exited") }
        "Exited" { @("ResultValidated") }
        default { @() }
    }
    Assert-Contract ($allowed -ccontains $State) (
        "probe $($Probe.Name) state transition $($Probe.State) -> $State is invalid"
    )
    $Probe.State = $State
    $Probe.LastMilestone = $Milestone
    $Probe.Progress = $Milestone
}

if ($null -eq ("EasyConContractProcessOperations" -as [type])) {
    Add-Type -TypeDefinition @'
using System.Diagnostics;
using System.Threading.Tasks;

public static class EasyConContractProcessOperations
{
    public static Task KillTreeAsync(Process process)
    {
        return Task.Run(() => process.Kill(true));
    }

    public static void ObserveTaskCompletion(Task task)
    {
        if (task == null)
        {
            return;
        }

        task.ContinueWith(completed =>
        {
            try
            {
                completed.GetAwaiter().GetResult();
            }
            catch
            {
            }
        }, TaskScheduler.Default);
    }
}
'@
}

function New-ContractProbeDeadline {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][ValidateRange(0, [int]::MaxValue)]
        [int]$TimeoutMilliseconds
    )

    return [pscustomobject]@{
        Name = $Name
        TimeoutMilliseconds = [long]$TimeoutMilliseconds
        Timer = [System.Diagnostics.Stopwatch]::new()
        Started = $false
    }
}

function Get-ContractProbeDeadlineRemaining {
    param([Parameter(Mandatory)][object]$Deadline)

    if (-not $Deadline.Started) {
        $Deadline.Timer.Start()
        $Deadline.Started = $true
    }
    $remaining = $Deadline.TimeoutMilliseconds - [long]$Deadline.Timer.ElapsedMilliseconds
    if ($remaining -le 0) {
        return 0
    }
    return [int][Math]::Min([long][int]::MaxValue, $remaining)
}

function Initialize-ContractProbeTeardownState {
    param([Parameter(Mandatory)][object]$Probe)

    $defaults = [ordered]@{
        OutputCaptureFailure = ""
        OutputCaptureIssues = [System.Collections.Generic.List[string]]::new()
        TeardownIssues = [System.Collections.Generic.List[string]]::new()
        OutputTaskOwned = $false
        ErrorTaskOwned = $false
        OutputTaskObserved = $false
        ErrorTaskObserved = $false
        OutputTaskSucceeded = $false
        ErrorTaskSucceeded = $false
        OutputDrainInitialization = "not-started"
        ErrorDrainInitialization = "not-started"
        KillTask = $null
        KillLaunchAttempted = $false
        KillTaskObserved = $true
        PipeCancellationRequested = $false
        LastProcessExited = $false
        DiagnosticCached = $false
        TeardownDiagnostic = ""
        WasReclaimed = $false
        ProcessId = ""
    }
    foreach ($entry in $defaults.GetEnumerator()) {
        if ($Probe.PSObject.Properties.Match([string]$entry.Key).Count -eq 0) {
            $Probe | Add-Member -NotePropertyName $entry.Key -NotePropertyValue $entry.Value
        }
    }
    if ($null -ne $Probe.Process) {
        try {
            $observedProcessId = [string]$Probe.Process.Id
            if (-not [string]::IsNullOrWhiteSpace($observedProcessId)) {
                $Probe.ProcessId = $observedProcessId
            }
        }
        catch {
        }
    }
    if ($null -eq $Probe.OutputCaptureIssues) {
        $Probe.OutputCaptureIssues = [System.Collections.Generic.List[string]]::new()
    }
    if ($null -eq $Probe.TeardownIssues) {
        $Probe.TeardownIssues = [System.Collections.Generic.List[string]]::new()
    }
    foreach ($stream in @(
        [pscustomobject]@{
            TaskProperty = "OutputTask"
            OwnedProperty = "OutputTaskOwned"
            InitializationProperty = "OutputDrainInitialization"
        },
        [pscustomobject]@{
            TaskProperty = "ErrorTask"
            OwnedProperty = "ErrorTaskOwned"
            InitializationProperty = "ErrorDrainInitialization"
        }
    )) {
        if ($null -ne $Probe.($stream.TaskProperty)) {
            $Probe.($stream.OwnedProperty) = $true
            if ($Probe.($stream.InitializationProperty) -ceq "not-started") {
                $Probe.($stream.InitializationProperty) = "started"
            }
        }
    }
    if ($null -ne $Probe.KillTask -and -not $Probe.KillLaunchAttempted) {
        $Probe.KillLaunchAttempted = $true
        $Probe.KillTaskObserved = $false
    }
}

function Add-ContractProbeOutputCaptureIssue {
    param(
        [Parameter(Mandatory)][object]$Probe,
        [Parameter(Mandatory)][string]$Issue
    )

    Initialize-ContractProbeTeardownState -Probe $Probe
    if (-not $Probe.OutputCaptureIssues.Contains($Issue)) {
        $Probe.OutputCaptureIssues.Add($Issue) | Out-Null
    }
}

function Add-ContractProbeTeardownIssue {
    param(
        [Parameter(Mandatory)][object]$Probe,
        [Parameter(Mandatory)][string]$Issue
    )

    Initialize-ContractProbeTeardownState -Probe $Probe
    if (-not $Probe.TeardownIssues.Contains($Issue)) {
        $Probe.TeardownIssues.Add($Issue) | Out-Null
    }
}

function Get-ContractTaskState {
    param(
        [AllowNull()][System.Threading.Tasks.Task]$Task,
        [Parameter(Mandatory)][bool]$Observed
    )

    if ($null -eq $Task) { return "not-started" }
    if (-not $Task.IsCompleted) { return "pending" }
    if ($Task.IsCanceled) { return "canceled observed=$Observed" }
    if ($Task.IsFaulted) { return "faulted observed=$Observed" }
    return "completed observed=$Observed"
}

function Set-ContractProbeDrainTask {
    param(
        [Parameter(Mandatory)][object]$Probe,
        [Parameter(Mandatory)]
        [ValidateSet("stdout", "stderr")]
        [string]$Name,
        [Parameter(Mandatory)][System.Threading.Tasks.Task]$Task
    )

    Initialize-ContractProbeTeardownState -Probe $Probe
    $properties = if ($Name -ceq "stdout") {
        [pscustomobject]@{
            Task = "OutputTask"
            Owned = "OutputTaskOwned"
            Initialization = "OutputDrainInitialization"
        }
    }
    else {
        [pscustomobject]@{
            Task = "ErrorTask"
            Owned = "ErrorTaskOwned"
            Initialization = "ErrorDrainInitialization"
        }
    }
    $Probe.($properties.Task) = $Task
    $Probe.($properties.Owned) = $true
    $Probe.($properties.Initialization) = "started"
    $Probe.DiagnosticCached = $false
}

function Get-ContractProbeDrainState {
    param(
        [Parameter(Mandatory)][object]$Probe,
        [Parameter(Mandatory)]
        [ValidateSet("stdout", "stderr")]
        [string]$Name
    )

    Initialize-ContractProbeTeardownState -Probe $Probe
    $properties = if ($Name -ceq "stdout") {
        [pscustomobject]@{
            Task = "OutputTask"
            Owned = "OutputTaskOwned"
            Observed = "OutputTaskObserved"
            Initialization = "OutputDrainInitialization"
        }
    }
    else {
        [pscustomobject]@{
            Task = "ErrorTask"
            Owned = "ErrorTaskOwned"
            Observed = "ErrorTaskObserved"
            Initialization = "ErrorDrainInitialization"
        }
    }
    if (-not $Probe.($properties.Owned)) {
        return "not-started owned=False observed=$($Probe.($properties.Observed))"
    }
    if ($null -eq $Probe.($properties.Task)) {
        return "owned-missing observed=$($Probe.($properties.Observed))"
    }
    $taskState = Get-ContractTaskState -Task $Probe.($properties.Task) `
        -Observed ([bool]$Probe.($properties.Observed))
    return "$taskState owned=True init=$($Probe.($properties.Initialization))"
}

function Complete-ContractProbeOutput {
    param([Parameter(Mandatory)][object]$Probe)

    Initialize-ContractProbeTeardownState -Probe $Probe
    if ($null -eq $Probe.Process) { return @() }
    $hasExited = $false
    try {
        $hasExited = [bool]$Probe.Process.HasExited
    }
    catch {
        Add-ContractProbeOutputCaptureIssue -Probe $Probe `
            -Issue "process exit observation failed: $($_.Exception.Message)"
    }
    if (-not $hasExited) { return @($Probe.OutputCaptureIssues) }

    $issues = [System.Collections.Generic.List[string]]::new()
    foreach ($stream in @(
        [pscustomobject]@{
            Name = "stdout"
            Task = $Probe.OutputTask
            Property = "Stdout"
            OwnedProperty = "OutputTaskOwned"
            ObservedProperty = "OutputTaskObserved"
            SucceededProperty = "OutputTaskSucceeded"
            InitializationProperty = "OutputDrainInitialization"
        },
        [pscustomobject]@{
            Name = "stderr"
            Task = $Probe.ErrorTask
            Property = "Stderr"
            OwnedProperty = "ErrorTaskOwned"
            ObservedProperty = "ErrorTaskObserved"
            SucceededProperty = "ErrorTaskSucceeded"
            InitializationProperty = "ErrorDrainInitialization"
        }
    )) {
        if (-not $Probe.($stream.OwnedProperty)) {
            continue
        }
        if ($null -eq $stream.Task) {
            Add-ContractProbeOutputCaptureIssue -Probe $Probe `
                -Issue "$($stream.Name) drain ownership did not retain a task"
            continue
        }
        if (-not $stream.Task.IsCompleted) {
            $issues.Add("$($stream.Name) drain pending") | Out-Null
            continue
        }
        if ($Probe.($stream.ObservedProperty)) { continue }
        try {
            $Probe.($stream.Property) = $stream.Task.GetAwaiter().GetResult()
            $Probe.($stream.SucceededProperty) = $true
            $Probe.($stream.InitializationProperty) = "observed"
        }
        catch {
            $Probe.($stream.SucceededProperty) = $false
            $Probe.($stream.InitializationProperty) = if ($stream.Task.IsCanceled) {
                "canceled"
            }
            else {
                "faulted"
            }
            Add-ContractProbeOutputCaptureIssue -Probe $Probe `
                -Issue "$($stream.Name) drain failed: $($_.Exception.Message)"
        }
        finally {
            $Probe.($stream.ObservedProperty) = $true
            $Probe.DiagnosticCached = $false
        }
    }
    $Probe.OutputCaptured = [bool](
        $Probe.OutputTaskOwned -and
        $Probe.ErrorTaskOwned -and
        $Probe.OutputTaskObserved -and
        $Probe.ErrorTaskObserved -and
        $Probe.OutputTaskSucceeded -and
        $Probe.ErrorTaskSucceeded
    )
    foreach ($issue in $Probe.OutputCaptureIssues) {
        $issues.Add($issue) | Out-Null
    }
    $Probe.OutputCaptureFailure = @($issues) -join "; "
    return @($issues)
}

function Complete-ContractProbeOutputFinalSnapshot {
    param(
        [Parameter(Mandatory)][object]$Probe,
        [ValidateRange(1, [int]::MaxValue)][int]$TimeoutMilliseconds = 1000
    )

    Initialize-ContractProbeTeardownState -Probe $Probe
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        while ($true) {
            $pending = @(
                @($Probe.OutputTask, $Probe.ErrorTask) | Where-Object {
                    $null -ne $_ -and -not $_.IsCompleted
                }
            )
            if ($pending.Count -eq 0) {
                break
            }
            $remaining = [long]$TimeoutMilliseconds - [long]$timer.ElapsedMilliseconds
            if ($remaining -le 0) {
                foreach ($stream in @(
                    [pscustomobject]@{ Name = "stdout"; Task = $Probe.OutputTask },
                    [pscustomobject]@{ Name = "stderr"; Task = $Probe.ErrorTask }
                )) {
                    if ($null -ne $stream.Task -and -not $stream.Task.IsCompleted) {
                        Add-ContractProbeOutputCaptureIssue -Probe $Probe `
                            -Issue "$($stream.Name) drain exceeded bounded final wait"
                    }
                }
                break
            }
            [System.Threading.Tasks.Task]::WaitAny(
                [System.Threading.Tasks.Task[]]$pending,
                [int][Math]::Min(20L, $remaining)
            ) | Out-Null
        }
    }
    finally {
        $timer.Stop()
    }
    return @(Complete-ContractProbeOutput -Probe $Probe)
}

function Request-ContractProbePipeCancellation {
    param([Parameter(Mandatory)][object]$Probe)

    Initialize-ContractProbeTeardownState -Probe $Probe
    if ($Probe.PipeCancellationRequested -or $null -eq $Probe.Process) { return }
    $Probe.PipeCancellationRequested = $true
    foreach ($readerName in @("StandardOutput", "StandardError")) {
        try {
            $reader = $Probe.Process.$readerName
            if ($null -ne $reader) { $reader.Dispose() }
        }
        catch {
            Add-ContractProbeTeardownIssue -Probe $Probe `
                -Issue "$readerName pipe close failed: $($_.Exception.Message)"
        }
    }
    foreach ($task in @($Probe.OutputTask, $Probe.ErrorTask)) {
        if ($null -ne $task -and -not $task.IsCompleted) {
            [EasyConContractProcessOperations]::ObserveTaskCompletion($task)
        }
    }
}

function Get-ContractProbeTrace {
    param([Parameter(Mandatory)][object]$Probe)

    if (-not (Test-Path -LiteralPath $Probe.Trace -PathType Leaf)) {
        return "<none>"
    }
    try { return Get-Content -Raw -LiteralPath $Probe.Trace }
    catch { return "<trace read failed: $($_.Exception.Message)>" }
}

function Update-ContractProbeTraceProgress {
    param([Parameter(Mandatory)][object]$Probe)

    $trace = Get-ContractProbeTrace -Probe $Probe
    $traceLines = @($trace -split '\r?\n' | Where-Object {
        -not [string]::IsNullOrWhiteSpace($_)
    })
    if (
        $traceLines.Count -gt 0 -and
        $traceLines[-1] -match '^[^|]+\|(?<Milestone>.+)$'
    ) {
        $Probe.LastMilestone = $Matches.Milestone
        $Probe.Progress = $Matches.Milestone
    }
    return $trace
}

function Get-ContractProbeDiagnostic {
    param([Parameter(Mandatory)][object]$Probe)

    Initialize-ContractProbeTeardownState -Probe $Probe
    $pidValue = if ([string]::IsNullOrWhiteSpace([string]$Probe.ProcessId)) {
        "<none>"
    }
    else {
        [string]$Probe.ProcessId
    }
    $exitCode = "<not-started>"
    if ($null -ne $Probe.Process) {
        try {
            $observedProcessId = [string]$Probe.Process.Id
            if (-not [string]::IsNullOrWhiteSpace($observedProcessId)) {
                $Probe.ProcessId = $observedProcessId
                $pidValue = $observedProcessId
            }
        }
        catch { $pidValue = "<unavailable: $($_.Exception.Message)>" }
        try {
            if ($Probe.Process.HasExited) { $exitCode = [string]$Probe.Process.ExitCode }
            else { $exitCode = "<running>" }
        }
        catch { $exitCode = "<unavailable: $($_.Exception.Message)>" }
    }
    $stdout = if (-not $Probe.OutputTaskOwned) {
        "<not-started>"
    }
    elseif ($Probe.OutputTaskObserved -and $Probe.OutputTaskSucceeded) {
        $Probe.Stdout
    }
    else {
        "<draining>"
    }
    $stderr = if (-not $Probe.ErrorTaskOwned) {
        "<not-started>"
    }
    elseif ($Probe.ErrorTaskObserved -and $Probe.ErrorTaskSucceeded) {
        $Probe.Stderr
    }
    else {
        "<draining>"
    }
    $trace = Update-ContractProbeTraceProgress -Probe $Probe
    $captureFailure = if ([string]::IsNullOrWhiteSpace($Probe.OutputCaptureFailure)) {
        "<none>"
    }
    else { $Probe.OutputCaptureFailure }
    $stdoutDrain = Get-ContractProbeDrainState -Probe $Probe -Name "stdout"
    $stderrDrain = Get-ContractProbeDrainState -Probe $Probe -Name "stderr"
    $killTask = Get-ContractTaskState -Task $Probe.KillTask `
        -Observed ([bool]$Probe.KillTaskObserved)
    return (
        "probe=$($Probe.Name) state=$($Probe.State) progress=$($Probe.Progress) " +
        "pid=$pidValue command=$($Probe.Command) ArgumentList=$($Probe.ArgumentListJson) " +
        "exitCode=$exitCode lastMilestone=$($Probe.LastMilestone)`n" +
        "stdout<<`n$stdout`n>>stdout`nstderr<<`n$stderr`n>>stderr`n" +
        "outputCaptured=$($Probe.OutputCaptured) stdoutDrain=$stdoutDrain " +
        "stderrDrain=$stderrDrain killTask=$killTask " +
        "pipeCancellationRequested=$($Probe.PipeCancellationRequested) " +
        "lastProcessExited=$($Probe.LastProcessExited) wasReclaimed=$($Probe.WasReclaimed)`n" +
        "outputCaptureFailure=$captureFailure`ntrace<<`n$trace`n>>trace"
    )
}

function Cache-ContractProbeDiagnostic {
    param([Parameter(Mandatory)][object]$Probe)

    Initialize-ContractProbeTeardownState -Probe $Probe
    if ($Probe.DiagnosticCached) { return }
    try { $Probe.TeardownDiagnostic = Get-ContractProbeDiagnostic -Probe $Probe }
    catch {
        Add-ContractProbeTeardownIssue -Probe $Probe `
            -Issue "diagnostic capture failed: $($_.Exception.Message)"
        $Probe.TeardownDiagnostic = (
            "probe=$($Probe.Name) state=$($Probe.State) progress=$($Probe.Progress) " +
            "pid=<unavailable> command=$($Probe.Command) ArgumentList=$($Probe.ArgumentListJson) " +
            "exitCode=<unavailable> lastMilestone=$($Probe.LastMilestone)`n" +
            "stdout<<`n$($Probe.Stdout)`n>>stdout`nstderr<<`n$($Probe.Stderr)`n>>stderr`n" +
            "diagnostic=<capture failed>"
        )
    }
    finally { $Probe.DiagnosticCached = $true }
}

function Get-ContractProbeDiagnostics {
    param([Parameter(Mandatory)][object[]]$Probe)

    return (@($Probe | ForEach-Object {
        if (
            $null -ne $_.PSObject.Properties["TeardownDiagnostic"] -and
            -not [string]::IsNullOrWhiteSpace($_.TeardownDiagnostic)
        ) { $_.TeardownDiagnostic }
        else { Get-ContractProbeDiagnostic -Probe $_ }
    }) -join "`n---`n")
}

function Update-ContractProbeTeardownSnapshot {
    param([Parameter(Mandatory)][object]$Probe)

    Initialize-ContractProbeTeardownState -Probe $Probe
    if ($Probe.WasReclaimed -or $null -eq $Probe.Process) {
        $Probe.LastProcessExited = $true
        return
    }
    try { $Probe.LastProcessExited = [bool]$Probe.Process.WaitForExit(0) }
    catch {
        Add-ContractProbeTeardownIssue -Probe $Probe `
            -Issue "process WaitForExit failed: $($_.Exception.Message)"
        return
    }
    if ($null -ne $Probe.KillTask -and $Probe.KillTask.IsCompleted -and -not $Probe.KillTaskObserved) {
        try { $null = $Probe.KillTask.GetAwaiter().GetResult() }
        catch {
            Add-ContractProbeTeardownIssue -Probe $Probe `
                -Issue "process tree kill failed: $($_.Exception.Message)"
        }
        finally { $Probe.KillTaskObserved = $true }
    }
    if ($Probe.LastProcessExited) { $null = Complete-ContractProbeOutput -Probe $Probe }
}

function Test-ContractProbeCanBeReclaimed {
    param([Parameter(Mandatory)][object]$Probe)

    Initialize-ContractProbeTeardownState -Probe $Probe
    if ($null -eq $Probe.Process) { return $true }
    $killObserved = (
        -not $Probe.KillLaunchAttempted -or
        ($null -ne $Probe.KillTask -and $Probe.KillTask.IsCompleted -and $Probe.KillTaskObserved)
    )
    $outputDrainSettled = (
        -not $Probe.OutputTaskOwned -or
        (
            $null -ne $Probe.OutputTask -and
            $Probe.OutputTask.IsCompleted -and
            $Probe.OutputTaskObserved
        )
    )
    $errorDrainSettled = (
        -not $Probe.ErrorTaskOwned -or
        (
            $null -ne $Probe.ErrorTask -and
            $Probe.ErrorTask.IsCompleted -and
            $Probe.ErrorTaskObserved
        )
    )
    return [bool](
        $Probe.LastProcessExited -and
        $killObserved -and
        $outputDrainSettled -and
        $errorDrainSettled -and
        $Probe.DiagnosticCached
    )
}

function Try-ReclaimContractProbe {
    param([Parameter(Mandatory)][object]$Probe)

    if ($Probe.WasReclaimed) { return }
    if ($null -eq $Probe.Process) {
        $Probe.WasReclaimed = $true
        return
    }
    if (-not (Test-ContractProbeCanBeReclaimed -Probe $Probe)) { return }
    try {
        $Probe.Process.Dispose()
        $Probe.WasReclaimed = $true
        # The diagnostic is part of the failure contract, so never retain a pre-reclaim snapshot.
        $Probe.DiagnosticCached = $false
        Cache-ContractProbeDiagnostic -Probe $Probe
    }
    catch {
        Add-ContractProbeTeardownIssue -Probe $Probe `
            -Issue "process dispose failed: $($_.Exception.Message)"
    }
}

function Stop-ContractProbes {
    param(
        [Parameter(Mandatory)][object[]]$Probe,
        [int]$TimeoutMilliseconds = 5000,
        [object]$Deadline
    )

    if ($null -eq $Deadline) {
        $Deadline = New-ContractProbeDeadline -Name "probe teardown" `
            -TimeoutMilliseconds $TimeoutMilliseconds
    }
    $null = Get-ContractProbeDeadlineRemaining -Deadline $Deadline
    foreach ($candidate in $Probe) {
        Initialize-ContractProbeTeardownState -Probe $candidate
    }

    # Every live child receives a tree-kill task before waits, drains, or diagnostics begin.
    foreach ($candidate in $Probe) {
        if ($candidate.WasReclaimed -or $null -eq $candidate.Process) { continue }
        $isLive = $true
        try {
            $candidate.LastProcessExited = [bool]$candidate.Process.HasExited
            $isLive = -not $candidate.LastProcessExited
        }
        catch {
            Add-ContractProbeTeardownIssue -Probe $candidate `
                -Issue "process exit observation failed before kill launch: $($_.Exception.Message)"
        }
        if ($isLive -and -not $candidate.KillLaunchAttempted) {
            $candidate.KillLaunchAttempted = $true
            $candidate.KillTaskObserved = $false
            try {
                $candidate.KillTask = [EasyConContractProcessOperations]::KillTreeAsync(
                    $candidate.Process
                )
            }
            catch {
                $candidate.KillTaskObserved = $true
                Add-ContractProbeTeardownIssue -Probe $candidate `
                    -Issue "process tree kill launch failed: $($_.Exception.Message)"
            }
        }
    }

    while ($true) {
        foreach ($candidate in $Probe) {
            Update-ContractProbeTeardownSnapshot -Probe $candidate
            if ($candidate.LastProcessExited) {
                Cache-ContractProbeDiagnostic -Probe $candidate
                Try-ReclaimContractProbe -Probe $candidate
            }
        }
        if (@($Probe | Where-Object { -not $_.WasReclaimed }).Count -eq 0) { break }
        $remaining = Get-ContractProbeDeadlineRemaining -Deadline $Deadline
        if ($remaining -le 0) {
            foreach ($candidate in $Probe) {
                Update-ContractProbeTeardownSnapshot -Probe $candidate
                if ($candidate.LastProcessExited -and -not $candidate.OutputCaptured) {
                    Request-ContractProbePipeCancellation -Probe $candidate
                    Update-ContractProbeTeardownSnapshot -Probe $candidate
                }
                Cache-ContractProbeDiagnostic -Probe $candidate
                Try-ReclaimContractProbe -Probe $candidate
            }
            break
        }
        [System.Threading.Thread]::Sleep([int][Math]::Min(20L,[long]$remaining))
    }

    $issues = [System.Collections.Generic.List[string]]::new()
    $livePids = [System.Collections.Generic.List[string]]::new()
    $unreclaimedPids = [System.Collections.Generic.List[string]]::new()
    foreach ($candidate in $Probe) {
        Initialize-ContractProbeTeardownState -Probe $candidate
        if (-not $candidate.DiagnosticCached) { Cache-ContractProbeDiagnostic -Probe $candidate }
        if (-not $candidate.WasReclaimed) {
            $pidValue = "$($candidate.Name):unknown"
            try { $pidValue = [string]$candidate.Process.Id } catch {}
            $unreclaimedPids.Add($pidValue) | Out-Null
            if (-not $candidate.LastProcessExited) {
                $livePids.Add($pidValue) | Out-Null
                $issues.Add(
                    "probe $($candidate.Name): owned child remains alive after teardown deadline"
                ) | Out-Null
            }
            if ($candidate.KillLaunchAttempted -and -not $candidate.KillTaskObserved) {
                $issues.Add(
                    "probe $($candidate.Name): process tree kill task remains unobserved after teardown deadline"
                ) | Out-Null
            }
            if ($candidate.OutputTaskOwned -and $null -eq $candidate.OutputTask) {
                $issues.Add(
                    "probe $($candidate.Name): stdout drain ownership did not retain a task"
                ) | Out-Null
            }
            elseif ($candidate.OutputTaskOwned -and -not $candidate.OutputTaskObserved) {
                $issues.Add(
                    "probe $($candidate.Name): stdout drain remains unobserved after teardown deadline"
                ) | Out-Null
            }
            if ($candidate.ErrorTaskOwned -and $null -eq $candidate.ErrorTask) {
                $issues.Add(
                    "probe $($candidate.Name): stderr drain ownership did not retain a task"
                ) | Out-Null
            }
            elseif ($candidate.ErrorTaskOwned -and -not $candidate.ErrorTaskObserved) {
                $issues.Add(
                    "probe $($candidate.Name): stderr drain remains unobserved after teardown deadline"
                ) | Out-Null
            }
        }
        foreach ($issue in $candidate.OutputCaptureIssues) {
            $issues.Add("probe $($candidate.Name): $issue") | Out-Null
        }
        foreach ($issue in $candidate.TeardownIssues) {
            $issues.Add("probe $($candidate.Name): $issue") | Out-Null
        }
    }
    if ($unreclaimedPids.Count -gt 0) { $script:contractProbeCleanupBlocked = $true }
    if ($issues.Count -gt 0) {
        $primary = $issues[0]
        $additional = if ($issues.Count -gt 1) {
            @($issues | Select-Object -Skip 1) -join "; "
        }
        else { "<none>" }
        $livePidText = if ($livePids.Count -eq 0) { "<none>" } else { $livePids -join "," }
        $unreclaimedPidText = if ($unreclaimedPids.Count -eq 0) {
            "<none>"
        }
        else { $unreclaimedPids -join "," }
        throw (
            "contract failed: probe teardown failed; primary=$primary; " +
            "additional=$additional; liveOwnedPids=$livePidText; " +
            "unreclaimedOwnedPids=$unreclaimedPidText`n" +
            "probe evidence<<`n$(Get-ContractProbeDiagnostics -Probe $Probe)`n>>probe evidence"
        )
    }
}

function Start-ContractProbe {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][System.Diagnostics.ProcessStartInfo]$StartInfo,
        [Parameter(Mandatory)][string[]]$Arguments,
        [Parameter(Mandatory)][string]$Ready,
        [Parameter(Mandatory)][string]$Release,
        [Parameter(Mandatory)][string]$Result,
        [Parameter(Mandatory)][string]$Trace,
        [string]$Start,
        [object]$Location,
        [object]$TeardownDeadline,
        [AllowEmptyCollection()][object[]]$OwnedProbe = @(),
        [scriptblock]$AfterProcessStarted,
        [scriptblock]$AfterOutputDrainStarted,
        [scriptblock]$AfterDrainsInitialized
    )

    if ($null -eq $TeardownDeadline) {
        $TeardownDeadline = New-ContractProbeDeadline -Name "probe teardown" `
            -TimeoutMilliseconds 5000
    }
    $probe = [pscustomobject]@{
        Name = $Name
        State = "Created"
        Progress = "created"
        LastMilestone = "created"
        Process = $null
        ProcessId = ""
        Command = $StartInfo.FileName
        ArgumentListJson = ConvertTo-Json -InputObject @($Arguments) -Compress
        OutputTask = $null
        ErrorTask = $null
        OutputTaskOwned = $false
        ErrorTaskOwned = $false
        OutputTaskObserved = $false
        ErrorTaskObserved = $false
        OutputTaskSucceeded = $false
        ErrorTaskSucceeded = $false
        OutputDrainInitialization = "not-started"
        ErrorDrainInitialization = "not-started"
        OutputCaptured = $false
        OutputCaptureFailure = ""
        Stdout = ""
        Stderr = ""
        Start = $Start
        Ready = $Ready
        Release = $Release
        Result = $Result
        Trace = $Trace
        Location = $Location
        TeardownDeadline = $TeardownDeadline
        TeardownDiagnostic = ""
        KillTask = $null
        WasReclaimed = $false
    }
    try {
        $probe.Process = [System.Diagnostics.Process]::Start($StartInfo)
        if ($null -eq $probe.Process) { throw "Process.Start returned null" }
        $probe.ProcessId = [string]$probe.Process.Id
        Set-ContractProbeState -Probe $probe -State Started -Milestone "process.started"
        if ($null -ne $AfterProcessStarted) { & $AfterProcessStarted $probe }
        Set-ContractProbeDrainTask -Probe $probe -Name "stdout" `
            -Task $probe.Process.StandardOutput.ReadToEndAsync()
        if ($null -ne $AfterOutputDrainStarted) {
            & $AfterOutputDrainStarted $probe
        }
        Set-ContractProbeDrainTask -Probe $probe -Name "stderr" `
            -Task $probe.Process.StandardError.ReadToEndAsync()
        if ($null -ne $AfterDrainsInitialized) {
            & $AfterDrainsInitialized $probe
        }
        return $probe
    }
    catch {
        $startFailure = $_
        $cleanupFailure = $null
        $cleanupProbes = [System.Collections.Generic.List[object]]::new()
        foreach ($ownedProbe in $OwnedProbe) {
            if ($null -ne $ownedProbe) {
                $cleanupProbes.Add($ownedProbe) | Out-Null
            }
        }
        $cleanupProbes.Add($probe) | Out-Null
        [object[]]$cleanupProbeArray = @($cleanupProbes | ForEach-Object { $_ })
        try { Stop-ContractProbes -Probe $cleanupProbeArray -Deadline $TeardownDeadline }
        catch { $cleanupFailure = $_ }
        $message = (
            "contract failed: probe=$Name initialization failed state=$($probe.State) " +
            "command=$($probe.Command) " +
            "ArgumentList=$($probe.ArgumentListJson); primary=$($startFailure.Exception.Message)`n" +
            "probe evidence<<`n$(Get-ContractProbeDiagnostics -Probe $cleanupProbeArray)`n>>probe evidence"
        )
        if ($null -ne $cleanupFailure) {
            $message += "`nteardown failure<<`n$($cleanupFailure.Exception.Message)`n>>teardown failure"
        }
        throw $message
    }
}

function Update-ContractProbeReadinessSnapshot {
    param(
        [Parameter(Mandatory)][object[]]$Probe,
        [Parameter(Mandatory)][scriptblock]$ProcessHasExitedAction
    )

    foreach ($candidate in $Probe) {
        Initialize-ContractProbeTeardownState -Probe $candidate
        $null = Update-ContractProbeTraceProgress -Probe $candidate
        if (
            $candidate.State -ceq "Started" -and
            (Test-Path -LiteralPath $candidate.Ready -PathType Leaf)
        ) {
            Set-ContractProbeState -Probe $candidate -State Ready `
                -Milestone "ready.observed"
        }
        $hasExited = $false
        try { $hasExited = [bool](& $ProcessHasExitedAction $candidate.Process) }
        catch {
            throw (
                "contract failed: probe process observation failed: $($_.Exception.Message); " +
                (Get-ContractProbeDiagnostic -Probe $candidate)
            )
        }
        $null = Update-ContractProbeTraceProgress -Probe $candidate
        if (
            @("Started", "Ready") -ccontains $candidate.State -and
            $hasExited
        ) {
            $captureIssues = @(Complete-ContractProbeOutputFinalSnapshot -Probe $candidate)
            Set-ContractProbeState -Probe $candidate -State ExitedEarly `
                -Milestone "process.exited-before-result"
            $captureText = if ($captureIssues.Count -eq 0) { "" } else {
                " outputCaptureIssues=$($captureIssues -join '; ');"
            }
            throw (
                "contract failed: child exited before ready/result;$captureText " +
                (Get-ContractProbeDiagnostic -Probe $candidate)
            )
        }
    }
}

function Wait-ContractProbesReady {
    param(
        [Parameter(Mandatory)][object[]]$Probe,
        [Parameter(Mandatory)][string[]]$RequiredName,
        [Parameter(Mandatory)][System.Diagnostics.Stopwatch]$Timer,
        [Parameter(Mandatory)][int]$TimeoutMilliseconds,
        [scriptblock]$ProcessHasExitedAction = {
            param([object]$Process)
            return $Process.HasExited
        }
    )

    $required = @($Probe | Where-Object { $RequiredName -ccontains $_.Name })
    Assert-Contract ($required.Count -eq $RequiredName.Count) `
        "required probe names must resolve exactly"
    while ($true) {
        Update-ContractProbeReadinessSnapshot -Probe $Probe `
            -ProcessHasExitedAction $ProcessHasExitedAction
        if (@($required | Where-Object { $_.State -cne "Ready" }).Count -eq 0) { return }
        $remaining = [long]$TimeoutMilliseconds - [long]$Timer.ElapsedMilliseconds
        if ($remaining -le 0) {
            # A final full marker/process/trace snapshot must classify an observed exit first.
            Update-ContractProbeReadinessSnapshot -Probe $Probe `
                -ProcessHasExitedAction $ProcessHasExitedAction
            if (@($required | Where-Object { $_.State -cne "Ready" }).Count -eq 0) { return }
            throw (
                "contract failed: timed out waiting for probe readiness; " +
                (Get-ContractProbeDiagnostics -Probe $Probe)
            )
        }
        [System.Threading.Thread]::Sleep([int][Math]::Min(20L,$remaining))
    }
}

function Publish-ContractProbeRelease {
    param([Parameter(Mandatory)][object[]]$Probe)

    foreach ($candidate in $Probe) {
        if ($candidate.Process.HasExited) {
            $captureIssues = @(Complete-ContractProbeOutputFinalSnapshot -Probe $candidate)
            Set-ContractProbeState -Probe $candidate -State ExitedEarly `
                -Milestone "process.exited-before-release"
            $captureText = if ($captureIssues.Count -eq 0) { "" } else {
                " outputCaptureIssues=$($captureIssues -join '; ');"
            }
            throw (
                "contract failed: child exited before ready/result;$captureText " +
                (Get-ContractProbeDiagnostic -Probe $candidate)
            )
        }
        Assert-Contract ($candidate.State -ceq "Ready") `
            "probe $($candidate.Name) must be Ready before release"
    }
    foreach ($releasePath in @($Probe.Release | Select-Object -Unique)) {
        [System.IO.File]::WriteAllText(
            $releasePath,"release",[System.Text.UTF8Encoding]::new($false)
        )
    }
    foreach ($candidate in $Probe) {
        Set-ContractProbeState -Probe $candidate -State Released `
            -Milestone "release.published"
    }
}

function Update-ContractProbeExitSnapshot {
    param([Parameter(Mandatory)][object[]]$Probe)

    foreach ($candidate in $Probe) {
        Initialize-ContractProbeTeardownState -Probe $candidate
        $null = Update-ContractProbeTraceProgress -Probe $candidate
        if ($candidate.State -ceq "Released") {
            $candidate.LastProcessExited = [bool]$candidate.Process.WaitForExit(0)
        }
        if ($candidate.State -ceq "Released" -and $candidate.LastProcessExited) {
            $captureIssues = @(Complete-ContractProbeOutputFinalSnapshot -Probe $candidate)
            if (-not $candidate.OutputCaptured) { continue }
            Set-ContractProbeState -Probe $candidate -State Exited `
                -Milestone "process.exited"
            if (
                $captureIssues.Count -gt 0 -or
                $candidate.Process.ExitCode -ne 0 -or
                -not (Test-Path -LiteralPath $candidate.Result -PathType Leaf)
            ) {
                throw (
                    "contract failed: child exited before a valid result; " +
                    "outputCaptureIssues=$($captureIssues -join '; '); " +
                    (Get-ContractProbeDiagnostic -Probe $candidate)
                )
            }
        }
    }
}

function Wait-ContractProbesExited {
    param(
        [Parameter(Mandatory)][object[]]$Probe,
        [Parameter(Mandatory)][int]$TimeoutMilliseconds
    )

    $deadline = New-ContractProbeDeadline -Name "release-to-exit" `
        -TimeoutMilliseconds $TimeoutMilliseconds
    while ($true) {
        Update-ContractProbeExitSnapshot -Probe $Probe
        if (@($Probe | Where-Object { $_.State -cne "Exited" }).Count -eq 0) { return }
        $remaining = Get-ContractProbeDeadlineRemaining -Deadline $deadline
        if ($remaining -le 0) {
            Update-ContractProbeExitSnapshot -Probe $Probe
            if (@($Probe | Where-Object { $_.State -cne "Exited" }).Count -eq 0) { return }
            throw (
                "contract failed: timed out waiting for released probes to exit; " +
                (Get-ContractProbeDiagnostics -Probe $Probe)
            )
        }
        [System.Threading.Thread]::Sleep([int][Math]::Min(20L,$remaining))
    }
}

function Assert-ContractProbeReadinessDeadlineRegression {
    param([Parameter(Mandatory)][string]$ProbeRoot)

    New-Item -ItemType Directory -Force -Path $ProbeRoot | Out-Null
    $trace = Join-Path $ProbeRoot "deadline-final-observation-trace.txt"
    Set-Content -LiteralPath $trace -Encoding utf8NoBOM -Value "synthetic|exit.before-ready"
    $process = [pscustomobject]@{ Id = 42424; HasExited = $false; ExitCode = 23 }
    $process | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value {
        param([int]$Timeout = -1)
        return $true
    }
    $probe = [pscustomobject]@{
        Name = "deadline-exit"
        State = "Started"
        Progress = "process.started"
        LastMilestone = "process.started"
        Process = $process
        Command = "synthetic-workspace-probe.exe"
        ArgumentListJson = '["--deadline-exit","path with spaces"]'
        OutputTask = [System.Threading.Tasks.Task]::FromResult([string]"synthetic stdout")
        ErrorTask = [System.Threading.Tasks.Task]::FromResult([string]"SYNTHETIC_WORKSPACE_DEADLINE_EXIT_STDERR")
        OutputCaptured = $false
        Stdout = ""
        Stderr = ""
        Ready = Join-Path $ProbeRoot "deadline-final-observation-ready.txt"
        Release = Join-Path $ProbeRoot "deadline-final-observation-release.txt"
        Result = Join-Path $ProbeRoot "deadline-final-observation-result.txt"
        Trace = $trace
        TeardownDeadline = New-ContractProbeDeadline -Name "workspace readiness regression teardown" `
            -TimeoutMilliseconds 1000
        WasReclaimed = $false
    }
    $exitReads = [pscustomobject]@{ Count = 0 }
    $processHasExited = {
        param([object]$CandidateProcess)
        $exitReads.Count++
        if ($exitReads.Count -eq 1) {
            $CandidateProcess.HasExited = $true
            return $false
        }
        return [bool]$CandidateProcess.HasExited
    }.GetNewClosure()
    $failure = $null
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        Wait-ContractProbesReady -Probe @($probe) -RequiredName @($probe.Name) `
            -Timer $timer -TimeoutMilliseconds 0 `
            -ProcessHasExitedAction $processHasExited
    }
    catch { $failure = $_ }
    finally { $timer.Stop() }
    $message = if ($null -eq $failure) { "<none>" } else { $failure.Exception.Message }
    Assert-Contract (
        $null -ne $failure -and
        $probe.State -ceq "ExitedEarly" -and
        $exitReads.Count -eq 2 -and
        $message -match 'child exited before ready/result' -and
        $message -notmatch 'timed out waiting for probe readiness' -and
        $message -match 'exitCode=23' -and
        $message -match 'SYNTHETIC_WORKSPACE_DEADLINE_EXIT_STDERR'
    ) (
        "workspace readiness deadline must prefer the final observed early exit; " +
        "exitReads=$($exitReads.Count) state=$($probe.State) observed=$message"
    )
}

function New-ContractProbeFixture {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory)][System.Diagnostics.ProcessStartInfo]$StartInfo,
        [Parameter(Mandatory)][string]$ProbeRoot,
        [System.Threading.Tasks.Task]$OutputTask,
        [System.Threading.Tasks.Task]$ErrorTask
    )

    if ($null -eq $OutputTask) { $OutputTask = $Process.StandardOutput.ReadToEndAsync() }
    if ($null -eq $ErrorTask) { $ErrorTask = $Process.StandardError.ReadToEndAsync() }
    $trace = Join-Path $ProbeRoot "$Name-trace.txt"
    Set-Content -LiteralPath $trace -Encoding utf8NoBOM -Value "$Name|ready.written"
    return [pscustomobject]@{
        Name = $Name
        State = "Ready"
        Progress = "ready.observed"
        LastMilestone = "ready.observed"
        Process = $Process
        Command = $StartInfo.FileName
        ArgumentListJson = ConvertTo-Json -InputObject @($StartInfo.ArgumentList) -Compress
        OutputTask = $OutputTask
        ErrorTask = $ErrorTask
        OutputCaptured = $false
        Stdout = ""
        Stderr = ""
        Ready = Join-Path $ProbeRoot "$Name-ready.txt"
        Release = Join-Path $ProbeRoot "$Name-release.txt"
        Result = Join-Path $ProbeRoot "$Name-result.txt"
        Trace = $trace
        WasReclaimed = $false
    }
}

function Assert-ContractProbeDrainInitializationFailure {
    param(
        [Parameter(Mandatory)][string]$ProbeRoot,
        [Parameter(Mandatory)]
        [ValidateSet("zero", "partial", "both")]
        [string]$Schedule
    )

    $capture = [pscustomobject]@{
        Probe = $null
        Pid = 0
    }
    $childArguments = @(
        "-NoLogo",
        "-NoProfile",
        "-Command",
        "Start-Sleep -Seconds 30"
    )
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = Join-Path $PSHOME "pwsh.exe"
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $childArguments) {
        $startInfo.ArgumentList.Add($argument)
    }
    $failureToken = "synthetic-$Schedule-drain-initialization-failure"
    $startParameters = @{
        Name = "drain-init-$Schedule"
        StartInfo = $startInfo
        Arguments = $childArguments
        Ready = Join-Path $ProbeRoot "drain-init-$Schedule-ready.txt"
        Release = Join-Path $ProbeRoot "drain-init-$Schedule-release.txt"
        Result = Join-Path $ProbeRoot "drain-init-$Schedule-result.txt"
        Trace = Join-Path $ProbeRoot "drain-init-$Schedule-trace.txt"
        TeardownDeadline = (
            New-ContractProbeDeadline -Name "drain initialization $Schedule teardown" `
                -TimeoutMilliseconds 1000
        )
        AfterProcessStarted = ({
            param([object]$Probe)
            $capture.Probe = $Probe
            $capture.Pid = $Probe.Process.Id
        }).GetNewClosure()
    }
    switch ($Schedule) {
        "zero" {
            $startParameters.AfterProcessStarted = ({
                param([object]$Probe)
                $capture.Probe = $Probe
                $capture.Pid = $Probe.Process.Id
                throw $failureToken
            }).GetNewClosure()
        }
        "partial" {
            $startParameters.AfterOutputDrainStarted = ({
                param([object]$Probe)
                throw $failureToken
            }).GetNewClosure()
        }
        "both" {
            $startParameters.AfterDrainsInitialized = ({
                param([object]$Probe)
                throw $failureToken
            }).GetNewClosure()
        }
    }

    $failure = $null
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        Start-ContractProbe @startParameters | Out-Null
    }
    catch {
        $failure = $_
    }
    finally {
        $timer.Stop()
    }

    $probe = $capture.Probe
    $message = if ($null -eq $failure) { "<none>" } else { $failure.Exception.Message }
    try {
        Assert-Contract (
            $null -ne $probe -and
            $capture.Pid -gt 0 -and
            $probe.State -ceq "Started" -and
            $probe.LastProcessExited -and
            $probe.KillLaunchAttempted -and
            $probe.KillTaskObserved -and
            $probe.DiagnosticCached -and
            $probe.WasReclaimed -and
            -not $script:contractProbeCleanupBlocked -and
            $timer.ElapsedMilliseconds -le 2000 -and
            $message -match [regex]::Escape($failureToken) -and
            $message -match 'state=Started' -and
            $message -match 'pid=\d+' -and
            $message -match 'ArgumentList=' -and
            $message -match 'stdoutDrain=' -and
            $message -match 'stderrDrain=' -and
            $message -match 'killTask='
        ) (
            "drain initialization $Schedule failure must preserve truthful ownership, " +
            "diagnostics, bounded teardown, and cleanup convergence; observed=$message"
        )

        switch ($Schedule) {
            "zero" {
                Assert-Contract (
                    -not $probe.OutputTaskOwned -and
                    -not $probe.ErrorTaskOwned -and
                    -not $probe.OutputTaskObserved -and
                    -not $probe.ErrorTaskObserved -and
                    $probe.OutputDrainInitialization -ceq "not-started" -and
                    $probe.ErrorDrainInitialization -ceq "not-started" -and
                    -not $probe.OutputCaptured
                ) "zero-drain initialization must record both drains as not owned"
            }
            "partial" {
                Assert-Contract (
                    $probe.OutputTaskOwned -and
                    $probe.OutputTaskObserved -and
                    $probe.OutputTaskSucceeded -and
                    -not $probe.ErrorTaskOwned -and
                    -not $probe.ErrorTaskObserved -and
                    $probe.ErrorDrainInitialization -ceq "not-started" -and
                    -not $probe.OutputCaptured
                ) "partial drain initialization must observe the owned stdout task only"
            }
            "both" {
                Assert-Contract (
                    $probe.OutputTaskOwned -and
                    $probe.ErrorTaskOwned -and
                    $probe.OutputTaskObserved -and
                    $probe.ErrorTaskObserved -and
                    $probe.OutputTaskSucceeded -and
                    $probe.ErrorTaskSucceeded -and
                    $probe.OutputCaptured
                ) "both initialized drains must be observed and captured before reclaim"
            }
        }
    }
    finally {
        if ($null -ne $probe -and $null -ne $probe.Process -and -not $probe.WasReclaimed) {
            try {
                if (-not $probe.Process.HasExited) {
                    $probe.Process.Kill($true)
                    $null = $probe.Process.WaitForExit(5000)
                }
            }
            catch {
            }
            try {
                $probe.Process.Dispose()
            }
            catch {
            }
        }
    }
}

function Assert-ContractProbeTeardownRegression {
    param([Parameter(Mandatory)][string]$ProbeRoot)

    New-Item -ItemType Directory -Force -Path $ProbeRoot | Out-Null
    $script:contractProbeCleanupBlocked = $false
    foreach ($schedule in @("zero", "partial", "both")) {
        Assert-ContractProbeDrainInitializationFailure -ProbeRoot $ProbeRoot -Schedule $schedule
    }
    $processes = [System.Collections.Generic.List[object]]::new()
    $probes = [System.Collections.Generic.List[object]]::new()
    $pendingStdout = $null
    try {
        $exitedStart = [System.Diagnostics.ProcessStartInfo]::new()
        $exitedStart.FileName = Join-Path $PSHOME "pwsh.exe"
        $exitedStart.UseShellExecute = $false
        $exitedStart.RedirectStandardOutput = $true
        $exitedStart.RedirectStandardError = $true
        foreach ($argument in @("-NoLogo","-NoProfile","-Command","exit 0")) {
            $exitedStart.ArgumentList.Add($argument)
        }
        $exited = [System.Diagnostics.Process]::Start($exitedStart)
        Assert-Contract ($null -ne $exited -and $exited.WaitForExit(5000)) `
            "faulted-drain fixture process must exit"
        $processes.Add($exited) | Out-Null

        $liveStart = [System.Diagnostics.ProcessStartInfo]::new()
        $liveStart.FileName = Join-Path $PSHOME "pwsh.exe"
        $liveStart.UseShellExecute = $false
        $liveStart.RedirectStandardOutput = $true
        $liveStart.RedirectStandardError = $true
        foreach ($argument in @("-NoLogo","-NoProfile","-Command","Start-Sleep -Seconds 30")) {
            $liveStart.ArgumentList.Add($argument)
        }
        $live = [System.Diagnostics.Process]::Start($liveStart)
        Assert-Contract ($null -ne $live -and -not $live.HasExited) `
            "faulted-drain fixture live process must start"
        $processes.Add($live) | Out-Null
        $faulted = New-ContractProbeFixture -Name "faulted-one" -Process $exited `
            -StartInfo $exitedStart -ProbeRoot $ProbeRoot `
            -OutputTask ([System.Threading.Tasks.Task]::FromException[string](
                [System.InvalidOperationException]::new("synthetic-workspace-drain-failure")
            )) -ErrorTask ([System.Threading.Tasks.Task]::FromResult([string]"faulted stderr"))
        $liveProbe = New-ContractProbeFixture -Name "live-two" -Process $live `
            -StartInfo $liveStart -ProbeRoot $ProbeRoot
        $probes.Add($faulted) | Out-Null
        $probes.Add($liveProbe) | Out-Null
        $failure = $null
        try { Stop-ContractProbes -Probe @($faulted,$liveProbe) -TimeoutMilliseconds 1000 }
        catch { $failure = $_ }
        $message = if ($null -eq $failure) { "<none>" } else { $failure.Exception.Message }
        Assert-Contract (
            $null -ne $failure -and
            -not $faulted.OutputCaptured -and
            $faulted.OutputTaskObserved -and
            $faulted.ErrorTaskObserved -and
            $faulted.WasReclaimed -and
            $liveProbe.WasReclaimed -and
            $message -match 'synthetic-workspace-drain-failure' -and
            $message -match 'probe=faulted-one' -and
            $message -match 'probe=live-two' -and
            $message -match 'ArgumentList=' -and
            $message -match 'stdout<<' -and
            $message -match 'stderr<<' -and
            $message -match 'lastMilestone='
        ) "faulted drain must retain first error and reclaim later owned children; observed=$message"

        $startupOwnedStart = [System.Diagnostics.ProcessStartInfo]::new()
        $startupOwnedStart.FileName = Join-Path $PSHOME "pwsh.exe"
        $startupOwnedStart.UseShellExecute = $false
        $startupOwnedStart.RedirectStandardOutput = $true
        $startupOwnedStart.RedirectStandardError = $true
        foreach ($argument in @("-NoLogo", "-NoProfile", "-Command", "Start-Sleep -Seconds 30")) {
            $startupOwnedStart.ArgumentList.Add($argument)
        }
        $startupOwned = [System.Diagnostics.Process]::Start($startupOwnedStart)
        Assert-Contract ($null -ne $startupOwned -and -not $startupOwned.HasExited) `
            "startup-failure fixture first owned process must start"
        $processes.Add($startupOwned) | Out-Null
        $startupOwnedProbe = New-ContractProbeFixture -Name "startup-owned-one" `
            -Process $startupOwned -StartInfo $startupOwnedStart -ProbeRoot $ProbeRoot
        $startupOwnedProbe.State = "Started"
        $startupOwnedProbe.Progress = "process.started"
        $startupOwnedProbe.LastMilestone = "process.started"
        Set-Content -LiteralPath $startupOwnedProbe.Trace -Encoding utf8NoBOM `
            -Value "one|process.started"
        $probes.Add($startupOwnedProbe) | Out-Null

        $startupFaultStart = [System.Diagnostics.ProcessStartInfo]::new()
        $startupFaultStart.FileName = Join-Path $PSHOME "pwsh.exe"
        $startupFaultStart.UseShellExecute = $false
        $startupFaultStart.RedirectStandardOutput = $true
        $startupFaultStart.RedirectStandardError = $true
        foreach ($argument in @("-NoLogo", "-NoProfile", "-Command", "Start-Sleep -Seconds 30")) {
            $startupFaultStart.ArgumentList.Add($argument)
        }
        $startupFailure = $null
        $startupTimer = [System.Diagnostics.Stopwatch]::StartNew()
        try {
            Start-ContractProbe -Name "startup-fault-two" -StartInfo $startupFaultStart `
                -Arguments @($startupFaultStart.ArgumentList) `
                -Start (Join-Path $ProbeRoot "startup-fault-two-start.txt") `
                -Ready (Join-Path $ProbeRoot "startup-fault-two-ready.txt") `
                -Release (Join-Path $ProbeRoot "startup-fault-two-release.txt") `
                -Result (Join-Path $ProbeRoot "startup-fault-two-result.txt") `
                -Trace (Join-Path $ProbeRoot "startup-fault-two-trace.txt") `
                -TeardownDeadline (New-ContractProbeDeadline -Name "workspace startup fault teardown" `
                    -TimeoutMilliseconds 1000) -OwnedProbe @($startupOwnedProbe) `
                -AfterDrainsInitialized {
                    param([object]$Probe)
                    throw "synthetic-workspace-startup-drain-initialization-failure"
                } | Out-Null
        }
        catch {
            $startupFailure = $_
        }
        finally {
            $startupTimer.Stop()
        }
        $startupMessage = if ($null -eq $startupFailure) { "<none>" } else {
            $startupFailure.Exception.Message
        }
        Assert-Contract (
            $null -ne $startupFailure -and
            $startupOwnedProbe.KillLaunchAttempted -and
            $startupOwnedProbe.WasReclaimed -and
            $startupTimer.ElapsedMilliseconds -le 2000 -and
            $startupMessage -match "synthetic-workspace-startup-drain-initialization-failure" -and
            $startupMessage -match "startup-owned-one" -and
            $startupMessage -match "startup-fault-two"
        ) (
            "a workspace startup failure must launch and reclaim every already-owned child " +
            "before its shared teardown deadline can be consumed; " +
            "elapsedMs=$($startupTimer.ElapsedMilliseconds) observed=$startupMessage"
        )

        $pendingStart = [System.Diagnostics.ProcessStartInfo]::new()
        $pendingStart.FileName = Join-Path $PSHOME "pwsh.exe"
        $pendingStart.UseShellExecute = $false
        $pendingStart.RedirectStandardOutput = $true
        $pendingStart.RedirectStandardError = $true
        foreach ($argument in @("-NoLogo","-NoProfile","-Command","exit 0")) {
            $pendingStart.ArgumentList.Add($argument)
        }
        $pending = [System.Diagnostics.Process]::Start($pendingStart)
        Assert-Contract ($null -ne $pending -and $pending.WaitForExit(5000)) `
            "pending-drain fixture exited process must exit"
        $processes.Add($pending) | Out-Null
        $pendingLiveStart = [System.Diagnostics.ProcessStartInfo]::new()
        $pendingLiveStart.FileName = Join-Path $PSHOME "pwsh.exe"
        $pendingLiveStart.UseShellExecute = $false
        $pendingLiveStart.RedirectStandardOutput = $true
        $pendingLiveStart.RedirectStandardError = $true
        foreach ($argument in @("-NoLogo","-NoProfile","-Command","Start-Sleep -Seconds 30")) {
            $pendingLiveStart.ArgumentList.Add($argument)
        }
        $pendingLive = [System.Diagnostics.Process]::Start($pendingLiveStart)
        Assert-Contract ($null -ne $pendingLive -and -not $pendingLive.HasExited) `
            "pending-drain fixture second live process must start"
        $processes.Add($pendingLive) | Out-Null
        $pendingStdout = [System.Threading.Tasks.TaskCompletionSource[string]]::new(
            [System.Threading.Tasks.TaskCreationOptions]::RunContinuationsAsynchronously
        )
        $pendingProbe = New-ContractProbeFixture -Name "pending-one" -Process $pending `
            -StartInfo $pendingStart -ProbeRoot $ProbeRoot -OutputTask $pendingStdout.Task `
            -ErrorTask ([System.Threading.Tasks.Task]::FromResult([string]"pending stderr"))
        $pendingLiveProbe = New-ContractProbeFixture -Name "pending-live-two" -Process $pendingLive `
            -StartInfo $pendingLiveStart -ProbeRoot $ProbeRoot
        $probes.Add($pendingProbe) | Out-Null
        $probes.Add($pendingLiveProbe) | Out-Null
        $pendingFailure = $null
        $timer = [System.Diagnostics.Stopwatch]::StartNew()
        try { Stop-ContractProbes -Probe @($pendingProbe,$pendingLiveProbe) -TimeoutMilliseconds 300 }
        catch { $pendingFailure = $_ }
        finally { $timer.Stop() }
        $pendingMessage = if ($null -eq $pendingFailure) { "<none>" } else {
            $pendingFailure.Exception.Message
        }
        Assert-Contract (
            $null -ne $pendingFailure -and
            -not $pendingProbe.OutputCaptured -and
            -not $pendingProbe.OutputTaskObserved -and
            -not $pendingProbe.WasReclaimed -and
            $pendingLiveProbe.WasReclaimed -and
            $pendingLiveProbe.KillLaunchAttempted -and
            $script:contractProbeCleanupBlocked -and
            $timer.ElapsedMilliseconds -le 1000 -and
            $pendingMessage -match 'pending-one' -and
            $pendingMessage -match 'pending-live-two'
        ) "pending drain must not starve live child kill or claim its own incomplete handle; observed=$pendingMessage"
        Assert-Contract ($pendingStdout.TrySetResult("workspace pending stdout released")) `
            "pending stdout task must complete explicitly"
        Stop-ContractProbes -Probe @($pendingProbe) -TimeoutMilliseconds 1000
        Assert-Contract (
            $pendingProbe.OutputCaptured -and
            $pendingProbe.OutputTaskObserved -and
            $pendingProbe.ErrorTaskObserved -and
            $pendingProbe.WasReclaimed -and
            $pendingProbe.Stdout -ceq "workspace pending stdout released"
        ) "completed pending output must be observed before workspace handle reclaim"
        $script:contractProbeCleanupBlocked = $false
    }
    finally {
        if ($null -ne $pendingStdout) { $pendingStdout.TrySetResult("fixture cleanup") | Out-Null }
        for ($index = 0; $index -lt $processes.Count; $index++) {
            if ($index -lt $probes.Count -and [bool]$probes[$index].WasReclaimed) { continue }
            $process = $processes[$index]
            try {
                if (-not $process.HasExited) {
                    $process.Kill($true)
                    $null = $process.WaitForExit(5000)
                }
            }
            finally { $process.Dispose() }
        }
    }
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

function Get-ContractFileIdentity {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $fsutil = Join-Path $env:SystemRoot "System32/fsutil.exe"
    $identity = @(& $fsutil file queryfileid $Path)
    Assert-Contract (
        $LASTEXITCODE -eq 0 -and
        $identity.Count -eq 1 -and
        -not [string]::IsNullOrWhiteSpace($identity[0])
    ) "contract file identity query must return one NTFS file ID for $Path"
    return $identity[0].Trim()
}

function Get-ContractFileSnapshot {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $item = Get-Item -Force -LiteralPath $Path -ErrorAction Stop
    return [pscustomobject]@{
        Bytes = [long]$item.Length
        Hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
        Identity = Get-ContractFileIdentity -Path $Path
        LastWriteTicks = $item.LastWriteTimeUtc.Ticks
    }
}

$modulePath = Join-Path $PSScriptRoot "windows_workspace.psm1"
$repository = Resolve-Path (Join-Path $PSScriptRoot "..")
$configurationPath = Join-Path $PSScriptRoot "windows_build_environment.json"
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
    "ecw-{0}" -f [guid]::NewGuid().ToString("N").Substring(0, 12)
)
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
Import-Module -Name $modulePath -Force
$workspaceModule = Get-Module windows_workspace

function Invoke-PrivateWorkspaceGates {
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [string]$BaseSha,

        [scriptblock]$GateInvoker,

        [switch]$RequireCleanTree,

        [switch]$RequireStagedCandidate
    )

    & $script:workspaceModule {
        param($Root, $Base, $Invoker, $RequireClean, $RequireStaged)
        $policy = Get-EasyConWindowsGatePolicy -RepositoryRoot $Root
        $outcome = Invoke-EasyConWindowsWorkspaceGates -RepositoryRoot $Root `
            -GateInvoker $Invoker -Policy $policy -BaseSha $Base `
            -RequireCleanTree:$RequireClean -RequireStagedCandidate:$RequireStaged
        if ($null -eq $outcome.Candidate) {
            return
        }
        Add-Member -InputObject $outcome.Candidate -NotePropertyName "BaseCommit" `
            -NotePropertyValue $outcome.BaseCommit -Force
        Add-Member -InputObject $outcome.Candidate -NotePropertyName "GateRecords" `
            -NotePropertyValue @($outcome.Gates) -Force
        return $outcome.Candidate
    } $RepositoryRoot $BaseSha $GateInvoker $RequireCleanTree.IsPresent $RequireStagedCandidate.IsPresent
}

function Invoke-PrivateTargetedCargoGate {
    param(
        [Parameter(Mandatory)]
        [string]$RepositoryRoot,

        [Parameter(Mandatory)]
        [string]$CargoCommand,

        [string[]]$CargoArguments = @(),

        [int]$CargoJobs = 4,

        [scriptblock]$GateInvoker
    )

    & $script:workspaceModule {
        param($Root, $Command, $Arguments, $Jobs, $Invoker)
        Invoke-EasyConWindowsTargetedCargoGate -RepositoryRoot $Root `
            -CargoCommand $Command -CargoJobs $Jobs -CargoArguments $Arguments `
            -GateInvoker $Invoker
    } $RepositoryRoot $CargoCommand $CargoArguments $CargoJobs $GateInvoker
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
        [ValidateSet("Setup", "Verify", "Workspace", "Targeted")]
        [string]$Mode,

        [switch]$LifecycleFailure
    )

    & $script:workspaceModule {
        param($WrapperMode, $FailLifecycle, $ResolvedRepository)

        $originalContext = ${function:Get-EasyConWindowsEnvironmentContext}
        $originalSetupCore = ${function:Invoke-EasyConWindowsSetupCore}
        $originalVerifyCore = ${function:Invoke-EasyConWindowsVerifyCore}
        $originalWorkspaceGates = ${function:Invoke-EasyConWindowsWorkspaceGates}
        $originalTargetedCargoGate = ${function:Invoke-EasyConWindowsTargetedCargoGate}
        $originalLifecycle = ${function:Invoke-EasyConEnvironmentLifecycle}
        $location = [pscustomobject]@{
            CacheRoot = "resolved-cache"
            EnvironmentRoot = "resolved-environment"
            IdentityKey = "resolved-identity"
            WorkspaceKey = "resolved-workspace"
        }
        $context = [pscustomobject]@{
            Repository = $ResolvedRepository
            ConfigurationPath = "resolved-configuration"
            Configuration = [pscustomobject]@{ target = "x86_64-pc-windows-msvc" }
            Fingerprint = [pscustomobject]@{ Value = ("f" * 64) }
            Location = $location
        }
        $state = [pscustomobject]@{
            ContextCall = $null
            LifecycleCall = $null
            ResolvedContext = $context
            SetupCalls = [System.Collections.Generic.List[object]]::new()
            VerifyCalls = [System.Collections.Generic.List[object]]::new()
            WorkspaceCalls = [System.Collections.Generic.List[object]]::new()
            TargetedCalls = [System.Collections.Generic.List[object]]::new()
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
            param(
                $RepositoryRoot,
                $BaseSha,
                [switch]$RequireCleanTree,
                [switch]$RequireStagedCandidate,
                $Policy
            )
            $state.WorkspaceCalls.Add([pscustomobject]@{
                RepositoryRoot = $RepositoryRoot
                BaseSha = $BaseSha
                RequireCleanTree = $RequireCleanTree.IsPresent
                RequireStagedCandidate = $RequireStagedCandidate.IsPresent
                Policy = $Policy
            }) | Out-Null
        }.GetNewClosure()
        $targetedProbe = {
            param($RepositoryRoot, $CargoCommand, $CargoArguments, $CargoJobs)
            $state.TargetedCalls.Add([pscustomobject]@{
                RepositoryRoot = $RepositoryRoot
                CargoCommand = $CargoCommand
                CargoArguments = @($CargoArguments)
                CargoJobs = $CargoJobs
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
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsTargetedCargoGate `
                -Value $targetedProbe
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
                        -RequireCleanTree | Out-Null
                }
                "Targeted" {
                    Invoke-EasyConWindowsWorkspace @parameters -GateMode Targeted `
                        -TargetedCargoCommand test `
                        -TargetedCargoArguments @("-p", "easycon-ecs", "--lib") | Out-Null
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
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsTargetedCargoGate `
                -Value $originalTargetedCargoGate
            Set-Item -LiteralPath Function:script:Invoke-EasyConEnvironmentLifecycle `
                -Value $originalLifecycle
        }
    } $Mode $LifecycleFailure.IsPresent $repository
}

function Invoke-PrivateWorkspaceEvidenceProbe {
    param(
        [Parameter(Mandatory)]
        [string]$ProbeRoot,

        [switch]$RequireCleanTree,

        [switch]$RequireStagedCandidate,

        [switch]$FailGate,

        [string]$BaseSha = ("c" * 40),

        [string]$ResolvedBaseCommit = ("c" * 40),

        [ValidateSet("None", "Verify", "Gates", IgnoreCase = $false)]
        [string]$PolicyMutationPhase = "None",

        [ValidateSet("None", "Json", "PolicyScript", "RunnerSource", IgnoreCase = $false)]
        [string]$CaptureMutationInput = "None"
    )

    $policyRepository = $repository.Path
    if ($PolicyMutationPhase -cne "None" -or $CaptureMutationInput -cne "None") {
        $policyRepository = Join-Path $ProbeRoot "policy-source"
        foreach ($name in @(
            "windows_gate_policy.json",
            "windows_gate_policy.ps1",
            "run_windows_workspace.ps1"
        )) {
            Set-ContractUtf8Text -Path (Join-Path $policyRepository "tools/$name") `
                -Value (Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot $name))
        }
    }

    & $script:workspaceModule {
        param(
            $Root,
            $RequireClean,
            $RequireStaged,
            $ShouldFail,
            $RequestedBaseSha,
            $ResolvedBase,
            $PolicyRepository,
            $MutationPhase,
            $CaptureMutation
        )

        $originalContext = ${function:Get-EasyConWindowsEnvironmentContext}
        $originalVerifyCore = ${function:Invoke-EasyConWindowsVerifyCore}
        $originalWorkspaceGates = ${function:Invoke-EasyConWindowsWorkspaceGates}
        $originalLifecycle = ${function:Invoke-EasyConEnvironmentLifecycle}
        $originalStructuredRecord = ${function:Write-EasyConStructuredRecord}
        $originalPolicyHash = ${function:Get-EasyConGatePolicyHash}
        $originalPolicyScriptPath = $script:EasyConGatePolicyScriptPath
        $hadPolicyScriptSnapshot = Test-Path -LiteralPath Variable:script:EasyConGatePolicyScriptSnapshot
        $originalPolicyScriptSnapshot = if ($hadPolicyScriptSnapshot) {
            $script:EasyConGatePolicyScriptSnapshot
        }
        else {
            $null
        }
        $hadRunnerSnapshot = Test-Path -LiteralPath Variable:script:EasyConGatePolicyRunnerSnapshot
        $originalRunnerSnapshot = if ($hadRunnerSnapshot) {
            $script:EasyConGatePolicyRunnerSnapshot
        }
        else {
            $null
        }
        $workspaceRoot = Join-Path $Root "workspace"
        New-Item -ItemType Directory -Force -Path $workspaceRoot | Out-Null
        $location = [pscustomobject]@{
            CacheRoot = (Join-Path $Root "cache")
            EnvironmentRoot = (Join-Path $Root "environment")
            IdentityKey = "contract-environment"
            WorkspaceKey = "contract-workspace"
            WorkspaceRoot = $workspaceRoot
        }
        $context = [pscustomobject]@{
            Repository = $PolicyRepository
            ConfigurationPath = "resolved-configuration"
            Configuration = [pscustomobject]@{ target = "x86_64-pc-windows-msvc" }
            Fingerprint = [pscustomobject]@{ Value = ("f" * 64) }
            Location = $location
        }
        $state = [pscustomobject]@{
            VerifyCalls = 0
            GateCalls = 0
            GateCall = $null
            Records = [System.Collections.Generic.List[object]]::new()
            Failure = $null
            WorkspaceRoot = $workspaceRoot
            Tree = ("b" * 40)
            RequestedBaseSha = $RequestedBaseSha
            ResolvedBaseCommit = $ResolvedBase
            PolicyMutated = $false
            PolicyHashBefore = $null
            PolicyHashAfter = $null
        }
        $policyInputPaths = [ordered]@{
            Json = Join-Path $PolicyRepository "tools/windows_gate_policy.json"
            PolicyScript = Join-Path $PolicyRepository "tools/windows_gate_policy.ps1"
            RunnerSource = Join-Path $PolicyRepository "tools/run_windows_workspace.ps1"
        }
        $policyPath = $policyInputPaths.Json
        if ($MutationPhase -cne "None") {
            $script:EasyConGatePolicyScriptPath = $policyInputPaths.PolicyScript
        }
        if ($MutationPhase -cne "None" -or $CaptureMutation -cne "None") {
            $strictUtf8 = [System.Text.UTF8Encoding]::new($false, $true)
            $newSnapshot = {
                param($Path, $RelativePath)

                $bytes = [System.IO.File]::ReadAllBytes($Path)
                return [pscustomobject]@{
                    Path = $Path
                    RelativePath = $RelativePath
                    Text = $strictUtf8.GetString($bytes)
                    Bytes = $bytes
                    Sha256 = [System.Convert]::ToHexString(
                        [System.Security.Cryptography.SHA256]::HashData($bytes)
                    ).ToLowerInvariant()
                }
            }.GetNewClosure()
            $script:EasyConGatePolicyScriptPath = $policyInputPaths.PolicyScript
            $script:EasyConGatePolicyScriptSnapshot = & $newSnapshot `
                $policyInputPaths.PolicyScript "tools/windows_gate_policy.ps1"
            $script:EasyConGatePolicyRunnerSnapshot = & $newSnapshot `
                $policyInputPaths.RunnerSource "tools/run_windows_workspace.ps1"
        }
        $state.PolicyHashBefore = (Get-EasyConGatePolicyHash -RepositoryRoot $PolicyRepository).Value
        $mutatePolicy = {
            if (-not $state.PolicyMutated) {
                $target = if ($CaptureMutation -ceq "None") {
                    $policyPath
                }
                else {
                    [string]$policyInputPaths[$CaptureMutation]
                }
                [System.IO.File]::AppendAllText(
                    $target,
                    " ",
                    [System.Text.UTF8Encoding]::new($false, $true)
                )
                $state.PolicyMutated = $true
            }
        }.GetNewClosure()
        $contextProbe = {
            param($RepositoryRoot, $ConfigurationPath, $CacheRoot)
            $null = $RepositoryRoot, $ConfigurationPath, $CacheRoot
            return $context
        }.GetNewClosure()
        $verifyProbe = {
            param($RepositoryRoot, $ConfigurationPath, $CacheRoot, $VsWherePath, $Context)
            $null = $RepositoryRoot, $ConfigurationPath, $CacheRoot, $VsWherePath
            $state.VerifyCalls++
            Assert-Contract ([object]::ReferenceEquals($Context, $context)) `
                "evidence probe must keep the resolved context"
            if ($MutationPhase -ceq "Verify") {
                & $mutatePolicy
            }
            return [pscustomobject]@{ Status = "ready" }
        }.GetNewClosure()
        $gatesProbe = {
            param(
                $RepositoryRoot,
                $BaseSha,
                [switch]$RequireCleanTree,
                [switch]$RequireStagedCandidate,
                $Policy
            )
            $state.GateCalls++
            $state.GateCall = [pscustomobject]@{
                RepositoryRoot = $RepositoryRoot
                BaseSha = $BaseSha
                RequireCleanTree = $RequireCleanTree.IsPresent
                RequireStagedCandidate = $RequireStagedCandidate.IsPresent
                Policy = $Policy
            }
            if ($ShouldFail) {
                throw "synthetic candidate gate failure"
            }
            $candidate = $null
            if ($RequireClean -or $RequireStaged) {
                $candidate = [pscustomobject]@{
                    CandidateMode = if ($RequireStaged) {
                        "staged-candidate"
                }
                else {
                    "clean-tree"
                }
                    HeadCommit = ("a" * 40)
                    Tree = $state.Tree
                }
            }
            if ($MutationPhase -ceq "Gates") {
                & $mutatePolicy
            }
            return [pscustomobject]@{
                Candidate = $candidate
                BaseCommit = if ($null -ne $candidate) { $state.ResolvedBaseCommit } else { $null }
                Gates = @([pscustomobject]@{
                    name = "synthetic contract gate"
                    status = "passed"
                    durationMs = 0L
                })
                Git = $null
            }
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
            $null = $Location, $SetupAction, $LeaseTimeoutMilliseconds
            Assert-Contract ($Mode -ceq "Workspace") `
                "evidence probe must use the Workspace lifecycle"
            $summary = & $VerifyAction
            & $WorkspaceAction $summary | Out-Null
            return $summary
        }.GetNewClosure()
        $recordProbe = {
            param($Kind, $Value)
            $state.Records.Add([pscustomobject]@{
                Kind = $Kind
                Value = $Value
            }) | Out-Null
        }.GetNewClosure()
        $policyHashProbe = {
            param(
                [string]$RepositoryRoot,
                [object[]]$Snapshots
            )

            if ($CaptureMutation -cne "None") {
                & $mutatePolicy
            }
            if ($PSBoundParameters.ContainsKey("Snapshots")) {
                return & $originalPolicyHash -RepositoryRoot $RepositoryRoot -Snapshots $Snapshots
            }
            return & $originalPolicyHash -RepositoryRoot $RepositoryRoot
        }.GetNewClosure()

        try {
            Set-Item -LiteralPath Function:script:Get-EasyConWindowsEnvironmentContext `
                -Value $contextProbe
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsVerifyCore `
                -Value $verifyProbe
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsWorkspaceGates `
                -Value $gatesProbe
            Set-Item -LiteralPath Function:script:Invoke-EasyConEnvironmentLifecycle `
                -Value $lifecycleProbe
            Set-Item -LiteralPath Function:script:Write-EasyConStructuredRecord `
                -Value $recordProbe
            if ($CaptureMutation -cne "None") {
                Set-Item -LiteralPath Function:script:Get-EasyConGatePolicyHash `
                    -Value $policyHashProbe
            }
            try {
                Invoke-EasyConWindowsWorkspace -RepositoryRoot "input-repository" `
                    -ConfigurationPath "input-configuration" -CacheRoot "input-cache" `
                    -BaseSha $RequestedBaseSha -RequireCleanTree:$RequireClean `
                    -RequireStagedCandidate:$RequireStaged | Out-Null
            }
            catch {
                $state.Failure = $_
            }
            $state.PolicyHashAfter = (Get-EasyConGatePolicyHash -RepositoryRoot $PolicyRepository).Value
            return $state
        }
        finally {
            Set-Item -LiteralPath Function:script:Get-EasyConWindowsEnvironmentContext `
                -Value $originalContext
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsVerifyCore `
                -Value $originalVerifyCore
            Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsWorkspaceGates `
                -Value $originalWorkspaceGates
            Set-Item -LiteralPath Function:script:Invoke-EasyConEnvironmentLifecycle `
                -Value $originalLifecycle
            Set-Item -LiteralPath Function:script:Write-EasyConStructuredRecord `
                -Value $originalStructuredRecord
            Set-Item -LiteralPath Function:script:Get-EasyConGatePolicyHash `
                -Value $originalPolicyHash
            $script:EasyConGatePolicyScriptPath = $originalPolicyScriptPath
            if ($hadPolicyScriptSnapshot) {
                Set-Variable -Scope Script -Name EasyConGatePolicyScriptSnapshot `
                    -Value $originalPolicyScriptSnapshot
            }
            else {
                Remove-Variable -Scope Script -Name EasyConGatePolicyScriptSnapshot `
                    -ErrorAction SilentlyContinue
            }
            if ($hadRunnerSnapshot) {
                Set-Variable -Scope Script -Name EasyConGatePolicyRunnerSnapshot `
                    -Value $originalRunnerSnapshot
            }
            else {
                Remove-Variable -Scope Script -Name EasyConGatePolicyRunnerSnapshot `
                    -ErrorAction SilentlyContinue
            }
        }
    } $ProbeRoot $RequireCleanTree.IsPresent $RequireStagedCandidate.IsPresent $FailGate.IsPresent `
        $BaseSha $ResolvedBaseCommit $policyRepository $PolicyMutationPhase $CaptureMutationInput
}

function Assert-WorkspacePolicySnapshotCaptureFailsClosed {
    param(
        [Parameter(Mandatory)]
        [ValidateSet("Json", "PolicyScript", "RunnerSource", IgnoreCase = $false)]
        [string]$MutationTarget
    )

    $captured = Invoke-PrivateWorkspaceEvidenceProbe -ProbeRoot (
        Join-Path $temporaryRoot ("workspace policy snapshot capture {0}" -f $MutationTarget)
    ) -RequireStagedCandidate -CaptureMutationInput $MutationTarget
    Assert-Contract (
        $null -ne $captured.Failure -and
        $captured.Failure.Exception.Message -match
            "policy inputs changed during snapshot capture"
    ) "$MutationTarget mutation must fail while capturing the policy snapshot"
    Assert-Contract (
        $captured.PolicyMutated -and
        $captured.PolicyHashBefore -cne $captured.PolicyHashAfter
    ) "$MutationTarget mutation must change the temporary policy input hash"
    Assert-Contract ($captured.VerifyCalls -eq 0 -and $captured.GateCalls -eq 0) `
        "$MutationTarget mutation must enter neither Verify nor a gate"
    Assert-Contract (
        -not (Test-Path -LiteralPath (Join-Path $captured.WorkspaceRoot "evidence")) -and
        @($captured.Records | Where-Object { $_.Kind -ceq "workspace" }).Count -eq 0
    ) "$MutationTarget mutation must publish neither evidence nor a passed record"
}

function Invoke-RunnerContractProcess {
    param(
        [Parameter(Mandatory)]
        [string[]]$Arguments
    )

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = Join-Path $PSHOME "pwsh.exe"
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in @(
        "-NoLogo",
        "-NoProfile",
        "-File",
        (Join-Path $PSScriptRoot "run_windows_workspace.ps1")
    ) + $Arguments) {
        $startInfo.ArgumentList.Add([string]$argument)
    }
    $process = [System.Diagnostics.Process]::Start($startInfo)
    try {
        Assert-Contract ($process.WaitForExit(15000)) `
            "runner contract process must finish without environment preparation"
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Output = $process.StandardOutput.ReadToEnd()
            Error = $process.StandardError.ReadToEnd()
        }
    }
    finally {
        if (-not $process.HasExited) {
            $process.Kill($true)
            $process.WaitForExit()
        }
        $process.Dispose()
    }
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
    Invoke-ContractCase -Name "marker-wait-observes-forced-check-to-wait-interleaving" -Action {
        $markerRoot = Join-Path $temporaryRoot "forced marker wait interleaving"
        $markerPath = Join-Path $markerRoot "created-before-registration.txt"
        New-Item -ItemType Directory -Force -Path $markerRoot | Out-Null
        Wait-ContractFileCreated -Path $markerPath -TimeoutMilliseconds 250 `
            -BeforeWaitRegistration {
                [System.IO.File]::WriteAllText(
                    $markerPath,"created",[System.Text.UTF8Encoding]::new($false)
                )
            }.GetNewClosure()
        Assert-Contract (Test-Path -LiteralPath $markerPath -PathType Leaf) `
            "forced check-to-wait marker must remain observable"
    }

    Invoke-ContractCase -Name "probe-readiness-final-observation-prefers-early-exit" -Action {
        Assert-ContractProbeReadinessDeadlineRegression -ProbeRoot (
            Join-Path $temporaryRoot "workspace readiness deadline regression with spaces"
        )
    }

    Invoke-ContractCase -Name "probe-teardown-is-exhaustive-bounded-and-diagnostic" -Action {
        Assert-ContractProbeTeardownRegression -ProbeRoot (
            Join-Path $temporaryRoot "workspace teardown regression with spaces"
        )
    }

    Invoke-ContractCase -Name "strict-environment-configuration" -Action {
        $configurationText = Get-Content -Raw -LiteralPath $configurationPath
        $configuration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
        Assert-Contract ($configuration.version -eq 5) "environment config schema must be v5"
        Assert-Contract ($configuration.vcpkg.internalTools.Count -eq 4) `
            "CMake, Ninja, 7-Zip, and its 7zr bootstrap must be audited"
        Assert-Contract ($configuration.vcpkg.nativeDependencies.Count -eq 3) `
            "direct native dependencies must be audited"
        Assert-Contract ($configuration.fingerprintInputs.path -ccontains "Cargo.lock") `
            "Cargo.lock must invalidate the prepared dependency environment"
        Assert-Contract ($configuration.fingerprintInputs.path -ccontains "tools/windows_workspace.psm1") `
            "environment implementation changes must invalidate the prepared environment"
        Assert-Contract (
            $configuration.fingerprintInputs.path -ccontains "crates/easycon-file-identity/Cargo.toml"
        ) "the file-identity manifest must invalidate the prepared environment"

        $reversedConfiguration = $configurationText | ConvertFrom-Json -Depth 32
        [array]::Reverse($reversedConfiguration.fingerprintInputs)
        $reversedFingerprintInputs = $reversedConfiguration | ConvertTo-Json -Depth 32
        $withoutFileIdentityConfiguration = $configurationText | ConvertFrom-Json -Depth 32
        $withoutFileIdentityConfiguration.fingerprintInputs = @(
            $withoutFileIdentityConfiguration.fingerprintInputs | Where-Object {
                $_.path -cne "crates/easycon-file-identity/Cargo.toml"
            }
        )
        $withoutFileIdentityManifest = $withoutFileIdentityConfiguration | ConvertTo-Json -Depth 32

        $mutations = [ordered]@{
            "duplicate key" = $configurationText.Replace(
                '"version": 5', '"version": 5, "version": 5'
            )
            "old schema" = $configurationText.Replace('"version": 5', '"version": 4')
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
            "missing file identity manifest" = $withoutFileIdentityManifest
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
        $policyImportState = & $script:workspaceModule {
            [pscustomobject]@{
                PolicyScriptPath = $script:EasyConGatePolicyScriptPath
                PolicySnapshot = $script:EasyConGatePolicyScriptSnapshot
                RunnerSnapshot = $script:EasyConGatePolicyRunnerSnapshot
            }
        }
        Assert-Contract (
            $policyImportState.PolicyScriptPath -ceq (Join-Path $PSScriptRoot "windows_gate_policy.ps1") -and
            $null -ne $policyImportState.PolicySnapshot -and
            $null -ne $policyImportState.RunnerSnapshot -and
            $policyImportState.PolicySnapshot.RelativePath -ceq "tools/windows_gate_policy.ps1" -and
            $policyImportState.RunnerSnapshot.RelativePath -ceq "tools/run_windows_workspace.ps1"
        ) "direct module import must retain policy file scope and initialize both private source snapshots"

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

        $workspaceCommand = Get-Command -Name "Invoke-EasyConWindowsWorkspace" `
            -Module windows_workspace -ErrorAction Stop
        Assert-Contract (-not $workspaceCommand.Parameters.ContainsKey("GateInvoker")) `
            "the public Workspace wrapper must not expose the test-only GateInvoker bypass"

        $started = [pscustomobject]@{ Gates = 0 }
        Assert-Throws -Pattern "parameter name 'GateInvoker'|cannot be found.*GateInvoker" -Action {
            Invoke-EasyConWindowsWorkspace -RepositoryRoot $repository `
                -ConfigurationPath $configurationPath `
                -CacheRoot (Join-Path $temporaryRoot "public workspace bypass cache") `
                -GateInvoker {
                    param($Name, $Program, $Arguments, $Root)
                    $started.Gates++
                }.GetNewClosure()
        }
        Assert-Contract ($started.Gates -eq 0) `
            "a rejected external Workspace GateInvoker must start zero Verify or gate work"

        $targetedStarted = [pscustomobject]@{ Gates = 0 }
        Assert-Throws -Pattern "parameter name 'GateInvoker'|cannot be found.*GateInvoker" -Action {
            Invoke-EasyConWindowsWorkspace -RepositoryRoot $repository `
                -ConfigurationPath $configurationPath `
                -CacheRoot (Join-Path $temporaryRoot "public targeted bypass cache") `
                -GateMode Targeted -TargetedCargoCommand test `
                -TargetedCargoArguments @("-p", "easycon-ecs") -GateInvoker {
                    param($Name, $Program, $Arguments, $Root)
                    $targetedStarted.Gates++
                }.GetNewClosure()
        }
        Assert-Contract ($targetedStarted.Gates -eq 0) `
            "a rejected external Targeted GateInvoker must start zero Verify or gate work"
    }

    Invoke-ContractCase -Name "runner-ast-source-handoff-remains-private-and-byte-bound" -Action {
        $runnerPath = Join-Path $PSScriptRoot "run_windows_workspace.ps1"
        $runnerCommand = Get-Command -Name $runnerPath -ErrorAction Stop
        $runnerText = [string]$runnerCommand.ScriptBlock.Ast.Extent.Text
        Assert-Contract (
            $runnerCommand.ScriptBlock.Ast.Extent.StartOffset -eq 0 -and
            $runnerCommand.ScriptBlock.Ast.Extent.EndOffset -eq $runnerText.Length
        ) "real runner AST must expose its complete parsed source"
        $handoff = & $script:workspaceModule {
            param($Path, $Text)

            $defaultSnapshot = $script:EasyConGatePolicyRunnerSnapshot
            try {
                Set-EasyConGatePolicyRunnerSnapshot -Path $Path -Text $Text
                return [pscustomobject]@{
                    DefaultSnapshot = $defaultSnapshot
                    OverlaySnapshot = $script:EasyConGatePolicyRunnerSnapshot
                }
            }
            finally {
                $script:EasyConGatePolicyRunnerSnapshot = $defaultSnapshot
            }
        } $runnerPath $runnerText
        $rawRunnerHash = [System.Convert]::ToHexString(
            [System.Security.Cryptography.SHA256]::HashData(
                [System.IO.File]::ReadAllBytes($runnerPath)
            )
        ).ToLowerInvariant()
        Assert-Contract (
            -not [object]::ReferenceEquals($handoff.DefaultSnapshot, $handoff.OverlaySnapshot) -and
            $handoff.OverlaySnapshot.Path -ceq $runnerPath -and
            $handoff.OverlaySnapshot.RelativePath -ceq "tools/run_windows_workspace.ps1" -and
            $handoff.OverlaySnapshot.Text -ceq $runnerText -and
            $handoff.OverlaySnapshot.Sha256 -ceq $rawRunnerHash
        ) "real runner AST handoff must privately replace the default with byte-bound source identity"
    }

    Invoke-ContractCase -Name "public-wrappers-forward-controlled-lifecycle" -Action {
        foreach ($mode in @("Setup", "Verify", "Workspace", "Targeted")) {
            $state = Invoke-PrivatePublicWrapperProbe -Mode $mode
            Assert-Contract (
                $state.ContextCall.RepositoryRoot -ceq "input-repository" -and
                $state.ContextCall.ConfigurationPath -ceq "input-configuration" -and
                $state.ContextCall.CacheRoot -ceq "input-cache"
            ) "$mode must forward caller inputs to context resolution"
            $expectedLifecycleMode = if ($mode -ceq "Targeted") { "Workspace" } else { $mode }
            Assert-Contract (
                $state.LifecycleCall.Mode -ceq $expectedLifecycleMode -and
                $state.LifecycleCall.Location.IdentityKey -ceq "resolved-identity" -and
                $state.LifecycleCall.LeaseTimeoutMilliseconds -eq 321
            ) "$mode must forward the resolved identity and controlled lifecycle mode"
            Assert-Contract ($state.VerifyCalls.Count -eq 1) `
                "$mode must execute exactly one controlled Verify action"
            $verify = $state.VerifyCalls[0]
            Assert-Contract (
                $verify.RepositoryRoot -ceq $state.ResolvedContext.Repository -and
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
                    $setup.RepositoryRoot -ceq $state.ResolvedContext.Repository -and
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
                    $workspace.RepositoryRoot -ceq $state.ResolvedContext.Repository -and
                    $workspace.BaseSha -ceq ("a" * 40) -and
                    $workspace.RequireCleanTree -and
                    $workspace.Policy.CargoJobs -eq 4
                ) "Workspace must forward base, clean-tree, and fixed policy parameters"
            }
            else {
                Assert-Contract ($state.WorkspaceCalls.Count -eq 0) `
                    "$mode must not execute Workspace gates"
            }
            if ($mode -ceq "Targeted") {
                Assert-Contract ($state.TargetedCalls.Count -eq 1) `
                    "Targeted must execute exactly one Cargo gate action after Verify"
                $targeted = $state.TargetedCalls[0]
                Assert-Contract (
                    $targeted.RepositoryRoot -ceq $state.ResolvedContext.Repository -and
                    $targeted.CargoCommand -ceq "test" -and
                    ($targeted.CargoArguments -join "`0") -ceq
                        (@("-p", "easycon-ecs", "--lib") -join "`0") -and
                    $targeted.CargoJobs -eq 4
                ) "Targeted must preserve each controlled Cargo argument token and fixed jobs policy"
            }
            else {
                Assert-Contract ($state.TargetedCalls.Count -eq 0) `
                    "$mode must not execute Targeted gates"
            }

            Assert-Throws -Pattern "synthetic public $expectedLifecycleMode lifecycle failure" -Action {
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
                    $fixtureGit = (Get-Command git.exe -ErrorAction Stop).Source
                    & $fixtureGit init --quiet $root
                    Assert-Contract ($LASTEXITCODE -eq 0) `
                        "the mocked pinned checkout must materialize its physical .git directory"
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

        $newPreGitCheckout = {
            param([Parameter(Mandatory)][string]$Label)

            $caseRoot = Join-Path $temporaryRoot (
                "vcpkg pre-git binding {0}-{1}" -f $Label, [guid]::NewGuid().ToString("N").Substring(0, 8)
            )
            $root = Join-Path $caseRoot "checkout"
            foreach ($relative in @(
                ".vcpkg-root",
                "bootstrap-vcpkg.bat",
                "bootstrap-vcpkg.sh",
                "scripts/buildsystems/vcpkg.cmake",
                "scripts/ordinary-tracked.txt"
            )) {
                Set-ContractFile -Path (Join-Path $root $relative) -Value "contract"
            }
            & $git init --quiet $root
            Assert-Contract ($LASTEXITCODE -eq 0) "pre-Git vcpkg fixture must initialize Git"
            & $git -C $root add -- .
            Assert-Contract ($LASTEXITCODE -eq 0) "pre-Git vcpkg fixture must stage required files"
            & $git -c user.name=EasyConContract -c user.email=contract@example.invalid `
                -C $root commit --quiet -m "pre-Git binding fixture"
            Assert-Contract ($LASTEXITCODE -eq 0) "pre-Git vcpkg fixture must commit required files"
            return [pscustomobject]@{
                CaseRoot = $caseRoot
                Root = $root
                Commit = @(& $git -C $root rev-parse HEAD)[0].Trim()
            }
        }.GetNewClosure()

        $readShareLockBehavior = @(& $workspaceModule {
            param([Parameter(Mandatory)][string]$Root)

            Initialize-EasyConPreparedTreeAuditor
            $flags = [System.Reflection.BindingFlags]::NonPublic -bor
                [System.Reflection.BindingFlags]::Static
            $create = [EasyCon.WindowsWorkspace.PreparedTreeAuditor].GetMethod(
                "CreateFile",
                $flags
            )
            if ($null -eq $create) {
                throw "prepared tree auditor is missing its private CreateFile binding probe"
            }
            $probeRoot = Join-Path $Root "read-share-lock-behavior"
            [System.IO.Directory]::CreateDirectory($probeRoot) | Out-Null
            $open = {
                param(
                    [Parameter(Mandatory)][string]$Path,
                    [Parameter(Mandatory)][uint32]$Access,
                    [Parameter(Mandatory)][bool]$IsDirectory
                )

                [uint32]$flags = 0x00200000
                if ($IsDirectory) {
                    $flags = $flags -bor 0x02000000
                }
                [object[]]$arguments = @(
                    $Path,
                    $Access,
                    [uint32]1,
                    [IntPtr]::Zero,
                    [uint32]3,
                    $flags,
                    [IntPtr]::Zero
                )
                $handle = [Microsoft.Win32.SafeHandles.SafeFileHandle]$create.Invoke(
                    $null,
                    $arguments
                )
                if ($null -eq $handle -or $handle.IsInvalid) {
                    throw "prepared tree auditor read-share lock probe could not open $Path"
                }
                return $handle
            }.GetNewClosure()
            $results = [System.Collections.Generic.List[object]]::new()
            foreach ($probe in @(
                [pscustomobject]@{ Name = "zero"; Access = [uint32]0 },
                [pscustomobject]@{ Name = "attributes"; Access = [uint32]0x80 },
                [pscustomobject]@{ Name = "read-data"; Access = [uint32]1 }
            )) {
                $path = Join-Path $probeRoot "$($probe.Name).txt"
                [System.IO.File]::WriteAllText($path, "contract")
                $handle = & $open $path ([uint32]$probe.Access) $false
                $moveAllowed = $false
                try {
                    Move-Item -LiteralPath $path -Destination "$path.moved" -ErrorAction Stop
                    $moveAllowed = $true
                }
                catch {
                }
                finally {
                    $handle.Dispose()
                }
                $results.Add([pscustomobject]@{
                    Name = $probe.Name
                    MoveAllowed = $moveAllowed
                }) | Out-Null
            }
            $directory = Join-Path $probeRoot "list-directory"
            [System.IO.Directory]::CreateDirectory($directory) | Out-Null
            $directoryHandle = & $open $directory ([uint32]1) $true
            $directoryMoveAllowed = $false
            try {
                Move-Item -LiteralPath $directory -Destination "$directory.moved" -ErrorAction Stop
                $directoryMoveAllowed = $true
            }
            catch {
            }
            finally {
                $directoryHandle.Dispose()
            }
            $results.Add([pscustomobject]@{
                Name = "list-directory"
                MoveAllowed = $directoryMoveAllowed
            }) | Out-Null
            return $results.ToArray()
        } $temporaryRoot)
        $readShareMoves = @{}
        foreach ($probe in $readShareLockBehavior) {
            $readShareMoves[[string]$probe.Name] = [bool]$probe.MoveAllowed
        }
        Assert-Contract (
            $readShareMoves.zero -and
            $readShareMoves.attributes -and
            -not $readShareMoves.'read-data' -and
            -not $readShareMoves.'list-directory'
        ) "zero/attributes handles must allow replacement while read-data/list-directory read-share locks reject it"

        $longPathFixture = & $newPreGitCheckout "extended-path"
        try {
            $longAuditedDirectory = $longPathFixture.Root
            while ($longAuditedDirectory.Length -le 280) {
                $longAuditedDirectory = Join-Path $longAuditedDirectory "audit-path-segment-0123456789"
            }
            $longAuditedFile = Join-Path $longAuditedDirectory "critical-vcpkg-entry.txt"
            Set-ContractFile -Path $longAuditedFile -Value "long physical binding fixture"
            Assert-Contract (
                $longAuditedDirectory.Length -gt 260 -and
                $longAuditedFile.Length -gt 260
            ) "the vcpkg physical-binding fixture must contain local audited directory and file paths longer than MAX_PATH"
            $longRelative = [System.IO.Path]::GetRelativePath(
                $longPathFixture.Root,
                $longAuditedFile
            )
            $longPathProbe = & $workspaceModule {
                param($Root, $TrustedRoot, $LongRelative)

                Initialize-EasyConPreparedTreeAuditor
                $binding = [EasyCon.WindowsWorkspace.PreparedTreeAuditor]::BindVcpkgCheckout(
                    $Root,
                    $TrustedRoot,
                    [string[]]@(".vcpkg-root", $LongRelative)
                )
                try {
                    $audit = $binding.Audit
                    if (-not [string]::IsNullOrWhiteSpace([string]$audit.FailureCategory)) {
                        return [pscustomobject]@{
                            Failure = [string]$audit.FailureMessage
                            RootPath = [string]$audit.RootPath
                            Audit = $audit
                        }
                    }
                    try {
                        $binding.AssertCurrent()
                        return [pscustomobject]@{
                            Failure = $null
                            RootPath = [string]$audit.RootPath
                            Audit = $audit
                        }
                    }
                    catch {
                        return [pscustomobject]@{
                            Failure = $_.Exception.Message
                            RootPath = [string]$audit.RootPath
                            Audit = $audit
                        }
                    }
                }
                finally {
                    $binding.Dispose()
                }
            } $longPathFixture.Root $longPathFixture.CaseRoot $longRelative
            Assert-Contract (
                [string]::IsNullOrWhiteSpace([string]$longPathProbe.Failure) -and
                $longPathProbe.RootPath -ceq $longPathFixture.Root -and
                -not $longPathProbe.RootPath.StartsWith('\\?\') -and
                $longPathProbe.Audit.PhysicalEntriesBound -eq (
                    $longPathProbe.Audit.PhysicalEntriesChecked + 1
                )
            ) (
                "vcpkg full-tree binding must bind and reopen local audited paths longer than MAX_PATH " +
                "without leaking an extended path; failure=$($longPathProbe.Failure)"
            )
            $extendedPathProbe = & $workspaceModule {
                param([Parameter(Mandatory)][string]$LocalPath)

                Initialize-EasyConPreparedTreeAuditor
                $flags = [System.Reflection.BindingFlags]::NonPublic -bor
                    [System.Reflection.BindingFlags]::Static
                $convert = [EasyCon.WindowsWorkspace.PreparedTreeAuditor].GetMethod(
                    "GetWin32ExtendedPath",
                    $flags
                )
                if ($null -eq $convert) {
                    throw "prepared tree auditor is missing its private Win32 extended-path converter"
                }
                $uncPath = '\\server\share\vcpkg\scripts\entry.txt'
                $extendedUncPath = '\\?\UNC\server\share\vcpkg\scripts\entry.txt'
                [object[]]$localArguments = @([string]$LocalPath)
                [object[]]$extendedLocalArguments = @("\\?\$LocalPath")
                [object[]]$uncArguments = @($uncPath)
                [object[]]$extendedUncArguments = @($extendedUncPath)
                $malformedFailure = $null
                try {
                    [void]$convert.Invoke(
                        $null,
                        [object[]]@('\\?\Volume{contract}\vcpkg\entry.txt')
                    )
                }
                catch {
                    $malformedFailure = if ($null -ne $_.Exception.InnerException) {
                        $_.Exception.InnerException.Message
                    }
                    else {
                        $_.Exception.Message
                    }
                }
                return [pscustomobject]@{
                    Local = [string]$convert.Invoke($null, $localArguments)
                    ExtendedLocal = [string]$convert.Invoke($null, $extendedLocalArguments)
                    Unc = [string]$convert.Invoke($null, $uncArguments)
                    ExtendedUnc = [string]$convert.Invoke($null, $extendedUncArguments)
                    MalformedFailure = $malformedFailure
                }
            } $longAuditedFile
            Assert-Contract (
                $extendedPathProbe.Local -ceq "\\?\$longAuditedFile" -and
                $extendedPathProbe.ExtendedLocal -ceq "\\?\$longAuditedFile" -and
                $extendedPathProbe.Unc -ceq '\\?\UNC\server\share\vcpkg\scripts\entry.txt' -and
                $extendedPathProbe.ExtendedUnc -ceq '\\?\UNC\server\share\vcpkg\scripts\entry.txt' -and
                -not [string]::IsNullOrWhiteSpace([string]$extendedPathProbe.MalformedFailure) -and
                -not $extendedPathProbe.MalformedFailure.Contains('\\?\')
            ) "Win32 extended-path conversion must preserve local and UNC logical path semantics"
        }
        finally {
            Remove-Item -LiteralPath $longPathFixture.CaseRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
        $bindingMetricsFixture = & $newPreGitCheckout "binding-metrics"
        try {
            $bindingMetricsRoot = Join-Path $bindingMetricsFixture.Root "bulk-audited-entries"
            for ($directoryIndex = 0; $directoryIndex -lt 32; $directoryIndex++) {
                $directory = Join-Path $bindingMetricsRoot ("{0:d2}" -f $directoryIndex)
                for ($fileIndex = 0; $fileIndex -lt 32; $fileIndex++) {
                    Set-ContractFile -Path (Join-Path $directory ("entry-{0:d2}.txt" -f $fileIndex)) `
                        -Value "bulk physical binding fixture"
                }
            }
            $bindingMetricsTimer = [System.Diagnostics.Stopwatch]::StartNew()
            $bindingMetricsProbe = & $workspaceModule {
                param($Root, $TrustedRoot)

                Initialize-EasyConPreparedTreeAuditor
                $binding = [EasyCon.WindowsWorkspace.PreparedTreeAuditor]::BindVcpkgCheckout(
                    $Root,
                    $TrustedRoot,
                    [string[]]@(
                        ".vcpkg-root",
                        "bootstrap-vcpkg.bat",
                        "bootstrap-vcpkg.sh",
                        "scripts/buildsystems/vcpkg.cmake"
                    )
                )
                try {
                    return $binding.Audit
                }
                finally {
                    $binding.Dispose()
                }
            } $bindingMetricsFixture.Root $bindingMetricsFixture.CaseRoot
            $bindingMetricsTimer.Stop()
            Assert-Contract (
                [string]::IsNullOrWhiteSpace([string]$bindingMetricsProbe.FailureCategory) -and
                $bindingMetricsProbe.ControlledEnumerationPasses -eq 1 -and
                $bindingMetricsProbe.PhysicalEntriesBound -eq (
                    $bindingMetricsProbe.PhysicalEntriesChecked + 1
                ) -and
                $bindingMetricsProbe.PhysicalEntriesBound -ge 1024 -and
                $bindingMetricsProbe.PhysicalCreateFileCalls -eq (
                    $bindingMetricsProbe.PhysicalEntriesBound + 6
                ) -and
                $bindingMetricsProbe.PhysicalBasicInformationQueries -eq (
                    $bindingMetricsProbe.PhysicalCreateFileCalls
                ) -and
                $bindingMetricsProbe.PhysicalBasicInformationQueries -eq (
                    $bindingMetricsProbe.PhysicalEntriesBound + 6
                ) -and
                $bindingMetricsProbe.PhysicalReadDataLockCalls -eq (
                    $bindingMetricsProbe.PhysicalCreateFileCalls
                ) -and
                $bindingMetricsProbe.PhysicalIdentityQueries -eq 18 -and
                $bindingMetricsProbe.PhysicalFinalPathQueries -eq 12 -and
                $bindingMetricsProbe.PhysicalIdentityQueries -lt (
                    $bindingMetricsProbe.PhysicalEntriesBound / 8
                ) -and
                $bindingMetricsProbe.PhysicalFinalPathQueries -lt (
                    $bindingMetricsProbe.PhysicalEntriesBound / 8
                )
            ) "vcpkg full-tree binding must retain every entry with one minimal read-data/list lock and basic query while limiting FileId/final-path work to critical paths and reopens"
            Write-Output (
                "CONTRACT_METRIC name=vcpkg-binding-operations entries={0} create={1} lock={2} basic={3} identity={4} final={5} durationMs={6}" -f
                $bindingMetricsProbe.PhysicalEntriesBound,
                $bindingMetricsProbe.PhysicalCreateFileCalls,
                $bindingMetricsProbe.PhysicalReadDataLockCalls,
                $bindingMetricsProbe.PhysicalBasicInformationQueries,
                $bindingMetricsProbe.PhysicalIdentityQueries,
                $bindingMetricsProbe.PhysicalFinalPathQueries,
                $bindingMetricsTimer.ElapsedMilliseconds
            )
        }
        finally {
            Remove-Item -LiteralPath $bindingMetricsFixture.CaseRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
        $preGitAttacks = @(
            [pscustomobject]@{
                Label = "checkout root"
                Replace = {
                    param($Root, $External)

                    $saved = "$Root.pre-git-original"
                    Move-Item -LiteralPath $Root -Destination $saved
                    New-Item -ItemType Junction -Path $Root -Target $External | Out-Null
                    return $saved
                }
                Restore = {
                    param($Root, $Saved)

                    if (Test-Path -LiteralPath $Root) {
                        $item = Get-Item -Force -LiteralPath $Root
                        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                            Remove-Item -LiteralPath $Root -Force
                        }
                    }
                    if (-not [string]::IsNullOrWhiteSpace($Saved) -and
                        (Test-Path -LiteralPath $Saved)) {
                        Move-Item -LiteralPath $Saved -Destination $Root
                    }
                }
            },
            [pscustomobject]@{
                Label = "checkout .git"
                Replace = {
                    param($Root, $External)

                    $entry = Join-Path $Root ".git"
                    $saved = "$entry.pre-git-original"
                    Move-Item -LiteralPath $entry -Destination $saved
                    New-Item -ItemType Junction -Path $entry -Target $External | Out-Null
                    return $saved
                }
                Restore = {
                    param($Root, $Saved)

                    $entry = Join-Path $Root ".git"
                    if (Test-Path -LiteralPath $entry) {
                        $item = Get-Item -Force -LiteralPath $entry
                        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                            Remove-Item -LiteralPath $entry -Force
                        }
                    }
                    if (-not [string]::IsNullOrWhiteSpace($Saved) -and
                        (Test-Path -LiteralPath $Saved)) {
                        Move-Item -LiteralPath $Saved -Destination $entry
                    }
                }
            },
            [pscustomobject]@{
                Label = "required entry"
                Replace = {
                    param($Root, $External)

                    $entry = Join-Path $Root "scripts/buildsystems/vcpkg.cmake"
                    $saved = "$entry.pre-git-original"
                    Move-Item -LiteralPath $entry -Destination $saved
                    New-Item -ItemType Junction -Path $entry -Target $External | Out-Null
                    return $saved
                }
                Restore = {
                    param($Root, $Saved)

                    $entry = Join-Path $Root "scripts/buildsystems/vcpkg.cmake"
                    if (Test-Path -LiteralPath $entry) {
                        $item = Get-Item -Force -LiteralPath $entry
                        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                            Remove-Item -LiteralPath $entry -Force
                        }
                    }
                    if (-not [string]::IsNullOrWhiteSpace($Saved) -and
                        (Test-Path -LiteralPath $Saved)) {
                        Move-Item -LiteralPath $Saved -Destination $entry
                    }
                }
            },
            [pscustomobject]@{
                Label = "noncritical entry"
                Replace = {
                    param($Root, $External)

                    $entry = Join-Path $Root "scripts/ordinary-tracked.txt"
                    $saved = "$entry.pre-git-original"
                    Move-Item -LiteralPath $entry -Destination $saved
                    New-Item -ItemType Junction -Path $entry -Target $External | Out-Null
                    return $saved
                }
                Restore = {
                    param($Root, $Saved)

                    $entry = Join-Path $Root "scripts/ordinary-tracked.txt"
                    if (Test-Path -LiteralPath $entry) {
                        $item = Get-Item -Force -LiteralPath $entry
                        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                            Remove-Item -LiteralPath $entry -Force
                        }
                    }
                    if (-not [string]::IsNullOrWhiteSpace($Saved) -and
                        (Test-Path -LiteralPath $Saved)) {
                        Move-Item -LiteralPath $Saved -Destination $entry
                    }
                }
            }
        )
        $invokePreGitBindingProbe = {
            param(
                [Parameter(Mandatory)][ValidateSet("Direct", "Verify", "Workspace")][string]$Mode,
                [Parameter(Mandatory)][object]$Attack
            )

            $fixture = & $newPreGitCheckout $Attack.Label.Replace(" ", "-")
            $external = Join-Path $fixture.CaseRoot "reparse-target"
            New-Item -ItemType Directory -Force -Path $external | Out-Null
            $state = [pscustomobject]@{
                ReplacementApplied = $false
                FirstGitCaptureReached = $false
                Saved = $null
                WorkspaceGateActions = 0
                GatesStarted = 0
            }
            $replace = $Attack.Replace
            $restore = $Attack.Restore
            $caseConfiguration = [pscustomobject]@{
                vcpkg = [pscustomobject]@{
                    scriptsCommit = $fixture.Commit
                    toolRelease = $toolRelease
                    toolCommit = $toolCommit
                    windowsAsset = [pscustomobject]@{
                        bytes = [long]$asset.Length
                        sha256 = $assetHash
                    }
                }
            }
            $capture = {
                param($Program, $Arguments, $Description, $WorkingDirectory, $StreamOutput)

                $null = $Program, $WorkingDirectory, $StreamOutput
                switch ($Description) {
                    "vcpkg scripts commit check" {
                        try {
                            $state.Saved = & $replace $fixture.Root $external
                        }
                        catch {
                            throw "pre-Git vcpkg physical binding rejected $($Attack.Label) reparse substitution: $($_.Exception.Message)"
                        }
                        $state.ReplacementApplied = $true
                        $state.FirstGitCaptureReached = $true
                        return $fixture.Commit
                    }
                    "vcpkg required tracked file check" { return [string]$Arguments[-1] }
                    "vcpkg scripts cleanliness check" { return @() }
                    "vcpkg tool version check" {
                        return "vcpkg package management program version $toolRelease-$toolCommit"
                    }
                    default { return @() }
                }
            }.GetNewClosure()
            try {
                $failure = $null
                try {
                    if ($Mode -ceq "Direct") {
                        Invoke-PrivateCommandWithNativeCapture `
                            -CommandName "Assert-EasyConVcpkgCheckout" -Parameters @{
                                VcpkgRoot = $fixture.Root
                                Configuration = $caseConfiguration
                                VcpkgExecutable = $vcpkgExecutable
                            } -NativeCapture $capture | Out-Null
                    }
                    else {
                        & $workspaceModule {
                            param(
                                $RequestedMode,
                                $Root,
                                $Configuration,
                                $Executable,
                                $Capture,
                                $ProbeState,
                                $PolicyRepository
                            )

                            $originalContext = ${function:Get-EasyConWindowsEnvironmentContext}
                            $originalVerifyCore = ${function:Invoke-EasyConWindowsVerifyCore}
                            $originalWorkspaceGates = ${function:Invoke-EasyConWindowsWorkspaceGates}
                            $originalLifecycle = ${function:Invoke-EasyConEnvironmentLifecycle}
                            $checkoutModule = $ExecutionContext.SessionState.Module
                            $context = [pscustomobject]@{
                                Repository = $PolicyRepository
                                ConfigurationPath = "contract-configuration"
                                Configuration = [pscustomobject]@{
                                    target = "x86_64-pc-windows-msvc"
                                }
                                Fingerprint = [pscustomobject]@{ Value = ("f" * 64) }
                                Location = [pscustomobject]@{
                                    CacheRoot = "contract-cache"
                                    EnvironmentRoot = "contract-environment"
                                    IdentityKey = "contract-identity"
                                    WorkspaceKey = "contract-workspace"
                                }
                            }
                            $contextProbe = { param($RepositoryRoot, $ConfigurationPath, $CacheRoot) return $context }.GetNewClosure()
                            $verifyProbe = {
                                param($RepositoryRoot, $ConfigurationPath, $CacheRoot, $VsWherePath, $Context)

                                $null = $RepositoryRoot, $ConfigurationPath, $CacheRoot, $VsWherePath, $Context
                                & $checkoutModule {
                                    param($CheckoutRoot, $CheckoutConfiguration, $CheckoutExecutable, $NativeCapture)

                                    $originalNative = ${function:Invoke-EasyConNativeCapture}
                                    try {
                                        Set-Item -LiteralPath Function:script:Invoke-EasyConNativeCapture `
                                            -Value $NativeCapture
                                        Assert-EasyConVcpkgCheckout -VcpkgRoot $CheckoutRoot `
                                            -Configuration $CheckoutConfiguration `
                                            -VcpkgExecutable $CheckoutExecutable | Out-Null
                                    }
                                    finally {
                                        Set-Item -LiteralPath Function:script:Invoke-EasyConNativeCapture `
                                            -Value $originalNative
                                    }
                                } $Root $Configuration $Executable $Capture
                                return [pscustomobject]@{ Status = "ready" }
                            }.GetNewClosure()
                            $workspaceProbe = {
                                param(
                                    $RepositoryRoot,
                                    $BaseSha,
                                    [switch]$RequireCleanTree,
                                    [switch]$RequireStagedCandidate,
                                    $Policy
                                )

                                $null = $RepositoryRoot, $BaseSha, $RequireCleanTree, $RequireStagedCandidate, $Policy
                                $ProbeState.WorkspaceGateActions++
                                $ProbeState.GatesStarted++
                            }.GetNewClosure()
                            $lifecycleProbe = {
                                param($Mode, $Location, $SetupAction, $VerifyAction, $WorkspaceAction, $LeaseTimeoutMilliseconds)

                                $null = $Location, $SetupAction, $LeaseTimeoutMilliseconds
                                $summary = & $VerifyAction
                                if ($Mode -ceq "Workspace") {
                                    & $WorkspaceAction $summary | Out-Null
                                }
                                return $summary
                            }.GetNewClosure()
                            try {
                                Set-Item -LiteralPath Function:script:Get-EasyConWindowsEnvironmentContext -Value $contextProbe
                                Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsVerifyCore -Value $verifyProbe
                                Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsWorkspaceGates -Value $workspaceProbe
                                Set-Item -LiteralPath Function:script:Invoke-EasyConEnvironmentLifecycle -Value $lifecycleProbe
                                if ($RequestedMode -ceq "Verify") {
                                    Invoke-EasyConWindowsVerify -RepositoryRoot "input-repository" `
                                        -ConfigurationPath "input-configuration" -CacheRoot "input-cache" | Out-Null
                                }
                                else {
                                    Invoke-EasyConWindowsWorkspace -RepositoryRoot "input-repository" `
                                        -ConfigurationPath "input-configuration" -CacheRoot "input-cache" | Out-Null
                                }
                            }
                            finally {
                                Set-Item -LiteralPath Function:script:Get-EasyConWindowsEnvironmentContext -Value $originalContext
                                Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsVerifyCore -Value $originalVerifyCore
                                Set-Item -LiteralPath Function:script:Invoke-EasyConWindowsWorkspaceGates -Value $originalWorkspaceGates
                                Set-Item -LiteralPath Function:script:Invoke-EasyConEnvironmentLifecycle -Value $originalLifecycle
                            }
                        } $Mode $fixture.Root $caseConfiguration $vcpkgExecutable $capture $state $repository.Path
                    }
                }
                catch {
                    $failure = ($_ | Out-String).Trim()
                }
                return [pscustomobject]@{
                    Failure = $failure
                    ReplacementApplied = $state.ReplacementApplied
                    FirstGitCaptureReached = $state.FirstGitCaptureReached
                    WorkspaceGateActions = $state.WorkspaceGateActions
                    GatesStarted = $state.GatesStarted
                }
            }
            finally {
                & $restore $fixture.Root $state.Saved
                Remove-Item -LiteralPath $fixture.CaseRoot -Recurse -Force -ErrorAction SilentlyContinue
            }
        }.GetNewClosure()
        foreach ($attack in $preGitAttacks) {
            $probe = & $invokePreGitBindingProbe "Direct" $attack
            Assert-Contract (
                $probe.Failure -match "pre-Git vcpkg physical binding rejected" -and
                -not $probe.ReplacementApplied -and
                -not $probe.FirstGitCaptureReached
            ) "physical vcpkg binding must reject audit-after $($attack.Label) reparse substitution before Git"
        }
        foreach ($mode in @("Verify", "Workspace")) {
            foreach ($attack in $preGitAttacks) {
                $probe = & $invokePreGitBindingProbe $mode $attack
                Assert-Contract (
                    $probe.Failure -match "pre-Git vcpkg physical binding rejected" -and
                    -not $probe.ReplacementApplied -and
                    -not $probe.FirstGitCaptureReached -and
                    $probe.WorkspaceGateActions -eq 0 -and
                    $probe.GatesStarted -eq 0
                ) (
                    "$mode must fail closed before a pre-Git $($attack.Label) reparse defect can start a gate; " +
                    "failure=$($probe.Failure); replacement=$($probe.ReplacementApplied); " +
                    "capture=$($probe.FirstGitCaptureReached); workspace=$($probe.WorkspaceGateActions); " +
                    "gates=$($probe.GatesStarted)"
                )
            }
        }
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
        $fixedTree = @(& $git -C $repository.Path rev-parse "$fixedSha`^{tree}")[0]
        Assert-Contract (
            $LASTEXITCODE -eq 0 -and $fixedTree -cmatch '^[0-9a-f]{40}$'
        ) "the real worktree contract must resolve the fixed source tree"
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

            $contractConfiguration = Get-PrivateWindowsBuildConfiguration -Path $configurationPath
            foreach ($input in @($contractConfiguration.fingerprintInputs)) {
                Assert-Contract (
                    (Test-Path -LiteralPath (Join-Path $firstWorktree $input.path) -PathType Leaf) -and
                    (Test-Path -LiteralPath (Join-Path $secondWorktree $input.path) -PathType Leaf)
                ) "each fixed-SHA worktree must contain the current fingerprint input $($input.path)"
            }
            $firstFingerprint = Get-PrivateEnvironmentFingerprint -RepositoryRoot $firstWorktree `
                -Configuration $contractConfiguration
            $secondFingerprint = Get-PrivateEnvironmentFingerprint -RepositoryRoot $secondWorktree `
                -Configuration $contractConfiguration
            Assert-Contract ($firstFingerprint.Value -ceq $secondFingerprint.Value) `
                "clean fixed-SHA worktrees must calculate one fingerprint without copied inputs"
            $firstLocation = Get-PrivateEnvironmentLocation -RepositoryRoot $firstWorktree `
                -Fingerprint $firstFingerprint.Value -Configuration $contractConfiguration -CacheRoot $cache
            $secondLocation = Get-PrivateEnvironmentLocation -RepositoryRoot $secondWorktree `
                -Fingerprint $secondFingerprint.Value -Configuration $contractConfiguration -CacheRoot $cache
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

            $preparedVcpkgRoot = Join-Path $firstLocation.EnvironmentRoot `
                "setup/vcpkg/scripts"
            $preparedVcpkgRelease = "2026-01-01"
            $preparedVcpkgToolCommit = "2" * 40
            $preparedVcpkgTool = Join-Path $firstLocation.EnvironmentRoot "tools/vcpkg.cmd"
            $preparedVcpkgInstalled = Join-Path $firstLocation.EnvironmentRoot `
                "setup/vcpkg/installed"
            $state = [pscustomobject]@{
                Provisions = 0
                Downloads = 0
                PreparedVcpkgCommit = $null
                PreparedVcpkgToolBytes = 0L
                PreparedVcpkgToolHash = $null
            }
            $readyMarker = Join-Path $firstLocation.EnvironmentRoot "contract-ready.txt"
            $setup = {
                $state.Provisions++
                $state.Downloads++
                foreach ($relative in @(
                    ".vcpkg-root",
                    "bootstrap-vcpkg.bat",
                    "bootstrap-vcpkg.sh",
                    "scripts/buildsystems/vcpkg.cmake"
                )) {
                    Set-ContractFile -Path (Join-Path $preparedVcpkgRoot $relative) `
                        -Value "real worktree prepared-vcpkg contract"
                }
                & $git -c core.longpaths=true init --quiet $preparedVcpkgRoot
                Assert-Contract ($LASTEXITCODE -eq 0) `
                    "the shared prepared-vcpkg fixture must initialize Git"
                & $git -c core.longpaths=true -C $preparedVcpkgRoot add -- .
                Assert-Contract ($LASTEXITCODE -eq 0) `
                    "the shared prepared-vcpkg fixture must stage required files"
                & $git -c core.longpaths=true -c user.name=EasyConContract `
                    -c user.email=contract@example.invalid `
                    -C $preparedVcpkgRoot commit --quiet -m "real worktree contract fixture"
                Assert-Contract ($LASTEXITCODE -eq 0) `
                    "the shared prepared-vcpkg fixture must commit required files"
                $preparedCommit = @(
                    & $git -c core.longpaths=true -C $preparedVcpkgRoot rev-parse HEAD
                )
                Assert-Contract (
                    $LASTEXITCODE -eq 0 -and $preparedCommit.Count -eq 1
                ) "the shared prepared-vcpkg fixture must resolve its commit"
                $state.PreparedVcpkgCommit = $preparedCommit[0]

                Set-ContractFile -Path $preparedVcpkgTool -Value (
                    "@echo off`r`necho vcpkg package management program version " +
                    "$preparedVcpkgRelease-$preparedVcpkgToolCommit`r`n"
                )
                $preparedTool = Get-Item -LiteralPath $preparedVcpkgTool
                $state.PreparedVcpkgToolBytes = [long]$preparedTool.Length
                $state.PreparedVcpkgToolHash = (
                    Get-FileHash -LiteralPath $preparedVcpkgTool -Algorithm SHA256
                ).Hash.ToLowerInvariant()
                $preparedTrackedFile = Get-Item -LiteralPath (
                    Join-Path $preparedVcpkgRoot "bootstrap-vcpkg.bat"
                )
                $preparedTrackedFile.LastWriteTimeUtc = `
                    $preparedTrackedFile.LastWriteTimeUtc.AddMinutes(-10)
                New-Item -ItemType Directory -Force -Path $preparedVcpkgInstalled | Out-Null
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

            $getGitIndexPath = {
                param([Parameter(Mandatory)][string]$Worktree)
                $output = @(& $git -C $Worktree rev-parse --git-path index)
                Assert-Contract (
                    $LASTEXITCODE -eq 0 -and
                    $output.Count -eq 1 -and
                    -not [string]::IsNullOrWhiteSpace($output[0])
                ) "the real worktree contract must resolve one Git index path"
                $indexPath = if ([System.IO.Path]::IsPathFullyQualified($output[0])) {
                    $output[0]
                }
                else {
                    Join-Path $Worktree $output[0]
                }
                return [System.IO.Path]::GetFullPath($indexPath)
            }.GetNewClosure()
            $getIndexSnapshot = {
                param([Parameter(Mandatory)][string]$IndexPath)
                $item = Get-Item -LiteralPath $IndexPath
                return [pscustomobject]@{
                    Hash = (Get-FileHash -LiteralPath $IndexPath -Algorithm SHA256).Hash
                    LastWriteTicks = $item.LastWriteTimeUtc.Ticks
                }
            }.GetNewClosure()

            $indexPaths = [ordered]@{
                FirstSource = & $getGitIndexPath $firstWorktree
                SecondSource = & $getGitIndexPath $secondWorktree
                PreparedVcpkg = Join-Path $preparedVcpkgRoot ".git/index"
            }
            $beforeIndexes = [ordered]@{}
            foreach ($name in $indexPaths.Keys) {
                $beforeIndexes[$name] = & $getIndexSnapshot $indexPaths[$name]
            }
            $immutablePaths = [ordered]@{
                PreparedMarker = Join-Path $preparedVcpkgRoot ".vcpkg-root"
                PreparedToolchain = Join-Path $preparedVcpkgRoot `
                    "scripts/buildsystems/vcpkg.cmake"
                PreparedExecutable = $preparedVcpkgTool
                FirstManifest = Join-Path $firstWorktree "vcpkg.json"
                SecondManifest = Join-Path $secondWorktree "vcpkg.json"
            }
            $beforeImmutableFiles = [ordered]@{}
            foreach ($name in $immutablePaths.Keys) {
                $beforeImmutableFiles[$name] = Get-ContractFileSnapshot `
                    -Path $immutablePaths[$name]
            }
            $preparedTree = @(& $git -C $preparedVcpkgRoot rev-parse 'HEAD^{tree}')[0]
            Assert-Contract (
                $LASTEXITCODE -eq 0 -and $preparedTree -cmatch '^[0-9a-f]{40}$'
            ) "the prepared vcpkg fixture must resolve its immutable Git tree"

            $verifyProbeRoot = Join-Path $temporaryRoot "real concurrent verify probes"
            $verifyProbeScript = Join-Path $verifyProbeRoot "verify-probe.ps1"
            [System.IO.Directory]::CreateDirectory($verifyProbeRoot) | Out-Null
            Set-ContractFile -Path $verifyProbeScript -Value @'
param(
    [Parameter(Mandatory)][string]$ModulePath,
    [Parameter(Mandatory)][string]$ConfigurationPath,
    [Parameter(Mandatory)][string]$RepositoryRoot,
    [Parameter(Mandatory)][string]$CacheRoot,
    [Parameter(Mandatory)][string]$ExpectedEnvironmentRoot,
    [Parameter(Mandatory)][string]$ExpectedWorkspaceRoot,
    [Parameter(Mandatory)][string]$EnvironmentReadyMarker,
    [Parameter(Mandatory)][string]$EnvironmentReadyValue,
    [Parameter(Mandatory)][string]$StartMarker,
    [Parameter(Mandatory)][string]$ReadyMarker,
    [Parameter(Mandatory)][string]$ReleaseMarker,
    [Parameter(Mandatory)][int]$ReleaseTimeoutMilliseconds,
    [Parameter(Mandatory)][string]$ResultPath,
    [Parameter(Mandatory)][string]$TracePath,
    [Parameter(Mandatory)][string]$VcpkgRoot,
    [Parameter(Mandatory)][string]$VcpkgExecutable,
    [Parameter(Mandatory)][string]$VcpkgInstalledRoot,
    [Parameter(Mandatory)][string]$VcpkgCommit,
    [Parameter(Mandatory)][string]$VcpkgToolRelease,
    [Parameter(Mandatory)][string]$VcpkgToolCommit,
    [Parameter(Mandatory)][long]$VcpkgToolBytes,
    [Parameter(Mandatory)][string]$VcpkgToolSha256
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Write-ProbeTrace {
    param([Parameter(Mandatory)][string]$Milestone)
    [System.IO.File]::AppendAllText(
        $TracePath,
        ("{0:o}|{1}`n" -f [datetime]::UtcNow,$Milestone),
        [System.Text.UTF8Encoding]::new($false)
    )
}

function Wait-ProbeMarker {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Description,
        [Parameter(Mandatory)][int]$TimeoutMilliseconds
    )

    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        while (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
            $remaining = [long]$TimeoutMilliseconds - [long]$timer.ElapsedMilliseconds
            if ($remaining -le 0) {
                throw "concurrent Verify probe timed out waiting for $Description"
            }
            [System.Threading.Thread]::Sleep([int][Math]::Min(20L,$remaining))
        }
    }
    finally {
        $timer.Stop()
    }
}

Wait-ProbeMarker -Path $StartMarker -Description "parent start" `
    -TimeoutMilliseconds $ReleaseTimeoutMilliseconds
Write-ProbeTrace "start.observed"

Import-Module -Name $ModulePath -Force
$module = Get-Module windows_workspace
$location = & $module {
    param($Root, $Cache, $Configuration)
    $configuration = Get-EasyConWindowsBuildConfiguration -Path $Configuration
    $fingerprint = Get-EasyConEnvironmentFingerprint `
        -RepositoryRoot $Root -Configuration $configuration
    Get-EasyConEnvironmentLocation -RepositoryRoot $Root `
        -Fingerprint $fingerprint.Value -Configuration $configuration -CacheRoot $Cache
} $RepositoryRoot $CacheRoot $ConfigurationPath
if (
    $location.EnvironmentRoot -cne $ExpectedEnvironmentRoot -or
    $location.WorkspaceRoot -cne $ExpectedWorkspaceRoot
) {
    throw "real worktree Verify recomputed an unexpected e/ or w/<key> location"
}
$vcpkgConfiguration = [pscustomobject]@{
    vcpkg = [pscustomobject]@{
        scriptsCommit = $VcpkgCommit
        toolRelease = $VcpkgToolRelease
        toolCommit = $VcpkgToolCommit
        windowsAsset = [pscustomobject]@{
            bytes = $VcpkgToolBytes
            sha256 = $VcpkgToolSha256
        }
    }
}
$verify = {
    if (
        -not (Test-Path -LiteralPath $EnvironmentReadyMarker -PathType Leaf) -or
        (Get-Content -Raw -LiteralPath $EnvironmentReadyMarker) -cne $EnvironmentReadyValue
    ) {
        throw "real concurrent Verify environment is not ready"
    }
    [System.IO.File]::WriteAllText(
        $ReadyMarker,
        "ready",
        [System.Text.UTF8Encoding]::new($false)
    )
    Write-ProbeTrace "ready.written"
    Wait-ProbeMarker -Path $ReleaseMarker -Description "layout release" `
        -TimeoutMilliseconds $ReleaseTimeoutMilliseconds
    Write-ProbeTrace "release.observed"
    $vcpkg = & $module {
        param($Root, $Configuration, $Executable)
        Assert-EasyConVcpkgCheckout -VcpkgRoot $Root `
            -Configuration $Configuration -VcpkgExecutable $Executable
    } $VcpkgRoot $vcpkgConfiguration $VcpkgExecutable
    $layout = & $module {
        param($VerifiedVcpkg, $Root, $Workspace, $Cache, $Environment, $Installed)
        New-EasyConVcpkgWorkspaceLayout -Vcpkg $VerifiedVcpkg `
            -RepositoryRoot $Root -WorkspaceRoot $Workspace -CacheRoot $Cache `
            -EnvironmentRoot $Environment -InstalledRoot $Installed
    } $vcpkg $RepositoryRoot $location.WorkspaceRoot $CacheRoot `
        $location.EnvironmentRoot $VcpkgInstalledRoot
    return [pscustomobject]@{
        status = "ready"
        environmentRoot = $location.EnvironmentRoot
        workspaceRoot = $location.WorkspaceRoot
        vcpkgRoot = $layout.Root
        vcpkgExecutable = Join-Path $layout.Root "vcpkg.exe"
        vcpkgMarker = Join-Path $layout.Root ".vcpkg-root"
        vcpkgWrapper = $layout.Toolchain
    }
}.GetNewClosure()

$summary = & $module {
    param($OwnedLocation, $VerifyAction)
    Invoke-EasyConEnvironmentLifecycle -Mode Verify -Location $OwnedLocation `
        -SetupAction { throw "concurrent Verify probe must not provision" } `
        -VerifyAction $VerifyAction `
        -WorkspaceAction { throw "concurrent Verify probe must not run workspace gates" } `
        -LeaseTimeoutMilliseconds 15000
} $location $verify

$result = [ordered]@{
    status = $summary.status
    environmentRoot = $summary.environmentRoot
    workspaceRoot = $location.WorkspaceRoot
    vcpkgRoot = $summary.vcpkgRoot
    vcpkgExecutable = $summary.vcpkgExecutable
    vcpkgMarker = $summary.vcpkgMarker
    vcpkgWrapper = $summary.vcpkgWrapper
}
[System.IO.File]::WriteAllText(
    $ResultPath,
    ($result | ConvertTo-Json -Compress),
    [System.Text.UTF8Encoding]::new($false)
)
Write-ProbeTrace "result.validated"
'@

            $probeReadinessTimeoutMilliseconds = 15000
            $probeReleaseTimeoutMilliseconds =
                $probeReadinessTimeoutMilliseconds + 5000
            Assert-Contract (
                $probeReleaseTimeoutMilliseconds -eq
                    ($probeReadinessTimeoutMilliseconds + 5000)
            ) (
                "a ready Verify child release watchdog must derive from one readiness deadline " +
                "plus the coordination and teardown margin"
            )

            $verifyProcesses = [System.Collections.Generic.List[object]]::new()
            $layoutRelease = Join-Path $verifyProbeRoot "layout-release.txt"
            $teardownDeadline = New-ContractProbeDeadline -Name "real Verify probe teardown" `
                -TimeoutMilliseconds 5000
            $primaryProbeFailure = $null
            try {
                foreach ($entry in @(
                    [pscustomobject]@{
                        Name = "one"
                        RepositoryRoot = $firstWorktree
                        Location = $firstLocation
                    },
                    [pscustomobject]@{
                        Name = "two"
                        RepositoryRoot = $secondWorktree
                        Location = $secondLocation
                    }
                )) {
                    $start = Join-Path $verifyProbeRoot "$($entry.Name)-start.txt"
                    $ready = Join-Path $verifyProbeRoot "$($entry.Name)-ready.txt"
                    $result = Join-Path $verifyProbeRoot "$($entry.Name)-result.json"
                    $trace = Join-Path $verifyProbeRoot "$($entry.Name)-trace.txt"
                    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
                    $startInfo.FileName = Join-Path $PSHOME "pwsh.exe"
                    $startInfo.WorkingDirectory = $entry.RepositoryRoot
                    $startInfo.UseShellExecute = $false
                    $startInfo.RedirectStandardOutput = $true
                    $startInfo.RedirectStandardError = $true
                    $arguments = @(
                        "-NoLogo", "-NoProfile", "-File", $verifyProbeScript,
                        "-ModulePath", $modulePath,
                        "-ConfigurationPath", $configurationPath,
                        "-RepositoryRoot", $entry.RepositoryRoot,
                        "-CacheRoot", $entry.Location.CacheRoot,
                        "-ExpectedEnvironmentRoot", $entry.Location.EnvironmentRoot,
                        "-ExpectedWorkspaceRoot", $entry.Location.WorkspaceRoot,
                        "-EnvironmentReadyMarker", $readyMarker,
                        "-EnvironmentReadyValue", $fixedSha,
                        "-StartMarker", $start,
                        "-ReadyMarker", $ready,
                        "-ReleaseMarker", $layoutRelease,
                        "-ReleaseTimeoutMilliseconds", $probeReleaseTimeoutMilliseconds,
                        "-ResultPath", $result,
                        "-TracePath", $trace,
                        "-VcpkgRoot", $preparedVcpkgRoot,
                        "-VcpkgExecutable", $preparedVcpkgTool,
                        "-VcpkgInstalledRoot", $preparedVcpkgInstalled,
                        "-VcpkgCommit", $state.PreparedVcpkgCommit,
                        "-VcpkgToolRelease", $preparedVcpkgRelease,
                        "-VcpkgToolCommit", $preparedVcpkgToolCommit,
                        "-VcpkgToolBytes", $state.PreparedVcpkgToolBytes,
                        "-VcpkgToolSha256", $state.PreparedVcpkgToolHash
                    )
                    foreach ($argument in $arguments) {
                        $startInfo.ArgumentList.Add([string]$argument)
                    }
                    $verifyProcesses.Add((Start-ContractProbe -Name $entry.Name `
                        -StartInfo $startInfo -Arguments $arguments -Start $start -Ready $ready `
                        -Release $layoutRelease -Result $result -Trace $trace `
                        -Location $entry.Location -TeardownDeadline $teardownDeadline `
                        -OwnedProbe $verifyProcesses.ToArray())) | Out-Null
                }

                $readinessTimer = [System.Diagnostics.Stopwatch]::StartNew()
                [System.IO.File]::WriteAllText(
                    $verifyProcesses[0].Start,
                    "start",
                    [System.Text.UTF8Encoding]::new($false)
                )
                Wait-ContractProbesReady -Probe @($verifyProcesses) -RequiredName @("one") `
                    -Timer $readinessTimer -TimeoutMilliseconds $probeReadinessTimeoutMilliseconds
                Assert-Contract (
                    -not (Test-Path -LiteralPath $verifyProcesses[1].Ready -PathType Leaf)
                ) "the startup-skew probe must remain parent-gated until the first probe is ready"
                [System.IO.File]::WriteAllText(
                    $verifyProcesses[1].Start,
                    "start",
                    [System.Text.UTF8Encoding]::new($false)
                )
                Wait-ContractProbesReady -Probe @($verifyProcesses) `
                    -RequiredName @("one","two") -Timer $readinessTimer `
                    -TimeoutMilliseconds $probeReadinessTimeoutMilliseconds
                $readinessTimer.Stop()
                Update-ContractProbeReadinessSnapshot -Probe @($verifyProcesses) `
                    -ProcessHasExitedAction { param([object]$Process) $Process.HasExited }

                foreach ($probe in $verifyProcesses) {
                    Assert-Contract (-not $probe.Process.HasExited) `
                        "real Verify probe $($probe.Name) must remain inside the marker barrier"
                }
                Assert-Contract (
                    $verifyProcesses[0].Location.EnvironmentRoot -ceq
                        $verifyProcesses[1].Location.EnvironmentRoot -and
                    $verifyProcesses[0].Location.WorkspaceRoot -cne
                        $verifyProcesses[1].Location.WorkspaceRoot
                ) "startup-skew probes must share e/ and retain distinct w/<key> roots"

                $environmentLease = $null
                $environmentLeaseFailure = $null
                try {
                    $environmentLease = Invoke-PrivateCommand `
                        -CommandName "Enter-EasyConEnvironmentLease" -Parameters @{
                            Location = $firstLocation
                            Access = "Exclusive"
                            TimeoutMilliseconds = 0
                            RetryMilliseconds = 0
                        }
                }
                catch {
                    $environmentLeaseFailure = $_
                }
                finally {
                    if ($null -ne $environmentLease) {
                        $environmentLease.Dispose()
                    }
                }
                Assert-Contract (
                    $null -ne $environmentLeaseFailure -and
                    $environmentLeaseFailure.Exception.Message -match 'busy|ownership'
                ) "both ready probes must keep the shared environment lease occupied"

                foreach ($probe in $verifyProcesses) {
                    $workspaceLease = $null
                    $workspaceLeaseFailure = $null
                    try {
                        $workspaceLease = Invoke-PrivateCommand `
                            -CommandName "Enter-EasyConWorkspaceLease" -Parameters @{
                                Location = $probe.Location
                                TimeoutMilliseconds = 0
                                RetryMilliseconds = 0
                            }
                    }
                    catch {
                        $workspaceLeaseFailure = $_
                    }
                    finally {
                        if ($null -ne $workspaceLease) {
                            $workspaceLease.Dispose()
                        }
                    }
                    Assert-Contract (
                        $null -ne $workspaceLeaseFailure -and
                        $workspaceLeaseFailure.Exception.Message -match 'busy|ownership'
                    ) "ready probe $($probe.Name) must hold its distinct w/<key> lease"
                }
                Publish-ContractProbeRelease -Probe @($verifyProcesses)
                Wait-ContractProbesExited -Probe @($verifyProcesses) `
                    -TimeoutMilliseconds $probeReadinessTimeoutMilliseconds
                foreach ($probe in $verifyProcesses) {
                    Assert-Contract (
                        $probe.State -ceq "Exited" -and
                        (Test-Path -LiteralPath $probe.Result -PathType Leaf)
                    ) "real Verify probe $($probe.Name) must exit with a result after release"
                    $probe.Result = Get-Content -Raw -LiteralPath $probe.Result | `
                        ConvertFrom-Json
                    Assert-Contract (
                        $probe.Result.status -ceq "ready" -and
                        $probe.Result.environmentRoot -ceq $firstLocation.EnvironmentRoot -and
                        $probe.Result.workspaceRoot -ceq $probe.Location.WorkspaceRoot -and
                        $probe.Result.vcpkgRoot -ceq
                            (Join-Path $probe.Location.WorkspaceRoot "vcpkg/root") -and
                        (Test-Path -LiteralPath $probe.Result.vcpkgExecutable -PathType Leaf) -and
                        (Test-Path -LiteralPath $probe.Result.vcpkgMarker -PathType Leaf) -and
                        (Test-Path -LiteralPath $probe.Result.vcpkgWrapper -PathType Leaf)
                    ) "real concurrent Verify must report shared e/ and its own w/<key>"
                    Set-ContractProbeState -Probe $probe -State ResultValidated `
                        -Milestone "result.validated"
                }
            }
            catch {
                $primaryProbeFailure = $_
                throw
            }
            finally {
                $probeCleanupFailure = $null
                try {
                    Stop-ContractProbes -Probe @($verifyProcesses) -Deadline $teardownDeadline
                }
                catch {
                    $probeCleanupFailure = $_
                }
                if ($null -ne $probeCleanupFailure) {
                    if ($null -ne $primaryProbeFailure) {
                        throw (
                            "contract failed: primary real Verify probe failure<<`n" +
                            "$($primaryProbeFailure.Exception.Message)`n>>primary real Verify probe failure`n" +
                            "teardown failure<<`n$($probeCleanupFailure.Exception.Message)`n>>teardown failure"
                        )
                    }
                    else {
                        throw $probeCleanupFailure
                    }
                }
            }

            Assert-Contract (
                $verifyProcesses[0].Result.environmentRoot -ceq `
                    $verifyProcesses[1].Result.environmentRoot -and
                $verifyProcesses[0].Result.workspaceRoot -cne `
                    $verifyProcesses[1].Result.workspaceRoot
            ) "concurrent real Verify must use one e/ and distinct w/<key> roots"
            for ($index = 0; $index -lt $verifyProcesses.Count; $index++) {
                $probe = $verifyProcesses[$index]
                $sourceManifestName = if ($index -eq 0) { "FirstManifest" } else { "SecondManifest" }
                $workspaceManifest = Join-Path $probe.Result.workspaceRoot `
                    "vcpkg/manifest/vcpkg.json"
                $wrapperText = Get-Content -Raw -LiteralPath $probe.Result.vcpkgWrapper
                $rootBinding = "set(Z_VCPKG_ROOT_DIR `"$($probe.Result.vcpkgRoot.Replace('\', '/'))`""
                Assert-Contract (
                    (Get-FileHash -LiteralPath $probe.Result.vcpkgExecutable `
                        -Algorithm SHA256).Hash.ToLowerInvariant() -ceq
                        $state.PreparedVcpkgToolHash -and
                    (Get-FileHash -LiteralPath $probe.Result.vcpkgMarker `
                        -Algorithm SHA256).Hash.ToLowerInvariant() -ceq
                        $beforeImmutableFiles.PreparedMarker.Hash -and
                    (Get-FileHash -LiteralPath $workspaceManifest `
                        -Algorithm SHA256).Hash.ToLowerInvariant() -ceq
                        $beforeImmutableFiles[$sourceManifestName].Hash -and
                    (Get-ContractFileIdentity -Path $probe.Result.vcpkgExecutable) -cne
                        $beforeImmutableFiles.PreparedExecutable.Identity -and
                    (Get-ContractFileIdentity -Path $probe.Result.vcpkgMarker) -cne
                        $beforeImmutableFiles.PreparedMarker.Identity -and
                    (Get-ContractFileIdentity -Path $probe.Result.vcpkgWrapper) -cne
                        $beforeImmutableFiles.PreparedToolchain.Identity -and
                    (Get-ContractFileIdentity -Path $workspaceManifest) -cne
                        $beforeImmutableFiles[$sourceManifestName].Identity -and
                    $wrapperText.IndexOf($rootBinding, [System.StringComparison]::Ordinal) -ge 0 -and
                    $wrapperText.IndexOf($rootBinding, [System.StringComparison]::Ordinal) -lt
                        $wrapperText.IndexOf("include(`"", [System.StringComparison]::Ordinal)
                ) "real concurrent layout $($probe.Name) must publish detached exact files and bind root before include"
                $rootEntries = @(Get-ChildItem -Force -LiteralPath $probe.Result.vcpkgRoot |
                    Select-Object -ExpandProperty Name | Sort-Object)
                Assert-Contract (
                    ($rootEntries -join ',') -ceq '.vcpkg-root,scripts,vcpkg.exe'
                ) "real concurrent layout $($probe.Name) must retain the exact applocal root"
            }
            foreach ($property in @(
                "vcpkgExecutable", "vcpkgMarker", "vcpkgWrapper"
            )) {
                Assert-Contract (
                    (Get-ContractFileIdentity -Path $verifyProcesses[0].Result.$property) -cne
                        (Get-ContractFileIdentity -Path $verifyProcesses[1].Result.$property)
                ) "concurrent real layouts must use independent $property file identities"
            }
            Assert-Contract (
                (Get-ContractFileIdentity -Path (
                    Join-Path $verifyProcesses[0].Result.workspaceRoot "vcpkg/manifest/vcpkg.json"
                )) -cne
                (Get-ContractFileIdentity -Path (
                    Join-Path $verifyProcesses[1].Result.workspaceRoot "vcpkg/manifest/vcpkg.json"
                ))
            ) "concurrent real layouts must use independent manifest file identities"
            $finalEnvironmentDirectories = @(
                Get-ChildItem -LiteralPath (Join-Path $cache "e") -Directory -Force
            )
            Assert-Contract (
                $finalEnvironmentDirectories.Count -eq 1 -and
                $finalEnvironmentDirectories[0].FullName -ceq `
                    $firstLocation.EnvironmentRoot -and
                $state.Provisions -eq 1 -and
                $state.Downloads -eq 1
            ) "concurrent real Verify must not duplicate e/, provision, or download"

            foreach ($name in $indexPaths.Keys) {
                $after = & $getIndexSnapshot $indexPaths[$name]
                Assert-Contract (
                    $after.Hash -ceq $beforeIndexes[$name].Hash -and
                    $after.LastWriteTicks -eq $beforeIndexes[$name].LastWriteTicks -and
                    -not (Test-Path -LiteralPath "$($indexPaths[$name]).lock")
                ) "concurrent real Verify must preserve $name index hash/mtime and avoid index.lock"
            }
            foreach ($name in $immutablePaths.Keys) {
                $afterImmutable = Get-ContractFileSnapshot -Path $immutablePaths[$name]
                Assert-Contract (
                    $afterImmutable.Hash -ceq $beforeImmutableFiles[$name].Hash -and
                    $afterImmutable.Identity -ceq $beforeImmutableFiles[$name].Identity -and
                    $afterImmutable.LastWriteTicks -eq
                        $beforeImmutableFiles[$name].LastWriteTicks
                ) "concurrent real layouts must preserve immutable $name hash, identity, and mtime"
            }
            $preparedHeadAfter = @(& $git -C $preparedVcpkgRoot rev-parse HEAD)[0]
            $preparedTreeAfter = @(& $git -C $preparedVcpkgRoot rev-parse 'HEAD^{tree}')[0]
            $preparedStatusAfter = @(
                & $git --no-optional-locks -c core.fsmonitor=false `
                    -c core.untrackedCache=false -C $preparedVcpkgRoot status `
                    --porcelain=v1 --untracked-files=all --ignored=matching
            )
            Assert-Contract (
                $LASTEXITCODE -eq 0 -and
                $preparedHeadAfter -ceq $state.PreparedVcpkgCommit -and
                $preparedTreeAfter -ceq $preparedTree -and
                $preparedStatusAfter.Count -eq 0 -and
                -not (Test-Path -LiteralPath (Join-Path $preparedVcpkgRoot ".git/index.lock"))
            ) "concurrent real layouts must leave the prepared vcpkg HEAD/tree/status immutable"

            foreach ($worktree in @($firstWorktree, $secondWorktree)) {
                $finalHead = @(& $git -C $worktree rev-parse --verify HEAD)
                $finalTree = @(& $git -C $worktree rev-parse 'HEAD^{tree}')[0]
                $finalStatus = @(
                    & $git --no-optional-locks -c core.fsmonitor=false `
                        -c core.untrackedCache=false -C $worktree status `
                        --porcelain=v1 --untracked-files=all
                )
                Assert-Contract (
                    $LASTEXITCODE -eq 0 -and
                    $finalHead.Count -eq 1 -and
                    $finalHead[0] -ceq $fixedSha -and
                    $finalTree -ceq $fixedTree -and
                    $finalStatus.Count -eq 0
                ) "Setup and concurrent Verify must leave each fixed-SHA checkout clean"
                & $git --no-optional-locks -c core.fsmonitor=false `
                    -c core.untrackedCache=false -C $worktree diff --quiet --exit-code
                $worktreeDiffExit = $LASTEXITCODE
                & $git --no-optional-locks -c core.fsmonitor=false `
                    -c core.untrackedCache=false -C $worktree diff --cached --quiet --exit-code
                Assert-Contract (
                    $worktreeDiffExit -eq 0 -and $LASTEXITCODE -eq 0
                ) "Setup and concurrent Verify must leave source index and worktree diffs empty"
            }
            foreach ($name in $indexPaths.Keys) {
                $afterCleanCheck = & $getIndexSnapshot $indexPaths[$name]
                Assert-Contract (
                    $afterCleanCheck.Hash -ceq $beforeIndexes[$name].Hash -and
                    $afterCleanCheck.LastWriteTicks -eq $beforeIndexes[$name].LastWriteTicks
                ) "the read-only status/diff checks must also preserve the $name index"
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

    Invoke-ContractCase -Name "workspace-file-publication-failures-are-atomic" -Action {
        $publicationRoot = Join-Path $temporaryRoot "workspace file atomic publication"
        $destination = Join-Path $publicationRoot "vcpkg.exe"
        Set-ContractFile -Path $destination -Value "complete original final"
        $original = Get-ContractFileSnapshot -Path $destination
        $content = [System.Text.Encoding]::UTF8.GetBytes("verified replacement content")
        $contentHash = [Convert]::ToHexString(
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
        $publicationParameters = @{
            Destination = $destination
            Algorithm = "SHA256"
            Hash = $contentHash
            Bytes = [long]$content.Length
            TrustedRoot = $publicationRoot
            Description = "contract workspace file"
            MaterializeAction = $materialize
            CleanupMaxAttempts = 1
            CleanupRetryMilliseconds = 0
        }

        $publishFailureParameters = $publicationParameters.Clone()
        $publishFailureParameters.MoveAction = {
            param($Temporary, $Final)
            $null = $Temporary, $Final
            throw [System.IO.IOException]::new("synthetic atomic file publish failure")
        }
        Assert-Throws -Pattern "synthetic atomic file publish failure" -Action {
            Invoke-PrivateCommand -CommandName "Publish-EasyConContentFileAtomically" `
                -Parameters $publishFailureParameters
        }
        $afterPublishFailure = Get-ContractFileSnapshot -Path $destination
        Assert-Contract (
            $afterPublishFailure.Hash -ceq $original.Hash -and
            $afterPublishFailure.Identity -ceq $original.Identity -and
            @(Get-ChildItem -Force -LiteralPath $publicationRoot |
                Where-Object { $_.Name -like '*.publish-*' }).Count -eq 0
        ) "a move failure must preserve the complete final and remove its verified temporary"

        $cleanupState = [pscustomobject]@{ Handle = $null; Temporary = $null }
        $cleanupFailureParameters = $publicationParameters.Clone()
        $cleanupFailureParameters.MoveAction = {
            param($Temporary, $Final)
            $null = $Final
            $cleanupState.Temporary = $Temporary
            $cleanupState.Handle = [System.IO.File]::Open(
                $Temporary,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::Read,
                [System.IO.FileShare]::None
            )
            throw [System.IO.IOException]::new("synthetic atomic file primary failure")
        }.GetNewClosure()
        $cleanupFailure = $null
        try {
            try {
                Invoke-PrivateCommand -CommandName "Publish-EasyConContentFileAtomically" `
                    -Parameters $cleanupFailureParameters | Out-Null
            }
            catch {
                $cleanupFailure = $_
            }
            Assert-Contract (
                $null -ne $cleanupFailure -and
                $cleanupFailure.Exception.Message -match "synthetic atomic file primary failure" -and
                $cleanupFailure.Exception.Message -match "cleanup" -and
                $cleanupFailure.Exception.Data.Contains("EasyConTemporaryCleanupFailure") -and
                $cleanupFailure.Exception.Data["EasyConResidualTemporary"] -ceq
                    $cleanupState.Temporary -and
                (Test-Path -LiteralPath $cleanupState.Temporary -PathType Leaf)
            ) "temporary cleanup failure must retain the publish primary and residual diagnostic"
            $afterCleanupFailure = Get-ContractFileSnapshot -Path $destination
            Assert-Contract (
                $afterCleanupFailure.Hash -ceq $original.Hash -and
                $afterCleanupFailure.Identity -ceq $original.Identity
            ) "temporary cleanup failure must not replace or truncate the complete final"
        }
        finally {
            if ($null -ne $cleanupState.Handle) {
                $cleanupState.Handle.Dispose()
            }
            if (
                -not [string]::IsNullOrWhiteSpace($cleanupState.Temporary) -and
                (Test-Path -LiteralPath $cleanupState.Temporary -PathType Leaf)
            ) {
                Remove-Item -LiteralPath $cleanupState.Temporary -Force
            }
        }

        Invoke-PrivateCommand -CommandName "Publish-EasyConContentFileAtomically" `
            -Parameters $publicationParameters | Out-Null
        Assert-Contract (
            (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant() -ceq
                $contentHash -and
            (Get-ContractFileIdentity -Path $destination) -cne $original.Identity -and
            @(Get-ChildItem -Force -LiteralPath $publicationRoot |
                Where-Object { $_.Name -like '*.publish-*' }).Count -eq 0
        ) "a later atomic publication must replace the final completely after failures clear"
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
        $vcpkgExecutable = Join-Path $location.EnvironmentRoot "downloads/vcpkg.exe"
        Set-ContractFile -Path (Join-Path $vendor "contract-crate/Cargo.toml") `
            -Value "[package]`nname='contract-crate'`nversion='1.0.0'"
        Set-ContractFile -Path (Join-Path $installed "x64-windows-static-md/lib/contract.lib") `
            -Value "native"
        Set-ContractFile -Path $toolchain -Value "# shared vcpkg toolchain"
        [System.IO.File]::WriteAllBytes((Join-Path $vcpkgRoot ".vcpkg-root"), [byte[]]@())
        Set-ContractFile -Path $vcpkgExecutable -Value "verified vcpkg executable"
        $vcpkgExecutableItem = Get-Item -Force -LiteralPath $vcpkgExecutable
        $vcpkgExecutableHash = (
            Get-FileHash -LiteralPath $vcpkgExecutable -Algorithm SHA256
        ).Hash.ToLowerInvariant()
        $vcpkgMarker = Join-Path $vcpkgRoot ".vcpkg-root"
        $vcpkgMarkerItem = Get-Item -Force -LiteralPath $vcpkgMarker
        $vcpkgMarkerHash = (
            Get-FileHash -LiteralPath $vcpkgMarker -Algorithm SHA256
        ).Hash.ToLowerInvariant()

        $cargoLayout = Invoke-PrivateCommand -CommandName "New-EasyConCargoWorkspaceLayout" `
            -Parameters @{
                WorkspaceRoot = $location.WorkspaceRoot
                CacheRoot = $location.CacheRoot
                VendorRoot = $vendor
                EnvironmentRoot = $location.EnvironmentRoot
            }
        $expectedManifest = Join-Path $location.WorkspaceRoot "vcpkg/manifest/vcpkg.json"
        $expectedRoot = Join-Path $location.WorkspaceRoot "vcpkg/root"
        $expectedExecutable = Join-Path $expectedRoot "vcpkg.exe"
        $expectedMarker = Join-Path $expectedRoot ".vcpkg-root"
        $expectedWrapper = Join-Path $expectedRoot "scripts/buildsystems/vcpkg.cmake"
        foreach ($parent in @(
            (Split-Path -Parent $expectedManifest),
            (Split-Path -Parent $expectedWrapper)
        )) {
            New-Item -ItemType Directory -Force -Path $parent | Out-Null
        }
        $hardlinkSources = [ordered]@{
            Manifest = Join-Path $repositoryRoot "vcpkg.json"
            Executable = $vcpkgExecutable
            Marker = $vcpkgMarker
            Wrapper = $toolchain
        }
        $hardlinkDestinations = [ordered]@{
            Manifest = $expectedManifest
            Executable = $expectedExecutable
            Marker = $expectedMarker
            Wrapper = $expectedWrapper
        }
        $sourceSnapshots = [ordered]@{}
        foreach ($name in $hardlinkSources.Keys) {
            New-Item -ItemType HardLink -Path $hardlinkDestinations[$name] `
                -Target $hardlinkSources[$name] | Out-Null
            $sourceSnapshots[$name] = Get-ContractFileSnapshot -Path $hardlinkSources[$name]
            Assert-Contract (
                (Get-ContractFileIdentity -Path $hardlinkDestinations[$name]) -ceq
                    $sourceSnapshots[$name].Identity
            ) "preseeded workspace $name must begin as a real hardlink to its source"
        }

        $vcpkgLayoutParameters = @{
            Vcpkg = [pscustomobject]@{
                Root = $vcpkgRoot
                Toolchain = $toolchain
                Executable = $vcpkgExecutable
                ExecutableBytes = [long]$vcpkgExecutableItem.Length
                ExecutableSha256 = $vcpkgExecutableHash
                Marker = $vcpkgMarker
                MarkerBytes = [long]$vcpkgMarkerItem.Length
                MarkerSha256 = $vcpkgMarkerHash
            }
            RepositoryRoot = $repositoryRoot
            WorkspaceRoot = $location.WorkspaceRoot
            CacheRoot = $location.CacheRoot
            EnvironmentRoot = $location.EnvironmentRoot
            InstalledRoot = $installed
        }
        $vcpkgLayout = Invoke-PrivateCommand -CommandName "New-EasyConVcpkgWorkspaceLayout" `
            -Parameters $vcpkgLayoutParameters
        $cargoConfig = Get-Content -Raw -LiteralPath $cargoLayout.Configuration
        $vcpkgWrapper = Get-Content -Raw -LiteralPath $vcpkgLayout.Toolchain
        $workspaceVcpkgExecutable = Join-Path $vcpkgLayout.Root "vcpkg.exe"
        $workspaceVcpkgMarker = Join-Path $vcpkgLayout.Root ".vcpkg-root"
        Assert-Contract (
            (Test-Path -LiteralPath $workspaceVcpkgExecutable -PathType Leaf) -and
            (Test-Path -LiteralPath $workspaceVcpkgMarker -PathType Leaf)
        ) "workspace vcpkg projection must provide an executable applocal root"
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
        $workspaceRootBinding = "set(Z_VCPKG_ROOT_DIR `"$($vcpkgLayout.Root.Replace("\", "/"))`""
        Assert-Contract (
            $vcpkgWrapper.IndexOf($workspaceRootBinding, [System.StringComparison]::Ordinal) -ge 0 -and
            $vcpkgWrapper.IndexOf($workspaceRootBinding, [System.StringComparison]::Ordinal) -lt
                $vcpkgWrapper.IndexOf("include(`"", [System.StringComparison]::Ordinal)
        ) "workspace vcpkg wrapper must bind its applocal root before including prepared scripts"
        Assert-Contract (-not $vcpkgWrapper.Contains($repositoryRoot)) `
            "workspace vcpkg wrapper must not bind to the source worktree"
        foreach ($name in $hardlinkSources.Keys) {
            $sourceAfter = Get-ContractFileSnapshot -Path $hardlinkSources[$name]
            Assert-Contract (
                $sourceAfter.Hash -ceq $sourceSnapshots[$name].Hash -and
                $sourceAfter.Bytes -eq $sourceSnapshots[$name].Bytes -and
                $sourceAfter.Identity -ceq $sourceSnapshots[$name].Identity -and
                (Get-ContractFileIdentity -Path $hardlinkDestinations[$name]) -cne
                    $sourceSnapshots[$name].Identity
            ) "workspace $name publication must detach a hardlink without changing its source"
        }
        Assert-Contract (
            (Get-FileHash -LiteralPath $workspaceVcpkgExecutable -Algorithm SHA256).Hash.ToLowerInvariant() -ceq
                $vcpkgExecutableHash -and
            (Get-Item -Force -LiteralPath $workspaceVcpkgMarker).Length -eq 0 -and
            (Get-FileHash -LiteralPath $workspaceVcpkgMarker -Algorithm SHA256).Hash.ToLowerInvariant() -ceq
                $vcpkgMarkerHash
        ) "workspace vcpkg executable and root marker must exactly match verified sources"
        $rootEntries = @(Get-ChildItem -Force -LiteralPath $vcpkgLayout.Root |
            Select-Object -ExpandProperty Name | Sort-Object)
        $scriptsEntries = @(Get-ChildItem -Force -LiteralPath (Join-Path $vcpkgLayout.Root "scripts") |
            Select-Object -ExpandProperty Name | Sort-Object)
        $buildsystemEntries = @(Get-ChildItem -Force -LiteralPath (
            Join-Path $vcpkgLayout.Root "scripts/buildsystems"
        ) | Select-Object -ExpandProperty Name | Sort-Object)
        Assert-Contract (
            ($rootEntries -join ',') -ceq '.vcpkg-root,scripts,vcpkg.exe' -and
            ($scriptsEntries -join ',') -ceq 'buildsystems' -and
            ($buildsystemEntries -join ',') -ceq 'vcpkg.cmake'
        ) "workspace vcpkg applocal root must contain only its exact controlled layout"

        Set-ContractFile -Path $workspaceVcpkgExecutable -Value "damaged workspace tool"
        $vcpkgLayout = Invoke-PrivateCommand -CommandName "New-EasyConVcpkgWorkspaceLayout" `
            -Parameters $vcpkgLayoutParameters
        Assert-Contract (
            (Get-FileHash -LiteralPath $workspaceVcpkgExecutable -Algorithm SHA256).Hash.ToLowerInvariant() -ceq
                $vcpkgExecutableHash
        ) "a damaged workspace vcpkg executable must recover from the verified source"

        $ordinaryIdentity = Get-ContractFileIdentity -Path $workspaceVcpkgExecutable
        $lockedSnapshot = Get-ContractFileSnapshot -Path $workspaceVcpkgExecutable
        $lockedHandle = [System.IO.File]::Open(
            $workspaceVcpkgExecutable,
            [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::Read,
            [System.IO.FileShare]::Read
        )
        $lockedFailure = $null
        try {
            try {
                Invoke-PrivateCommand -CommandName "New-EasyConVcpkgWorkspaceLayout" `
                    -Parameters $vcpkgLayoutParameters | Out-Null
            }
            catch {
                $lockedFailure = $_
            }
            Assert-Contract ($null -ne $lockedFailure) `
                "a same-hash locked workspace vcpkg executable must not use a reuse fast path"
            $lockedAfter = Get-ContractFileSnapshot -Path $workspaceVcpkgExecutable
            Assert-Contract (
                $lockedAfter.Hash -ceq $lockedSnapshot.Hash -and
                $lockedAfter.Identity -ceq $lockedSnapshot.Identity -and
                @(Get-ChildItem -Force -Recurse -LiteralPath $vcpkgLayout.Root |
                    Where-Object { $_.Name -like '*.publish-*' }).Count -eq 0
            ) "locked atomic replacement must preserve the complete final and clean its temporary"
        }
        finally {
            $lockedHandle.Dispose()
        }
        $vcpkgLayout = Invoke-PrivateCommand -CommandName "New-EasyConVcpkgWorkspaceLayout" `
            -Parameters $vcpkgLayoutParameters
        Assert-Contract (
            (Get-ContractFileIdentity -Path $workspaceVcpkgExecutable) -cne $ordinaryIdentity -and
            (Get-FileHash -LiteralPath $workspaceVcpkgExecutable -Algorithm SHA256).Hash.ToLowerInvariant() -ceq
                $vcpkgExecutableHash
        ) "an unlocked same-hash ordinary destination must be replaced by a fresh byte copy"

        $beforeWrongKind = [ordered]@{}
        foreach ($name in $hardlinkDestinations.Keys) {
            $beforeWrongKind[$name] = Get-ContractFileSnapshot -Path $hardlinkDestinations[$name]
        }
        Remove-Item -LiteralPath $workspaceVcpkgMarker -Force
        New-Item -ItemType Directory -Path $workspaceVcpkgMarker | Out-Null
        Assert-Throws -Pattern "regular file" -Action {
            Invoke-PrivateCommand -CommandName "New-EasyConVcpkgWorkspaceLayout" `
                -Parameters $vcpkgLayoutParameters
        }
        foreach ($name in @("Manifest", "Executable", "Wrapper")) {
            $afterWrongKind = Get-ContractFileSnapshot -Path $hardlinkDestinations[$name]
            Assert-Contract (
                $afterWrongKind.Hash -ceq $beforeWrongKind[$name].Hash -and
                $afterWrongKind.Identity -ceq $beforeWrongKind[$name].Identity
            ) "wrong-kind preflight must not publish a new $name file"
        }
        Remove-Item -LiteralPath $workspaceVcpkgMarker -Force
        Invoke-PrivateCommand -CommandName "New-EasyConVcpkgWorkspaceLayout" `
            -Parameters $vcpkgLayoutParameters | Out-Null
        Assert-Contract (
            (Test-Path -LiteralPath $workspaceVcpkgMarker -PathType Leaf) -and
            (Get-Item -Force -LiteralPath $workspaceVcpkgMarker).Length -eq 0
        ) "wrong-kind workspace marker must recover after the blocking directory is removed"

        $unexpectedWorkspace = Join-Path $location.WritableRoot "unexpected-vcpkg-layout"
        $unexpectedRoot = Join-Path $unexpectedWorkspace "vcpkg/root"
        Set-ContractFile -Path (Join-Path $unexpectedRoot "unexpected.txt") -Value "blocked"
        $unexpectedParameters = $vcpkgLayoutParameters.Clone()
        $unexpectedParameters.WorkspaceRoot = $unexpectedWorkspace
        Assert-Throws -Pattern "unexpected input" -Action {
            Invoke-PrivateCommand -CommandName "New-EasyConVcpkgWorkspaceLayout" `
                -Parameters $unexpectedParameters
        }
        Assert-Contract (
            -not (Test-Path -LiteralPath (Join-Path $unexpectedRoot "vcpkg.exe")) -and
            -not (Test-Path -LiteralPath (Join-Path $unexpectedRoot ".vcpkg-root")) -and
            -not (Test-Path -LiteralPath (
                Join-Path $unexpectedRoot "scripts/buildsystems/vcpkg.cmake"
            )) -and
            -not (Test-Path -LiteralPath (
                Join-Path $unexpectedWorkspace "vcpkg/manifest/vcpkg.json"
            ))
        ) "unexpected workspace root input must fail before publishing any controlled file"
        Remove-Item -LiteralPath (Join-Path $unexpectedRoot "unexpected.txt") -Force
        Invoke-PrivateCommand -CommandName "New-EasyConVcpkgWorkspaceLayout" `
            -Parameters $unexpectedParameters | Out-Null

        $reparseWorkspace = Join-Path $location.WritableRoot "reparse-vcpkg-layout"
        $reparseRoot = Join-Path $reparseWorkspace "vcpkg/root"
        $reparseExternal = Join-Path $temporaryRoot "vcpkg layout reparse external"
        $reparseSentinel = Join-Path $reparseExternal "sentinel.txt"
        Set-ContractFile -Path $reparseSentinel -Value "external must remain unchanged"
        $reparseSentinelHash = (
            Get-FileHash -LiteralPath $reparseSentinel -Algorithm SHA256
        ).Hash
        New-Item -ItemType Directory -Force -Path $reparseRoot | Out-Null
        New-Item -ItemType Junction -Path (Join-Path $reparseRoot ".vcpkg-root") `
            -Target $reparseExternal | Out-Null
        $reparseParameters = $vcpkgLayoutParameters.Clone()
        $reparseParameters.WorkspaceRoot = $reparseWorkspace
        Assert-Throws -Pattern "reparse point" -Action {
            Invoke-PrivateCommand -CommandName "New-EasyConVcpkgWorkspaceLayout" `
                -Parameters $reparseParameters
        }
        Assert-Contract (
            (Get-FileHash -LiteralPath $reparseSentinel -Algorithm SHA256).Hash -ceq
                $reparseSentinelHash -and
            -not (Test-Path -LiteralPath (Join-Path $reparseExternal "vcpkg.exe")) -and
            -not (Test-Path -LiteralPath (Join-Path $reparseRoot "vcpkg.exe"))
        ) "reparse workspace layout must fail closed without following or partially publishing"
        Remove-Item -LiteralPath (Join-Path $reparseRoot ".vcpkg-root") -Force
        Invoke-PrivateCommand -CommandName "New-EasyConVcpkgWorkspaceLayout" `
            -Parameters $reparseParameters | Out-Null

        foreach ($name in $hardlinkSources.Keys) {
            $sourceAfterRecovery = Get-ContractFileSnapshot -Path $hardlinkSources[$name]
            Assert-Contract (
                $sourceAfterRecovery.Hash -ceq $sourceSnapshots[$name].Hash -and
                $sourceAfterRecovery.Identity -ceq $sourceSnapshots[$name].Identity
            ) "all workspace recovery paths must preserve the $name source file"
        }

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
        Remove-Item -LiteralPath $escapedWritableLeak -Force

        $auditParameters = @{
            Path = $installed
            TrustedRoot = $location.EnvironmentRoot
            RepositoryRoot = $repositoryRoot
            WritableRoot = $location.WritableRoot
            Description = "contract streaming prepared tree"
        }
        $missedLeaks = [System.Collections.Generic.List[string]]::new()
        $expectPreparedLeak = {
            param(
                [Parameter(Mandatory)]
                [string]$Fixture,

                [Parameter(Mandatory)]
                [string]$Pattern,

                [Parameter(Mandatory)]
                [string]$Name
            )

            $failure = $null
            try {
                Invoke-PrivateCommand `
                    -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                    -Parameters $auditParameters | Out-Null
            }
            catch {
                $failure = $_
            }
            if ($null -eq $failure) {
                $missedLeaks.Add($Name) | Out-Null
            }
            else {
                Assert-Contract ($failure.Exception.Message -match $Pattern) `
                    "prepared leak '$Name' failure '$($failure.Exception.Message)' must match '$Pattern'"
            }
            Remove-Item -LiteralPath $Fixture -Force
        }.GetNewClosure()

        $escapedRepository = $repositoryRoot | ConvertTo-Json -Compress
        $escapedOtherWritable = $otherWorktreeWrite | ConvertTo-Json -Compress
        $jsonTemplateLeak = Join-Path $installed "contract-escaped-source.json.in"
        [System.IO.File]::WriteAllText(
            $jsonTemplateLeak,
            "  {`n  `"generated`": { `"path`": $escapedRepository }`n}",
            [System.Text.UTF8Encoding]::new($true, $true)
        )
        & $expectPreparedLeak $jsonTemplateLeak "source worktree absolute path" `
            "BOM JSON template escaped source"

        $jsonBoundaryLeak = Join-Path $installed "contract-escaped-writable-extensionless"
        $jsonPrefix = '{"portable":true,'
        $jsonProperty = '"generatedPath":'
        $jsonBoundary = 65536
        $jsonPadding = " " * ($jsonBoundary - 6 - $jsonPrefix.Length - $jsonProperty.Length)
        Set-ContractUtf8Text -Path $jsonBoundaryLeak `
            -Value ($jsonPrefix + $jsonPadding + $jsonProperty + $escapedOtherWritable + '}')
        & $expectPreparedLeak $jsonBoundaryLeak "writable absolute path" `
            "extensionless escaped writable JSON crossing the audit window"

        foreach ($trailingLeak in @(
            [pscustomobject]@{
                Path = Join-Path $installed "contract-trailing-source.json.in"
                Value = '{"path":' + $escapedRepository + '} trailing'
                Name = "JSON template escaped source before trailing junk"
            },
            [pscustomobject]@{
                Path = Join-Path $installed "contract-trailing-writable-extensionless"
                Value = '{"path":' + $escapedOtherWritable + '} trailing'
                Name = "extensionless escaped writable path before trailing junk"
            }
        )) {
            Set-ContractUtf8Text -Path $trailingLeak.Path -Value $trailingLeak.Value
            & $expectPreparedLeak $trailingLeak.Path `
                "source worktree absolute path|writable absolute path" $trailingLeak.Name
        }

        foreach ($afterErrorControl in @(
            (Join-Path $installed "contract-forbidden-text-after-json-error.json.in"),
            (Join-Path $installed "contract-forbidden-text-after-json-error-extensionless")
        )) {
            Set-ContractUtf8Text -Path $afterErrorControl -Value (
                '{"portable":true} trailing ' + $escapedRepository
            )
            Invoke-PrivateCommand `
                -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                -Parameters $auditParameters | Out-Null
            Remove-Item -LiteralPath $afterErrorControl -Force
        }

        $utf16JsonPropertyLeak = Join-Path $installed `
            "contract-no-bom-utf16le-json.targets"
        [System.IO.File]::WriteAllText(
            $utf16JsonPropertyLeak,
            ('{' + $escapedRepository + ':"generated"}'),
            [System.Text.UnicodeEncoding]::new($false, $false, $true)
        )
        & $expectPreparedLeak $utf16JsonPropertyLeak "source worktree absolute path" `
            "no-BOM UTF-16LE JSON escaped property name"

        $xmlRepository = [System.Security.SecurityElement]::Escape($repositoryRoot)
        $utf16LeLeak = Join-Path $installed "contract-no-bom-utf16le.targets"
        $utf16LeXml = '<?xml version="1.0" encoding="utf-16"?>' +
            '<Project><PropertyGroup><ContractPath>' + $xmlRepository +
            '</ContractPath></PropertyGroup></Project>'
        [System.IO.File]::WriteAllText(
            $utf16LeLeak,
            $utf16LeXml,
            [System.Text.UnicodeEncoding]::new($false, $false, $true)
        )
        $xmlReader = [System.Xml.XmlReader]::Create($utf16LeLeak)
        try {
            while ($xmlReader.Read()) {}
        }
        finally {
            $xmlReader.Dispose()
        }
        & $expectPreparedLeak $utf16LeLeak "source worktree absolute path" `
            "no-BOM UTF-16LE XML targets"

        $utf16BeLeak = Join-Path $installed "contract-no-bom-utf16be.targets"
        $utf16BePadding = "x" * 16400
        $utf16BeXml = '<?xml version="1.0" encoding="utf-16BE"?>' +
            '<Project><PropertyGroup><ContractPath>' + $utf16BePadding + $xmlRepository +
            '</ContractPath></PropertyGroup></Project>'
        [System.IO.File]::WriteAllText(
            $utf16BeLeak,
            $utf16BeXml,
            [System.Text.UnicodeEncoding]::new($true, $false, $true)
        )
        $xmlReader = [System.Xml.XmlReader]::Create($utf16BeLeak)
        try {
            while ($xmlReader.Read()) {}
        }
        finally {
            $xmlReader.Dispose()
        }
        & $expectPreparedLeak $utf16BeLeak "source worktree absolute path" `
            "no-BOM UTF-16BE XML targets crossing the text window"

        $invalidJson = Join-Path $installed "contract-invalid.json"
        Set-ContractUtf8Text -Path $invalidJson -Value '{"unterminated":'
        Assert-Throws -Pattern "damaged structured JSON" -Action {
            Invoke-PrivateCommand `
                -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                -Parameters $auditParameters
        }
        Remove-Item -LiteralPath $invalidJson -Force

        Assert-Contract ($missedLeaks.Count -eq 0) (
            "prepared audit accepted forbidden structured paths before parser failure: {0}" -f
                ($missedLeaks -join ', ')
        )

        $directPreparedAudit = {
            param([Parameter(Mandatory)][string]$Fixture)
            & $workspaceModule {
                param($AuditPath, $RepositoryReference)
                Initialize-EasyConPreparedArtifactAuditor
                [EasyCon.WindowsWorkspace.PreparedArtifactAuditor]::Audit(
                    $AuditPath,
                    [string[]]@("source worktree"),
                    [string[]]@($RepositoryReference)
                )
            } $Fixture $repositoryRoot
        }.GetNewClosure()
        $getInstalledFingerprint = {
            Invoke-PrivateCommand -CommandName "Get-EasyConTreeFingerprint" `
                -Parameters @{
                    Path = $installed
                    TrustedRoot = $location.EnvironmentRoot
                }
        }.GetNewClosure()

        $oversizedToken = Join-Path $installed "contract-oversized-token.json.in"
        Set-ContractUtf8Text -Path $oversizedToken -Value (
            '{"payload":"' + ("x" * 65537) + '"}'
        )
        $beforeResourceFailure = & $getInstalledFingerprint
        Assert-Throws -Pattern "structured JSON audit failed closed.*token exceeds" -Action {
            Invoke-PrivateCommand `
                -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                -Parameters $auditParameters
        }
        $oversizedAudit = & $directPreparedAudit $oversizedToken
        $afterResourceFailure = & $getInstalledFingerprint
        Assert-Contract (
            $oversizedAudit.JsonLimitExceeded -and
            $oversizedAudit.MaximumJsonWindowBytes -le
                [EasyCon.WindowsWorkspace.PreparedArtifactAuditor]::JsonWindowLimitBytes -and
            $beforeResourceFailure.Files -eq $afterResourceFailure.Files -and
            $beforeResourceFailure.Sha256 -ceq $afterResourceFailure.Sha256
        ) "single-token resource failure must stay bounded, fail closed, and preserve the tree"
        Remove-Item -LiteralPath $oversizedToken -Force

        $oversizedAfterMatch = Join-Path $installed `
            "contract-match-before-oversized-token.json.in"
        Set-ContractUtf8Text -Path $oversizedAfterMatch -Value (
            '{"path":' + $escapedRepository + ',"payload":"' +
            ("x" * 65537) + '"}'
        )
        $oversizedMatchedAudit = & $directPreparedAudit $oversizedAfterMatch
        Assert-Throws -Pattern "source worktree absolute path" -Action {
            Invoke-PrivateCommand `
                -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                -Parameters $auditParameters
        }
        Assert-Contract (
            $oversizedMatchedAudit.JsonLimitExceeded -and
            $oversizedMatchedAudit.StructuredMatchedLabel -ceq "source worktree" -and
            $oversizedMatchedAudit.MaximumJsonWindowBytes -le
                [EasyCon.WindowsWorkspace.PreparedArtifactAuditor]::JsonWindowLimitBytes
        ) "a match decoded before an oversized token must survive bounded-reader failure"
        Remove-Item -LiteralPath $oversizedAfterMatch -Force

        $excessiveDepth = Join-Path $installed "contract-excessive-depth-extensionless"
        Set-ContractUtf8Text -Path $excessiveDepth -Value (
            '{"path":' + $escapedRepository + ',"nested":' +
            ("[" * 257) + '0' + ("]" * 257) + '}'
        )
        $beforeDepthFailure = & $getInstalledFingerprint
        $depthAudit = & $directPreparedAudit $excessiveDepth
        Assert-Throws -Pattern "source worktree absolute path" -Action {
            Invoke-PrivateCommand `
                -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                -Parameters $auditParameters
        }
        $afterDepthFailure = & $getInstalledFingerprint
        Assert-Contract (
            $depthAudit.JsonLimitExceeded -and
            $depthAudit.StructuredMatchedLabel -ceq "source worktree" -and
            $depthAudit.MaximumJsonWindowBytes -le
                [EasyCon.WindowsWorkspace.PreparedArtifactAuditor]::JsonWindowLimitBytes -and
            $beforeDepthFailure.Files -eq $afterDepthFailure.Files -and
            $beforeDepthFailure.Sha256 -ceq $afterDepthFailure.Sha256
        ) (
            "a match decoded before excessive JSON depth must survive reader-state " +
            "failure without polluting the tree"
        )
        Remove-Item -LiteralPath $excessiveDepth -Force

        $lockedAuditFixture = Join-Path $installed "contract-read-failure.json.in"
        Set-ContractUtf8Text -Path $lockedAuditFixture -Value '{"mode":"portable"}'
        $beforeReadFailure = & $getInstalledFingerprint
        $lockedAuditHandle = [System.IO.File]::Open(
            $lockedAuditFixture,
            [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::ReadWrite,
            [System.IO.FileShare]::None
        )
        try {
            $lockedDirectAudit = & $directPreparedAudit $lockedAuditFixture
            Assert-Contract (
                -not [string]::IsNullOrWhiteSpace($lockedDirectAudit.ResourceFailure)
            ) "injected prepared read failure must return controlled resource state"
            Assert-Throws -Pattern "audit failed closed" -Action {
                Invoke-PrivateCommand `
                    -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                    -Parameters $auditParameters
            }
        }
        finally {
            $lockedAuditHandle.Dispose()
        }
        $afterReadFailure = & $getInstalledFingerprint
        Assert-Contract (
            $beforeReadFailure.Files -eq $afterReadFailure.Files -and
            $beforeReadFailure.Sha256 -ceq $afterReadFailure.Sha256
        ) "injected prepared read failure must fail closed without polluting the tree"
        Remove-Item -LiteralPath $lockedAuditFixture -Force

        $damagedBomText = Join-Path $installed "contract-damaged-utf16.template"
        [System.IO.File]::WriteAllBytes($damagedBomText, [byte[]](0xff, 0xfe, 0x41))
        Assert-Throws -Pattern "damaged BOM text artifact" -Action {
            Invoke-PrivateCommand `
                -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
                -Parameters $auditParameters
        }
        Remove-Item -LiteralPath $damagedBomText -Force

        $binaryWithSource = Join-Path $installed "contract-binary-with-source.bin"
        $binaryBytes = [System.Collections.Generic.List[byte]]::new()
        $binaryBytes.AddRange([byte[]](0x00, 0xff, 0x80, 0x7f))
        $binaryBytes.AddRange([System.Text.Encoding]::UTF8.GetBytes($repositoryRoot))
        [System.IO.File]::WriteAllBytes($binaryWithSource, $binaryBytes.ToArray())
        Invoke-PrivateCommand `
            -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
            -Parameters $auditParameters | Out-Null

        Assert-Contract ($missedLeaks.Count -eq 0) (
            "prepared audit accepted forbidden text carriers: {0}" -f
                ($missedLeaks -join ', ')
        )

        $cleanJsonTemplate = Join-Path $installed "contract-clean-json.json.in"
        [System.IO.File]::WriteAllText(
            $cleanJsonTemplate,
            " `r`n { `"mode`": `"portable`" }",
            [System.Text.UTF8Encoding]::new($true, $true)
        )
        $manyFiles = Join-Path $installed "contract-many-files"
        for ($directoryIndex = 0; $directoryIndex -lt 16; $directoryIndex++) {
            $directory = Join-Path $manyFiles $directoryIndex
            [System.IO.Directory]::CreateDirectory($directory) | Out-Null
            for ($fileIndex = 0; $fileIndex -lt 32; $fileIndex++) {
                [System.IO.File]::WriteAllText(
                    (Join-Path $directory ("artifact-{0}.txt" -f $fileIndex)),
                    "portable prepared artifact",
                    [System.Text.UTF8Encoding]::new($false, $true)
                )
            }
        }
        $largeJson = Join-Path $installed "contract-large-json.json.in"
        $largeJsonBuilder = [System.Text.StringBuilder]::new(2MB)
        [void]$largeJsonBuilder.Append('{"items":[')
        for ($index = 0; $index -lt 140000; $index++) {
            if ($index -ne 0) {
                [void]$largeJsonBuilder.Append(',')
            }
            [void]$largeJsonBuilder.Append('"portable"')
        }
        [void]$largeJsonBuilder.Append(']}')
        Set-ContractUtf8Text -Path $largeJson -Value $largeJsonBuilder.ToString()

        $boundedAuditParameters = $auditParameters.Clone()
        $boundedAuditParameters.PassThru = $true
        $audit = Invoke-PrivateCommand `
            -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
            -Parameters $boundedAuditParameters
        Assert-Contract ($audit.FilesScanned -ge 520) `
            "streaming prepared audit must process the complete many-file tree"
        Assert-Contract ($audit.JsonDocumentsScanned -ge 2) `
            "structured JSON detection must not depend on the file extension"
        Assert-Contract (
            $audit.TotalBytesRead -gt (8 * $audit.JsonWindowLimitBytes) -and
            $audit.MaximumJsonWindowBytes -le $audit.JsonWindowLimitBytes
        ) "large JSON must use the declared bounded incremental reader window"

        $singleScanRoot = Join-Path $location.EnvironmentRoot "contract-single-scan"
        Set-ContractUtf8Text -Path (Join-Path $singleScanRoot "a.txt") -Value "a"
        Set-ContractUtf8Text -Path (Join-Path $singleScanRoot "nested/b.txt") -Value "b"
        $singleScan = Invoke-PrivateCommand `
            -CommandName "Get-EasyConPreparedTreeVerification" `
            -Parameters @{
                Path = $singleScanRoot
                TrustedRoot = $location.EnvironmentRoot
                RepositoryRoot = $repositoryRoot
                WritableRoot = $location.WritableRoot
                Description = "contract single scan prepared tree"
            }
        Assert-Contract (
            $singleScan.ControlledEnumerationPasses -eq 1 -and
            $singleScan.FilesScanned -eq 2 -and
            $singleScan.FileContentReads -eq $singleScan.FilesScanned -and
            $singleScan.FileHashesComputed -eq $singleScan.FilesScanned -and
            $singleScan.PhysicalEntriesChecked -ge $singleScan.FilesScanned
        ) "prepared tree verification must enumerate once and combine every file read, hash, and physical boundary check"
        Assert-Contract (
            $singleScan.TreeFiles -eq 2 -and
            $singleScan.TreeSha256 -ceq
                "ea515f52b71f9c89f77908efa68aed580cf901fedd831fcbdcd1c42ce216efa2"
        ) "prepared tree verification must preserve the stamp v2 sorted digest format"
        $legacyDigestBuilder = [System.Text.StringBuilder]::new()
        $legacyFiles = @(Get-ChildItem -LiteralPath $singleScanRoot -Recurse -Force -File |
            Sort-Object FullName)
        foreach ($legacyFile in $legacyFiles) {
            $legacyRelative = [System.IO.Path]::GetRelativePath(
                $singleScanRoot, $legacyFile.FullName
            ).Replace('\', '/')
            $legacyHash = (
                Get-FileHash -LiteralPath $legacyFile.FullName -Algorithm SHA256
            ).Hash.ToLowerInvariant()
            [void]$legacyDigestBuilder.Append($legacyRelative).Append("`0").Append(
                $legacyFile.Length
            ).Append("`0").Append($legacyHash).Append("`n")
        }
        $legacyTreeSha256 = [System.Convert]::ToHexString(
            [System.Security.Cryptography.SHA256]::HashData(
                [System.Text.Encoding]::UTF8.GetBytes($legacyDigestBuilder.ToString())
            )
        ).ToLowerInvariant()
        Assert-Contract (
            $singleScan.TreeFiles -eq $legacyFiles.Count -and
            $singleScan.TreeSha256 -ceq $legacyTreeSha256
        ) "prepared tree verification must remain byte-compatible with the prior stamp v2 fingerprint"

        $legacyCultureRoot = Join-Path $location.EnvironmentRoot "contract-legacy-culture-digest"
        $legacyCultureNames = @(
            ".cargo_vcs_info.json",
            ".cargo-checksum.json",
            ("z-{0}.json" -f [char]0x4e2d),
            ("z_{0}.json" -f [char]0x6587),
            ("z.{0}.json" -f [char]0x6587),
            "Z-Alpha.json"
        )
        foreach ($name in $legacyCultureNames) {
            Set-ContractUtf8Text -Path (Join-Path $legacyCultureRoot $name) `
                -Value '{"mode":"portable"}'
        }
        $previousCulture = [System.Threading.Thread]::CurrentThread.CurrentCulture
        $previousUiCulture = [System.Threading.Thread]::CurrentThread.CurrentUICulture
        try {
            $zhCn = [System.Globalization.CultureInfo]::GetCultureInfo("zh-CN")
            [System.Threading.Thread]::CurrentThread.CurrentCulture = $zhCn
            [System.Threading.Thread]::CurrentThread.CurrentUICulture = $zhCn
            $legacyCultureScan = Invoke-PrivateCommand `
                -CommandName "Get-EasyConPreparedTreeVerification" `
                -Parameters @{
                    Path = $legacyCultureRoot
                    TrustedRoot = $location.EnvironmentRoot
                    RepositoryRoot = $repositoryRoot
                    WritableRoot = $location.WritableRoot
                    Description = "contract legacy current-culture prepared tree"
                }
            $legacyCultureFiles = @(Get-ChildItem -LiteralPath $legacyCultureRoot -Recurse -Force -File |
                Sort-Object FullName)
            $legacyCultureOrder = @($legacyCultureFiles | Select-Object -ExpandProperty FullName)
            $ordinalCultureOrder = [System.Collections.Generic.List[string]]::new(
                [string[]]$legacyCultureOrder
            )
            $ordinalCultureOrder.Sort([System.StringComparer]::OrdinalIgnoreCase)
            Assert-Contract (
                ($legacyCultureOrder -join "`n") -cne ($ordinalCultureOrder -join "`n") -and
                [array]::IndexOf(
                    [string[]]$legacyCultureOrder,
                    (Join-Path $legacyCultureRoot ".cargo_vcs_info.json")
                ) -lt [array]::IndexOf(
                    [string[]]$legacyCultureOrder,
                    (Join-Path $legacyCultureRoot ".cargo-checksum.json")
                )
            ) "zh-CN punctuation and Unicode fixture must distinguish the legacy Sort-Object FullName order"
            $legacyCultureBuilder = [System.Text.StringBuilder]::new()
            foreach ($legacyCultureFile in $legacyCultureFiles) {
                $legacyCultureRelative = [System.IO.Path]::GetRelativePath(
                    $legacyCultureRoot, $legacyCultureFile.FullName
                ).Replace('\', '/')
                $legacyCultureHash = (
                    Get-FileHash -LiteralPath $legacyCultureFile.FullName -Algorithm SHA256
                ).Hash.ToLowerInvariant()
                [void]$legacyCultureBuilder.Append($legacyCultureRelative).Append("`0").Append(
                    $legacyCultureFile.Length
                ).Append("`0").Append($legacyCultureHash).Append("`n")
            }
            $legacyCultureDigest = [System.Convert]::ToHexString(
                [System.Security.Cryptography.SHA256]::HashData(
                    [System.Text.Encoding]::UTF8.GetBytes($legacyCultureBuilder.ToString())
                )
            ).ToLowerInvariant()
            Assert-Contract (
                $legacyCultureScan.TreeFiles -eq $legacyCultureFiles.Count -and
                $legacyCultureScan.TreeSha256 -ceq $legacyCultureDigest
            ) "prepared tree verification must preserve the prior current-culture Sort-Object FullName stamp v2 digest"
        }
        finally {
            [System.Threading.Thread]::CurrentThread.CurrentCulture = $previousCulture
            [System.Threading.Thread]::CurrentThread.CurrentUICulture = $previousUiCulture
        }

        $legacyCaseRoot = Join-Path $location.EnvironmentRoot "contract-legacy-case-digest"
        [System.IO.Directory]::CreateDirectory($legacyCaseRoot) | Out-Null
        $fsutil = Join-Path $env:SystemRoot "System32/fsutil.exe"
        & $fsutil file SetCaseSensitiveInfo $legacyCaseRoot enable | Out-Null
        Assert-Contract ($LASTEXITCODE -eq 0) `
            "legacy digest fixture must enable Windows case-sensitive directory semantics"
        $legacyCaseBase = "abcdef"
        foreach ($bits in 0..39) {
            $characters = $legacyCaseBase.ToCharArray()
            for ($index = 0; $index -lt $characters.Length; $index++) {
                if (($bits -band (1 -shl $index)) -ne 0) {
                    $characters[$index] = [char]::ToUpperInvariant($characters[$index])
                }
            }
            Set-ContractUtf8Text -Path (
                Join-Path $legacyCaseRoot ((-join $characters) + ".txt")
            ) -Value ("payload-{0:d2}" -f $bits)
        }
        $legacyCaseScan = Invoke-PrivateCommand `
            -CommandName "Get-EasyConPreparedTreeVerification" `
            -Parameters @{
                Path = $legacyCaseRoot
                TrustedRoot = $location.EnvironmentRoot
                RepositoryRoot = $repositoryRoot
                WritableRoot = $location.WritableRoot
                Description = "contract legacy case-sensitive prepared tree"
            }
        $legacyCaseFiles = @(
            Get-ChildItem -LiteralPath $legacyCaseRoot -Recurse -Force -File |
                Sort-Object FullName
        )
        $legacyCaseCompare = [System.Globalization.CultureInfo]::CurrentCulture.CompareInfo
        $legacyCaseFirstPath = $legacyCaseFiles[0].FullName
        Assert-Contract (
            $legacyCaseFiles.Count -eq 40 -and
            @($legacyCaseFiles | Where-Object {
                $legacyCaseCompare.Compare(
                    $legacyCaseFirstPath,
                    $_.FullName,
                    [System.Globalization.CompareOptions]::IgnoreCase
                ) -ne 0
            }).Count -eq 0
        ) "case-sensitive digest fixture must contain 40 distinct comparer-equal paths"
        $legacyCaseBuilder = [System.Text.StringBuilder]::new()
        foreach ($legacyCaseFile in $legacyCaseFiles) {
            $legacyCaseRelative = [System.IO.Path]::GetRelativePath(
                $legacyCaseRoot, $legacyCaseFile.FullName
            ).Replace('\', '/')
            $legacyCaseHash = (
                Get-FileHash -LiteralPath $legacyCaseFile.FullName -Algorithm SHA256
            ).Hash.ToLowerInvariant()
            [void]$legacyCaseBuilder.Append($legacyCaseRelative).Append("`0").Append(
                $legacyCaseFile.Length
            ).Append("`0").Append($legacyCaseHash).Append("`n")
        }
        $legacyCaseDigest = [System.Convert]::ToHexString(
            [System.Security.Cryptography.SHA256]::HashData(
                [System.Text.Encoding]::UTF8.GetBytes($legacyCaseBuilder.ToString())
            )
        ).ToLowerInvariant()
        Assert-Contract (
            $legacyCaseScan.TreeFiles -eq $legacyCaseFiles.Count -and
            $legacyCaseScan.TreeSha256 -ceq $legacyCaseDigest
        ) "prepared tree verification must preserve single-pass legacy sorting for comparer-equal paths"

        $singlePhysicalPath = Invoke-PrivateCommand `
            -CommandName "Assert-EasyConPreparedPhysicalTree" `
            -Parameters @{
                Path = $singleScanRoot
                TrustedRoot = $location.EnvironmentRoot
            }
        Assert-Contract ($singlePhysicalPath -ceq $singleScanRoot) `
            "physical-only prepared tree traversal must preserve its resolved root"
        $singlePhysicalAudit = Invoke-PrivateCommand `
            -CommandName "Assert-EasyConPreparedPhysicalTree" `
            -Parameters @{
                Path = $singleScanRoot
                TrustedRoot = $location.EnvironmentRoot
                PassThru = $true
            }
        Assert-Contract (
            $singlePhysicalAudit.ControlledEnumerationPasses -eq 1 -and
            $singlePhysicalAudit.FilesScanned -eq $singleScan.FilesScanned -and
            $singlePhysicalAudit.PhysicalEntriesChecked -ge $singlePhysicalAudit.FilesScanned
        ) "physical-only prepared tree traversal must return a concrete single-pass audit only when requested internally"
        $physicalReparseExternal = Join-Path $temporaryRoot "single physical reparse external"
        $physicalReparse = Join-Path $singleScanRoot "blocked-reparse"
        New-Item -ItemType Directory -Force -Path $physicalReparseExternal | Out-Null
        New-Item -ItemType Junction -Path $physicalReparse -Target $physicalReparseExternal | Out-Null
        try {
            Assert-Throws -Pattern "reparse point" -Action {
                Invoke-PrivateCommand -CommandName "Assert-EasyConPreparedPhysicalTree" `
                    -Parameters @{
                        Path = $singleScanRoot
                        TrustedRoot = $location.EnvironmentRoot
                    }
            }
        }
        finally {
            Remove-Item -LiteralPath $physicalReparse -Force
        }

        $preparedAuditSource = & $workspaceModule {
            @(
                ${function:Assert-EasyConPreparedTreeDoesNotReferenceRepository}.Ast.Extent.Text
                ${function:Assert-EasyConPreparedTextArtifactDoesNotReferenceRoots}.Ast.Extent.Text
            ) -join "`n"
        }
        Assert-Contract ($preparedAuditSource -notmatch 'ReadAllBytes|JsonDocument\b|\.Clone\(') `
            "prepared audit must not materialize whole JSON documents or cloned nodes"
        Assert-Contract ($preparedAuditSource -notmatch 'Get-ChildItem[^\r\n]*-Recurse') `
            "prepared audit must not materialize a recursive FileInfo list"

        $performanceRoot = Join-Path $installed "contract-streaming-performance"
        $performanceJson = $largeJsonBuilder.ToString()
        $newPerformanceTree = {
            param(
                [Parameter(Mandatory)]
                [string]$Root,

                [Parameter(Mandatory)]
                [int]$Scale
            )

            [System.IO.Directory]::CreateDirectory($Root) | Out-Null
            for ($index = 0; $index -lt (64 * $Scale); $index++) {
                [System.IO.File]::WriteAllText(
                    (Join-Path $Root ("artifact-{0}.txt" -f $index)),
                    "portable prepared performance artifact",
                    [System.Text.UTF8Encoding]::new($false, $true)
                )
            }
            for ($index = 0; $index -lt $Scale; $index++) {
                [System.IO.File]::WriteAllText(
                    (Join-Path $Root ("structured-{0}.json.in" -f $index)),
                    $performanceJson,
                    [System.Text.UTF8Encoding]::new($false, $true)
                )
            }
        }.GetNewClosure()
        $smallPerformanceTree = Join-Path $performanceRoot "one"
        $largePerformanceTree = Join-Path $performanceRoot "two"
        & $newPerformanceTree $smallPerformanceTree 1
        & $newPerformanceTree $largePerformanceTree 2

        $performanceParameters = $auditParameters.Clone()
        $performanceParameters.PassThru = $true
        $performanceParameters.Path = $smallPerformanceTree
        $performanceParameters.Description = "contract small streaming performance tree"
        $smallTimer = [System.Diagnostics.Stopwatch]::StartNew()
        $smallAudit = Invoke-PrivateCommand `
            -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
            -Parameters $performanceParameters
        $smallTimer.Stop()

        $performanceParameters.Path = $largePerformanceTree
        $performanceParameters.Description = "contract large streaming performance tree"
        $largeTimer = [System.Diagnostics.Stopwatch]::StartNew()
        $largeAudit = Invoke-PrivateCommand `
            -CommandName "Assert-EasyConPreparedTreeDoesNotReferenceRepository" `
            -Parameters $performanceParameters
        $largeTimer.Stop()

        Assert-Contract (
            $largeAudit.FilesScanned -eq (2 * $smallAudit.FilesScanned) -and
            $largeAudit.TotalBytesRead -eq (2 * $smallAudit.TotalBytesRead)
        ) "relative streaming performance fixture must exactly double files and bytes"
        $relativeTimeLimit = (3 * $smallTimer.ElapsedMilliseconds) + 3000
        Assert-Contract ($largeTimer.ElapsedMilliseconds -le $relativeTimeLimit) (
            "doubling the prepared tree must remain bounded and near-linear; " +
            "smallMs=$($smallTimer.ElapsedMilliseconds) " +
            "largeMs=$($largeTimer.ElapsedMilliseconds) limitMs=$relativeTimeLimit"
        )
        Write-Output (
            "CONTRACT_METRIC name=prepared-streaming-relative-time " +
            "smallFiles=$($smallAudit.FilesScanned) " +
            "smallBytes=$($smallAudit.TotalBytesRead) " +
            "smallMs=$($smallTimer.ElapsedMilliseconds) " +
            "largeFiles=$($largeAudit.FilesScanned) " +
            "largeBytes=$($largeAudit.TotalBytesRead) " +
            "largeMs=$($largeTimer.ElapsedMilliseconds) " +
            "limitMs=$relativeTimeLimit"
        )
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
            $jobsIndex = [Array]::IndexOf([object[]]$gate.Arguments, "--jobs")
            Assert-Contract (
                $jobsIndex -ge 0 -and $jobsIndex + 1 -lt $gate.Arguments.Count -and
                $gate.Arguments[$jobsIndex + 1] -ceq "4"
            ) "Workspace Cargo build gates must use the fixed jobs budget"
            Assert-Contract ($gate.Arguments -cnotcontains "--offline") `
                "Workspace duty separation must not be described as an offline contract"
        }
        Assert-Contract (-not @($gates | Where-Object {
            $_.Name -match "install|download|provision|vcpkg"
        })) "Workspace gates must not provision, install, or download"
    }

    Invoke-ContractCase -Name "runner-rejects-targeted-parameters-outside-targeted-mode" -Action {
        $result = Invoke-RunnerContractProcess -Arguments @(
            "-Mode", "Verify", "-TargetedCargoCommand", "test"
        )
        Assert-Contract ($result.ExitCode -ne 0) `
            "non-Targeted runner invocation with Cargo parameters must fail closed"
        Assert-Contract (
            ($result.Output + "`n" + $result.Error) -match
                "Targeted Cargo parameters require -Mode Targeted"
        ) "runner must reject targeted parameters before Verify; output=$($result.Output) error=$($result.Error)"

        $lowercaseMode = Invoke-RunnerContractProcess -Arguments @("-Mode", "verify")
        Assert-Contract ($lowercaseMode.ExitCode -ne 0) `
            "runner must reject a lowercase mode before any lifecycle action starts"
        Assert-Contract (
            ($lowercaseMode.Output + "`n" + $lowercaseMode.Error) -match
                "Mode.*(exact supported case|ValidateSet|cannot validate)"
        ) "runner must require the exact supported mode case; output=$($lowercaseMode.Output) error=$($lowercaseMode.Error)"
    }

    Invoke-ContractCase -Name "targeted-cargo-gate-is-package-bound-and-tokenized" -Action {
        $captured = [System.Collections.Generic.List[object]]::new()
        $requestedArguments = @(
            "-p", "easycon-ecs", "--test", "world_contract", "--", "--exact", "--nocapture"
        )
        Invoke-PrivateTargetedCargoGate -RepositoryRoot $repository -CargoCommand test `
            -CargoArguments $requestedArguments -GateInvoker {
                param($Name, $Program, $Arguments, $Root)
                $captured.Add([pscustomobject]@{
                    Name = $Name
                    Program = $Program
                    Arguments = @($Arguments)
                    Root = $Root
                }) | Out-Null
            }.GetNewClosure()
        Assert-Contract ($captured.Count -eq 1) `
            "a valid targeted command must invoke exactly one Cargo gate"
        $gate = $captured[0]
        Assert-Contract ($gate.Name -ceq "cargo test --locked --jobs 4 -p easycon-ecs --test world_contract -- --exact --nocapture") `
            "targeted gate name must describe the controlled command"
        Assert-Contract (
            ($gate.Arguments -join "`0") -ceq
                ((@("test", "--locked", "--jobs", "4") + $requestedArguments) -join "`0")
        ) "targeted Cargo arguments must remain distinct native tokens with --locked and --jobs inserted"
        Assert-Contract ($gate.Root -ceq $repository.Path) `
            "targeted Cargo gate must use the resolved repository root"

        $packageEquals = [System.Collections.Generic.List[object]]::new()
        Invoke-PrivateTargetedCargoGate -RepositoryRoot $repository -CargoCommand check `
            -CargoArguments @("--package=easycon-ecs") -GateInvoker {
                param($Name, $Program, $Arguments, $Root)
                $packageEquals.Add([pscustomobject]@{
                    Name = $Name
                    Arguments = @($Arguments)
                }) | Out-Null
            }.GetNewClosure()
        Assert-Contract (
            $packageEquals.Count -eq 1 -and
            ($packageEquals[0].Arguments -join "`0") -ceq
                (@("check", "--locked", "--jobs", "4", "--package=easycon-ecs") -join "`0")
        ) "--package=<name> must satisfy the explicit package requirement"

        $rejected = @(
            [pscustomobject]@{ Name = "missing package"; Arguments = @("--lib") },
            [pscustomobject]@{ Name = "missing short package value"; Arguments = @("-p") },
            [pscustomobject]@{ Name = "missing long package value"; Arguments = @("--package") },
            [pscustomobject]@{ Name = "empty equals package"; Arguments = @("--package=") }
        )
        foreach ($case in $rejected) {
            $started = 0
            Assert-Throws -Pattern "package" -Action {
                Invoke-PrivateTargetedCargoGate -RepositoryRoot $repository `
                    -CargoCommand test -CargoArguments $case.Arguments -GateInvoker {
                        param($Name, $Program, $Arguments, $Root)
                        $null = $Name, $Program, $Arguments, $Root
                        $started++
                    }.GetNewClosure()
            }
            Assert-Contract ($started -eq 0) `
                "targeted $($case.Name) must fail before starting Cargo"
        }

        $blocked = @(
            "--workspace",
            "--workspace=true",
            "--all",
            "--all=true",
            "--manifest-path",
            "--manifest-path=other/Cargo.toml",
            "--target-dir",
            "--target-dir=other-target",
            "--config",
            "--config=build.target-dir=other-target",
            "--target",
            "--target=x86_64-unknown-linux-gnu",
            "--offline",
            "--offline=true",
            "--frozen",
            "--frozen=true",
            "--jobs",
            "--jobs=1",
            "-j",
            "-j1",
            "-j=1"
        )
        foreach ($argument in $blocked) {
            $started = 0
            Assert-Throws -Pattern "not permitted" -Action {
                Invoke-PrivateTargetedCargoGate -RepositoryRoot $repository `
                    -CargoCommand clippy -CargoArguments @(
                        "-p", "easycon-ecs", $argument
                    ) -GateInvoker {
                        param($Name, $Program, $Arguments, $Root)
                        $null = $Name, $Program, $Arguments, $Root
                        $started++
                    }.GetNewClosure()
            }
            Assert-Contract ($started -eq 0) `
                "targeted Cargo must reject $argument before starting a gate"
        }

        $manifestAliases = @(
            [pscustomobject]@{
                Name = "manifest short alias"
                Arguments = @("-p", "easycon-ecs", "-m", "other/Cargo.toml")
            },
            [pscustomobject]@{
                Name = "compact manifest short alias"
                Arguments = @("-p", "easycon-ecs", "-mother/Cargo.toml")
            }
        )
        foreach ($case in $manifestAliases) {
            $started = 0
            Assert-Throws -Pattern "manifest|not permitted" -Action {
                Invoke-PrivateTargetedCargoGate -RepositoryRoot $repository `
                    -CargoCommand test -CargoArguments $case.Arguments -GateInvoker {
                        param($Name, $Program, $Arguments, $Root)
                        $null = $Name, $Program, $Arguments, $Root
                        $started++
                    }.GetNewClosure()
            }
            Assert-Contract ($started -eq 0) `
                "targeted Cargo must reject $($case.Name) before starting a gate"
        }

        $sourceWritingVariants = @("--fix", "--fix=true")
        foreach ($argument in $sourceWritingVariants) {
            $started = 0
            Assert-Throws -Pattern "fix|not permitted" -Action {
                Invoke-PrivateTargetedCargoGate -RepositoryRoot $repository `
                    -CargoCommand clippy -CargoArguments @(
                        "-p", "easycon-ecs", $argument
                    ) -GateInvoker {
                        param($Name, $Program, $Arguments, $Root)
                        $null = $Name, $Program, $Arguments, $Root
                        $started++
                    }.GetNewClosure()
            }
            Assert-Contract ($started -eq 0) `
                "targeted Cargo must reject source-writing $argument before starting a gate"
        }

        $postDelimiter = [System.Collections.Generic.List[object]]::new()
        Invoke-PrivateTargetedCargoGate -RepositoryRoot $repository -CargoCommand test `
            -CargoArguments @("-p", "easycon-ecs", "--", "-m", "other/Cargo.toml", "--jobs", "1") -GateInvoker {
                param($Name, $Program, $Arguments, $Root)
                $postDelimiter.Add([pscustomobject]@{
                    Name = $Name
                    Arguments = @($Arguments)
                }) | Out-Null
            }.GetNewClosure()
        Assert-Contract ($postDelimiter.Count -eq 1) `
            "test-binary arguments after -- must not be parsed as Cargo option overrides"

        Assert-Throws -Pattern "ValidateSet|only supports" -Action {
            Invoke-PrivateTargetedCargoGate -RepositoryRoot $repository `
                -CargoCommand build -CargoArguments @("-p", "easycon-ecs")
        }
    }

    Invoke-ContractCase -Name "staged-candidate-tree-is-stable-and-fails-closed" -Action {
        $git = (Get-Command git.exe -ErrorAction Stop).Source
        $newFixture = {
            param($Name)
            $root = Join-Path $temporaryRoot ("candidate tree {0}" -f $Name)
            Set-ContractFile -Path (Join-Path $root "tracked.txt") -Value "base`n"
            Set-ContractUtf8Text -Path (Join-Path $root "tools/windows_gate_policy.json") `
                -Value (Get-Content -LiteralPath (Join-Path $PSScriptRoot "windows_gate_policy.json") -Raw)
            & $git init --quiet $root
            & $git -C $root add -- .
            & $git -c user.name=EasyConContract -c user.email=contract@example.invalid `
                -C $root commit --quiet -m "contract fixture"
            Assert-Contract ($LASTEXITCODE -eq 0) `
                "candidate tree fixture $Name must commit its baseline"
            return $root
        }.GetNewClosure()

        $stagedRepository = & $newFixture "staged"
        Set-ContractFile -Path (Join-Path $stagedRepository "tracked.txt") -Value "candidate`n"
        & $git -C $stagedRepository add -- tracked.txt
        Assert-Contract ($LASTEXITCODE -eq 0) "candidate fixture must stage its only change"
        $binding = Invoke-PrivateWorkspaceGates -RepositoryRoot $stagedRepository `
            -RequireStagedCandidate -GateInvoker {
                param($Name, $Program, $Arguments, $Root)
                $null = $Name, $Program, $Arguments, $Root
            }
        $candidateTree = @(& $git -C $stagedRepository write-tree)[0].Trim()
        Assert-Contract (
            $binding.CandidateMode -ceq "staged-candidate" -and
            $binding.Tree -ceq $candidateTree -and
            $binding.HeadCommit -match "^[0-9a-f]{40}$"
        ) "staged Workspace must bind the candidate tree created from the current index"

        $headCommit = @(& $git -C $stagedRepository rev-parse HEAD)[0].Trim()
        $branchRef = @(& $git -C $stagedRepository symbolic-ref HEAD)[0].Trim()
        Assert-Contract (
            $LASTEXITCODE -eq 0 -and
            $headCommit -match "^[0-9a-f]{40}$" -and
            $branchRef -match "^refs/heads/"
        ) "candidate fixture must expose an immutable HEAD and branch ref"
        $baseAliases = @(
            [pscustomobject]@{ Name = "HEAD"; Value = "HEAD" },
            [pscustomobject]@{ Name = "branch ref"; Value = $branchRef },
            [pscustomobject]@{ Name = "abbreviated commit"; Value = $headCommit.Substring(0, 12) }
        )
        foreach ($baseAlias in $baseAliases) {
            $gateState = [pscustomobject]@{ Starts = 0 }
            $baseBinding = Invoke-PrivateWorkspaceGates -RepositoryRoot $stagedRepository `
                -BaseSha $baseAlias.Value -RequireStagedCandidate -GateInvoker {
                    param($Name, $Program, $Arguments, $Root)
                    $null = $Name, $Program, $Arguments, $Root
                    $gateState.Starts++
                }.GetNewClosure()
            Assert-Contract (
                $gateState.Starts -gt 0 -and
                $baseBinding.PSObject.Properties.Name -ccontains "BaseCommit" -and
                [string]$baseBinding.BaseCommit -ceq $headCommit -and
                [string]$baseBinding.BaseCommit -match "^[0-9a-f]{40}$"
            ) "workspace BaseSha $($baseAlias.Name) must bind one immutable full commit before gates"
        }

        $invalidBaseState = [pscustomobject]@{ Starts = 0 }
        Assert-Throws -Pattern "workspace base commit|base commit" -Action {
            Invoke-PrivateWorkspaceGates -RepositoryRoot $stagedRepository `
                -BaseSha "refs/heads/not-a-workspace-base" -RequireStagedCandidate -GateInvoker {
                    param($Name, $Program, $Arguments, $Root)
                    $null = $Name, $Program, $Arguments, $Root
                    $invalidBaseState.Starts++
                }.GetNewClosure()
        }
        Assert-Contract ($invalidBaseState.Starts -eq 0) `
            "invalid BaseSha must fail before any workspace gate starts"

        $diagnosticBindings = @(Invoke-PrivateWorkspaceGates -RepositoryRoot $stagedRepository `
            -RequireStagedCandidate -GateInvoker {
                param($Name, $Program, $Arguments, $Root)
                $null = $Name, $Program, $Arguments, $Root
                "synthetic gate diagnostics"
            })
        Assert-Contract (
            $diagnosticBindings.Count -eq 1 -and
            $diagnosticBindings[0].CandidateMode -ceq "staged-candidate" -and
            $diagnosticBindings[0].Tree -ceq $candidateTree -and
            $diagnosticBindings[0].HeadCommit -match "^[0-9a-f]{40}$"
        ) "Workspace gate diagnostics must not become candidate binding output"

        $unstagedRepository = & $newFixture "unstaged"
        Set-ContractFile -Path (Join-Path $unstagedRepository "tracked.txt") -Value "unstaged`n"
        Assert-Throws -Pattern "unstaged" -Action {
            Invoke-PrivateWorkspaceGates -RepositoryRoot $unstagedRepository `
                -RequireStagedCandidate -GateInvoker {
                    param($Name, $Program, $Arguments, $Root)
                    $null = $Name, $Program, $Arguments, $Root
                }
        }

        $untrackedRepository = & $newFixture "untracked"
        Set-ContractFile -Path (Join-Path $untrackedRepository "untracked.txt") -Value "untracked`n"
        Assert-Throws -Pattern "untracked" -Action {
            Invoke-PrivateWorkspaceGates -RepositoryRoot $untrackedRepository `
                -RequireStagedCandidate -GateInvoker {
                    param($Name, $Program, $Arguments, $Root)
                    $null = $Name, $Program, $Arguments, $Root
                }
        }

        $changedRepository = & $newFixture "tree-change"
        $changedFile = Join-Path $changedRepository "tracked.txt"
        Set-ContractFile -Path $changedFile -Value "candidate before gate`n"
        & $git -C $changedRepository add -- tracked.txt
        $changeState = [pscustomobject]@{ Applied = $false }
        $changingGate = {
            param($Name, $Program, $Arguments, $Root)
            $null = $Name, $Program, $Arguments, $Root
            if (-not $changeState.Applied) {
                Set-ContractFile -Path $changedFile -Value "candidate after gate`n"
                & $git -C $changedRepository add -- tracked.txt
                Assert-Contract ($LASTEXITCODE -eq 0) `
                    "tree-change gate must stage its synthetic mutation"
                $changeState.Applied = $true
            }
        }.GetNewClosure()
        Assert-Throws -Pattern "candidate tree changed" -Action {
            Invoke-PrivateWorkspaceGates -RepositoryRoot $changedRepository `
                -RequireStagedCandidate -GateInvoker $changingGate
        }
        Assert-Contract $changeState.Applied `
            "candidate tree mutation must occur inside the gate window"

        Assert-Throws -Pattern "mutually exclusive" -Action {
            Invoke-PrivateWorkspaceGates -RepositoryRoot $stagedRepository `
                -RequireCleanTree -RequireStagedCandidate -GateInvoker {
                    param($Name, $Program, $Arguments, $Root)
                    $null = $Name, $Program, $Arguments, $Root
                }
        }
    }

    Invoke-ContractCase -Name "workspace-evidence-is-atomic-and-tree-bound" -Action {
        $plain = Invoke-PrivateWorkspaceEvidenceProbe -ProbeRoot (
            Join-Path $temporaryRoot "plain workspace evidence"
        )
        Assert-Contract ($null -eq $plain.Failure -and $plain.GateCalls -eq 1) `
            "ordinary Workspace must complete the controlled gate probe"
        Assert-Contract (-not (Test-Path -LiteralPath (Join-Path $plain.WorkspaceRoot "evidence"))) `
            "ordinary Workspace must not write a reusable tree credential"
        $plainRecords = @($plain.Records | Where-Object { $_.Kind -ceq "workspace" })
        Assert-Contract ($plainRecords.Count -eq 1) `
            "ordinary Workspace must still publish a final structured summary"
        Assert-Contract (
            $plainRecords[0].Value.schemaVersion -eq 2 -and
            $plainRecords[0].Value.credential -ceq "none" -and
            [string]::IsNullOrEmpty([string]$plainRecords[0].Value.candidateMode) -and
            [string]::IsNullOrEmpty([string]$plainRecords[0].Value.tree) -and
            [string]::IsNullOrEmpty([string]$plainRecords[0].Value.evidenceFile) -and
            $plainRecords[0].Value.environmentFingerprint -ceq ("f" * 64) -and
            $plainRecords[0].Value.gatePolicyHash -cmatch "^[0-9a-f]{64}$" -and
            @($plainRecords[0].Value.gates).Count -eq 1 -and
            $plain.GateCall.Policy.CargoJobs -eq 4
        ) "ordinary Workspace summary must be v2, credential none, and policy-bound"

        $clean = Invoke-PrivateWorkspaceEvidenceProbe -ProbeRoot (
            Join-Path $temporaryRoot "clean workspace evidence"
        ) -RequireCleanTree
        Assert-Contract ($null -eq $clean.Failure -and $clean.GateCalls -eq 1) `
            "clean-tree Workspace must complete before evidence publication"
        $cleanEvidenceDirectory = Join-Path $clean.WorkspaceRoot "evidence/v2"
        $cleanEvidenceFiles = @(Get-ChildItem -LiteralPath $cleanEvidenceDirectory -File)
        Assert-Contract ($cleanEvidenceFiles.Count -eq 1 -and $cleanEvidenceFiles[0].Name -cmatch (
            "^workspace-clean-tree-{0}-[0-9a-f]{{32}}\.json$" -f $clean.Tree
        )) "clean-tree evidence must use the v2 candidate/tree/runId path"
        $cleanEvidence = Get-Content -Raw -LiteralPath $cleanEvidenceFiles[0].FullName | ConvertFrom-Json -Depth 16
        Assert-Contract (
            $cleanEvidence.schemaVersion -eq 2 -and
            $cleanEvidence.credential -ceq "tree" -and
            $cleanEvidence.candidateMode -ceq "clean-tree" -and
            $cleanEvidence.tree -ceq $clean.Tree -and
            $cleanEvidence.runId -cmatch "^[0-9a-f]{32}$" -and
            $cleanEvidence.evidenceFile -ceq ("evidence/v2/{0}" -f $cleanEvidenceFiles[0].Name)
        ) "RequireCleanTree Workspace must publish a v2 tree credential"

        $success = Invoke-PrivateWorkspaceEvidenceProbe -ProbeRoot (
            Join-Path $temporaryRoot "staged workspace evidence"
        ) -RequireStagedCandidate
        Assert-Contract ($null -eq $success.Failure -and $success.GateCalls -eq 1) `
            "staged candidate Workspace must complete before evidence publication"
        $evidenceDirectory = Join-Path $success.WorkspaceRoot "evidence/v2"
        $evidenceFiles = @(Get-ChildItem -LiteralPath $evidenceDirectory -File)
        Assert-Contract ($evidenceFiles.Count -eq 1 -and $evidenceFiles[0].Name -cmatch (
            "^workspace-staged-candidate-{0}-[0-9a-f]{{32}}\.json$" -f $success.Tree
        )) "successful staged candidate Workspace must publish one v2 evidence file"
        $evidencePath = $evidenceFiles[0].FullName
        $evidence = Get-Content -Raw -LiteralPath $evidencePath | ConvertFrom-Json -Depth 16
        Assert-Contract (
            $evidence.schemaVersion -eq 2 -and
            $evidence.status -ceq "passed" -and
            $evidence.mode -ceq "workspace" -and
            $evidence.runId -cmatch "^[0-9a-f]{32}$" -and
            $evidence.credential -ceq "tree" -and
            $evidence.candidateMode -ceq "staged-candidate" -and
            $evidence.baseCommit -ceq ("c" * 40) -and
            $evidence.headCommit -ceq ("a" * 40) -and
            $evidence.tree -ceq $success.Tree -and
            $evidence.environmentFingerprint -ceq ("f" * 64) -and
            $evidence.gatePolicyHash -cmatch "^[0-9a-f]{64}$" -and
            $evidence.verifyDurationMs -ge 0 -and
            @($evidence.gates).Count -eq 1 -and
            $evidence.gates[0].name -ceq "synthetic contract gate" -and
            $evidence.gates[0].status -ceq "passed" -and
            $evidence.gates[0].durationMs -ge 0 -and
            $evidence.target -ceq "x86_64-pc-windows-msvc" -and
            $evidence.environmentIdentity -ceq "contract-environment" -and
            $evidence.workspaceIdentity -ceq "contract-workspace" -and
            $evidence.totalDurationMs -ge 0 -and
            $evidence.durationMs -eq $evidence.totalDurationMs -and
            $evidence.evidenceFile -ceq ("evidence/v2/{0}" -f $evidenceFiles[0].Name) -and
            -not [string]::IsNullOrWhiteSpace([string]$evidence.startedUtc) -and
            -not [string]::IsNullOrWhiteSpace([string]$evidence.completedUtc)
        ) "workspace evidence must bind v2 policy, environment, candidate, timing, and publication path"
        Assert-Contract (
            @(Get-ChildItem -LiteralPath $evidenceDirectory -Filter "*.write-*" -File `
                -ErrorAction SilentlyContinue).Count -eq 0
        ) "successful evidence publication must leave no no-replace temporary file"
        $successRecords = @($success.Records | Where-Object { $_.Kind -ceq "workspace" })
        Assert-Contract (
            $successRecords.Count -eq 1 -and
            $successRecords[0].Value.credential -ceq "tree" -and
            $successRecords[0].Value.tree -ceq $success.Tree -and
            $successRecords[0].Value.evidenceFile -ceq $evidence.evidenceFile
        ) "final EASYCON_WORKSPACE record must follow successful no-replace publication"

        $collisionRoot = Join-Path $temporaryRoot "workspace evidence no-replace collision"
        $collisionDirectory = Join-Path $collisionRoot "evidence/v2"
        New-Item -ItemType Directory -Force -Path $collisionDirectory | Out-Null
        $collisionPath = Join-Path $collisionDirectory "workspace-staged-candidate-$(("b" * 40))-$("a" * 32).json"
        $originalEvidence = "{`"status`":`"original`"}`n"
        Set-ContractUtf8Text -Path $collisionPath -Value $originalEvidence
        Assert-Throws -Pattern "already exists.*not be replaced" -Action {
            Invoke-PrivateCommand -CommandName "Publish-EasyConWorkspaceEvidenceNoReplace" -Parameters @{
                Path = $collisionPath
                Text = "{`"status`":`"replacement`"}`n"
                TrustedRoot = $collisionRoot
            }
        }
        Assert-Contract ((Get-Content -Raw -LiteralPath $collisionPath) -ceq $originalEvidence) `
            "evidence collision must preserve the original file"
        Assert-Contract (
            @(Get-ChildItem -LiteralPath $collisionDirectory -Filter "*.write-*" -File `
                -ErrorAction SilentlyContinue).Count -eq 0
        ) "failed no-replace publication must leave no temporary file"

        $aliasEvidence = Invoke-PrivateWorkspaceEvidenceProbe -ProbeRoot (
            Join-Path $temporaryRoot "resolved base workspace evidence"
        ) -RequireStagedCandidate -BaseSha "HEAD" -ResolvedBaseCommit ("d" * 40)
        Assert-Contract ($null -eq $aliasEvidence.Failure -and $aliasEvidence.GateCalls -eq 1) `
            "resolved BaseSha evidence probe must complete"
        $aliasEvidenceFiles = @(Get-ChildItem -LiteralPath (Join-Path $aliasEvidence.WorkspaceRoot "evidence/v2") -File)
        Assert-Contract ($aliasEvidenceFiles.Count -eq 1) `
            "resolved BaseSha probe must publish exactly one v2 evidence file"
        $aliasEvidenceRecord = Get-Content -Raw -LiteralPath $aliasEvidenceFiles[0].FullName | ConvertFrom-Json -Depth 16
        Assert-Contract (
            $aliasEvidence.GateCall.BaseSha -ceq "HEAD" -and
            $aliasEvidenceRecord.baseCommit -ceq ("d" * 40) -and
            $aliasEvidenceRecord.baseCommit -match "^[0-9a-f]{40}$"
        ) "workspace evidence must publish the resolved immutable BaseSha rather than its input ref"

        $failed = Invoke-PrivateWorkspaceEvidenceProbe -ProbeRoot (
            Join-Path $temporaryRoot "failed workspace evidence"
        ) -RequireStagedCandidate -FailGate
        Assert-Contract (
            $null -ne $failed.Failure -and
            $failed.Failure.Exception.Message -match "synthetic candidate gate failure"
        ) "failed candidate Workspace must retain the gate failure"
        Assert-Contract (-not (Test-Path -LiteralPath (Join-Path $failed.WorkspaceRoot "evidence"))) `
            "failed candidate Workspace must not leave passed evidence"
        Assert-Contract (
            @($failed.Records | Where-Object { $_.Kind -ceq "workspace" }).Count -eq 0
        ) "failed candidate Workspace must not emit a passed workspace record"
    }

    Invoke-ContractCase -Name "workspace-gate-output-remains-structured-at-evidence-boundary" -Action {
        $git = (Get-Command git.exe -ErrorAction Stop).Source
        $repository = Join-Path $temporaryRoot "workspace gate output evidence boundary"
        $workspace = Join-Path $temporaryRoot "workspace gate output evidence workspace"
        Set-ContractFile -Path (Join-Path $repository "tracked.txt") -Value "base`n"
        New-Item -ItemType Directory -Force -Path $workspace | Out-Null
        & $git init --quiet $repository
        & $git -C $repository add -- tracked.txt
        & $git -c user.name=EasyConContract -c user.email=contract@example.invalid `
            -C $repository commit --quiet -m "contract fixture"
        Assert-Contract ($LASTEXITCODE -eq 0) `
            "workspace gate output fixture must commit its clean baseline"

        $policy = [pscustomobject]@{
            CargoJobs = 4
            Gates = @(
                [pscustomobject]@{
                    Name = "synthetic pwsh gate with stdout"
                    Tool = "pwsh"
                    Arguments = @(
                        "-NoLogo",
                        "-NoProfile",
                        "-Command",
                        "Write-Output 'synthetic gate diagnostics'"
                    )
                }
            )
        }
        $outcome = Invoke-PrivateCommand -CommandName "Invoke-EasyConWindowsWorkspaceGates" `
            -Parameters @{
                RepositoryRoot = $repository
                RequireCleanTree = $true
                Policy = $policy
            }
        $context = [pscustomobject]@{
            Fingerprint = [pscustomobject]@{ Value = ("f" * 64) }
            Configuration = [pscustomobject]@{ target = "x86_64-pc-windows-msvc" }
            Location = [pscustomobject]@{
                WorkspaceRoot = $workspace
                IdentityKey = "contract-environment"
                WorkspaceKey = "contract-workspace"
            }
        }
        $timestamp = [DateTimeOffset]::UtcNow
        $record = Invoke-PrivateCommand -CommandName "New-EasyConWorkspaceEvidenceRecord" `
            -Parameters @{
                Context = $context
                Candidate = $outcome.Candidate
                BaseCommit = $outcome.BaseCommit
                GatePolicyHash = ("a" * 64)
                Gates = @($outcome.Gates)
                VerifyDurationMilliseconds = 0L
                RunId = ("b" * 32)
                StartedUtc = $timestamp
                CompletedUtc = $timestamp
                TotalDurationMilliseconds = 1L
                Credential = "tree"
                EvidenceFile = "evidence/v2/collector-boundary.json"
            }
        Assert-Contract (
            @($record.gates).Count -eq @($outcome.Gates).Count -and
            $record.gates[0].name -ceq "synthetic pwsh gate with stdout" -and
            -not (@($record.gates | Where-Object {
                $_.name -ceq "synthetic gate diagnostics"
            }).Count -gt 0)
        ) "Workspace gate output must not corrupt ordered evidence timing records"
    }

    Invoke-ContractCase -Name "workspace-policy-snapshots-are-strict-byte-bound" -Action {
        $policyRepository = Join-Path $temporaryRoot "workspace policy strict byte snapshots"
        foreach ($name in @(
            "windows_gate_policy.json",
            "windows_gate_policy.ps1",
            "run_windows_workspace.ps1"
        )) {
            Set-ContractUtf8Text -Path (Join-Path $policyRepository "tools/$name") `
                -Value (Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot $name))
        }
        $moduleForHash = $script:workspaceModule
        $getHash = {
            param($Repository)

            return (& $moduleForHash {
                param($Root)

                return (Get-EasyConGatePolicyHash -RepositoryRoot $Root).Value
            } $Repository)
        }.GetNewClosure()
        $policyPath = Join-Path $policyRepository "tools/windows_gate_policy.json"
        $strictUtf8 = [System.Text.UTF8Encoding]::new($false, $true)
        $policyText = [System.IO.File]::ReadAllText($policyPath, $strictUtf8)
        $lfText = $policyText.Replace("`r`n", "`n").Replace("`r", "`n")
        Assert-Contract ($lfText.Contains("`n")) `
            "strict byte snapshot fixture must contain a newline"
        [System.IO.File]::WriteAllText($policyPath, $lfText, $strictUtf8)
        $lfHash = & $getHash $policyRepository
        [System.IO.File]::WriteAllText($policyPath, $lfText.Replace("`n", "`r`n"), $strictUtf8)
        $crlfHash = & $getHash $policyRepository
        Assert-Contract ($lfHash -cne $crlfHash) `
            "policy source hash must bind exact newline bytes without normalization"

        $textBytes = $strictUtf8.GetBytes($lfText)
        $bomBytes = [byte[]]::new($textBytes.Length + 3)
        $bomBytes[0] = 0xef
        $bomBytes[1] = 0xbb
        $bomBytes[2] = 0xbf
        [System.Array]::Copy($textBytes, 0, $bomBytes, 3, $textBytes.Length)
        [System.IO.File]::WriteAllBytes($policyPath, $bomBytes)
        Assert-Throws -Pattern "strict UTF-8 without BOM" -Action {
            & $getHash $policyRepository
        }
    }

    Invoke-ContractCase -Name "workspace-policy-revalidation-fails-closed" -Action {
        $changedDuringVerify = Invoke-PrivateWorkspaceEvidenceProbe -ProbeRoot (
            Join-Path $temporaryRoot "workspace policy changed during verify"
        ) -RequireStagedCandidate -PolicyMutationPhase Verify
        Assert-Contract (
            $null -ne $changedDuringVerify.Failure -and
            $changedDuringVerify.Failure.Exception.Message -match
                "policy inputs changed after Verify and before the first gate"
        ) "a policy hash change during Verify must fail before the first gate"
        Assert-Contract (
            $changedDuringVerify.PolicyMutated -and
            $changedDuringVerify.PolicyHashBefore -cne $changedDuringVerify.PolicyHashAfter
        ) "the Verify race fixture must change the real temporary policy hash"
        Assert-Contract ($changedDuringVerify.GateCalls -eq 0) `
            "a policy hash change during Verify must start zero gates"
        Assert-Contract (
            -not (Test-Path -LiteralPath (Join-Path $changedDuringVerify.WorkspaceRoot "evidence")) -and
            @($changedDuringVerify.Records | Where-Object { $_.Kind -ceq "workspace" }).Count -eq 0
        ) "a rejected Verify policy change must publish neither evidence nor a passed record"

        $changedDuringGates = Invoke-PrivateWorkspaceEvidenceProbe -ProbeRoot (
            Join-Path $temporaryRoot "workspace policy changed during gates"
        ) -RequireStagedCandidate -PolicyMutationPhase Gates
        Assert-Contract (
            $null -ne $changedDuringGates.Failure -and
            $changedDuringGates.Failure.Exception.Message -match
                "policy inputs changed after the final gate"
        ) "a policy hash change during gates must fail before evidence publication"
        Assert-Contract (
            $changedDuringGates.PolicyMutated -and
            $changedDuringGates.PolicyHashBefore -cne $changedDuringGates.PolicyHashAfter
        ) "the gate race fixture must change the real temporary policy hash"
        Assert-Contract ($changedDuringGates.GateCalls -eq 1) `
            "a policy hash change during gates must occur after the controlled gate"
        Assert-Contract (
            -not (Test-Path -LiteralPath (Join-Path $changedDuringGates.WorkspaceRoot "evidence")) -and
            @($changedDuringGates.Records | Where-Object { $_.Kind -ceq "workspace" }).Count -eq 0
        ) "a rejected gate policy change must publish neither evidence nor a passed record"
    }

    Invoke-ContractCase -Name "workspace-policy-json-snapshot-capture-fails-closed" -Action {
        Assert-WorkspacePolicySnapshotCaptureFailsClosed -MutationTarget Json
    }

    Invoke-ContractCase -Name "workspace-policy-script-snapshot-capture-fails-closed" -Action {
        Assert-WorkspacePolicySnapshotCaptureFailsClosed -MutationTarget PolicyScript
    }

    Invoke-ContractCase -Name "workspace-runner-snapshot-capture-fails-closed" -Action {
        Assert-WorkspacePolicySnapshotCaptureFailsClosed -MutationTarget RunnerSource
    }

    Invoke-ContractCase -Name "require-clean-tree-detects-ignored-python-bytecode" -Action {
        $git = (Get-Command git.exe -ErrorAction Stop).Source
        $cleanRepository = Join-Path $temporaryRoot "ignored bytecode clean tree"
        Set-ContractFile -Path (Join-Path $cleanRepository ".gitignore") `
            -Value "__pycache__/`n*.pyc`n"
        Set-ContractFile -Path (Join-Path $cleanRepository "tools/contract.py") `
            -Value "print('contract')`n"
        Set-ContractUtf8Text -Path (Join-Path $cleanRepository "tools/windows_gate_policy.json") `
            -Value (Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot "windows_gate_policy.json"))
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

    Invoke-SelectedContractCases
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
        if (-not $script:contractProbeCleanupBlocked) {
            Remove-Item -LiteralPath $resolvedTemporary -Recurse -Force -ErrorAction Stop
        }
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

$expectedContractCases = if ($script:exactCaseRequested) {
    1
}
else {
    $script:contractCases.Count
}
Assert-Contract (
    $script:contractCaseNames.Count -eq $script:contractCases.Count -and
    $script:executedContractCases -eq $expectedContractCases
) (
    "contract summary must match registered, unique, and executed counts; " +
    "registered=$($script:contractCases.Count) unique=$($script:contractCaseNames.Count) " +
    "executed=$script:executedContractCases expected=$expectedContractCases"
)
$contractMode = if ($script:exactCaseRequested) { "exact" } else { "full" }
Write-Output (
    "Windows workspace contracts passed: {0}/{1} cases (mode={2} registered={3} unique={4})" -f
        $script:executedContractCases,
        $expectedContractCases,
        $contractMode,
        $script:contractCases.Count,
        $script:contractCaseNames.Count
)

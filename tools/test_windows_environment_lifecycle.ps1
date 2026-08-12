[CmdletBinding()]
param(
    [string]$ExactCase
)

$ErrorActionPreference = "Stop"
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

function Assert-Sequence {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]]$Actual,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]]$Expected,

        [Parameter(Mandatory)]
        [string]$Description
    )

    Assert-Contract (($Actual -join "|") -ceq ($Expected -join "|")) `
        "$Description; actual=$($Actual -join ',') expected=$($Expected -join ',')"
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
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Action
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

function Get-ContractProcessEnvironmentSnapshot {
    $entries = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in [Environment]::GetEnvironmentVariables("Process").GetEnumerator()) {
        $entries.Add([pscustomobject]@{
            Name = [string]$entry.Key
            Value = [string]$entry.Value
        })
    }
    return @($entries | Sort-Object -Property Name -CaseSensitive)
}

function Restore-ContractProcessEnvironment {
    param(
        [Parameter(Mandatory)]
        [object[]]$Snapshot
    )

    foreach ($entry in @([Environment]::GetEnvironmentVariables("Process").GetEnumerator())) {
        Remove-Item -LiteralPath "Env:$([string]$entry.Key)" -ErrorAction SilentlyContinue
    }
    foreach ($entry in $Snapshot) {
        Set-Item -LiteralPath "Env:$($entry.Name)" -Value $entry.Value
    }
}

function Assert-EnvironmentSnapshot {
    param(
        [Parameter(Mandatory)]
        [object[]]$Actual,

        [Parameter(Mandatory)]
        [object[]]$Expected,

        [Parameter(Mandatory)]
        [string]$Description
    )

    Assert-Contract ($Actual.Count -eq $Expected.Count) `
        "$Description must restore the exact variable count"
    for ($index = 0; $index -lt $Expected.Count; $index++) {
        Assert-Contract (
            $Actual[$index].Name -ceq $Expected[$index].Name -and
            $Actual[$index].Value -ceq $Expected[$index].Value
        ) "$Description differs at environment entry $index"
    }
}

function Wait-ContractFileCreated {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [int]$TimeoutMilliseconds = 10000,

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

    if ($null -eq $Task) {
        return "not-started"
    }
    if (-not $Task.IsCompleted) {
        return "pending"
    }
    if ($Task.IsCanceled) {
        return "canceled observed=$Observed"
    }
    if ($Task.IsFaulted) {
        return "faulted observed=$Observed"
    }
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
    if ($null -eq $Probe.Process) {
        return @()
    }

    $hasExited = $false
    try {
        $hasExited = [bool]$Probe.Process.HasExited
    }
    catch {
        Add-ContractProbeOutputCaptureIssue -Probe $Probe `
            -Issue "process exit observation failed: $($_.Exception.Message)"
    }
    if (-not $hasExited) {
        return @($Probe.OutputCaptureIssues)
    }

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
        if ($Probe.($stream.ObservedProperty)) {
            continue
        }
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
    if ($Probe.PipeCancellationRequested -or $null -eq $Probe.Process) {
        return
    }
    $Probe.PipeCancellationRequested = $true
    foreach ($readerName in @("StandardOutput", "StandardError")) {
        try {
            $reader = $Probe.Process.$readerName
            if ($null -ne $reader) {
                $reader.Dispose()
            }
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
    try {
        return Get-Content -Raw -LiteralPath $Probe.Trace
    }
    catch {
        return "<trace read failed: $($_.Exception.Message)>"
    }
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
        catch {
            $pidValue = "<unavailable: $($_.Exception.Message)>"
        }
        try {
            if ($Probe.Process.HasExited) {
                $exitCode = [string]$Probe.Process.ExitCode
            }
            else {
                $exitCode = "<running>"
            }
        }
        catch {
            $exitCode = "<unavailable: $($_.Exception.Message)>"
        }
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
    else {
        $Probe.OutputCaptureFailure
    }
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
        "outputCaptureFailure=$captureFailure`n" +
        "trace<<`n$trace`n>>trace"
    )
}

function Cache-ContractProbeDiagnostic {
    param([Parameter(Mandatory)][object]$Probe)

    Initialize-ContractProbeTeardownState -Probe $Probe
    if ($Probe.DiagnosticCached) {
        return
    }
    try {
        $Probe.TeardownDiagnostic = Get-ContractProbeDiagnostic -Probe $Probe
    }
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
    finally {
        $Probe.DiagnosticCached = $true
    }
}

function Get-ContractProbeDiagnostics {
    param([Parameter(Mandatory)][object[]]$Probe)

    return (@($Probe | ForEach-Object {
        if (
            $null -ne $_.PSObject.Properties["TeardownDiagnostic"] -and
            -not [string]::IsNullOrWhiteSpace($_.TeardownDiagnostic)
        ) {
            $_.TeardownDiagnostic
        }
        else {
            Get-ContractProbeDiagnostic -Probe $_
        }
    }) -join "`n---`n")
}

function Update-ContractProbeTeardownSnapshot {
    param([Parameter(Mandatory)][object]$Probe)

    Initialize-ContractProbeTeardownState -Probe $Probe
    if ($Probe.WasReclaimed -or $null -eq $Probe.Process) {
        $Probe.LastProcessExited = $true
        return
    }
    try {
        $Probe.LastProcessExited = [bool]$Probe.Process.WaitForExit(0)
    }
    catch {
        Add-ContractProbeTeardownIssue -Probe $Probe `
            -Issue "process WaitForExit failed: $($_.Exception.Message)"
        return
    }
    if ($null -ne $Probe.KillTask -and $Probe.KillTask.IsCompleted -and -not $Probe.KillTaskObserved) {
        try {
            $null = $Probe.KillTask.GetAwaiter().GetResult()
        }
        catch {
            Add-ContractProbeTeardownIssue -Probe $Probe `
                -Issue "process tree kill failed: $($_.Exception.Message)"
        }
        finally {
            $Probe.KillTaskObserved = $true
        }
    }
    if ($Probe.LastProcessExited) {
        $null = Complete-ContractProbeOutput -Probe $Probe
    }
}

function Test-ContractProbeCanBeReclaimed {
    param([Parameter(Mandatory)][object]$Probe)

    Initialize-ContractProbeTeardownState -Probe $Probe
    if ($null -eq $Probe.Process) {
        return $true
    }
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

    if ($Probe.WasReclaimed) {
        return
    }
    if ($null -eq $Probe.Process) {
        $Probe.WasReclaimed = $true
        return
    }
    if (-not (Test-ContractProbeCanBeReclaimed -Probe $Probe)) {
        return
    }
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

    # Launch every live tree kill before any wait, drain, or diagnostic can consume the deadline.
    foreach ($candidate in $Probe) {
        if ($candidate.WasReclaimed -or $null -eq $candidate.Process) {
            continue
        }
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
        if (@($Probe | Where-Object { -not $_.WasReclaimed }).Count -eq 0) {
            break
        }
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
        [System.Threading.Thread]::Sleep([int][Math]::Min(20L, [long]$remaining))
    }

    $issues = [System.Collections.Generic.List[string]]::new()
    $livePids = [System.Collections.Generic.List[string]]::new()
    $unreclaimedPids = [System.Collections.Generic.List[string]]::new()
    foreach ($candidate in $Probe) {
        Initialize-ContractProbeTeardownState -Probe $candidate
        if (-not $candidate.DiagnosticCached) {
            Cache-ContractProbeDiagnostic -Probe $candidate
        }
        if (-not $candidate.WasReclaimed) {
            $pidValue = "$($candidate.Name):unknown"
            try {
                $pidValue = [string]$candidate.Process.Id
            }
            catch {
            }
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
    if ($unreclaimedPids.Count -gt 0) {
        $script:contractProbeCleanupBlocked = $true
    }
    if ($issues.Count -gt 0) {
        $primary = $issues[0]
        $additional = if ($issues.Count -gt 1) {
            @($issues | Select-Object -Skip 1) -join "; "
        }
        else {
            "<none>"
        }
        $livePidText = if ($livePids.Count -eq 0) { "<none>" } else { $livePids -join "," }
        $unreclaimedPidText = if ($unreclaimedPids.Count -eq 0) {
            "<none>"
        }
        else {
            $unreclaimedPids -join ","
        }
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
        if ($null -eq $probe.Process) {
            throw "Process.Start returned null"
        }
        $probe.ProcessId = [string]$probe.Process.Id
        Set-ContractProbeState -Probe $probe -State Started -Milestone "process.started"
        if ($null -ne $AfterProcessStarted) {
            & $AfterProcessStarted $probe
        }
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
        try {
            Stop-ContractProbes -Probe $cleanupProbeArray -Deadline $TeardownDeadline
        }
        catch {
            $cleanupFailure = $_
        }
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
        try {
            $hasExited = [bool](& $ProcessHasExitedAction $candidate.Process)
        }
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
            $captureText = if ($captureIssues.Count -eq 0) {
                ""
            }
            else {
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
        if (@($required | Where-Object { $_.State -cne "Ready" }).Count -eq 0) {
            return
        }
        $remaining = [long]$TimeoutMilliseconds - [long]$Timer.ElapsedMilliseconds
        if ($remaining -le 0) {
            # The final snapshot gives an observed early exit precedence over timeout.
            Update-ContractProbeReadinessSnapshot -Probe $Probe `
                -ProcessHasExitedAction $ProcessHasExitedAction
            if (@($required | Where-Object { $_.State -cne "Ready" }).Count -eq 0) {
                return
            }
            throw (
                "contract failed: timed out waiting for probe readiness; " +
                (Get-ContractProbeDiagnostics -Probe $Probe)
            )
        }
        [System.Threading.Thread]::Sleep([int][Math]::Min(20L, $remaining))
    }
}

function Publish-ContractProbeRelease {
    param([Parameter(Mandatory)][object[]]$Probe)

    foreach ($candidate in $Probe) {
        if ($candidate.Process.HasExited) {
            $captureIssues = @(Complete-ContractProbeOutputFinalSnapshot -Probe $candidate)
            Set-ContractProbeState -Probe $candidate -State ExitedEarly `
                -Milestone "process.exited-before-release"
            $captureText = if ($captureIssues.Count -eq 0) {
                ""
            }
            else {
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
            $releasePath,
            "release",
            [System.Text.UTF8Encoding]::new($false)
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
            if (-not $candidate.OutputCaptured) {
                continue
            }
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
        if (@($Probe | Where-Object { $_.State -cne "Exited" }).Count -eq 0) {
            return
        }
        $remaining = Get-ContractProbeDeadlineRemaining -Deadline $deadline
        if ($remaining -le 0) {
            Update-ContractProbeExitSnapshot -Probe $Probe
            if (@($Probe | Where-Object { $_.State -cne "Exited" }).Count -eq 0) {
                return
            }
            throw (
                "contract failed: timed out waiting for released probes to exit; " +
                (Get-ContractProbeDiagnostics -Probe $Probe)
            )
        }
        [System.Threading.Thread]::Sleep([int][Math]::Min(20L, $remaining))
    }
}

function Assert-ContractProbeReadinessDeadlineRegression {
    param([Parameter(Mandatory)][string]$ProbeRoot)

    New-Item -ItemType Directory -Force -Path $ProbeRoot | Out-Null
    $trace = Join-Path $ProbeRoot "deadline-final-observation-trace.txt"
    [System.IO.File]::WriteAllText(
        $trace,
        "2026-08-10T00:00:00.0000000Z|exit.before-ready`n",
        [System.Text.UTF8Encoding]::new($false)
    )
    $process = [pscustomobject]@{
        Id = 42423
        HasExited = $false
        ExitCode = 23
    }
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
        Command = "synthetic-probe.exe"
        ArgumentListJson = '["--deadline-exit","path with spaces"]'
        OutputTask = [System.Threading.Tasks.Task]::FromResult([string]"synthetic stdout")
        ErrorTask = [System.Threading.Tasks.Task]::FromResult(
            [string]"SYNTHETIC_DEADLINE_EXIT_STDERR"
        )
        OutputCaptured = $false
        OutputCaptureFailure = ""
        Stdout = ""
        Stderr = ""
        Ready = Join-Path $ProbeRoot "deadline-final-observation-ready.txt"
        Release = Join-Path $ProbeRoot "deadline-final-observation-release.txt"
        Result = Join-Path $ProbeRoot "deadline-final-observation-result.txt"
        Trace = $trace
        TeardownDeadline = New-ContractProbeDeadline -Name "readiness regression teardown" `
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
    catch {
        $failure = $_
    }
    finally {
        $timer.Stop()
    }
    $message = if ($null -eq $failure) { "<none>" } else { $failure.Exception.Message }
    Assert-Contract (
        $null -ne $failure -and
        $probe.State -ceq "ExitedEarly" -and
        $exitReads.Count -eq 2 -and
        $message -match 'child exited before ready/result' -and
        $message -notmatch 'timed out waiting for probe readiness' -and
        $message -match 'exitCode=23' -and
        $message -match 'SYNTHETIC_DEADLINE_EXIT_STDERR'
    ) (
        "readiness deadline must take one final marker/process/trace snapshot and prefer " +
        "the observed early exit; exitReads=$($exitReads.Count) state=$($probe.State) " +
        "observed=$message"
    )
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
        $firstStart = [System.Diagnostics.ProcessStartInfo]::new()
        $firstStart.FileName = Join-Path $PSHOME "pwsh.exe"
        $firstStart.UseShellExecute = $false
        $firstStart.RedirectStandardOutput = $true
        $firstStart.RedirectStandardError = $true
        foreach ($argument in @("-NoLogo", "-NoProfile", "-Command", "exit 0")) {
            $firstStart.ArgumentList.Add($argument)
        }
        $firstProcess = [System.Diagnostics.Process]::Start($firstStart)
        Assert-Contract ($null -ne $firstProcess -and $firstProcess.WaitForExit(5000)) `
            "the teardown regression's first process must exit"
        $processes.Add($firstProcess) | Out-Null

        $secondStart = [System.Diagnostics.ProcessStartInfo]::new()
        $secondStart.FileName = Join-Path $PSHOME "pwsh.exe"
        $secondStart.UseShellExecute = $false
        $secondStart.RedirectStandardOutput = $true
        $secondStart.RedirectStandardError = $true
        foreach ($argument in @("-NoLogo", "-NoProfile", "-Command", "Start-Sleep -Seconds 30")) {
            $secondStart.ArgumentList.Add($argument)
        }
        $secondProcess = [System.Diagnostics.Process]::Start($secondStart)
        Assert-Contract ($null -ne $secondProcess -and -not $secondProcess.HasExited) `
            "the teardown regression's second process must remain running"
        $processes.Add($secondProcess) | Out-Null

        $firstProbe = [pscustomobject]@{
            Name = "faulted-one"
            State = "Ready"
            Progress = "ready.observed"
            LastMilestone = "ready.observed"
            Process = $firstProcess
            Command = $firstStart.FileName
            ArgumentListJson = ConvertTo-Json -InputObject @($firstStart.ArgumentList) -Compress
            OutputTask = [System.Threading.Tasks.Task]::FromException[string](
                [System.InvalidOperationException]::new("synthetic-drain-failure")
            )
            ErrorTask = [System.Threading.Tasks.Task]::FromResult([string]"first stderr")
            OutputCaptured = $false
            Stdout = ""
            Stderr = ""
            Ready = Join-Path $ProbeRoot "faulted-one-ready.txt"
            Release = Join-Path $ProbeRoot "faulted-one-release.txt"
            Result = Join-Path $ProbeRoot "faulted-one-result.txt"
            Trace = Join-Path $ProbeRoot "faulted-one-trace.txt"
            WasReclaimed = $false
        }
        $secondProbe = [pscustomobject]@{
            Name = "live-two"
            State = "Ready"
            Progress = "ready.observed"
            LastMilestone = "ready.observed"
            Process = $secondProcess
            Command = $secondStart.FileName
            ArgumentListJson = ConvertTo-Json -InputObject @($secondStart.ArgumentList) -Compress
            OutputTask = $secondProcess.StandardOutput.ReadToEndAsync()
            ErrorTask = $secondProcess.StandardError.ReadToEndAsync()
            OutputCaptured = $false
            Stdout = ""
            Stderr = ""
            Ready = Join-Path $ProbeRoot "live-two-ready.txt"
            Release = Join-Path $ProbeRoot "live-two-release.txt"
            Result = Join-Path $ProbeRoot "live-two-result.txt"
            Trace = Join-Path $ProbeRoot "live-two-trace.txt"
            WasReclaimed = $false
        }
        Set-Content -LiteralPath $firstProbe.Trace -Encoding utf8NoBOM -Value "one|ready.written"
        Set-Content -LiteralPath $secondProbe.Trace -Encoding utf8NoBOM -Value "two|ready.written"
        $probes.Add($firstProbe) | Out-Null
        $probes.Add($secondProbe) | Out-Null
        $failure = $null
        $timer = [System.Diagnostics.Stopwatch]::StartNew()
        try {
            Stop-ContractProbes -Probe @($firstProbe, $secondProbe) -TimeoutMilliseconds 1000
        }
        catch {
            $failure = $_
        }
        finally {
            $timer.Stop()
        }
        $message = if ($null -eq $failure) { "<none>" } else { $failure.Exception.Message }
        Assert-Contract (
            $null -ne $failure -and
            -not $firstProbe.OutputCaptured -and
            $firstProbe.OutputTaskObserved -and
            $firstProbe.ErrorTaskObserved -and
            $firstProbe.WasReclaimed -and
            $secondProbe.WasReclaimed -and
            $timer.ElapsedMilliseconds -le 2000 -and
            $message -match 'synthetic-drain-failure' -and
            $message -match 'probe=faulted-one' -and
            $message -match 'probe=live-two' -and
            $message -match 'pid=\d+' -and
            $message -match 'ArgumentList=' -and
            $message -match 'stdout<<' -and
            $message -match 'stderr<<' -and
            $message -match 'lastMilestone='
        ) (
            "faulted drain teardown must preserve the first failure, reclaim every owned child, " +
            "and expose complete diagnostics; elapsedMs=$($timer.ElapsedMilliseconds) observed=$message"
        )

        $startupOwnedStart = [System.Diagnostics.ProcessStartInfo]::new()
        $startupOwnedStart.FileName = Join-Path $PSHOME "pwsh.exe"
        $startupOwnedStart.UseShellExecute = $false
        $startupOwnedStart.RedirectStandardOutput = $true
        $startupOwnedStart.RedirectStandardError = $true
        foreach ($argument in @("-NoLogo", "-NoProfile", "-Command", "Start-Sleep -Seconds 30")) {
            $startupOwnedStart.ArgumentList.Add($argument)
        }
        $startupOwnedProcess = [System.Diagnostics.Process]::Start($startupOwnedStart)
        Assert-Contract (
            $null -ne $startupOwnedProcess -and -not $startupOwnedProcess.HasExited
        ) "startup-failure regression's first owned process must remain running"
        $processes.Add($startupOwnedProcess) | Out-Null
        $startupOwnedProbe = [pscustomobject]@{
            Name = "startup-owned-one"
            State = "Started"
            Progress = "process.started"
            LastMilestone = "process.started"
            Process = $startupOwnedProcess
            Command = $startupOwnedStart.FileName
            ArgumentListJson = ConvertTo-Json -InputObject @($startupOwnedStart.ArgumentList) -Compress
            OutputTask = $startupOwnedProcess.StandardOutput.ReadToEndAsync()
            ErrorTask = $startupOwnedProcess.StandardError.ReadToEndAsync()
            OutputCaptured = $false
            Stdout = ""
            Stderr = ""
            Ready = Join-Path $ProbeRoot "startup-owned-one-ready.txt"
            Release = Join-Path $ProbeRoot "startup-owned-one-release.txt"
            Result = Join-Path $ProbeRoot "startup-owned-one-result.txt"
            Trace = Join-Path $ProbeRoot "startup-owned-one-trace.txt"
            WasReclaimed = $false
        }
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
                -TeardownDeadline (New-ContractProbeDeadline -Name "startup fault teardown" `
                    -TimeoutMilliseconds 1000) -OwnedProbe @($startupOwnedProbe) `
                -AfterDrainsInitialized {
                    param([object]$Probe)
                    throw "synthetic-startup-drain-initialization-failure"
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
            $startupMessage -match "synthetic-startup-drain-initialization-failure" -and
            $startupMessage -match "startup-owned-one" -and
            $startupMessage -match "startup-fault-two"
        ) (
            "a startup failure must launch and reclaim every already-owned child before " +
            "its shared teardown deadline can be consumed; elapsedMs=$($startupTimer.ElapsedMilliseconds) " +
            "observed=$startupMessage"
        )

        $pendingStart = [System.Diagnostics.ProcessStartInfo]::new()
        $pendingStart.FileName = Join-Path $PSHOME "pwsh.exe"
        $pendingStart.UseShellExecute = $false
        $pendingStart.RedirectStandardOutput = $true
        $pendingStart.RedirectStandardError = $true
        foreach ($argument in @("-NoLogo", "-NoProfile", "-Command", "exit 0")) {
            $pendingStart.ArgumentList.Add($argument)
        }
        $pendingProcess = [System.Diagnostics.Process]::Start($pendingStart)
        Assert-Contract ($null -ne $pendingProcess -and $pendingProcess.WaitForExit(5000)) `
            "the pending-drain regression's first process must exit"
        $processes.Add($pendingProcess) | Out-Null

        $pendingLiveStart = [System.Diagnostics.ProcessStartInfo]::new()
        $pendingLiveStart.FileName = Join-Path $PSHOME "pwsh.exe"
        $pendingLiveStart.UseShellExecute = $false
        $pendingLiveStart.RedirectStandardOutput = $true
        $pendingLiveStart.RedirectStandardError = $true
        foreach ($argument in @("-NoLogo", "-NoProfile", "-Command", "Start-Sleep -Seconds 30")) {
            $pendingLiveStart.ArgumentList.Add($argument)
        }
        $pendingLiveProcess = [System.Diagnostics.Process]::Start($pendingLiveStart)
        Assert-Contract ($null -ne $pendingLiveProcess -and -not $pendingLiveProcess.HasExited) `
            "the pending-drain regression's second process must remain running"
        $processes.Add($pendingLiveProcess) | Out-Null

        $pendingStdout = [System.Threading.Tasks.TaskCompletionSource[string]]::new(
            [System.Threading.Tasks.TaskCreationOptions]::RunContinuationsAsynchronously
        )
        $pendingProbe = [pscustomobject]@{
            Name = "pending-one"
            State = "Ready"
            Progress = "ready.observed"
            LastMilestone = "ready.observed"
            Process = $pendingProcess
            Command = $pendingStart.FileName
            ArgumentListJson = ConvertTo-Json -InputObject @($pendingStart.ArgumentList) -Compress
            OutputTask = $pendingStdout.Task
            ErrorTask = [System.Threading.Tasks.Task]::FromResult([string]"pending stderr")
            OutputCaptured = $false
            Stdout = ""
            Stderr = ""
            Ready = Join-Path $ProbeRoot "pending-one-ready.txt"
            Release = Join-Path $ProbeRoot "pending-one-release.txt"
            Result = Join-Path $ProbeRoot "pending-one-result.txt"
            Trace = Join-Path $ProbeRoot "pending-one-trace.txt"
            WasReclaimed = $false
        }
        $liveProbe = [pscustomobject]@{
            Name = "pending-live-two"
            State = "Ready"
            Progress = "ready.observed"
            LastMilestone = "ready.observed"
            Process = $pendingLiveProcess
            Command = $pendingLiveStart.FileName
            ArgumentListJson = ConvertTo-Json -InputObject @($pendingLiveStart.ArgumentList) -Compress
            OutputTask = $pendingLiveProcess.StandardOutput.ReadToEndAsync()
            ErrorTask = $pendingLiveProcess.StandardError.ReadToEndAsync()
            OutputCaptured = $false
            Stdout = ""
            Stderr = ""
            Ready = Join-Path $ProbeRoot "pending-live-two-ready.txt"
            Release = Join-Path $ProbeRoot "pending-live-two-release.txt"
            Result = Join-Path $ProbeRoot "pending-live-two-result.txt"
            Trace = Join-Path $ProbeRoot "pending-live-two-trace.txt"
            WasReclaimed = $false
        }
        Set-Content -LiteralPath $pendingProbe.Trace -Encoding utf8NoBOM -Value "one|process.exited"
        Set-Content -LiteralPath $liveProbe.Trace -Encoding utf8NoBOM -Value "two|ready.written"
        $probes.Add($pendingProbe) | Out-Null
        $probes.Add($liveProbe) | Out-Null
        $pendingFailure = $null
        $pendingTimer = [System.Diagnostics.Stopwatch]::StartNew()
        try {
            Stop-ContractProbes -Probe @($pendingProbe, $liveProbe) -TimeoutMilliseconds 300
        }
        catch {
            $pendingFailure = $_
        }
        finally {
            $pendingTimer.Stop()
        }
        $pendingMessage = if ($null -eq $pendingFailure) {
            "<none>"
        }
        else {
            $pendingFailure.Exception.Message
        }
        Assert-Contract (
            $null -ne $pendingFailure -and
            -not $pendingProbe.OutputCaptured -and
            -not $pendingProbe.OutputTaskObserved -and
            -not $pendingProbe.WasReclaimed -and
            $liveProbe.WasReclaimed -and
            $liveProbe.KillLaunchAttempted -and
            $script:contractProbeCleanupBlocked -and
            $pendingTimer.ElapsedMilliseconds -le 1000 -and
            $pendingMessage -match 'pending-one' -and
            $pendingMessage -match 'pending-live-two'
        ) (
            "pending drain teardown must launch the second live child kill before draining, " +
            "leave incomplete output and its handle unreclaimed, and block cleanup; " +
            "elapsedMs=$($pendingTimer.ElapsedMilliseconds) observed=$pendingMessage"
        )
        Assert-Contract ($pendingStdout.TrySetResult("pending stdout released")) `
            "the pending-drain regression must explicitly complete stdout"
        Stop-ContractProbes -Probe @($pendingProbe) -TimeoutMilliseconds 1000
        Assert-Contract (
            $pendingProbe.OutputCaptured -and
            $pendingProbe.OutputTaskObserved -and
            $pendingProbe.ErrorTaskObserved -and
            $pendingProbe.WasReclaimed -and
            $pendingProbe.Stdout -ceq "pending stdout released"
        ) "explicitly completed output must be observed before its process handle is reclaimed"
        $script:contractProbeCleanupBlocked = $false
    }
    finally {
        if ($null -ne $pendingStdout) {
            $pendingStdout.TrySetResult("fixture cleanup") | Out-Null
        }
        for ($index = 0; $index -lt $processes.Count; $index++) {
            if ($index -lt $probes.Count -and [bool]$probes[$index].WasReclaimed) {
                continue
            }
            $process = $processes[$index]
            try {
                if (-not $process.HasExited) {
                    $process.Kill($true)
                    $null = $process.WaitForExit(5000)
                }
            }
            finally {
                $process.Dispose()
            }
        }
    }
}

function Invoke-SharedWorkspaceProbeSynchronizationContract {
    param(
        [Parameter(Mandatory)][string]$ProbeRoot,
        [Parameter(Mandatory)][object]$FirstLocation,
        [Parameter(Mandatory)][object]$SecondLocation,
        [Parameter(Mandatory)][string]$ModulePath,
        [ValidateSet("Synchronization", "EarlyExit")]
        [string]$Scenario = "Synchronization",
        [int]$ReadinessTimeoutMilliseconds = 10000,
        [int]$CoordinationAndTeardownMarginMilliseconds = 5000,
        [int]$ExitTimeoutMilliseconds = 10000
    )

    $releaseTimeoutMilliseconds =
        $ReadinessTimeoutMilliseconds + $CoordinationAndTeardownMarginMilliseconds
    Assert-Contract (
        $releaseTimeoutMilliseconds -eq (
            $ReadinessTimeoutMilliseconds + $CoordinationAndTeardownMarginMilliseconds
        )
    ) "child release watchdog must derive from readiness plus coordination/teardown margin"
    New-Item -ItemType Directory -Force -Path $ProbeRoot | Out-Null
    $childPath = Join-Path $ProbeRoot "workspace-probe.ps1"
    Set-Content -LiteralPath $childPath -Encoding utf8NoBOM -Value @'
param(
    [Parameter(Mandatory)][string]$ModulePath,
    [Parameter(Mandatory)][string]$CacheRoot,
    [Parameter(Mandatory)][string]$EnvironmentRoot,
    [Parameter(Mandatory)][string]$StampPath,
    [Parameter(Mandatory)][string]$LockPath,
    [Parameter(Mandatory)][string]$WorkspaceKey,
    [Parameter(Mandatory)][string]$WorkspaceRoot,
    [Parameter(Mandatory)][string]$WorkspaceLockPath,
    [Parameter(Mandatory)][string]$StartPath,
    [Parameter(Mandatory)][string]$ReadyPath,
    [Parameter(Mandatory)][string]$ReleasePath,
    [Parameter(Mandatory)][string]$ResultPath,
    [Parameter(Mandatory)][string]$TracePath,
    [Parameter(Mandatory)][string]$Name,
    [Parameter(Mandatory)][string]$Scenario,
    [Parameter(Mandatory)][int]$ReleaseTimeoutMilliseconds
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
                throw "shared workspace probe timed out waiting for $Description"
            }
            [System.Threading.Thread]::Sleep([int][Math]::Min(20L,$remaining))
        }
    }
    finally {
        $timer.Stop()
    }
}
Wait-ProbeMarker -Path $StartPath -Description "parent start" `
    -TimeoutMilliseconds $ReleaseTimeoutMilliseconds
Write-ProbeTrace "start.observed"
Import-Module -Name $ModulePath -Force
$module = Get-Module windows_workspace
$location = [pscustomobject]@{
    CacheRoot = $CacheRoot
    EnvironmentRoot = $EnvironmentRoot
    StampPath = $StampPath
    LockPath = $LockPath
    IdentityKey = "shared-workspace-probe"
    WorkspaceKey = $WorkspaceKey
    WorkspaceRoot = $WorkspaceRoot
    WorkspaceLockPath = $WorkspaceLockPath
    CargoTargetDirectory = Join-Path $WorkspaceRoot "target"
}
$verify = {
    if (
        -not (Test-Path -LiteralPath $StampPath -PathType Leaf) -or
        (Get-Content -Raw -LiteralPath $StampPath).Trim() -cne "ready"
    ) {
        throw "shared workspace probe environment is not ready"
    }
    Write-ProbeTrace "verify.completed"
    return "verified"
}.GetNewClosure()
$workspace = {
    param($Summary)
    if ($Summary -cne "verified") {
        throw "shared workspace probe did not receive verified state"
    }
    if ($Scenario -ceq "EarlyExit" -and $Name -ceq "two") {
        Write-ProbeTrace "exit.before-ready"
        [Console]::Error.WriteLine("SYNTHETIC_SECOND_PROBE_EARLY_EXIT")
        exit 23
    }
    [System.IO.File]::WriteAllText($ReadyPath,"ready",[System.Text.UTF8Encoding]::new($false))
    Write-ProbeTrace "ready.written"
    Wait-ProbeMarker -Path $ReleasePath -Description "parent release" `
        -TimeoutMilliseconds $ReleaseTimeoutMilliseconds
    Write-ProbeTrace "release.observed"
}.GetNewClosure()
& $module {
    param($OwnedLocation,$VerifyAction,$WorkspaceAction)
    Invoke-EasyConEnvironmentLifecycle -Mode Workspace -Location $OwnedLocation `
        -SetupAction { throw "Workspace probe must not provision" } `
        -VerifyAction $VerifyAction -WorkspaceAction $WorkspaceAction `
        -LeaseTimeoutMilliseconds 10000
} $location $verify $workspace | Out-Null
[System.IO.File]::WriteAllText($ResultPath,"passed",[System.Text.UTF8Encoding]::new($false))
Write-ProbeTrace "result.validated"
'@

    $probes = [System.Collections.Generic.List[object]]::new()
    $teardownDeadline = New-ContractProbeDeadline -Name "shared workspace probe teardown" `
        -TimeoutMilliseconds $CoordinationAndTeardownMarginMilliseconds
    $primaryFailure = $null
    try {
        foreach ($entry in @(
            [pscustomobject]@{ Name = "one"; Location = $FirstLocation },
            [pscustomobject]@{ Name = "two"; Location = $SecondLocation }
        )) {
            $start = Join-Path $ProbeRoot "$($entry.Name)-start.txt"
            $ready = Join-Path $ProbeRoot "$($entry.Name)-ready.txt"
            $release = Join-Path $ProbeRoot "release.txt"
            $result = Join-Path $ProbeRoot "$($entry.Name)-result.txt"
            $trace = Join-Path $ProbeRoot "$($entry.Name)-trace.txt"
            $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
            $startInfo.FileName = Join-Path $PSHOME "pwsh.exe"
            $startInfo.UseShellExecute = $false
            $startInfo.RedirectStandardOutput = $true
            $startInfo.RedirectStandardError = $true
            $arguments = @(
                "-NoLogo", "-NoProfile", "-File", $childPath,
                "-ModulePath", $ModulePath,
                "-CacheRoot", $entry.Location.CacheRoot,
                "-EnvironmentRoot", $entry.Location.EnvironmentRoot,
                "-StampPath", $entry.Location.StampPath,
                "-LockPath", $entry.Location.LockPath,
                "-WorkspaceKey", $entry.Location.WorkspaceKey,
                "-WorkspaceRoot", $entry.Location.WorkspaceRoot,
                "-WorkspaceLockPath", $entry.Location.WorkspaceLockPath,
                "-StartPath", $start,
                "-ReadyPath", $ready,
                "-ReleasePath", $release,
                "-ResultPath", $result,
                "-TracePath", $trace,
                "-Name", $entry.Name,
                "-Scenario", $Scenario,
                "-ReleaseTimeoutMilliseconds", [string]$releaseTimeoutMilliseconds
            )
            foreach ($argument in $arguments) {
                $startInfo.ArgumentList.Add([string]$argument)
            }
            $probes.Add((Start-ContractProbe -Name $entry.Name -StartInfo $startInfo `
                -Arguments $arguments -Start $start -Ready $ready -Release $release -Result $result `
                -Trace $trace -Location $entry.Location -TeardownDeadline $teardownDeadline `
                -OwnedProbe $probes.ToArray())) | Out-Null
        }
        $readinessTimer = [System.Diagnostics.Stopwatch]::StartNew()
        [System.IO.File]::WriteAllText(
            $probes[0].Start,"start",[System.Text.UTF8Encoding]::new($false)
        )
        Wait-ContractProbesReady -Probe @($probes) -RequiredName @("one") `
            -Timer $readinessTimer -TimeoutMilliseconds $ReadinessTimeoutMilliseconds
        Assert-Contract (
            -not (Test-Path -LiteralPath $probes[1].Ready -PathType Leaf)
        ) "the startup-skew probe must remain parent-gated until the first probe is ready"
        [System.IO.File]::WriteAllText(
            $probes[1].Start,"start",[System.Text.UTF8Encoding]::new($false)
        )
        Wait-ContractProbesReady -Probe @($probes) -RequiredName @("one","two") `
            -Timer $readinessTimer -TimeoutMilliseconds $ReadinessTimeoutMilliseconds
        $readinessTimer.Stop()
        Update-ContractProbeReadinessSnapshot -Probe @($probes) `
            -ProcessHasExitedAction { param([object]$Process) $Process.HasExited }
        foreach ($probe in $probes) {
            Assert-Contract (-not $probe.Process.HasExited) `
                "workspace probe $($probe.Name) must remain inside its synchronized gate"
        }
        Assert-Contract (
            $probes[0].Location.EnvironmentRoot -ceq $probes[1].Location.EnvironmentRoot -and
            $probes[0].Location.WorkspaceRoot -cne $probes[1].Location.WorkspaceRoot
        ) "concurrent probes must share e/ and retain distinct w/<key> roots"
        Assert-Throws -Pattern "busy|ownership" -Action {
            Enter-PrivateEnvironmentLease -Location $FirstLocation -Access Exclusive `
                -TimeoutMilliseconds 0
        }
        foreach ($probe in $probes) {
            Assert-Throws -Pattern "busy|ownership" -Action {
                Enter-PrivateWorkspaceLease -Location $probe.Location -TimeoutMilliseconds 0
            }
        }
        Publish-ContractProbeRelease -Probe @($probes)
        Wait-ContractProbesExited -Probe @($probes) -TimeoutMilliseconds $ExitTimeoutMilliseconds
        foreach ($probe in $probes) {
            Assert-Contract (
                (Get-Content -Raw -LiteralPath $probe.Result).Trim() -ceq "passed"
            ) "workspace probe $($probe.Name) must publish a passed result after release"
            Set-ContractProbeState -Probe $probe -State ResultValidated `
                -Milestone "result.validated"
        }
        return [pscustomobject]@{
            AllReadyMilliseconds = $readinessTimer.ElapsedMilliseconds
            ReleaseWatchdogMilliseconds = $releaseTimeoutMilliseconds
            ExitTimeoutMilliseconds = $ExitTimeoutMilliseconds
            Probes = @($probes)
        }
    }
    catch {
        $primaryFailure = $_
        throw
    }
    finally {
        $cleanupFailure = $null
        try {
            Stop-ContractProbes -Probe @($probes) -Deadline $teardownDeadline
        }
        catch {
            $cleanupFailure = $_
        }
        if ($null -ne $cleanupFailure) {
            if ($null -ne $primaryFailure) {
                throw (
                    "contract failed: primary workspace probe failure<<`n" +
                    "$($primaryFailure.Exception.Message)`n>>primary workspace probe failure`n" +
                    "teardown failure<<`n$($cleanupFailure.Exception.Message)`n>>teardown failure"
                )
            }
            else {
                throw $cleanupFailure
            }
        }
    }
}

if ($null -eq ("EasyConLifecycleContractNativeMethods" -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class EasyConLifecycleContractNativeMethods
{
    private const uint GenericRead = 0x80000000;
    private const uint FileShareRead = 0x00000001;
    private const uint FileShareWrite = 0x00000002;
    private const uint OpenExisting = 3;
    private const uint FileFlagBackupSemantics = 0x02000000;
    private const uint FileFlagOpenReparsePoint = 0x00200000;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFileW(
        string fileName,
        uint desiredAccess,
        uint shareMode,
        IntPtr securityAttributes,
        uint creationDisposition,
        uint flagsAndAttributes,
        IntPtr templateFile
    );

    public static SafeFileHandle OpenDirectoryReparsePointWithoutDeleteSharing(string path)
    {
        SafeFileHandle handle = CreateFileW(
            path,
            GenericRead,
            FileShareRead | FileShareWrite,
            IntPtr.Zero,
            OpenExisting,
            FileFlagBackupSemantics | FileFlagOpenReparsePoint,
            IntPtr.Zero
        );
        if (handle.IsInvalid)
        {
            int error = Marshal.GetLastWin32Error();
            handle.Dispose();
            throw new Win32Exception(error, "failed to lock junction without delete sharing");
        }
        return handle;
    }
}
'@
}

function Open-ContractLockedJunction {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    return [EasyConLifecycleContractNativeMethods]::OpenDirectoryReparsePointWithoutDeleteSharing(
        $Path
    )
}

$modulePath = Join-Path $PSScriptRoot "windows_workspace.psm1"
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
    "easycon lifecycle contract {0}" -f [guid]::NewGuid().ToString("N")
)
$environmentRoot = Join-Path $temporaryRoot "e/contract"
$location = [pscustomobject]@{
    CacheRoot = $temporaryRoot
    EnvironmentRoot = $environmentRoot
    StampPath = Join-Path $environmentRoot "environment-stamp.json"
    LockPath = Join-Path $temporaryRoot "locks/contract.lock"
    IdentityKey = "contract"
    WorkspaceKey = "contract-workspace"
    WorkspaceRoot = Join-Path $temporaryRoot "w/contract-workspace"
    WorkspaceLockPath = Join-Path $temporaryRoot "locks/workspace-contract-workspace.lock"
    CargoTargetDirectory = Join-Path $temporaryRoot "w/contract-workspace/target"
}
$secondWorktreeLocation = [pscustomobject]@{
    CacheRoot = $temporaryRoot
    EnvironmentRoot = $environmentRoot
    StampPath = Join-Path $environmentRoot "environment-stamp.json"
    LockPath = Join-Path $temporaryRoot "locks/contract.lock"
    IdentityKey = "contract"
    WorkspaceKey = "contract-workspace-two"
    WorkspaceRoot = Join-Path $temporaryRoot "w/contract-workspace-two"
    WorkspaceLockPath = Join-Path $temporaryRoot "locks/workspace-contract-workspace-two.lock"
    CargoTargetDirectory = Join-Path $temporaryRoot "w/contract-workspace-two/target"
}

function New-ContractProbeLocation {
    param(
        [Parameter(Mandatory)][string]$CacheRoot,
        [Parameter(Mandatory)][string]$IdentityKey,
        [Parameter(Mandatory)][string]$WorkspaceKey
    )

    $environmentRoot = Join-Path $CacheRoot "e/$IdentityKey"
    return [pscustomobject]@{
        CacheRoot = $CacheRoot
        EnvironmentRoot = $environmentRoot
        StampPath = Join-Path $environmentRoot "environment-stamp.json"
        LockPath = Join-Path $CacheRoot "locks/$IdentityKey.lock"
        IdentityKey = $IdentityKey
        WorkspaceKey = $WorkspaceKey
        WorkspaceRoot = Join-Path $CacheRoot "w/$WorkspaceKey"
        WorkspaceLockPath = Join-Path $CacheRoot "locks/workspace-$WorkspaceKey.lock"
        CargoTargetDirectory = Join-Path $CacheRoot "w/$WorkspaceKey/target"
    }
}

Import-Module -Name $modulePath -Force
$workspaceModule = Get-Module windows_workspace

function Invoke-PrivateEnvironmentLifecycle {
    param(
        [Parameter(Mandatory)]
        [string]$Mode,

        [Parameter(Mandatory)]
        [object]$Location,

        [Parameter(Mandatory)]
        [scriptblock]$SetupAction,

        [Parameter(Mandatory)]
        [scriptblock]$VerifyAction,

        [Parameter(Mandatory)]
        [scriptblock]$WorkspaceAction,

        [int]$LeaseTimeoutMilliseconds = 0
    )

    & $script:workspaceModule {
        param($LifecycleMode, $OwnedLocation, $Setup, $Verify, $Workspace, $Timeout)
        Invoke-EasyConEnvironmentLifecycle -Mode $LifecycleMode -Location $OwnedLocation `
            -SetupAction $Setup -VerifyAction $Verify -WorkspaceAction $Workspace `
            -LeaseTimeoutMilliseconds $Timeout
    } $Mode $Location $SetupAction $VerifyAction $WorkspaceAction $LeaseTimeoutMilliseconds
}

function Enter-PrivateEnvironmentLease {
    param(
        [Parameter(Mandatory)]
        [object]$Location,

        [Parameter(Mandatory)]
        [string]$Access,

        [int]$TimeoutMilliseconds = 0
    )

    & $script:workspaceModule {
        param($OwnedLocation, $LeaseAccess, $Timeout)
        Enter-EasyConEnvironmentLease -Location $OwnedLocation -Access $LeaseAccess `
            -TimeoutMilliseconds $Timeout
    } $Location $Access $TimeoutMilliseconds
}

function Enter-PrivateWorkspaceLease {
    param(
        [Parameter(Mandatory)]
        [object]$Location,

        [int]$TimeoutMilliseconds = 0
    )

    & $script:workspaceModule {
        param($OwnedLocation, $Timeout)
        Enter-EasyConWorkspaceLease -Location $OwnedLocation `
            -TimeoutMilliseconds $Timeout -RetryMilliseconds 0
    } $Location $TimeoutMilliseconds
}

function Get-PrivateSharedContentAsset {
    param(
        [Parameter(Mandatory)]
        [hashtable]$Parameters
    )

    & $script:workspaceModule {
        param($Arguments)
        Get-EasyConSharedContentAsset @Arguments
    } $Parameters
}

function Enter-PrivateSharedCacheLease {
    param(
        [Parameter(Mandatory)]
        [string]$CacheRoot,

        [Parameter(Mandatory)]
        [string]$Access,

        [int]$TimeoutMilliseconds = 0
    )

    & $script:workspaceModule {
        param($Root, $LeaseAccess, $Timeout)
        Enter-EasyConSharedCacheLease -CacheRoot $Root -Access $LeaseAccess `
            -TimeoutMilliseconds $Timeout -RetryMilliseconds 0
    } $CacheRoot $Access $TimeoutMilliseconds
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

function Invoke-EnvironmentRestorationContract {
    param(
        [Parameter(Mandatory)]
        [string]$Mode,

        [Parameter(Mandatory)]
        [scriptblock]$SetupAction,

        [Parameter(Mandatory)]
        [scriptblock]$VerifyAction,

        [Parameter(Mandatory)]
        [scriptblock]$WorkspaceAction,

        [string]$FailurePattern
    )

    $outer = Get-ContractProcessEnvironmentSnapshot
    $expected = $null
    $actual = $null
    try {
        foreach ($name in @(
            "EasyCon_Contract_Present",
            "EasyCon_Contract_Empty",
            "EasyCon_Contract_Cased",
            "EASYCON_CONTRACT_ABSENT"
        )) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        }
        Set-Item -LiteralPath "Env:EasyCon_Contract_Present" -Value "original"
        Set-Item -LiteralPath "Env:EasyCon_Contract_Empty" -Value ""
        Set-Item -LiteralPath "Env:EasyCon_Contract_Cased" -Value "mixed-case-name"
        Set-Item -LiteralPath "Env:RUSTC" -Value "poison-rustc"
        $expected = Get-ContractProcessEnvironmentSnapshot

        if ([string]::IsNullOrWhiteSpace($FailurePattern)) {
            Invoke-PrivateEnvironmentLifecycle -Mode $Mode -Location $location `
                -SetupAction $SetupAction -VerifyAction $VerifyAction `
                -WorkspaceAction $WorkspaceAction | Out-Null
        }
        else {
            Assert-Throws -Pattern $FailurePattern -Action {
                Invoke-PrivateEnvironmentLifecycle -Mode $Mode -Location $location `
                    -SetupAction $SetupAction -VerifyAction $VerifyAction `
                    -WorkspaceAction $WorkspaceAction
            }
        }
        $actual = Get-ContractProcessEnvironmentSnapshot
    }
    finally {
        Restore-ContractProcessEnvironment -Snapshot $outer
    }
    Assert-EnvironmentSnapshot -Actual $actual -Expected $expected `
        -Description "$Mode lifecycle"
}

$events = [System.Collections.Generic.List[string]]::new()
$state = [pscustomobject]@{
    FailSetup = $false
    RejectStale = $false
}
$sharedPayload = [System.Text.Encoding]::UTF8.GetBytes("lifecycle shared asset")
$sharedHash = [Convert]::ToHexString(
    [System.Security.Cryptography.SHA256]::HashData($sharedPayload)
).ToLowerInvariant()
$sharedState = [pscustomobject]@{ Downloads = 0 }
$sharedDownload = {
    param($Url, $Destination)
    $null = $Url
    $sharedState.Downloads++
    [System.IO.File]::WriteAllBytes($Destination, $sharedPayload)
}.GetNewClosure()
$sharedAssetParameters = @{
    SharedCacheRoot = (Join-Path $temporaryRoot "caches")
    Url = "https://example.invalid/lifecycle.bin"
    Algorithm = "SHA256"
    Hash = $sharedHash
    Bytes = [long]$sharedPayload.Length
    Description = "lifecycle shared asset"
    DownloadAction = $sharedDownload
}
$setupAction = {
    $events.Add("setup") | Out-Null
    Get-PrivateSharedContentAsset -Parameters $sharedAssetParameters | Out-Null
    if ($state.RejectStale) {
        Assert-Contract (-not (Test-Path -LiteralPath (Join-Path $environmentRoot "stale.txt"))) `
            "damaged rebuild must remove the old environment tree before Setup"
        Assert-Contract (-not (Test-Path -LiteralPath (Join-Path $environmentRoot "partial.txt"))) `
            "interrupted rebuild must remove partial state before retry"
    }
    New-Item -ItemType Directory -Force -Path $environmentRoot | Out-Null
    if ($state.FailSetup) {
        Set-Content -LiteralPath (Join-Path $environmentRoot "partial.txt") `
            -Value "partial" -Encoding utf8NoBOM
        throw "synthetic interrupted Setup"
    }
    Set-Content -LiteralPath $location.StampPath -Value "ready" -Encoding utf8NoBOM
}.GetNewClosure()
$verifyAction = {
    $events.Add("verify") | Out-Null
    if (
        -not (Test-Path -LiteralPath $location.StampPath -PathType Leaf) -or
        (Get-Content -Raw -LiteralPath $location.StampPath).Trim() -cne "ready"
    ) {
        throw "synthetic environment mismatch"
    }
    return "verified"
}.GetNewClosure()
$workspaceAction = {
    param($Summary)
    Assert-Contract ($Summary -ceq "verified") "Workspace must receive the verified summary"
    $events.Add("workspace") | Out-Null
}.GetNewClosure()

$contractFailure = $null
try {
    Invoke-ContractCase -Name "marker-wait-observes-forced-check-to-wait-interleaving" -Action {
        $markerRoot = Join-Path $temporaryRoot "forced marker wait interleaving"
        $markerPath = Join-Path $markerRoot "created-before-registration.txt"
        New-Item -ItemType Directory -Force -Path $markerRoot | Out-Null
        Wait-ContractFileCreated -Path $markerPath -TimeoutMilliseconds 250 `
            -BeforeWaitRegistration {
                [System.IO.File]::WriteAllText(
                    $markerPath,
                    "created",
                    [System.Text.UTF8Encoding]::new($false)
                )
            }.GetNewClosure()
        Assert-Contract (Test-Path -LiteralPath $markerPath -PathType Leaf) `
            "forced check-to-wait marker must remain observable"
    }
    Invoke-ContractCase -Name "probe-readiness-final-observation-prefers-early-exit" -Action {
        Assert-ContractProbeReadinessDeadlineRegression -ProbeRoot (
            Join-Path $temporaryRoot "readiness deadline regression with spaces"
        )
    }
    Invoke-ContractCase -Name "probe-teardown-is-exhaustive-bounded-and-diagnostic" -Action {
        Assert-ContractProbeTeardownRegression -ProbeRoot (
            Join-Path $temporaryRoot "teardown regression with spaces"
        )
    }
    Invoke-ContractCase -Name "workspace-probe-early-exit-is-diagnostic" -Action {
        $probeRoot = Join-Path $temporaryRoot "early exit diagnostic with spaces"
        $probeLocation = New-ContractProbeLocation -CacheRoot $probeRoot `
            -IdentityKey "early-exit" -WorkspaceKey "early-exit-one"
        $probeSecondLocation = New-ContractProbeLocation -CacheRoot $probeRoot `
            -IdentityKey "early-exit" -WorkspaceKey "early-exit-two"
        New-Item -ItemType Directory -Force -Path $probeLocation.EnvironmentRoot | Out-Null
        Set-Content -LiteralPath $probeLocation.StampPath -Value "ready" -Encoding utf8NoBOM
        $earlyExitFailure = $null
        try {
            Invoke-SharedWorkspaceProbeSynchronizationContract `
                -ProbeRoot $probeRoot -FirstLocation $probeLocation `
                -SecondLocation $probeSecondLocation `
                -ModulePath $modulePath -Scenario EarlyExit | Out-Null
        }
        catch {
            $earlyExitFailure = $_
        }
        $earlyExitMessage = if ($null -eq $earlyExitFailure) {
            "<none>"
        }
        else {
            $earlyExitFailure.Exception.Message
        }
        Assert-Contract (
            $null -ne $earlyExitFailure -and
            $earlyExitMessage -match 'child exited before ready/result' -and
            $earlyExitMessage -match 'probe=two' -and
            $earlyExitMessage -match 'exitCode=23' -and
            $earlyExitMessage -match 'SYNTHETIC_SECOND_PROBE_EARLY_EXIT' -and
            $earlyExitMessage -match 'ArgumentList=' -and
            $earlyExitMessage -match 'lastMilestone=exit.before-ready'
        ) "early exit must fail before the readiness deadline with complete diagnostics; observed=$earlyExitMessage"
    }
    Invoke-ContractCase -Name "workspace-probe-startup-skew-and-release" -Action {
        $probeRoot = Join-Path $temporaryRoot "startup skew with spaces"
        $probeLocation = New-ContractProbeLocation -CacheRoot $probeRoot `
            -IdentityKey "startup-skew" -WorkspaceKey "startup-skew-one"
        $probeSecondLocation = New-ContractProbeLocation -CacheRoot $probeRoot `
            -IdentityKey "startup-skew" -WorkspaceKey "startup-skew-two"
        New-Item -ItemType Directory -Force -Path $probeLocation.EnvironmentRoot | Out-Null
        Set-Content -LiteralPath $probeLocation.StampPath -Value "ready" -Encoding utf8NoBOM
        $startupSkew = Invoke-SharedWorkspaceProbeSynchronizationContract `
            -ProbeRoot $probeRoot -FirstLocation $probeLocation `
            -SecondLocation $probeSecondLocation `
            -ModulePath $modulePath
        Assert-Contract (
            $startupSkew.AllReadyMilliseconds -lt 10000 -and
            @($startupSkew.Probes | Where-Object { $_.State -ceq "ResultValidated" }).Count -eq 2
        ) "startup-skew probes must share one readiness deadline and validate both results"
    }

    Invoke-ContractCase -Name "lifecycle-core-contracts" -Action {
    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "setup", "verify") `
        -Description "first Setup must verify, prepare, and verify the published result"
    Assert-Contract ($sharedState.Downloads -eq 1) `
        "the first lifecycle identity must fetch a missing fixed asset once"

    $events.Clear()
    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify") `
        -Description "already-ready Setup must not provision again"

    $events.Clear()
    $verifyWithWorkspaceOwnership = {
        Assert-Throws -Pattern "busy|ownership" -Action {
            Enter-PrivateWorkspaceLease -Location $location -TimeoutMilliseconds 0
        }
        $events.Add("verify") | Out-Null
        if ((Get-Content -Raw -LiteralPath $location.StampPath).Trim() -cne "ready") {
            throw "synthetic environment mismatch"
        }
        return "verified"
    }.GetNewClosure()
    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyWithWorkspaceOwnership `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify") `
        -Description "already-ready Setup VerifyCore must hold the workspace writable lease"

    $readyMarker = Join-Path $environmentRoot "ready-preserved.txt"
    Set-Content -LiteralPath $readyMarker -Value "preserve" -Encoding utf8NoBOM
    $readyLeaseState = [pscustomobject]@{ SetupCalls = 0; VerifyCalls = 0 }
    $writerLease = Enter-PrivateSharedCacheLease -CacheRoot $location.CacheRoot `
        -Access Exclusive -TimeoutMilliseconds 0
    try {
        $expectedBusy = $null
        try {
            Enter-PrivateSharedCacheLease -CacheRoot $location.CacheRoot -Access Shared `
                -TimeoutMilliseconds 0 | Out-Null
        }
        catch {
            $expectedBusy = $_
        }
        Assert-Contract ($null -ne $expectedBusy) `
            "the regression fixture must hold the shared-cache writer lease"

        $actualBusy = $null
        try {
            Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
                -SetupAction {
                    $readyLeaseState.SetupCalls++
                }.GetNewClosure() -VerifyAction {
                    $readyLeaseState.VerifyCalls++
                    return "verified"
                }.GetNewClosure() -WorkspaceAction { param($Summary) } `
                -LeaseTimeoutMilliseconds 0 | Out-Null
        }
        catch {
            $actualBusy = $_
        }
        Assert-Contract ($null -ne $actualBusy) `
            "ready Setup must report shared-cache lease contention"
        Assert-Contract ($actualBusy.Exception.Message -ceq $expectedBusy.Exception.Message) `
            "ready Setup must preserve the original shared-cache busy diagnostic"
        $stampExists = Test-Path -LiteralPath $location.StampPath -PathType Leaf
        $markerExists = Test-Path -LiteralPath $readyMarker -PathType Leaf
        $readyStatePreserved = (
            $stampExists -and
            (Get-Content -Raw -LiteralPath $location.StampPath).Trim() -ceq "ready" -and
            $markerExists -and
            (Get-Content -Raw -LiteralPath $readyMarker).Trim() -ceq "preserve"
        )
        Assert-Contract (
            $readyLeaseState.VerifyCalls -eq 0 -and
            $readyLeaseState.SetupCalls -eq 0 -and
            $readyStatePreserved
        ) (
            "shared-cache contention must preserve ready state without verification or " +
            "provision; VerifyCalls=$($readyLeaseState.VerifyCalls) " +
            "SetupCalls=$($readyLeaseState.SetupCalls) StampExists=$stampExists " +
            "MarkerExists=$markerExists"
        )
    }
    finally {
        $writerLease.Dispose()
    }

    $waitProbeRoot = Join-Path $temporaryRoot "shared cache wait probe"
    $waitEnvironment = Join-Path $waitProbeRoot "environment"
    $waitStamp = Join-Path $waitEnvironment "environment-stamp.json"
    $waitMarker = Join-Path $waitEnvironment "ready-preserved.txt"
    $busyObserved = Join-Path $waitProbeRoot "busy-observed.txt"
    $waitingObserved = Join-Path $waitProbeRoot "waiting-observed.txt"
    $waitResult = Join-Path $waitProbeRoot "result.txt"
    New-Item -ItemType Directory -Force -Path $waitEnvironment | Out-Null
    Set-Content -LiteralPath $waitStamp -Value "ready" -Encoding utf8NoBOM
    Set-Content -LiteralPath $waitMarker -Value "preserve" -Encoding utf8NoBOM
    $waitChild = Join-Path $waitProbeRoot "wait-child.ps1"
    Set-Content -LiteralPath $waitChild -Encoding utf8NoBOM -Value @'
param(
    [Parameter(Mandatory)][string]$ModulePath,
    [Parameter(Mandatory)][string]$CacheRoot,
    [Parameter(Mandatory)][string]$EnvironmentRoot,
    [Parameter(Mandatory)][string]$StampPath,
    [Parameter(Mandatory)][string]$ReadyMarker,
    [Parameter(Mandatory)][string]$BusyObserved,
    [Parameter(Mandatory)][string]$WaitingObserved,
    [Parameter(Mandatory)][string]$ResultPath
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
Import-Module -Name $ModulePath -Force
$module = Get-Module windows_workspace
$location = [pscustomobject]@{
    CacheRoot = $CacheRoot
    EnvironmentRoot = $EnvironmentRoot
    StampPath = $StampPath
    LockPath = Join-Path $CacheRoot "locks/wait-probe.lock"
    IdentityKey = "wait-probe"
    WorkspaceKey = "wait-probe-workspace"
    WorkspaceRoot = Join-Path $CacheRoot "w/wait-probe-workspace"
    WorkspaceLockPath = Join-Path $CacheRoot "locks/workspace-wait-probe-workspace.lock"
    CargoTargetDirectory = Join-Path $CacheRoot "w/wait-probe-workspace/target"
}
$state = [pscustomobject]@{ SetupCalls = 0; VerifyCalls = 0 }
$setup = { $state.SetupCalls++ }.GetNewClosure()
$verify = {
    $state.VerifyCalls++
    if (
        -not (Test-Path -LiteralPath $StampPath -PathType Leaf) -or
        (Get-Content -Raw -LiteralPath $StampPath).Trim() -cne "ready"
    ) {
        throw "wait probe environment is not ready"
    }
    return "verified"
}.GetNewClosure()
$invokeLifecycle = {
    param([int]$Timeout)
    & $module {
        param($OwnedLocation, $SetupAction, $VerifyAction, $LeaseTimeout)
        Invoke-EasyConEnvironmentLifecycle -Mode Setup -Location $OwnedLocation `
            -SetupAction $SetupAction -VerifyAction $VerifyAction `
            -WorkspaceAction { param($Summary) } `
            -LeaseTimeoutMilliseconds $LeaseTimeout
    } $location $setup $verify $Timeout
}.GetNewClosure()
$expectedBusy = $null
try {
    & $module {
        param($Root)
        Enter-EasyConSharedCacheLease -CacheRoot $Root -Access Shared `
            -TimeoutMilliseconds 0 -RetryMilliseconds 0
    } $CacheRoot | ForEach-Object { $_.Dispose() }
}
catch {
    $expectedBusy = $_
}
if ($null -eq $expectedBusy) {
    throw "wait probe did not observe the held writer lease"
}
$actualBusy = $null
try {
    & $invokeLifecycle 0 | Out-Null
}
catch {
    $actualBusy = $_
}
if (
    $null -eq $actualBusy -or
    $actualBusy.Exception.Message -cne $expectedBusy.Exception.Message -or
    $state.SetupCalls -ne 0 -or
    $state.VerifyCalls -ne 0 -or
    -not (Test-Path -LiteralPath $ReadyMarker -PathType Leaf)
) {
    throw "wait probe timeout mutated ready state or changed the busy diagnostic"
}
[System.IO.File]::WriteAllText($BusyObserved, "busy", [System.Text.UTF8Encoding]::new($false))
[System.IO.File]::WriteAllText($WaitingObserved, "waiting", [System.Text.UTF8Encoding]::new($false))
& $invokeLifecycle 10000 | Out-Null
if (
    $state.SetupCalls -ne 0 -or
    $state.VerifyCalls -ne 1 -or
    -not (Test-Path -LiteralPath $ReadyMarker -PathType Leaf)
) {
    throw "wait probe did not resume as already-ready after writer release"
}
[System.IO.File]::WriteAllText($ResultPath, "passed", [System.Text.UTF8Encoding]::new($false))
'@
    $waitWriter = Enter-PrivateSharedCacheLease -CacheRoot $location.CacheRoot `
        -Access Exclusive -TimeoutMilliseconds 0
    $waitProcess = $null
    try {
        $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = Join-Path $PSHOME "pwsh.exe"
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        foreach ($argument in @(
            "-NoLogo", "-NoProfile", "-File", $waitChild,
            "-ModulePath", $modulePath,
            "-CacheRoot", $location.CacheRoot,
            "-EnvironmentRoot", $waitEnvironment,
            "-StampPath", $waitStamp,
            "-ReadyMarker", $waitMarker,
            "-BusyObserved", $busyObserved,
            "-WaitingObserved", $waitingObserved,
            "-ResultPath", $waitResult
        )) {
            $startInfo.ArgumentList.Add([string]$argument)
        }
        $waitProcess = [System.Diagnostics.Process]::Start($startInfo)
        Wait-ContractFileCreated -Path $busyObserved
        Wait-ContractFileCreated -Path $waitingObserved
        Assert-Contract (-not $waitProcess.WaitForExit(250)) `
            "the synchronized contender must still be waiting while the writer lease is held"
        $waitWriter.Dispose()
        $waitWriter = $null
        Assert-Contract ($waitProcess.WaitForExit(10000)) `
            "the synchronized contender must finish after the writer lease is released"
        $waitOutput = $waitProcess.StandardOutput.ReadToEnd()
        $waitError = $waitProcess.StandardError.ReadToEnd()
        Assert-Contract (
            $waitProcess.ExitCode -eq 0 -and
            (Test-Path -LiteralPath $waitResult -PathType Leaf) -and
            (Get-Content -Raw -LiteralPath $waitResult).Trim() -ceq "passed"
        ) "shared-cache waiter failed after release; output=$waitOutput error=$waitError"
    }
    finally {
        if ($null -ne $waitWriter) {
            $waitWriter.Dispose()
        }
        if ($null -ne $waitProcess) {
            if (-not $waitProcess.HasExited) {
                $waitProcess.Kill($true)
                $waitProcess.WaitForExit()
            }
            $waitProcess.Dispose()
        }
    }

    Set-Content -LiteralPath $location.StampPath -Value "damaged" -Encoding utf8NoBOM
    Set-Content -LiteralPath (Join-Path $environmentRoot "stale.txt") `
        -Value "stale" -Encoding utf8NoBOM
    $state.RejectStale = $true
    $events.Clear()
    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "setup", "verify") `
        -Description "damaged prepared state must be rebuilt through the public lifecycle"

    Set-Content -LiteralPath $location.StampPath -Value "damaged" -Encoding utf8NoBOM
    $state.FailSetup = $true
    Assert-Throws -Pattern "synthetic interrupted Setup" -Action {
        Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
            -SetupAction $setupAction -VerifyAction $verifyAction `
            -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0
    }
    $state.FailSetup = $false
    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Contract (-not (Test-Path -LiteralPath (Join-Path $environmentRoot "partial.txt"))) `
        "a later Setup must recover from interrupted partial state"
    Assert-Contract ($sharedState.Downloads -eq 1) `
        "damage and interrupted rebuilds must reuse the verified shared asset"

    $junctionRecoveryEnvironment = Join-Path $temporaryRoot "e/junction-recovery"
    $junctionRecoveryLocation = [pscustomobject]@{
        CacheRoot = $temporaryRoot
        EnvironmentRoot = $junctionRecoveryEnvironment
        StampPath = Join-Path $junctionRecoveryEnvironment "environment-stamp.json"
        LockPath = Join-Path $temporaryRoot "locks/junction-recovery.lock"
        IdentityKey = "junction-recovery"
        WorkspaceKey = "junction-recovery-workspace"
        WorkspaceRoot = Join-Path $temporaryRoot "w/junction-recovery-workspace"
        WorkspaceLockPath = Join-Path $temporaryRoot "locks/workspace-junction-recovery-workspace.lock"
        CargoTargetDirectory = Join-Path $temporaryRoot "w/junction-recovery-workspace/target"
    }
    $vcpkgTransientRoot = Join-Path $junctionRecoveryEnvironment "setup/vcpkg"
    $knownTransientTrees = @(
        Join-Path $vcpkgTransientRoot "buildtrees"
        Join-Path $vcpkgTransientRoot "packages"
    )
    $installedTree = Join-Path $vcpkgTransientRoot "installed"
    $externalTargets = @(
        Join-Path $temporaryRoot "junction-recovery-external-buildtrees"
        Join-Path $temporaryRoot "junction-recovery-external-packages"
    )
    $externalMarkers = @(
        Join-Path $externalTargets[0] "keep-buildtrees.txt"
        Join-Path $externalTargets[1] "keep-packages.txt"
    )
    foreach ($index in 0..1) {
        New-Item -ItemType Directory -Force -Path $externalTargets[$index] | Out-Null
        Set-Content -LiteralPath $externalMarkers[$index] -Value "external" `
            -Encoding utf8NoBOM
    }

    $createLockedTransientResidue = {
        New-Item -ItemType Directory -Force -Path $knownTransientTrees | Out-Null
        New-Item -ItemType Directory -Force -Path $installedTree | Out-Null
        $installedMarker = Join-Path $installedTree "keep-installed.txt"
        Set-Content -LiteralPath $installedMarker -Value "installed" -Encoding utf8NoBOM
        $handles = [System.Collections.Generic.List[Microsoft.Win32.SafeHandles.SafeFileHandle]]::new()
        $junctions = [System.Collections.Generic.List[string]]::new()
        foreach ($index in 0..1) {
            $junction = Join-Path $knownTransientTrees[$index] "hosted-port-source"
            New-Item -ItemType Junction -Path $junction -Target $externalTargets[$index] | Out-Null
            $junctionItem = Get-Item -Force -LiteralPath $junction
            Assert-Contract (
                ($junctionItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
            ) "the lifecycle recovery fixture must contain a real NTFS junction"
            $junctions.Add($junction) | Out-Null
            $handle = Open-ContractLockedJunction -Path $junction
            Assert-Contract (-not $handle.IsInvalid -and -not $handle.IsClosed) `
                "the lifecycle fixture must retain an open junction handle"
            $handles.Add($handle) | Out-Null
        }
        return [pscustomobject]@{
            Handles = $handles.ToArray()
            Junctions = $junctions.ToArray()
            InstalledMarker = $installedMarker
        }
    }.GetNewClosure()

    $lockedResidue = & $createLockedTransientResidue
    $initialCleanupFailures = [System.Collections.Generic.List[object]]::new()
    try {
        foreach ($tree in $knownTransientTrees) {
            try {
                Remove-PrivateTransientBuildTree -Path $tree `
                    -TrustedRoot $junctionRecoveryEnvironment
            }
            catch {
                $initialCleanupFailures.Add($_)
            }
        }
        Assert-Contract ($initialCleanupFailures.Count -eq 2) (
            "locked junctions must leave both known vcpkg transient roots for recovery; " +
            "failures=$($initialCleanupFailures.Count) roots=" +
            (($knownTransientTrees | ForEach-Object {
                "$_=$(Test-Path -LiteralPath $_)"
            }) -join ",")
        )
        foreach ($junction in $lockedResidue.Junctions) {
            Assert-Contract (Test-Path -LiteralPath $junction) `
                "failed transient cleanup must retain the locked junction itself"
        }
        Assert-Contract (Test-Path -LiteralPath $lockedResidue.InstalledMarker -PathType Leaf) `
            "failed transient cleanup must not delete the installed tree"
        foreach ($marker in $externalMarkers) {
            Assert-Contract (Test-Path -LiteralPath $marker -PathType Leaf) `
                "failed transient cleanup must not follow a junction target"
        }
    }
    finally {
        foreach ($handle in $lockedResidue.Handles) {
            $handle.Dispose()
        }
    }

    $junctionRecoveryState = [pscustomobject]@{ SetupCalls = 0; VerifyCalls = 0 }
    $junctionRecoverySetup = {
        $junctionRecoveryState.SetupCalls++
        foreach ($tree in $knownTransientTrees) {
            Assert-Contract (-not (Test-Path -LiteralPath $tree)) `
                "Setup must remove known vcpkg transient residue before provisioning"
        }
        foreach ($marker in $externalMarkers) {
            Assert-Contract (Test-Path -LiteralPath $marker -PathType Leaf) `
                "Setup recovery must preserve external junction targets"
        }
        Set-Content -LiteralPath $junctionRecoveryLocation.StampPath -Value "ready" `
            -Encoding utf8NoBOM
    }.GetNewClosure()
    $junctionRecoveryVerify = {
        $junctionRecoveryState.VerifyCalls++
        if (
            -not (Test-Path -LiteralPath $junctionRecoveryLocation.StampPath -PathType Leaf) -or
            (Get-Content -Raw -LiteralPath $junctionRecoveryLocation.StampPath).Trim() -cne "ready"
        ) {
            throw "synthetic locked junction environment mismatch"
        }
        return "verified"
    }.GetNewClosure()

    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $junctionRecoveryLocation `
        -SetupAction $junctionRecoverySetup -VerifyAction $junctionRecoveryVerify `
        -WorkspaceAction { param($Summary) } -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Contract (
        $junctionRecoveryState.SetupCalls -eq 1 -and
        $junctionRecoveryState.VerifyCalls -eq 2
    ) "a later public Setup lifecycle must recover the released junction residue exactly once"

    $unknownReparse = Join-Path $junctionRecoveryEnvironment "unknown/reparse-output"
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $unknownReparse) | Out-Null
    New-Item -ItemType Junction -Path $unknownReparse -Target $externalTargets[0] | Out-Null
    Set-Content -LiteralPath $junctionRecoveryLocation.StampPath -Value "damaged" `
        -Encoding utf8NoBOM
    Assert-Throws -Pattern "physical tree contains a reparse point" -Action {
        Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $junctionRecoveryLocation `
            -SetupAction $junctionRecoverySetup -VerifyAction $junctionRecoveryVerify `
            -WorkspaceAction { param($Summary) } -LeaseTimeoutMilliseconds 0
    }
    Assert-Contract ($junctionRecoveryState.SetupCalls -eq 1) `
        "an unknown prepared-tree reparse point must fail closed before SetupAction"
    Assert-Contract (Test-Path -LiteralPath $externalMarkers[0] -PathType Leaf) `
        "unknown reparse rejection must preserve its external target"
    Remove-Item -Force -LiteralPath $unknownReparse
    Remove-Item -Force -LiteralPath (Split-Path -Parent $unknownReparse)

    $lockedResidue = & $createLockedTransientResidue
    Set-Content -LiteralPath $junctionRecoveryLocation.StampPath -Value "damaged" `
        -Encoding utf8NoBOM
    $lockedSetupFailure = $null
    try {
        try {
            Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $junctionRecoveryLocation `
                -SetupAction $junctionRecoverySetup -VerifyAction $junctionRecoveryVerify `
                -WorkspaceAction { param($Summary) } -LeaseTimeoutMilliseconds 0 | Out-Null
        }
        catch {
            $lockedSetupFailure = $_
        }
        Assert-Contract ($null -ne $lockedSetupFailure) `
            "Setup recovery must fail while known transient junctions remain locked"
        Assert-Contract (
            $lockedSetupFailure.Exception.Message -match
                "synthetic locked junction environment mismatch"
        ) "locked recovery cleanup must retain the verification failure as the primary error"
        Assert-Contract (
            $lockedSetupFailure.Exception.Data.Contains(
                "EasyConVcpkgRecoveryCleanupFailure0"
            ) -and
            $lockedSetupFailure.Exception.Data.Contains(
                "EasyConVcpkgRecoveryCleanupFailure1"
            ) -and
            $lockedSetupFailure.Exception.Data.Contains(
                "EasyConResidualVcpkgTransientTree0"
            ) -and
            $lockedSetupFailure.Exception.Data.Contains(
                "EasyConResidualVcpkgTransientTree1"
            )
        ) "locked recovery must attach cleanup and residual diagnostics for both known roots"
        Assert-Contract ($junctionRecoveryState.SetupCalls -eq 1) `
            "locked known transient cleanup must fail before SetupAction"
        Assert-Contract (Test-Path -LiteralPath $lockedResidue.InstalledMarker -PathType Leaf) `
            "known transient recovery cleanup must not delete the installed tree"
        foreach ($marker in $externalMarkers) {
            Assert-Contract (Test-Path -LiteralPath $marker -PathType Leaf) `
                "locked recovery cleanup must not follow external junction targets"
        }
    }
    finally {
        foreach ($handle in $lockedResidue.Handles) {
            $handle.Dispose()
        }
    }

    Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $junctionRecoveryLocation `
        -SetupAction $junctionRecoverySetup -VerifyAction $junctionRecoveryVerify `
        -WorkspaceAction { param($Summary) } -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Contract ($junctionRecoveryState.SetupCalls -eq 2) `
        "Setup must recover deterministically after locked junction handles are released"
    foreach ($marker in $externalMarkers) {
        Assert-Contract (Test-Path -LiteralPath $marker -PathType Leaf) `
            "completed Setup recovery must preserve each external junction target"
    }

    foreach ($identityName in @("fingerprint-change", "worktree-change")) {
        $alternateRoot = Join-Path $temporaryRoot "e/$identityName"
        $alternateLocation = [pscustomobject]@{
            CacheRoot = $temporaryRoot
            EnvironmentRoot = $alternateRoot
            StampPath = Join-Path $alternateRoot "environment-stamp.json"
            LockPath = Join-Path $temporaryRoot "locks/$identityName.lock"
            IdentityKey = $identityName
            WorkspaceKey = "$identityName-workspace"
            WorkspaceRoot = Join-Path $temporaryRoot "w/$identityName-workspace"
            WorkspaceLockPath = Join-Path $temporaryRoot "locks/workspace-$identityName-workspace.lock"
            CargoTargetDirectory = Join-Path $temporaryRoot "w/$identityName-workspace/target"
        }
        $alternateSetup = {
            Get-PrivateSharedContentAsset -Parameters $sharedAssetParameters | Out-Null
            New-Item -ItemType Directory -Force -Path $alternateRoot | Out-Null
            Set-Content -LiteralPath $alternateLocation.StampPath -Value "ready" `
                -Encoding utf8NoBOM
        }.GetNewClosure()
        $alternateVerify = {
            if (-not (Test-Path -LiteralPath $alternateLocation.StampPath -PathType Leaf)) {
                throw "alternate identity is not ready"
            }
            return "verified"
        }.GetNewClosure()
        Invoke-PrivateEnvironmentLifecycle -Mode Setup -Location $alternateLocation `
            -SetupAction $alternateSetup -VerifyAction $alternateVerify `
            -WorkspaceAction { param($Summary) } -LeaseTimeoutMilliseconds 0 | Out-Null
    }
    Assert-Contract ($sharedState.Downloads -eq 1) `
        "fingerprint and worktree identity changes must reuse the verified shared asset"

    $events.Clear()
    Invoke-PrivateEnvironmentLifecycle -Mode Verify -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify") `
        -Description "Verify must never provision or run workspace gates"

    $firstWorkspaceLease = Enter-PrivateWorkspaceLease -Location $location -TimeoutMilliseconds 0
    $secondWorkspaceLease = $null
    try {
        $secondWorkspaceLease = Enter-PrivateWorkspaceLease -Location $secondWorktreeLocation `
            -TimeoutMilliseconds 0
        Assert-Throws -Pattern "busy|ownership" -Action {
            Enter-PrivateWorkspaceLease -Location $location -TimeoutMilliseconds 0
        }
    }
    finally {
        if ($null -ne $secondWorkspaceLease) {
            $secondWorkspaceLease.Dispose()
        }
        $firstWorkspaceLease.Dispose()
    }

    $workspaceProbeSummary = Invoke-SharedWorkspaceProbeSynchronizationContract `
        -ProbeRoot (Join-Path $temporaryRoot "shared environment workspace probes") `
        -FirstLocation $location -SecondLocation $secondWorktreeLocation `
        -ModulePath $modulePath
    Assert-Contract (
        $workspaceProbeSummary.Probes.Count -eq 2 -and
        @($workspaceProbeSummary.Probes | Where-Object {
            $_.State -ceq "ResultValidated"
        }).Count -eq 2 -and
        $workspaceProbeSummary.ReleaseWatchdogMilliseconds -eq 15000 -and
        $workspaceProbeSummary.ExitTimeoutMilliseconds -eq 10000
    ) "shared workspace probes must validate both results under derived watchdogs"

    $events.Clear()
    $workspaceWithOwnership = {
        param($Summary)
        Assert-Contract ($Summary -ceq "verified") "Workspace must receive verified state"
        Assert-Throws -Pattern "busy|ownership" -Action {
            Enter-PrivateEnvironmentLease -Location $location -Access Exclusive `
                -TimeoutMilliseconds 0
        }
        Assert-Throws -Pattern "busy|ownership" -Action {
            Enter-PrivateSharedCacheLease -CacheRoot $location.CacheRoot -Access Exclusive `
                -TimeoutMilliseconds 0
        }
        $events.Add("workspace") | Out-Null
    }.GetNewClosure()
    Invoke-PrivateEnvironmentLifecycle -Mode Workspace -Location $location `
        -SetupAction $setupAction -VerifyAction $verifyAction `
        -WorkspaceAction $workspaceWithOwnership -LeaseTimeoutMilliseconds 0 | Out-Null
    Assert-Sequence -Actual $events.ToArray() -Expected @("verify", "workspace") `
        -Description "Workspace must hold shared ownership from Verify through every gate"

    $events.Clear()
    Assert-Throws -Pattern "synthetic environment mismatch" -Action {
        Invoke-PrivateEnvironmentLifecycle -Mode Workspace -Location $location `
            -SetupAction $setupAction -VerifyAction { throw "synthetic environment mismatch" } `
            -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0
    }
    Assert-Sequence -Actual $events.ToArray() -Expected @() `
        -Description "Workspace gates must not start after verification fails"

    foreach ($mode in @("Setup", "Verify", "Workspace")) {
        $contenderLocation = if ($mode -ceq "Setup") {
            $location
        }
        else {
            $secondWorktreeLocation
        }
        $exclusive = Enter-PrivateEnvironmentLease -Location $location -Access Exclusive `
            -TimeoutMilliseconds 0
        try {
            $events.Clear()
            Assert-Throws -Pattern "busy|ownership" -Action {
                Invoke-PrivateEnvironmentLifecycle -Mode $mode -Location $contenderLocation `
                    -SetupAction $setupAction -VerifyAction $verifyAction `
                    -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0
            }
            Assert-Sequence -Actual $events.ToArray() -Expected @() `
                -Description "an active Setup must exclude a competing $mode worktree before state access"
        }
        finally {
            $exclusive.Dispose()
        }
    }

    Assert-Throws -Pattern "synthetic verify failure" -Action {
        Invoke-PrivateEnvironmentLifecycle -Mode Verify -Location $location `
            -SetupAction $setupAction -VerifyAction { throw "synthetic verify failure" } `
            -WorkspaceAction $workspaceAction -LeaseTimeoutMilliseconds 0
    }
    $released = Enter-PrivateEnvironmentLease -Location $location -Access Exclusive `
        -TimeoutMilliseconds 0
    $released.Dispose()

    $mutateEnvironment = {
        Remove-Item -LiteralPath "Env:EasyCon_Contract_Present" -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath "Env:EasyCon_Contract_Empty" -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath "Env:EasyCon_Contract_Cased" -ErrorAction SilentlyContinue
        Set-Item -LiteralPath "Env:EASYCON_CONTRACT_PRESENT" -Value "changed"
        Set-Item -LiteralPath "Env:EASYCON_CONTRACT_ABSENT" -Value "added"
        Remove-Item -LiteralPath "Env:RUSTC" -ErrorAction SilentlyContinue
        Set-Item -LiteralPath "Env:CXX" -Value "controlled-cl.exe"
    }.GetNewClosure()
    $verifiedEnvironment = {
        & $mutateEnvironment
        return "verified"
    }.GetNewClosure()
    $successfulWorkspace = {
        param($Summary)
        Assert-Contract ($Summary -ceq "verified") "Workspace must receive verified state"
        Assert-Contract ([string]::IsNullOrEmpty($env:RUSTC)) `
            "Workspace gate must not see the poisoned compiler"
        Assert-Contract ($env:CXX -ceq "controlled-cl.exe") `
            "Workspace gate must see the controlled compiler"
        Set-Item -LiteralPath "Env:EASYCON_GATE_MUTATION" -Value "gate"
    }.GetNewClosure()

    Invoke-EnvironmentRestorationContract -Mode Verify `
        -SetupAction { throw "Verify cannot setup" } `
        -VerifyAction $verifiedEnvironment -WorkspaceAction { param($Summary) }
    Invoke-EnvironmentRestorationContract -Mode Verify `
        -SetupAction { throw "Verify cannot setup" } `
        -VerifyAction {
            & $mutateEnvironment
            throw "synthetic verify restore failure"
        }.GetNewClosure() -WorkspaceAction { param($Summary) } `
        -FailurePattern "synthetic verify restore failure"
    Invoke-EnvironmentRestorationContract -Mode Workspace `
        -SetupAction { throw "Workspace cannot setup" } `
        -VerifyAction $verifiedEnvironment -WorkspaceAction $successfulWorkspace
    Invoke-EnvironmentRestorationContract -Mode Workspace `
        -SetupAction { throw "Workspace cannot setup" } `
        -VerifyAction $verifiedEnvironment -WorkspaceAction {
            param($Summary)
            Assert-Contract ([string]::IsNullOrEmpty($env:RUSTC)) `
                "failing gate must still see the sanitized environment"
            Set-Item -LiteralPath "Env:EASYCON_GATE_MUTATION" -Value "failing-gate"
            throw "synthetic gate restore failure"
        } -FailurePattern "synthetic gate restore failure"
    Invoke-EnvironmentRestorationContract -Mode Setup -SetupAction {
        & $mutateEnvironment
        throw "synthetic setup restore failure"
    }.GetNewClosure() -VerifyAction {
        & $mutateEnvironment
        throw "synthetic setup not ready"
    }.GetNewClosure() -WorkspaceAction { param($Summary) } `
        -FailurePattern "synthetic setup restore failure"

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
        if (
            -not $script:contractProbeCleanupBlocked -and
            (Test-Path -LiteralPath $temporaryRoot)
        ) {
            Remove-Item -LiteralPath $temporaryRoot -Recurse -Force -ErrorAction Stop
        }
    }
    catch {
        $cleanupFailure = $_
    }
    if ($null -ne $cleanupFailure) {
        if ($null -ne $contractFailure) {
            throw (
                "contract failed: primary lifecycle contract failure<<`n" +
                "$($contractFailure.Exception.Message)`n>>primary lifecycle contract failure`n" +
                "cleanup failure<<`n$($cleanupFailure.Exception.Message)`n>>cleanup failure"
            )
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
    "Windows environment lifecycle contracts passed: {0}/{1} cases (mode={2} registered={3} unique={4})" -f
        $script:executedContractCases,
        $expectedContractCases,
        $contractMode,
        $script:contractCases.Count,
        $script:contractCaseNames.Count
)

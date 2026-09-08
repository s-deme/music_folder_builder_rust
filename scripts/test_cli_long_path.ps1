[CmdletBinding()]
param(
    [string]$WorkspaceRoot = (Split-Path -Parent $PSScriptRoot),
    [string]$ExecutablePath = "target/release/music-folder.exe"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

function Invoke-Cli {
    param(
        [string]$Path,
        [string[]]$Arguments
    )
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Path
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) {
        $startInfo.ArgumentList.Add($argument)
    }
    $process = [Diagnostics.Process]::Start($startInfo)
    Assert-Condition ($null -ne $process) "failed to start CLI process: $Path"
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    return [pscustomobject]@{
        ExitCode = $process.ExitCode
        Stdout = $stdout
        Stderr = $stderr
    }
}

Assert-Condition $IsWindows "CLI long-path smoke requires Windows"
$longPathPolicy = Get-ItemPropertyValue `
    -LiteralPath "HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem" `
    -Name "LongPathsEnabled" `
    -ErrorAction Stop
Assert-Condition ($longPathPolicy -eq 1) "Windows LongPathsEnabled must be 1 for the CLI artifact smoke"

$cli = if ([IO.Path]::IsPathRooted($ExecutablePath)) {
    [IO.Path]::GetFullPath($ExecutablePath)
}
else {
    [IO.Path]::GetFullPath((Join-Path $WorkspaceRoot $ExecutablePath))
}
Assert-Condition (Test-Path -LiteralPath $cli -PathType Leaf) "CLI executable is missing: $cli"

$scratch = Join-Path ([IO.Path]::GetTempPath()) ("music-folder-cli-long-path-{0}" -f [Guid]::NewGuid())
$deepDirectory = $scratch
while (([IO.Path]::Combine($deepDirectory, "history-long-path.db")).Length -le 280) {
    $deepDirectory = [IO.Path]::Combine($deepDirectory, "1234567890abcdefghijklmnopqrst")
}

try {
    [IO.Directory]::CreateDirectory($deepDirectory) | Out-Null
    $database = [IO.Path]::Combine($deepDirectory, "history-long-path.db")
    Assert-Condition ($database.Length -gt 260) "smoke database path did not exceed 260 characters"

    $help = Invoke-Cli $cli @("--help")
    Assert-Condition ($help.ExitCode -eq 0) "CLI artifact failed to launch: $($help.Stderr)"
    Assert-Condition ($help.Stdout -match "Safe music library organizer") "CLI help output is unexpected"

    $history = Invoke-Cli $cli @("--output", "json", "history", "list", "--db", $database)
    Assert-Condition ($history.ExitCode -eq 0) "CLI could not use a >260-character SQLite path: $($history.Stderr)"
    $envelope = $history.Stdout | ConvertFrom-Json
    Assert-Condition ($envelope.schema_version -eq 1) "CLI long-path response schema is unexpected"
    Assert-Condition ($envelope.command -eq "history.list") "CLI long-path response command is unexpected"
    Assert-Condition ($envelope.status -eq "success") "CLI long-path response is not successful"
    Assert-Condition ([IO.File]::Exists($database)) "CLI did not create the SQLite database at the long path"
}
finally {
    if ([IO.Directory]::Exists($scratch)) {
        [IO.Directory]::Delete($scratch, $true)
    }
}

Write-Host "CLI artifact with >260-character SQLite path smoke passed"

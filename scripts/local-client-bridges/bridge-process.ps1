[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('start','owns','stop')][string]$Action,
    [Parameter(Mandatory)][ValidateSet('workbuddy','trae')][string]$Bridge,
    [int]$ProcessId = 0
)
$ErrorActionPreference = 'Stop'
$nodePath = (Get-Command node -ErrorAction Stop).Source
$scriptPath = Join-Path $PSScriptRoot ($Bridge + '-bridge.js')
$bridgePort = if ($Bridge -eq 'workbuddy') { 8791 } else { 8792 }
$cdpPort = if ($Bridge -eq 'workbuddy') { 9224 } else { 9226 }
if ($Action -eq 'start') {
    # WMI creates the process outside the calling host's Job Object.
    $line = '"' + $nodePath + '" "' + $scriptPath + '" --port ' + $bridgePort + ' --cdp ' + $cdpPort
    $created = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $line; CurrentDirectory = $PSScriptRoot }
    if ($created.ReturnValue -ne 0) { throw ('WMI launch failed: ' + $created.ReturnValue) }
    Write-Output $created.ProcessId
    exit 0
}
if ($ProcessId -lt 1) { exit 1 }
$candidate = Get-CimInstance Win32_Process -Filter ('ProcessId = ' + $ProcessId)
$scriptPattern = '(?i)(?:^|\s)"?' + [regex]::Escape($scriptPath) + '"?(?:\s|$)'
$portPattern = '(?:^|\s)--port\s+' + $bridgePort + '(?:\s|$)'
$owned = $candidate -and $candidate.ExecutablePath -eq $nodePath -and $candidate.CommandLine -match $scriptPattern -and $candidate.CommandLine -match $portPattern
if (-not $owned) { exit 1 }
if ($Action -eq 'stop') { Stop-Process -Id $ProcessId -ErrorAction Stop }
Write-Output 'owned'

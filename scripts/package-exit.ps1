# Real acceptance only: request quit on the isolated application's window thread.
param([Parameter(Mandatory = $true)][int]$ApplicationPid)
$ErrorActionPreference = 'Stop'
$application = Get-Process -Id $ApplicationPid
if (-not $application.MainWindowTitle.Contains('LLM Gateway')) {
    throw 'Target is not the acceptance application window'
}
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class PackageWindowQuit {
    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool PostThreadMessage(uint thread, uint message, UIntPtr wParam, IntPtr lParam);
}
'@
[uint32]$ownerProcess = 0
$windowThread = [PackageWindowQuit]::GetWindowThreadProcessId($application.MainWindowHandle, [ref]$ownerProcess)
if ($ownerProcess -ne $ApplicationPid -or $windowThread -eq 0) {
    throw 'Window ownership check failed'
}
if (-not [PackageWindowQuit]::PostThreadMessage($windowThread, 18, [UIntPtr]::Zero, [IntPtr]::Zero)) {
    throw 'Cannot request application thread quit'
}
if (-not $application.WaitForExit(20000)) {
    throw 'Application did not exit after window thread quit'
}
Write-Output 'APPLICATION_THREAD_QUIT_OK'

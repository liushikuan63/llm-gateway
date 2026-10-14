# Real acceptance only: request quit on the isolated application's window thread.
param([Parameter(Mandatory = $true)][int]$ApplicationPid, [switch]$AllowHidden)
$ErrorActionPreference = 'Stop'
$application = Get-Process -Id $ApplicationPid
if (-not $AllowHidden -and -not $application.MainWindowTitle.Contains('LLM Gateway')) {
    throw 'Target is not the acceptance application window'
}
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class PackageWindowQuit {
    public delegate bool EnumWindowProc(IntPtr window, IntPtr data);
    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumWindowProc callback, IntPtr data);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr window, StringBuilder text, int maxCount);
    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool PostThreadMessage(uint thread, uint message, UIntPtr wParam, IntPtr lParam);
    public static IntPtr FindOwnedMainWindow(uint expectedProcess) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((window, data) => {
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (owner != expectedProcess) return true;
            var title = new StringBuilder(512);
            GetWindowText(window, title, title.Capacity);
            if (title.ToString() != "LLM Gateway") return true;
            found = window;
            return false;
        }, IntPtr.Zero);
        return found;
    }
}
'@
[uint32]$ownerProcess = 0
$targetWindow = $application.MainWindowHandle
if ($AllowHidden) {
    $targetWindow = [PackageWindowQuit]::FindOwnedMainWindow([uint32]$ApplicationPid)
    if ($targetWindow -eq [IntPtr]::Zero) { throw 'Owned hidden main window not found' }
}
$windowThread = [PackageWindowQuit]::GetWindowThreadProcessId($targetWindow, [ref]$ownerProcess)
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

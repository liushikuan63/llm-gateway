param([Parameter(Mandatory)][string]$Executable,[Parameter(Mandatory)][int]$Port,[Parameter(Mandatory)][string]$Worker,[Parameter(Mandatory)][string]$Node,[string]$Mode='inspect',[string]$ReceiptDirectory,[string]$ExpectedIdentityHash,[ValidateSet(0,1,2)][int]$RequestSource=0,[string]$CampaignId)
$ErrorActionPreference='Stop'
$Executable=[IO.Path]::GetFullPath($Executable).Replace('/','\')
$launchStarted=[DateTime]::Now
$workerArguments=@('--port',[string]$Port,'--mode',$Mode)
if ($ReceiptDirectory) {$workerArguments+=@('--receipts',$ReceiptDirectory)}
if ($Mode -in @('claim-native','verify-native')) {
    if ($ExpectedIdentityHash -notmatch '^[a-f0-9]{64}$' -or $RequestSource -eq 0 -or $CampaignId -notmatch '^daily_\d{4}-\d{2}-\d{2}$') {throw 'Invalid native account binding'}
    $workerArguments+=@('--expected-identity-hash',$ExpectedIdentityHash,'--request-source',[string]$RequestSource,'--campaign-id',$CampaignId)
}
if (-not (Test-Path -LiteralPath $Executable) -or -not (Test-Path -LiteralPath $Worker)) {throw 'Missing verified client or worker'}
$listeners=@(Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue)
if ($listeners.Count -gt 0) {
    foreach ($listener in $listeners) {
        $owner=Get-CimInstance Win32_Process -Filter ('ProcessId='+$listener.OwningProcess)
        if (-not $owner -or $owner.ExecutablePath -ne $Executable -or $owner.CommandLine -notmatch ('--remote-debugging-port='+$Port+'(?:\s|$)')) {throw 'Private debug port belongs to a different process'}
    }
    & $Node $Worker @workerArguments
    exit $LASTEXITCODE
}
if (Get-CimInstance Win32_Process | Where-Object {$_.ExecutablePath -eq $Executable}) {throw 'Client is already running without the private endpoint; close it before importing login'}
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class RewardPrivateClient {
 [StructLayout(LayoutKind.Sequential,CharSet=CharSet.Unicode)] public struct SI { public int cb; public string reserved,desktop,title; public int x,y,cx,cy,xchars,ychars,fill,flags; public short show,reserved2; public IntPtr reservedPtr,stdin,stdout,stderr; }
 [StructLayout(LayoutKind.Sequential)] public struct PI { public IntPtr process,thread; public int pid,tid; }
 [StructLayout(LayoutKind.Sequential)] public struct BASIC { public long perProcess,perJob; public uint flags; public UIntPtr min,max; public uint active; public UIntPtr affinity; public uint priority,scheduling; }
 [StructLayout(LayoutKind.Sequential)] public struct IO { public ulong a,b,c,d,e,f; }
 [StructLayout(LayoutKind.Sequential)] public struct EXT { public BASIC basic; public IO io; public UIntPtr processMemory,jobMemory,peakProcess,peakJob; }
 [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] public static extern IntPtr CreateDesktopW(string name,IntPtr device,IntPtr mode,uint flags,uint access,IntPtr security);
 [DllImport("user32.dll")] public static extern bool CloseDesktop(IntPtr desktop);
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] public static extern bool CreateProcessW(string exe,StringBuilder command,IntPtr ps,IntPtr ts,bool inherit,uint flags,IntPtr environment,string directory,ref SI si,out PI pi);
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] public static extern IntPtr CreateJobObjectW(IntPtr security,string name);
 [DllImport("kernel32.dll",SetLastError=true)] public static extern bool SetInformationJobObject(IntPtr job,int kind,ref EXT info,int size);
 [DllImport("kernel32.dll",SetLastError=true)] public static extern bool AssignProcessToJobObject(IntPtr job,IntPtr process);
 [DllImport("kernel32.dll")] public static extern uint ResumeThread(IntPtr thread);
 [DllImport("kernel32.dll")] public static extern bool TerminateProcess(IntPtr process,uint code);
 [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr handle);
}
'@
$desktopName='LLMGWRewards-'+[guid]::NewGuid().ToString('N')
$desktop=[RewardPrivateClient]::CreateDesktopW($desktopName,[IntPtr]::Zero,[IntPtr]::Zero,0,0xCF,[IntPtr]::Zero)
if ($desktop -eq [IntPtr]::Zero) {throw 'Private desktop unavailable; no foreground fallback'}
$job=[RewardPrivateClient]::CreateJobObjectW([IntPtr]::Zero,$null)
$process=[RewardPrivateClient+PI]::new()
try {
    if ($job -eq [IntPtr]::Zero) {throw 'Private job unavailable'}
    $limits=[RewardPrivateClient+EXT]::new();$limits.basic.flags=0x2000
    if (-not [RewardPrivateClient]::SetInformationJobObject($job,9,[ref]$limits,[Runtime.InteropServices.Marshal]::SizeOf($limits))) {throw 'Private job limit failed'}
    $startup=[RewardPrivateClient+SI]::new();$startup.cb=[Runtime.InteropServices.Marshal]::SizeOf($startup);$startup.desktop='WinSta0\'+$desktopName;$startup.flags=0x81;$startup.show=0
    $command=[Text.StringBuilder]::new(('"'+$Executable+'" --remote-debugging-port='+$Port+' --remote-debugging-address=127.0.0.1 --disable-gpu'))
    if (-not [RewardPrivateClient]::CreateProcessW($Executable,$command,[IntPtr]::Zero,[IntPtr]::Zero,$false,0x08000004,[IntPtr]::Zero,(Split-Path -Parent $Executable),[ref]$startup,[ref]$process)) {throw 'Private client launch failed'}
    if (-not [RewardPrivateClient]::AssignProcessToJobObject($job,$process.process)) {[RewardPrivateClient]::TerminateProcess($process.process,1)|Out-Null;throw 'Private client ownership failed'}
    $resume=[RewardPrivateClient]::ResumeThread($process.thread)
    if ($resume -eq 0xFFFFFFFF -or $resume -eq 0) {throw 'Private client resume failed'}
    & $Node $Worker @workerArguments '--owned-pid' ([string]$process.pid)
    $workerExit=$LASTEXITCODE
} finally {
    # Electron may relaunch through its native launcher outside the initial Job.
    # Select only this executable and unique debug port, created during this run.
    $ownedRoots=@(Get-CimInstance Win32_Process | Where-Object {$_.ExecutablePath -eq $Executable -and $_.CommandLine -match ('--remote-debugging-port='+$Port+'(?:\s|$)') -and $_.CreationDate -ge $launchStarted -and $_.CommandLine -notmatch '--type='})
    foreach ($root in $ownedRoots) {
        $current=Get-CimInstance Win32_Process -Filter ('ProcessId='+$root.ProcessId)
        if ($current -and $current.CreationDate -eq $root.CreationDate -and $current.CommandLine -ceq $root.CommandLine) {
            $stop=Start-Process 'C:\Windows\System32\taskkill.exe' -ArgumentList @('/PID',[string]$root.ProcessId,'/T','/F') -WindowStyle Hidden -Wait -PassThru
            if ($stop.ExitCode -ne 0 -and (Get-CimInstance Win32_Process -Filter ('ProcessId='+$root.ProcessId) | Where-Object {$_.CreationDate -eq $root.CreationDate})) {Write-Warning 'Private client cleanup requires inspection'}
        }
    }
    if ($job -ne [IntPtr]::Zero) {[RewardPrivateClient]::CloseHandle($job)|Out-Null}
    if ($process.thread -ne [IntPtr]::Zero) {[RewardPrivateClient]::CloseHandle($process.thread)|Out-Null}
    if ($process.process -ne [IntPtr]::Zero) {[RewardPrivateClient]::CloseHandle($process.process)|Out-Null}
    [RewardPrivateClient]::CloseDesktop($desktop)|Out-Null
}
exit $workerExit

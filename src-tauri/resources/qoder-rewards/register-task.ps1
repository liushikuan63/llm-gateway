param([Parameter(Mandatory)][string]$Executable,[Parameter(Mandatory)][string]$DataDirectory,[Parameter(Mandatory)][int]$Hour,[Parameter(Mandatory)][int]$Minute,[ValidateSet('local','Asia/Shanghai')][string]$Timezone,[ValidateSet('true','false')][string]$Enabled,[string]$TriggerMinutes='')
$ErrorActionPreference='Stop'
$Executable=[IO.Path]::GetFullPath($Executable)
$DataDirectory=[IO.Path]::GetFullPath($DataDirectory)
if ($Hour -lt 0 -or $Hour -gt 23 -or $Minute -lt 0 -or $Minute -gt 59 -or -not (Test-Path -LiteralPath $Executable) -or -not (Test-Path -LiteralPath (Join-Path $DataDirectory 'config.toml'))) {throw 'Invalid owned task configuration'}
$identity=[Security.Principal.WindowsIdentity]::GetCurrent()
$digest=[Security.Cryptography.SHA256]::Create().ComputeHash([Text.Encoding]::UTF8.GetBytes($DataDirectory.ToLowerInvariant()+'|'+$identity.User.Value))
$key=([BitConverter]::ToString($digest)).Replace('-','').Substring(0,16)
$name='LLMGW-Benefits-'+$key
$owner='LLMGW-Benefits-v1:'+ $key
$existing=Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
if ($existing -and $existing.Description -ne $owner) {throw 'Task name occupied by another owner'}
if ($existing) {
    $backup=Join-Path $DataDirectory 'benefits-task-backups'
    [IO.Directory]::CreateDirectory($backup)|Out-Null
    [IO.File]::WriteAllText((Join-Path $backup ((Get-Date -Format 'yyyyMMdd-HHmmss-ffff')+'.xml')),(Export-ScheduledTask -TaskName $name),[Text.UTF8Encoding]::new($false))
}
$offset=if ($Timezone -eq 'Asia/Shanghai') {[TimeSpan]::FromHours(8)}else{[TimeZoneInfo]::Local.GetUtcOffset([DateTime]::Now)}
$clock=[DateTimeOffset]::UtcNow.ToOffset($offset)
$boundaryFormat=if ($Timezone -eq 'Asia/Shanghai') {'yyyy-MM-ddTHH:mm:sszzz'}else{'yyyy-MM-ddTHH:mm:ss'}
$times=@($Hour*60+$Minute)
if ($TriggerMinutes -ne '') {
    if ($TriggerMinutes -notmatch '^\d{1,4}(,\d{1,4}){0,63}$') {throw 'Invalid trigger times'}
    $times=@($TriggerMinutes.Split(',')|ForEach-Object {[int]$_}|Sort-Object -Unique)
    if (@($times|Where-Object {$_ -lt 0 -or $_ -ge 1440}).Count -ne 0) {throw 'Invalid trigger times'}
}
$boundaries=@($times|ForEach-Object {[DateTimeOffset]::new($clock.Year,$clock.Month,$clock.Day,[int][Math]::Floor($_/60),[int]($_%60),0,$offset).ToString($boundaryFormat)})
$triggers=($boundaries|ForEach-Object {"<CalendarTrigger><StartBoundary>$_</StartBoundary><Enabled>true</Enabled><ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay></CalendarTrigger>"}) -join ''
$escape={param([string]$Value)[Security.SecurityElement]::Escape($Value)}
$exeXml=&$escape $Executable
$argsXml=&$escape ('--benefits-once --data-dir "'+$DataDirectory+'"')
$sidXml=&$escape $identity.User.Value
$xml=@"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.3" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
 <RegistrationInfo><Description>$owner</Description></RegistrationInfo>
 <Triggers>$triggers</Triggers>
 <Principals><Principal id="Author"><UserId>$sidXml</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
 <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><Enabled>$Enabled</Enabled><WakeToRun>false</WakeToRun><ExecutionTimeLimit>PT5M</ExecutionTimeLimit></Settings>
 <Actions Context="Author"><Exec><Command>$exeXml</Command><Arguments>$argsXml</Arguments></Exec></Actions>
</Task>
"@
Register-ScheduledTask -TaskName $name -Xml $xml -Force | Out-Null
$saved=Get-ScheduledTask -TaskName $name
if ($saved.Description -ne $owner -or $saved.Actions[0].Execute -ne $Executable -or $saved.Actions[0].Arguments -ne ('--benefits-once --data-dir "'+$DataDirectory+'"') -or (@($saved.Triggers|ForEach-Object {$_.StartBoundary}|Sort-Object) -join '|') -ne (@($boundaries|Sort-Object) -join '|')) {throw 'Task read-back mismatch'}
$info=Get-ScheduledTaskInfo -TaskName $name
@{task_name=$name;enabled=($Enabled -eq 'true');start_boundaries=$boundaries;next_run=$info.NextRunTime.ToString('o');background=$true;logon_type='InteractiveToken'}|ConvertTo-Json -Compress

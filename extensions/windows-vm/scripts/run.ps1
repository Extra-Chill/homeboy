# Invoked via SSH with a JSON request on stdin. All paths are run-scoped.
$ErrorActionPreference = 'Stop'
$request = [Console]::In.ReadToEnd() | ConvertFrom-Json
$root = "C:\runs\$($request.id)"
$task = "homeboy-$($request.id)"
$registered = $false
$failure = $null
try {
    if (Test-Path -LiteralPath $root) { throw 'directory_exists: Run directory already exists' }
    New-Item -ItemType Directory -Path $root -ErrorAction Stop | Out-Null
    $acl = Get-Acl -LiteralPath $root
    $acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($request.user, 'Modify', 'ContainerInherit,ObjectInherit', 'None', 'Allow')))
    Set-Acl -LiteralPath $root -AclObject $acl
    $start = [DateTime]::UtcNow.ToString('o')
    [IO.File]::WriteAllText("$root\payload.ps1", $request.script)
    # A kernel job owns the worker and every descendant, including detached children.
    # KILL_ON_JOB_CLOSE works on timeout/Stop-ScheduledTask as well as normal exit.
    $worker = @'
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public class RunJob {
 [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr CreateJobObject(IntPtr a, string n);
 [DllImport("kernel32.dll")] public static extern bool SetInformationJobObject(IntPtr h, int c, IntPtr p, uint s);
 [DllImport("kernel32.dll")] public static extern bool AssignProcessToJobObject(IntPtr h, IntPtr p);
 [StructLayout(LayoutKind.Sequential)] public struct Basic { public long a,b; public uint flags; public UIntPtr c,d; public uint e; public UIntPtr f; public uint g,h; }
 [StructLayout(LayoutKind.Sequential)] public struct IO { public ulong a,b,c,d,e,f; }
 [StructLayout(LayoutKind.Sequential)] public struct Extended { public Basic basic; public IO io; public UIntPtr a,b,c,d; }
 public static IntPtr Start() {
  var h = CreateJobObject(IntPtr.Zero, null);
  var info = new Extended(); info.basic.flags = 0x2000;
  int size = Marshal.SizeOf(info); var p = Marshal.AllocHGlobal(size);
  Marshal.StructureToPtr(info,p,false);
  bool ok = SetInformationJobObject(h,9,p,(uint)size); Marshal.FreeHGlobal(p);
  if (!ok || !AssignProcessToJobObject(h,System.Diagnostics.Process.GetCurrentProcess().Handle)) throw new Exception("job_setup_failed");
  return h;
 }
}
"@
$jobHandle = [RunJob]::Start()
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$config = Get-Content -Raw -LiteralPath "$root\request.json" | ConvertFrom-Json
if ($config.as -eq 'standard') {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    if ($identity.Groups.Value -contains 'S-1-5-32-544') { throw 'invalid_user: Standard task has an administrator group token' }
}
# CreateNew must succeed; never delete or reuse a previous output.
$stream = [IO.File]::Open("$root\output.txt", [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
$stream.Close()
$code = 1
try {
    # Separate PowerShell process makes both exit N and native exit codes observable.
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$root\payload.ps1" 2>&1 | Out-File -LiteralPath "$root\output.txt" -Encoding UTF8
    $code = $LASTEXITCODE
} catch { $_ | Out-String | Add-Content -LiteralPath "$root\output.txt" -Encoding UTF8 }
Add-Content -LiteralPath "$root\output.txt" -Value "`nEXIT:$code NONCE:$($config.nonce)" -Encoding UTF8
'@
    [IO.File]::WriteAllText("$root\worker.ps1", $worker)
    # No credentials stored in the guest request file.
    @{ nonce = $request.nonce; as = $request.as } | ConvertTo-Json | Set-Content -LiteralPath "$root\request.json"
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -ExecutionPolicy Bypass -File $root\worker.ps1"
    $level = if ($request.as -eq 'admin') { 'Highest' } else { 'Limited' }
    if ($request.as -eq 'standard') {
        $members = Get-LocalGroupMember -SID 'S-1-5-32-544'
        $sid = (Get-LocalUser -Name $request.user).SID.Value
        if ($members.SID.Value -contains $sid) { throw 'invalid_user: Standard user is an administrator' }
    }
    if ($request.interactive) {
        $principal = New-ScheduledTaskPrincipal -UserId $request.user -LogonType Interactive -RunLevel $level
        Register-ScheduledTask -TaskName $task -Action $action -Principal $principal | Out-Null
    } else {
        Register-ScheduledTask -TaskName $task -Action $action -User $request.user -Password $request.password -RunLevel $level | Out-Null
    }
    $registered = $true
    Start-ScheduledTask -TaskName $task
    $deadline = [DateTime]::UtcNow.AddSeconds($request.timeout)
    do {
        Start-Sleep -Milliseconds 250
        if ([DateTime]::UtcNow -gt $deadline) { throw 'timeout: Scheduled task exceeded deadline' }
        $info = Get-ScheduledTaskInfo -TaskName $task
        $state = (Get-ScheduledTask -TaskName $task).State
    } while ($state -eq 'Running' -or $state -eq 'Queued' -or $info.LastRunTime -lt [DateTime]::Parse($start).ToLocalTime().AddSeconds(-1))
    if ($info.LastTaskResult -ne 0) { throw "task_failed: Scheduled task result $($info.LastTaskResult)" }
    if (!(Test-Path -LiteralPath "$root\output.txt")) { throw 'missing_output: Task produced no output' }
    $created = (Get-Item -LiteralPath "$root\output.txt").CreationTimeUtc.ToString('o')
    @{ nonce = $request.nonce; startedAt = $start; requestStartedAt = $request.startedAt; outputCreatedAt = $created } |
        ConvertTo-Json | Set-Content -LiteralPath "$root\result.json" -Encoding UTF8
    New-Item -ItemType Directory -Path "$root\artifacts" | Out-Null
    for ($i = 0; $i -lt $request.artifacts.Count; $i++) {
        $source = $request.artifacts[$i]
        if (!(Test-Path -LiteralPath $source)) { throw "missing_artifact: $source" }
        Copy-Item -LiteralPath $source -Destination "$root\artifacts\$i" -Recurse -ErrorAction Stop
    }
} catch { $failure = $_.Exception.Message }
finally {
    if ($registered) {
        try {
            Stop-ScheduledTask -TaskName $task -ErrorAction Stop
            Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction Stop
        } catch { $failure = "cleanup_failed: $($_.Exception.Message); $failure" }
    }
}
if ($failure) {
    @{ ok = $false; error = $failure } | ConvertTo-Json -Compress
    exit 1
}
@{ ok = $true; root = $root } | ConvertTo-Json -Compress

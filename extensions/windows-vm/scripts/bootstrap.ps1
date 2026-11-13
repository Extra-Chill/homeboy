$ErrorActionPreference = 'Stop'
# prepare reads these markers over SSH: done means ready, failed carries the error.
trap { Set-Content -LiteralPath C:\OEM\bootstrap.failed -Value ($_ | Out-String); exit 1 }
$config = Get-Content -Raw C:\OEM\bootstrap.json | ConvertFrom-Json
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0 | Out-Null
Set-Service sshd -StartupType Automatic
if (!(Get-NetFirewallRule -Name homeboy-ssh -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule -Name homeboy-ssh -DisplayName 'Homeboy OpenSSH' -Direction Inbound -Protocol TCP -LocalPort 22 -Action Allow | Out-Null
}
New-Item -Path HKLM:\SOFTWARE\OpenSSH -Force | Out-Null
New-ItemProperty -Path HKLM:\SOFTWARE\OpenSSH -Name DefaultShell -Value 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe' -PropertyType String -Force | Out-Null
$keyfile = 'C:\ProgramData\ssh\administrators_authorized_keys'
[IO.File]::WriteAllText($keyfile, $config.publicKey + "`n")
& icacls.exe $keyfile /inheritance:r /grant '*S-1-5-32-544:F' '*S-1-5-18:F' | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Cannot secure authorized keys' }
# SSH comes up before the toolchain so a failed bootstrap stays diagnosable.
Start-Service sshd
$password = ConvertTo-SecureString $config.standardPassword -AsPlainText -Force
if (!(Get-LocalUser -Name $config.standardUser -ErrorAction SilentlyContinue)) {
    New-LocalUser -Name $config.standardUser -Password $password -PasswordNeverExpires | Out-Null
}
Add-LocalGroupMember -SID 'S-1-5-32-545' -Member $config.standardUser -ErrorAction SilentlyContinue
# Task Scheduler logs the standard user on as a batch job; client Windows grants
# that right only to administrators, so add the standard user explicitly.
$sid = (Get-LocalUser -Name $config.standardUser).SID.Value
$rights = 'C:\Windows\Temp\homeboy-rights.inf'; $batch = 'C:\Windows\Temp\homeboy-batch.inf'
& secedit.exe /export /cfg $rights /areas USER_RIGHTS | Out-Null
$holders = @((Select-String -LiteralPath $rights -Pattern '^SeBatchLogonRight\s*=\s*(.*)$').Matches.Groups[1].Value -split ',' | Where-Object { $_ }) + "*$sid" | Select-Object -Unique
Set-Content -LiteralPath $batch -Encoding Unicode -Value @('[Unicode]', 'Unicode=yes', '[Version]', 'signature="$CHICAGO$"', 'Revision=1', '[Privilege Rights]', "SeBatchLogonRight = $($holders -join ',')")
& secedit.exe /configure /db C:\Windows\Temp\homeboy-batch.sdb /cfg $batch /areas USER_RIGHTS | Out-Null
if ($LASTEXITCODE -ne 0) { throw "Cannot grant batch logon to the standard user: secedit $LASTEXITCODE" }
# winget is not reliably available in the OEM SYSTEM session. Use official installers.
Invoke-WebRequest https://nodejs.org/dist/index.json -OutFile C:\OEM\node-index.json -UseBasicParsing
# Windows PowerShell emits a JSON array as one pipeline object; parenthesize to enumerate it.
$node = ((Get-Content -Raw C:\OEM\node-index.json | ConvertFrom-Json) | Where-Object { $_.lts } | Select-Object -First 1).version
Invoke-WebRequest "https://nodejs.org/dist/$node/node-$node-x64.msi" -OutFile C:\OEM\node.msi -UseBasicParsing
$p = Start-Process msiexec.exe -ArgumentList '/i C:\OEM\node.msi /qn /norestart' -Wait -PassThru
if ($p.ExitCode -notin @(0,3010)) { throw 'Node installation failed' }
$release = Invoke-RestMethod https://api.github.com/repos/git-for-windows/git/releases/latest
$url = ($release.assets | Where-Object { $_.name -match '^Git-.*-64-bit.exe$' } | Select-Object -First 1).browser_download_url
Invoke-WebRequest $url -OutFile C:\OEM\git.exe -UseBasicParsing
$p = Start-Process C:\OEM\git.exe -ArgumentList '/VERYSILENT /NORESTART /SP-' -Wait -PassThru
if ($p.ExitCode -ne 0) { throw 'Git installation failed' }
# Remove guest copy of bootstrap credentials. The host OEM directory remains private.
Remove-Item -LiteralPath C:\OEM\bootstrap.json -Force
Set-Content -LiteralPath C:\OEM\bootstrap.done -Value (Get-Date -Format o)

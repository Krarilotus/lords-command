# lords panel add, on the PC that gets the panel: installs it from this run's staging dir (this script's own dir,
# staging\<run> beside the app's files), as panel.json beside it says, and adds the panel's key and SSH alias. Every
# installed file it replaces is backed up first, into backup-<run> of this run's own; the token's copy gets an
# owner-only access list before it takes its place, and is never backed up (a secret keeps no copies). It runs under
# its step's watchdog (panel add's runner), which ends it and what it starts at the step's deadline. Prints one line
# of JSON: the key's .pub and what it did. No private key leaves this PC: the key is made here; only its .pub is
# printed.
$ErrorActionPreference = 'Stop'
$staging = $PSScriptRoot
$p = Get-Content -Raw -LiteralPath (Join-Path $staging 'panel.json') | ConvertFrom-Json
$app = Split-Path -Parent (Split-Path -Parent $staging)
$ssh = Join-Path $env:USERPROFILE '.ssh'
$done = @()

# The app first: refused while a panel runs from here, before anything else changes.
$open = @(Get-Process lords-command -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$app\*" })
if ($open.Count) { throw 'lords-command runs here; close it, then run panel add again' }

# The key, made here.
$key = Join-Path $ssh $p.key
if (-not (Test-Path -LiteralPath $key)) {
    New-Item -ItemType Directory -Force -Path $ssh | Out-Null
    $gen = Start-Process -FilePath 'ssh-keygen.exe' -NoNewWindow -PassThru `
        -ArgumentList @('-q', '-t', 'ed25519', '-N', '""', '-C', $p.comment, '-f', $key)
    $gen.WaitForExit()
    if ($gen.ExitCode -ne 0) { throw "ssh-keygen failed ($($gen.ExitCode))" }
    $done += "made $key"
}

# The alias: appended to ~/.ssh/config only while it holds what the plan read, after a backup.
if ($p.block) {
    $cfg = Join-Path $ssh 'config'
    $text = if (Test-Path -LiteralPath $cfg) { [IO.File]::ReadAllText($cfg) } else { '' }
    if ($text -cne $p.config_before) { throw '~/.ssh/config changed since the plan; run panel add again' }
    $backup = "$cfg.before-lords-panel"
    if ((Test-Path -LiteralPath $cfg) -and -not (Test-Path -LiteralPath $backup)) {
        Copy-Item -LiteralPath $cfg -Destination $backup
        $done += "backed up $backup"
    }
    $sep = if ($text -and -not $text.EndsWith("`n")) { "`r`n" } else { '' }
    [IO.File]::AppendAllText($cfg, $sep + $p.block)
    $done += "added Host $($p.alias) to $cfg"
}

# The files: each one there backed up before it is replaced; config.txt written only where there is none.
$backups = Join-Path $app ('backup-' + $p.run)
foreach ($f in 'lords-command.exe', 'README.md', 'LICENSE') {
    $to = Join-Path $app $f
    if (Test-Path -LiteralPath $to) {
        if (-not (Test-Path -LiteralPath $backups)) {
            New-Item -ItemType Directory -Path $backups | Out-Null
        }
        Copy-Item -LiteralPath $to -Destination (Join-Path $backups $f)
        $done += "backed up $to into $backups"
    }
    Move-Item -Force -LiteralPath (Join-Path $staging $f) -Destination $to
    $done += "installed $to"
}
$conf = Join-Path $app 'config.txt'
if ($p.config_txt -and -not (Test-Path -LiteralPath $conf)) {
    [IO.File]::WriteAllText($conf, $p.config_txt)
    $done += "wrote $conf"
}

# The token: owner-only before it takes its place (a protected list, one entry: this account, full control).
$staged = Join-Path $staging 'panel-token'
$acl = New-Object System.Security.AccessControl.FileSecurity
$acl.SetAccessRuleProtection($true, $false)
$me = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($me, 'FullControl', 'Allow')))
Set-Acl -LiteralPath $staged -AclObject $acl
Move-Item -Force -LiteralPath $staged -Destination (Join-Path $app 'panel-token')
$done += "placed $(Join-Path $app 'panel-token') (owner-only)"

# The Start-menu entry.
$lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\lords-command.lnk'
if (-not (Test-Path -LiteralPath $lnk)) {
    $s = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
    $s.TargetPath = Join-Path $app 'lords-command.exe'
    $s.WorkingDirectory = $app
    $s.Save()
    $done += "made $lnk"
}

[ordered]@{ public = ([IO.File]::ReadAllText("$key.pub")).Trim(); done = $done } | ConvertTo-Json -Compress

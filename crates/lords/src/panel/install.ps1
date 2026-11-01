# lords panel add, on the PC that gets the panel: installs it from this run's staging dir (this script's own dir,
# staging\<run> beside the app's files), as panel.json beside it says, and adds the panel's key and SSH alias. For a
# client embedded in another app (panel.json's `embedded`), which reads only config.txt and the token here, it installs
# no window, README, LICENSE or Start-menu entry, and leaves any that are here as they are.
#
# All or nothing (embedded panel review r1 R2). Before each change to a live file it writes a line to this run's
# journal (journal, beside it): `made <path>`, or `replaced <path> <copy>`, the copy made first; a line may end with
# what this run wrote there (the ssh config), so that its undo leaves another's change alone. Each change is one move
# into place. The client goes live last: with no token here, the token, then config.txt; with one, config.txt if it is
# missing, then the token. The last move's source is named first (`final <name>`): once it is gone from this dir, the
# install is complete. On any error it runs undo.ps1, which undoes what the journal names, last first; after a kill (its
# step's watchdog runs no catch) panel add runs undo.ps1 as a step of its own. Every installed file it replaces is
# backed up first, into backup-<run>; the token's copy gets an owner-only access list before it takes its place, and is
# never copied (a secret keeps no copies): only the last move replaces it. It runs under its step's watchdog (panel
# add's runner), which ends it and what it starts at the step's deadline. Prints one line of JSON: the key's .pub and
# what it did. No private key leaves this PC: the key is made here; only its .pub is printed.
$ErrorActionPreference = 'Stop'
$staging = $PSScriptRoot
$p = Get-Content -Raw -LiteralPath (Join-Path $staging 'panel.json') | ConvertFrom-Json
$app = Split-Path -Parent (Split-Path -Parent $staging)
$ssh = Join-Path $env:USERPROFILE '.ssh'
$embedded = $p.embedded -eq $true
$journal = Join-Path $staging 'journal'
$token = Join-Path $app 'panel-token'
$conf = Join-Path $app 'config.txt'
$done = @()
function Note($line) { Add-Content -LiteralPath $journal -Value $line -Encoding UTF8 }
# `File.Replace` without a backup takes a null path; powershell would pass `$null` to it as an empty one.

# Refused before anything changes: a window running from here (its exe is replaced; an embedded client's run replaces
# nothing it runs from), and a config.txt the plan saw that is gone since (the token would stand without it).
if (-not $embedded) {
    $open = @(Get-Process lords-command -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$app\*" })
    if ($open.Count) { throw 'lords-command runs here; close it, then run panel add again' }
}
$hadConf = Test-Path -LiteralPath $conf
if (-not $p.config_txt -and -not $hadConf) { throw 'config.txt went away since the plan; run panel add again' }
$hadToken = Test-Path -LiteralPath $token

try {
    # The key, made here.
    $key = Join-Path $ssh $p.key
    if (-not (Test-Path -LiteralPath $key)) {
        if (-not (Test-Path -LiteralPath $ssh)) {
            Note "made`t$ssh"
            New-Item -ItemType Directory -Path $ssh | Out-Null
        }
        Note "made`t$key"
        Note "made`t$key.pub"
        $gen = Start-Process -FilePath 'ssh-keygen.exe' -NoNewWindow -PassThru `
            -ArgumentList @('-q', '-t', 'ed25519', '-N', '""', '-C', $p.comment, '-f', $key)
        $gen.WaitForExit()
        if ($gen.ExitCode -ne 0) { throw "ssh-keygen failed ($($gen.ExitCode))" }
        $done += "made $key"
    }

    # The alias: ~/.ssh/config only while it holds what the plan read, backed up once, then its bytes and the block
    # put in its place by one move.
    if ($p.block) {
        $cfg = Join-Path $ssh 'config'
        $had = Test-Path -LiteralPath $cfg
        $text = if ($had) { [IO.File]::ReadAllText($cfg) } else { '' }
        if ($text -cne $p.config_before) { throw '~/.ssh/config changed since the plan; run panel add again' }
        $backup = "$cfg.before-lords-panel"
        if ($had -and -not (Test-Path -LiteralPath $backup)) {
            Note "made`t$backup"
            Copy-Item -LiteralPath $cfg -Destination $backup
            $done += "backed up $backup"
        }
        $sep = if ($text -and -not $text.EndsWith("`n")) { "`r`n" } else { '' }
        $was = if ($had) { [IO.File]::ReadAllBytes($cfg) } else { [byte[]]@() }
        $new = Join-Path $staging 'ssh-config.new'
        [IO.File]::WriteAllBytes($new, [byte[]]($was + [Text.Encoding]::UTF8.GetBytes($sep + $p.block)))
        $tmp = "$cfg.lords-panel-$($p.run)"
        Note "made`t$tmp"
        Copy-Item -LiteralPath $new -Destination $tmp
        if ($had) {
            $copy = Join-Path $staging 'ssh-config.before'
            Copy-Item -LiteralPath $cfg -Destination $copy
            Note "replaced`t$cfg`t$copy`t$new"
            [IO.File]::Replace($tmp, $cfg, [NullString]::Value)
        } else {
            Note "made`t$cfg`t$new"
            [IO.File]::Move($tmp, $cfg)
        }
        $done += "added Host $($p.alias) to $cfg"
    }

    # The window's files (a standalone client's alone): each one there backed up into backup-<run> before it is
    # replaced.
    if (-not $embedded) {
        $backups = Join-Path $app ('backup-' + $p.run)
        foreach ($f in 'lords-command.exe', 'README.md', 'LICENSE') {
            $to = Join-Path $app $f
            if (Test-Path -LiteralPath $to) {
                if (-not (Test-Path -LiteralPath $backups)) {
                    Note "made`t$backups"
                    New-Item -ItemType Directory -Path $backups | Out-Null
                }
                $copy = Join-Path $backups $f
                Note "replaced`t$to`t$copy"
                Copy-Item -LiteralPath $to -Destination $copy
                $done += "backed up $to into $backups"
            } else {
                Note "made`t$to"
            }
            Move-Item -Force -LiteralPath (Join-Path $staging $f) -Destination $to
            $done += "installed $to"
        }
    }

    # The Start-menu entry (a standalone client's alone).
    $lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\lords-command.lnk'
    if (-not $embedded -and -not (Test-Path -LiteralPath $lnk)) {
        Note "made`t$lnk"
        $s = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
        $s.TargetPath = Join-Path $app 'lords-command.exe'
        $s.WorkingDirectory = $app
        $s.Save()
        $done += "made $lnk"
    }

    # The client goes live last, so no state between two moves has config.txt without its token. The token's copy:
    # owner-only since its first byte (the staging dir's list), and given its own protected list (one entry: this
    # account, full control) before it takes its place, so it keeps that list beside config.txt.
    $staged = Join-Path $staging 'panel-token'
    $acl = New-Object System.Security.AccessControl.FileSecurity
    $acl.SetAccessRuleProtection($true, $false)
    $me = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
    $acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($me, 'FullControl', 'Allow')))
    Set-Acl -LiteralPath $staged -AclObject $acl
    $newConf = Join-Path $staging 'config.txt'
    if (-not $hadConf) { [IO.File]::WriteAllText($newConf, $p.config_txt) }
    if ($hadToken) {
        if (-not $hadConf) {
            Note "made`t$conf"
            [IO.File]::Move($newConf, $conf)
            $done += "wrote $conf"
        }
        # Replaced in one move; the replacement takes the old token's security, so it is given its own list again, the
        # list alone (a fresh one: Set-Acl here would write the descriptor's other parts, which needs a privilege).
        Note "final`tpanel-token"
        [IO.File]::Replace($staged, $token, [NullString]::Value)
        $own = New-Object System.Security.AccessControl.FileSecurity
        $own.SetAccessRuleProtection($true, $false)
        $own.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($me, 'FullControl', 'Allow')))
        [IO.File]::SetAccessControl($token, $own)
        $done += "placed $token (owner-only)"
    } else {
        if ($hadConf) { Note "final`tpanel-token" } else { Note "made`t$token" }
        [IO.File]::Move($staged, $token)
        $done += "placed $token (owner-only)"
        if (-not $hadConf) {
            Note "final`tconfig.txt"
            [IO.File]::Move($newConf, $conf)
            $done += "wrote $conf"
        }
    }
    Note 'done'
} catch {
    & (Join-Path $staging 'undo.ps1') | Out-Null
    throw
}

[ordered]@{ public = ([IO.File]::ReadAllText("$key.pub")).Trim(); done = $done } | ConvertTo-Json -Compress

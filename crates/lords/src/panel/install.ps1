# lords panel add, on the PC that gets the panel: installs it from this dir (incoming\, beside the app's dir) and adds
# its key and SSH alias, as panel.json beside this script says. Prints one line of JSON: the key's .pub and what it did.
# No private key leaves this PC: the key is made here and only its .pub is printed. Bounded: ssh-keygen has 20 s, and
# nothing else here waits on anything.
$ErrorActionPreference = 'Stop'
$incoming = $PSScriptRoot
$p = Get-Content -Raw -LiteralPath (Join-Path $incoming 'panel.json') | ConvertFrom-Json
$app = Split-Path -Parent $incoming
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
    if (-not $gen.WaitForExit(20000)) { $gen.Kill(); throw 'ssh-keygen took over 20 s' }
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

# The files: the exe replaced, config.txt written only where there is none.
foreach ($f in 'lords-command.exe', 'README.md', 'LICENSE') {
    Move-Item -Force -LiteralPath (Join-Path $incoming $f) -Destination (Join-Path $app $f)
    $done += "installed $(Join-Path $app $f)"
}
$conf = Join-Path $app 'config.txt'
if ($p.config_txt -and -not (Test-Path -LiteralPath $conf)) {
    [IO.File]::WriteAllText($conf, $p.config_txt)
    $done += "wrote $conf"
}

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

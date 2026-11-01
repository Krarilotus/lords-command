# lords panel add, the last step on the PC that got the panel: the route checked headless, through this run's own
# forward, and believed only on the panel server's proof (embedded panel review r1 R1). It runs ssh -N <alias>, waits
# until the forward's port is listened on by that ssh alone (who listens is read, and a reading that fails stops it),
# asks the server there for its proof over a fresh nonce, sending no token, and compares it with its own made from the
# panel's token here: only a server that holds the token can make it. Another listener on the port, that ssh ended
# before the answer, or an answer without the proof fails. It stops that ssh by its PID. It runs under its step's watchdog (panel add's runner), which
# ends it and what it starts at the step's deadline; its own waits are shorter. Prints one line of JSON. It removes
# nothing: this run's staging dir goes in a step of its own.
$ErrorActionPreference = 'Stop'
$p = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'panel.json') | ConvertFrom-Json
$app = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$token = ([IO.File]::ReadAllText((Join-Path $app 'panel-token'))).Trim()
$port = [int]$p.local_port
$until = (Get-Date).AddSeconds(25)
$ssh = Start-Process -FilePath 'ssh.exe' -WindowStyle Hidden -PassThru `
    -ArgumentList @('-N', '-o', 'BatchMode=yes', '-o', 'ExitOnForwardFailure=yes', '-o', 'ConnectTimeout=10', $p.alias)
try {
    while ($true) {
        if ($ssh.HasExited) {
            throw "ssh $($p.alias) ended (exit $($ssh.ExitCode)) before its forward to 127.0.0.1:$port answered"
        }
        $on = @(Get-NetTCPConnection -State Listen | Where-Object { $_.LocalPort -eq $port })
        $others = @($on | Where-Object { $_.OwningProcess -ne $ssh.Id })
        if ($others.Count) {
            throw "127.0.0.1:$port is held by PID $($others[0].OwningProcess), not this run's ssh $($p.alias)"
        }
        if ($on.Count) { break }
        if ((Get-Date) -ge $until) { throw "the forward to 127.0.0.1:$port didn't come up (ssh $($p.alias))" }
        Start-Sleep -Milliseconds 300
    }
    $bytes = New-Object byte[] 16
    [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    $nonce = -join ($bytes | ForEach-Object { $_.ToString('x2') })
    # No token goes with it: whatever answers there gets nothing to make the proof from (sol-loop S1 on cbc9b9b).
    $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 8 -Uri "http://127.0.0.1:$port/proof?nonce=$nonce"
    $mac = New-Object System.Security.Cryptography.HMACSHA256 (, [Text.Encoding]::ASCII.GetBytes($token))
    $want = -join ($mac.ComputeHash([Text.Encoding]::ASCII.GetBytes("lords-panel proof $nonce")) |
        ForEach-Object { $_.ToString('x2') })
    $proof = try { [string]($r.Content | ConvertFrom-Json).proof } catch { '' }
    if ($proof -cne $want) { throw "127.0.0.1:$port answered without the panel's proof: it isn't this PC's panel server" }
    if ($ssh.HasExited) { throw "ssh $($p.alias) ended before the answer was in" }
    [ordered]@{ ok = $true; status = $r.StatusCode } | ConvertTo-Json -Compress
} finally {
    if (-not $ssh.HasExited) { Stop-Process -Id $ssh.Id -Force }
}

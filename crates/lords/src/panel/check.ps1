# lords panel add, the last step on the PC that got the panel: the route checked headless, through the new forward. It
# runs ssh -N <alias>, sends one GET /status with the panel's token to the forward's port, and stops that ssh by its
# PID; where a forward of the same alias holds that port already, the GET goes through that one. It runs under its
# step's watchdog (panel add's runner), which ends it and what it starts at the step's deadline; its own waits are
# shorter. Prints one line of JSON. It removes nothing: this run's staging dir goes in a step of its own.
$ErrorActionPreference = 'Stop'
$p = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'panel.json') | ConvertFrom-Json
$app = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$token = ([IO.File]::ReadAllText((Join-Path $app 'panel-token'))).Trim()
$until = (Get-Date).AddSeconds(25)
$ssh = Start-Process -FilePath 'ssh.exe' -WindowStyle Hidden -PassThru `
    -ArgumentList @('-N', '-o', 'BatchMode=yes', '-o', 'ExitOnForwardFailure=yes', '-o', 'ConnectTimeout=10', $p.alias)
try {
    # The port is looked at before ssh's end counts: a window or an embedded view of the same alias may hold the
    # forward already, and then this ssh fails its own forward and ends, whenever this script next gets to run.
    $up = $false
    while (-not $up -and (Get-Date) -lt $until) {
        try {
            $t = [Net.Sockets.TcpClient]::new()
            $t.Connect('127.0.0.1', [int]$p.local_port)
            $t.Close()
            $up = $true
        } catch {
            if ($ssh.HasExited) { break }
            Start-Sleep -Milliseconds 300
        }
    }
    if (-not $up) { throw "the forward to 127.0.0.1:$($p.local_port) didn't come up (ssh $($p.alias))" }
    $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 8 -Headers @{ 'Lords-Panel-Token' = $token } `
        -Uri "http://127.0.0.1:$($p.local_port)/status"
    [ordered]@{ ok = ($r.StatusCode -eq 200); status = $r.StatusCode } | ConvertTo-Json -Compress
} finally {
    if (-not $ssh.HasExited) { Stop-Process -Id $ssh.Id -Force }
}

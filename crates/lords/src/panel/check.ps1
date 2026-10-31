# lords panel add, last step on the PC that got the panel: the route checked headless, through the new forward. It
# runs ssh -N <alias> for at most 30 s, sends one GET /status to the forward's port, stops that ssh by its PID, and
# removes the files panel add put beside it. Prints one line of JSON. Stops itself after 40 s.
$ErrorActionPreference = 'Stop'
$deadline = (Get-Date).AddSeconds(40)
$p = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'panel.json') | ConvertFrom-Json
$ssh = Start-Process -FilePath 'ssh.exe' -WindowStyle Hidden -PassThru `
    -ArgumentList @('-N', '-o', 'BatchMode=yes', '-o', 'ExitOnForwardFailure=yes', '-o', 'ConnectTimeout=10', $p.alias)
try {
    $up = $false
    while (-not $up -and -not $ssh.HasExited -and (Get-Date) -lt $deadline.AddSeconds(-10)) {
        try {
            $t = [Net.Sockets.TcpClient]::new()
            $t.Connect('127.0.0.1', [int]$p.local_port)
            $t.Close()
            $up = $true
        } catch { Start-Sleep -Milliseconds 300 }
    }
    if (-not $up) { throw "the forward to 127.0.0.1:$($p.local_port) didn't come up (ssh $($p.alias))" }
    $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 8 -Uri "http://127.0.0.1:$($p.local_port)/status"
    [ordered]@{ ok = ($r.StatusCode -eq 200); status = $r.StatusCode } | ConvertTo-Json -Compress
} finally {
    if (-not $ssh.HasExited) { Stop-Process -Id $ssh.Id -Force }
    foreach ($f in 'panel.json', 'install.ps1', 'check.ps1') {
        Remove-Item -LiteralPath (Join-Path $PSScriptRoot $f) -ErrorAction SilentlyContinue
    }
}

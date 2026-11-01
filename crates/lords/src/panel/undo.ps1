# lords panel add, on the PC that gets the panel: this run's install undone from its journal (embedded panel review r1
# R2). install.ps1 writes a line there before each change it makes; this reads them back. If the install's last move
# happened (the source its `final` line names is gone from this staging dir), the install is complete and nothing is
# undone. Else each change, last first: what it made is removed (a dir only while empty), what it replaced is moved
# back from its copy; a line that ends with what this run wrote there (the ssh config) is undone only while the file
# holds exactly that, so another's change stays. A change whose line was written but which wasn't made finds nothing
# to undo. It ends the journal with `done` or `undone`; run again, it does nothing more. install.ps1 runs it on an
# error; panel add runs it as a step of its own after a failed install (a watchdog's kill runs no catch), and for an
# earlier run's staging dir. Prints one line of JSON: the state, and what it undid.
$ErrorActionPreference = 'Stop'
$staging = $PSScriptRoot
$journal = Join-Path $staging 'journal'
$lines = @(Get-Content -LiteralPath $journal -Encoding UTF8 | Where-Object { $_ })
function Said($state, $undone) { [ordered]@{ state = $state; undone = @($undone) } | ConvertTo-Json -Compress }
function Holds($path, $what) {
    [Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) -ceq [Convert]::ToBase64String([IO.File]::ReadAllBytes($what))
}
if ($lines -contains 'done') { Said 'complete' @(); return }
if ($lines -contains 'undone') { Said 'undone' @(); return }
$final = @($lines | Where-Object { $_.StartsWith("final`t") }) | Select-Object -Last 1
if ($final -and -not (Test-Path -LiteralPath (Join-Path $staging $final.Split("`t")[1]))) {
    Add-Content -LiteralPath $journal -Value 'done' -Encoding UTF8
    Said 'complete' @()
    return
}
$undone = @()
for ($i = $lines.Count - 1; $i -ge 0; $i--) {
    $f = $lines[$i].Split("`t")
    if ($f[0] -eq 'made' -and (Test-Path -LiteralPath $f[1])) {
        if ((Get-Item -Force -LiteralPath $f[1]).PSIsContainer) {
            if (-not @(Get-ChildItem -Force -LiteralPath $f[1]).Count) {
                Remove-Item -Force -LiteralPath $f[1]
                $undone += "removed $($f[1])"
            }
        } elseif ($f.Count -lt 3 -or (Holds $f[1] $f[2])) {
            Remove-Item -Force -LiteralPath $f[1]
            $undone += "removed $($f[1])"
        } else {
            $undone += "left $($f[1]) as it is: it doesn't hold what this run wrote"
        }
    } elseif ($f[0] -eq 'replaced' -and (Test-Path -LiteralPath $f[2])) {
        $there = Test-Path -LiteralPath $f[1]
        if ($f.Count -ge 4 -and $there -and -not (Holds $f[1] $f[3])) {
            $undone += "left $($f[1]) as it is: it doesn't hold what this run wrote"
        } elseif ($there) {
            [IO.File]::Replace($f[2], $f[1], [NullString]::Value)
            $undone += "restored $($f[1])"
        } else {
            [IO.File]::Move($f[2], $f[1])
            $undone += "restored $($f[1])"
        }
    }
}
Add-Content -LiteralPath $journal -Value 'undone' -Encoding UTF8
Said 'undone' $undone

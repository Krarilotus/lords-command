# Build with remapped local paths and package only explicitly listed public files.
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $projectRoot
$previousFlags = $env:CARGO_ENCODED_RUSTFLAGS
try {
    $name = 'lords-command'
    $flags = @('--remap-path-prefix=' + $projectRoot + '=' + $name)
    if ($env:USERPROFILE) { $flags += '--remap-path-prefix=' + $env:USERPROFILE + '=build-home' }
    if ($previousFlags) { $flags = @($previousFlags.Split([char]31)) + $flags }
    $env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]31
    cargo build --release --locked -p lords -p lords-command
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    $versionLine = Get-Content -LiteralPath Cargo.toml | Where-Object { $_ -match '^version\s*=' } | Select-Object -First 1
    $version = [regex]::Match($versionLine, '"([^"]+)"').Groups[1].Value
    if (-not $version) { throw 'Cannot read the workspace version' }
    $dist = Join-Path $projectRoot 'dist'
    New-Item -ItemType Directory -Path $dist -Force | Out-Null
    $archive = Join-Path $dist "$name-$version-windows-x64.zip"
    if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive }
    # No directory traversal and no config auto-discovery: config.txt is never packaged.
    Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
    $publicFiles = [ordered]@{
        'lords-command.exe' = 'target/release/lords-command.exe'
        'lords.exe' = 'target/release/lords.exe'
        'config.example.txt' = 'config.example.txt'
        'README.md' = 'README.md'
        'LICENSE' = 'LICENSE'
    }
    $zip = [IO.Compression.ZipArchive]::new([IO.File]::Create($archive), [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($entry in $publicFiles.GetEnumerator()) {
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, (Join-Path $projectRoot $entry.Value), $entry.Key) | Out-Null
        }
    } finally { $zip.Dispose() }
    Get-FileHash -LiteralPath $archive -Algorithm SHA256 | Format-List
} finally {
    $env:CARGO_ENCODED_RUSTFLAGS = $previousFlags
    Pop-Location
}

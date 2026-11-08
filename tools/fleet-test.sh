#!/bin/sh
# The full check as fleet jobs (kind `lords.test`), so it never runs on a developer's PC by hand: rustfmt, clippy with
# `-D warnings`, and every workspace test, on this checkout's committed HEAD, on Windows and on Linux (D27). Uncommitted
# changes aren't in it. `--os windows` or `--os linux` submits one. Prints each job's id. Then `fleet wait <id>` as a
# background task, and `fleet results <id> --to DIR`.
# FLEET_OWNER is your own thread (`t3:<id>`); in a T3 session it defaults to that session's thread. FLEET names the
# fleet CLI where `fleet` isn't on PATH.
set -e
fleet=${FLEET:-fleet}
os=both
case "$1" in
    "") ;;
    --os)
        case "$2" in
            windows|linux) os=$2 ;;
            *) echo "fleet-test: --os windows or --os linux" >&2; exit 2 ;;
        esac
        ;;
    *) echo "usage: fleet-test.sh [--os windows|linux]" >&2; exit 2 ;;
esac
top=$(git rev-parse --show-toplevel)
rev=$(git rev-parse HEAD)
short=$(git rev-parse --short=8 HEAD)
owner=${FLEET_OWNER:-${T3CODE_THREAD_ID:+t3:$T3CODE_THREAD_ID}}
if [ -z "$owner" ]; then
    echo "fleet-test: set FLEET_OWNER to your own thread (t3:<id>)" >&2
    exit 2
fi
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "fleet-test: uncommitted changes aren't tested; the job tests $short" >&2
fi
# Each run its own job ids and files (commit, seconds, this shell's pid), so two runs never share either.
stamp="$(date +%s)-$$"
# The path as a TOML basic string: a quote or backslash in it can't end the string.
top_toml=$(printf '%s' "$top" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')
# One step per job: each check's log and exit code go to the job's results; the job fails if any of them fails.
# The target dir is in the job's own directory, so concurrent jobs never share one, and beside the checkout, not in
# it: `check --public` reads every file git doesn't ignore, and a target dir inside would be 4 GB of it. The job also
# has its own temp root there, fully resolved, so a host whose %TEMP% is an 8.3 short name doesn't change test paths.
windows_step='["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", '"'''"'$env:CARGO_TARGET_DIR = "$PWD\..\target-job"; $env:TEMP = (New-Item -ItemType Directory -Force -Path "$PWD\..\tmp-job").FullName; $env:TMP = $env:TEMP; $env:CARGO_BUILD_JOBS = '"'"'4'"'"'; $env:RUST_TEST_THREADS = '"'"'4'"'"'; $ErrorActionPreference = '"'"'Continue'"'"'; $cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"; @((git rev-parse HEAD), (git status --porcelain | Measure-Object).Count, (git --version), (& $cargo --version), (& "$env:USERPROFILE\.cargo\bin\rustc.exe" -vV | Select-Object -First 1), $env:COMPUTERNAME) | Set-Content -Path "$env:FLEET_OUT\toolchain.txt"; & $cargo fmt --all --check 2>&1 | ForEach-Object { "$_" } | Out-File -Encoding utf8 -FilePath "$env:FLEET_OUT\fmt.log"; $f = $LASTEXITCODE; & $cargo clippy --locked --workspace --all-targets -- -D warnings 2>&1 | ForEach-Object { "$_" } | Out-File -Encoding utf8 -FilePath "$env:FLEET_OUT\clippy.log"; $c = $LASTEXITCODE; & $cargo test --locked --workspace --no-fail-fast 2>&1 | ForEach-Object { "$_" } | Out-File -Encoding utf8 -FilePath "$env:FLEET_OUT\test.log"; $t = $LASTEXITCODE; Set-Content -Path "$env:FLEET_OUT\codes.txt" -Value ("fmt " + $f + "; clippy " + $c + "; test " + $t); if ($f -ne 0 -or $c -ne 0 -or $t -ne 0) { exit 1 }; exit 0'"'''"']'
linux_step='["bash", "-c", "export PATH=$HOME/.cargo/bin:$PATH CARGO_TARGET_DIR=$PWD/../target-job CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=4; command -v cargo > /dev/null || exit 3; mkdir -p $PWD/../tmp-job; export TMPDIR=$(cd $PWD/../tmp-job && pwd); { git rev-parse HEAD; git status --porcelain | wc -l; git --version; cargo --version; rustc -vV | head -1; uname -srm; hostname; } > $FLEET_OUT/toolchain.txt 2>&1; cargo fmt --all --check > $FLEET_OUT/fmt.log 2>&1; f=$?; cargo clippy --locked --workspace --all-targets -- -D warnings > $FLEET_OUT/clippy.log 2>&1; c=$?; cargo test --locked --workspace --no-fail-fast > $FLEET_OUT/test.log 2>&1; t=$?; echo \"fmt $f; clippy $c; test $t\" > $FLEET_OUT/codes.txt; [ $f -eq 0 ] && [ $c -eq 0 ] && [ $t -eq 0 ]"]'
submit() {
    name=$1 target=$2 step=$3
    job=$(git rev-parse --git-path "lords-fleet-test-$name-$short-$stamp.toml")
    {
        printf 'id = "lords-test-%s%s-%s"\n' "$4" "$short" "$stamp"
        printf 'group = "lords"\nkind = "lords.test"\npriority = 650\n'
        printf 'priority_reason = "Lords Command'"'"'s full check of %s on %s: rustfmt, clippy -D warnings, workspace tests"\n' "$short" "$name"
        printf 'est_min = 15\nsteps = [%s]\n' "$step"
        printf '[requires]\nos = "%s"\nhas = ["rust"]\ncores = 4\nram_gb = 4\n' "$target"
        printf '[[inputs]]\nname = "checkout"\nsource = { git = "%s", rev = "%s" }\n' "$top_toml" "$rev"
    } > "$job"
    FLEET_OWNER=$owner "$fleet" submit "$job"
    rm -f -- "$job"
}
if [ "$os" != linux ]; then submit windows windows "$windows_step" ""; fi
if [ "$os" != windows ]; then submit linux linux "$linux_step" "linux-"; fi

#!/bin/sh
# The full check as one fleet job (kind `lords.test`), so it never runs on a developer's PC by hand: rustfmt, clippy
# with `-D warnings`, and every workspace test, on this checkout's committed HEAD. Uncommitted changes aren't in it.
# Prints the job's id. Then `fleet wait <id>` as a background task, and `fleet results <id> --to DIR`.
# FLEET_OWNER is your own thread (`t3:<id>`); in a T3 session it defaults to that session's thread. FLEET names the
# fleet CLI where `fleet` isn't on PATH.
set -e
fleet=${FLEET:-fleet}
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
job=$(git rev-parse --git-path "lords-fleet-test-$short.toml")
# One step: each check's log and exit code go to the job's results; the job fails if any of them fails.
# The target dir is inside the job's own directory, so concurrent jobs never share one.
cat > "$job" <<EOF
id = "lords-test-$short-$(date +%s)"
group = "lords"
kind = "lords.test"
priority = 650
priority_reason = "Lords Command's full check of $short: rustfmt, clippy -D warnings, workspace tests"
est_min = 10
steps = [["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", '''\$env:CARGO_TARGET_DIR = "\$PWD\\target-job"; \$env:CARGO_BUILD_JOBS = '4'; \$env:RUST_TEST_THREADS = '4'; \$ErrorActionPreference = 'Continue'; \$cargo = "\$env:USERPROFILE\\.cargo\\bin\\cargo.exe"; @((git rev-parse HEAD), (git status --porcelain | Measure-Object).Count, (& \$cargo --version), (& "\$env:USERPROFILE\\.cargo\\bin\\rustc.exe" -vV | Select-Object -First 1), \$env:COMPUTERNAME) | Set-Content -Path "\$env:FLEET_OUT\\toolchain.txt"; & \$cargo fmt --all --check 2>&1 | ForEach-Object { "\$_" } | Out-File -Encoding utf8 -FilePath "\$env:FLEET_OUT\\fmt.log"; \$f = \$LASTEXITCODE; & \$cargo clippy --locked --workspace --all-targets -- -D warnings 2>&1 | ForEach-Object { "\$_" } | Out-File -Encoding utf8 -FilePath "\$env:FLEET_OUT\\clippy.log"; \$c = \$LASTEXITCODE; & \$cargo test --locked --workspace --no-fail-fast 2>&1 | ForEach-Object { "\$_" } | Out-File -Encoding utf8 -FilePath "\$env:FLEET_OUT\\test.log"; \$t = \$LASTEXITCODE; Set-Content -Path "\$env:FLEET_OUT\\codes.txt" -Value ("fmt " + \$f + "; clippy " + \$c + "; test " + \$t); if (\$f -ne 0 -or \$c -ne 0 -or \$t -ne 0) { exit 1 }; exit 0''']]
[requires]
os = "windows"
has = ["rust"]
cores = 4
ram_gb = 4
[[inputs]]
name = "checkout"
source = { git = '$top', rev = "$rev" }
EOF
FLEET_OWNER=$owner "$fleet" submit "$job"

#!/bin/sh
# Lords Command's commit check: the pre-commit hook `lords hooks install` writes runs it before the privacy check.
# Clippy with `-D warnings` carries the filesystem and process boundary (clippy.toml; Sol review r5, R5-03), so a
# commit that breaks it doesn't land. Run from anywhere in the repo.
set -e
cd "$(git rev-parse --show-toplevel)"
cargo fmt --all --check
cargo clippy -j 4 --workspace --all-targets -- -D warnings

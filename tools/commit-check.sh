#!/bin/sh
# Lords Command's commit check: the pre-commit hook `lords hooks install` writes runs it before the privacy check.
# Clippy with `-D warnings` carries the filesystem and process boundary (clippy.toml; Sol review r5, R5-03), so a
# commit that breaks it doesn't land. The text lint runs too: it refuses whatever would switch clippy's check off
# (Sol review r6, R6-03). Run from anywhere in the repo.
set -e
cd "$(git rev-parse --show-toplevel)"
cargo fmt --all --check
cargo clippy -j 4 --workspace --all-targets -- -D warnings
cargo test -j 4 -q -p lords-core --test fsguard_lint

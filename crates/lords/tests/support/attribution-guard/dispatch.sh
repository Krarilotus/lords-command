#!/bin/sh
# Invoked by a hook wrapper; keep the original working directory and environment.
hook=$1
shift
guard_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
if ! common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null); then
    # A repository still being created (newer Git runs hooks during init/clone) has no hook of its own
    # yet, so pass through as Git would. Message and push checks fail closed.
    case "$hook" in
        prepare-commit-msg|commit-msg|pre-push)
            echo 'attribution-guard: cannot locate the repository; refusing' >&2
            exit 1 ;;
        *) exit 0 ;;
    esac
fi
hook_dir=$common/hooks
# Optional scoped migration: parent records the ORIGINAL core.hooksPath here
# before replacing that scoped value. Relative paths retain Git's hook cwd.
previous=$(git config --path --get attributionGuard.previousHooksPath)
result=$?
case "$result" in
    0)
        if [ -z "$previous" ]; then
            echo 'attribution-guard: empty previousHooksPath; review scoped config' >&2
            exit 1
        fi
        hook_dir=$previous
        ;;
    1) ;;
    *) echo 'attribution-guard: cannot read previousHooksPath' >&2; exit "$result" ;;
esac
repo_hook=$hook_dir/$hook
# attributionGuard.skipPreviousHook (multi-valued) names delegated hooks not to run, e.g. an old
# commit-msg sed rule that strips human co-authors before the guard can see them.
if git config --get-all attributionGuard.skipPreviousHook 2>/dev/null | grep -qx -- "$hook"; then
    repo_hook=
fi
# Never alter core.hooksPath, including for the repo hook's child Git processes.
# The hook's location is looked up directly, avoiding Git hook/config recursion.
if [ -e "$repo_hook" ] && [ "$repo_hook" -ef "$guard_dir/hooks/$hook" ]; then
    echo 'attribution-guard: default hook points back to dispatcher; refusing recursion' >&2
    exit 1
fi
# Fast path (README "Fast path"): the hooks dir has no delegator for these names, so Git skips a repository's own
# hook for one of them. Every other hook call reports such a hook: blocking names refuse, post-* names (whose exit
# status can't stop Git) warn. Operations that reach no other hook (`git branch`, `git add`) stay unreported until
# the next one; the installer's doctor check refuses a machine with such a repository. A name whose delegator was
# restored (build_hooks.py --with), or that skipPreviousHook names, isn't reported. File tests only, unless a hook
# is found.
for dropped in reference-transaction post-index-change; do
    [ -e "$guard_dir/hooks/$dropped" ] && continue
    [ -x "$hook_dir/$dropped" ] || continue
    git config --get-all attributionGuard.skipPreviousHook 2>/dev/null | grep -qx -- "$dropped" && continue
    echo "attribution-guard: this repository has its own $dropped hook ($hook_dir/$dropped), but the global hooks dir has no delegator for $dropped (fast path), so Git skips it. Restore it on this machine (python build_hooks.py --with $dropped in the installed guard dir), or remove that hook." >&2
    case "$hook" in
        post-*|p4-post-changelist|fsmonitor-watchman) ;;
        *) exit 1 ;;
    esac
done
run_guard() {
    # First candidate that really runs Python 3: on Windows `python3` can be the Microsoft Store
    # stub, which exists on PATH but fails. The probe reads /dev/null, keeping pre-push stdin intact.
    for py in /c/Python313/python.exe python3 python; do
        if command -v "$py" >/dev/null 2>&1 && "$py" -c 'import sys; sys.exit(sys.version_info[0] != 3)' </dev/null >/dev/null 2>&1; then
            "$py" "$guard_dir/guard.py" "$@"
            return $?
        fi
    done
    echo 'attribution-guard: Python 3 required; install Python or restore previous hooksPath' >&2
    return 1
}
case "$hook" in
    prepare-commit-msg|commit-msg)
        if [ -x "$repo_hook" ]; then
            "$repo_hook" "$@"
            result=$?
            [ "$result" -eq 0 ] || exit "$result"
        fi
        run_guard scrub "$1"
        exit $?
        ;;
    pre-push)
        spool=$(mktemp "${TMPDIR:-/tmp}/attribution-guard.XXXXXXXX") || exit 1
        trap 'rm -f -- "$spool"' EXIT
        trap 'exit 1' HUP INT TERM
        cat >"$spool" || exit 1
        if [ -x "$repo_hook" ]; then
            "$repo_hook" "$@" <"$spool"
            result=$?
            [ "$result" -eq 0 ] || exit "$result"
        fi
        # $2 is the destination URL: the guard asks it what it already has.
        run_guard pre-push "$2" <"$spool"
        exit $?
        ;;
    *)
        if [ -x "$repo_hook" ]; then
            exec "$repo_hook" "$@"
        fi
        exit 0
        ;;
esac

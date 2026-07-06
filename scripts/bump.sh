#!/usr/bin/env bash
#
# bump.sh — bump the crate version and commit on the current branch.
#
# Usage:
#   bump.sh patch                # X.Y.Z → X.Y.(Z+1)
#   bump.sh minor                # X.Y.Z → X.(Y+1).0
#   bump.sh major                # X.Y.Z → (X+1).0.0
#   bump.sh 0.2.0                # explicit version (any SemVer)
#   bump.sh 1.0.0-rc.1           # explicit version with pre-release suffix
#
#   bump.sh --edit-only <bump>   # rewrite files only, no commit
#
# Edits `[package].version` in `Cargo.toml`, refreshes `Cargo.lock`,
# and commits the result on whatever branch you're currently on. No
# fetch, no push — `scripts/release-macos.sh` does the tag-and-push.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# On Windows (WSL) the cargo on PATH is often cargo.exe. Prefer plain
# cargo, fall back to cargo.exe, and fail loudly if neither is present.
if command -v cargo >/dev/null 2>&1; then
    CARGO=cargo
elif command -v cargo.exe >/dev/null 2>&1; then
    CARGO=cargo.exe
else
    echo "Error: cargo not found on PATH (looked for 'cargo' and 'cargo.exe')" >&2
    exit 1
fi

EDIT_ONLY=0
BUMP=""
for arg in "$@"; do
    case "$arg" in
        --edit-only) EDIT_ONLY=1 ;;
        -h|--help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        -*)
            echo "Error: unknown flag $arg" >&2
            exit 1
            ;;
        *)
            if [[ -n "$BUMP" ]]; then
                echo "Error: unexpected extra argument $arg" >&2
                exit 1
            fi
            BUMP="$arg"
            ;;
    esac
done

if [[ -z "$BUMP" ]]; then
    echo "Usage: bump.sh [--edit-only] patch | minor | major | <X.Y.Z>" >&2
    exit 1
fi

# Read current version + compute new -----------------------------------------
#
# `[package].version` is the crate's version. This is a single-crate
# repo, so there's just the one field to move.

echo "→ reading current version"
CURRENT="$(awk -F\" '
    /^\[package\]/ { p = 1 }
    p && /^version = / { print $2; exit }
' Cargo.toml)"

if [[ -z "$CURRENT" ]]; then
    echo "Error: could not read [package].version" >&2
    exit 1
fi

case "$BUMP" in
    patch|minor|major)
        # Strip pre-release suffix (e.g., -rc.1) before SemVer math.
        BASE="${CURRENT%%-*}"
        IFS=. read -r MAJOR MINOR PATCH <<< "$BASE"
        case "$BUMP" in
            patch) NEW="$MAJOR.$MINOR.$((PATCH + 1))" ;;
            minor) NEW="$MAJOR.$((MINOR + 1)).0" ;;
            major) NEW="$((MAJOR + 1)).0.0" ;;
        esac
        ;;
    *)
        # Explicit version — accept any SemVer string verbatim
        # (including pre-release suffixes like 1.0.0-rc.1).
        NEW="$BUMP"
        ;;
esac

echo
echo "Bumping $CURRENT → $NEW"
echo

# Edit Cargo.toml -------------------------------------------------------------

# Portable in-place sed (BSD on macOS uses `-i ''`, GNU on Linux uses `-i`).
sed_inplace() {
    if [[ "$(uname)" == "Darwin" ]]; then
        sed -i '' "$@"
    else
        sed -i "$@"
    fi
}

# Rewrite the version line under `[package]` only. A blunt global
# s/"$CURRENT"/"$NEW"/ would also catch a `truce = { version = "..." }`
# line in `[dependencies]` if it ever happened to share the crate's
# version number — a different axis (truce framework version vs. our
# release version) that must not be touched here. Anchor to the
# [package] section instead.
echo "→ editing Cargo.toml"
sed_inplace "/^\[package\]/,/^\[/ s/^version = \"$CURRENT\"/version = \"$NEW\"/" Cargo.toml

# Sanity check: confirm the edit landed.
POST="$(awk -F\" '
    /^\[package\]/ { p = 1 }
    p && /^version = / { print $2; exit }
' Cargo.toml)"
if [[ "$POST" != "$NEW" ]]; then
    echo "Error: post-edit version is '$POST', expected '$NEW' (sed did not rewrite [package].version)" >&2
    exit 1
fi

# Refresh Cargo.lock ----------------------------------------------------------

echo "→ refreshing Cargo.lock ($CARGO check)"
"$CARGO" check

# Commit ----------------------------------------------------------------------

if (( EDIT_ONLY )); then
    echo
    echo "Edited Cargo.toml + Cargo.lock for v$NEW. No commit made."
    exit 0
fi

echo "→ committing on $(git rev-parse --abbrev-ref HEAD)"
git add Cargo.toml Cargo.lock
git commit -m "Release v$NEW"

echo
echo "Bump committed. Push, then run scripts/release-macos.sh:"
echo "  git push origin $(git rev-parse --abbrev-ref HEAD)"
echo "  ./scripts/release-macos.sh"

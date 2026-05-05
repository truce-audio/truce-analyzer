#!/usr/bin/env bash
# Release driver for the Windows half of a truce-analyzer release,
# run from inside WSL. Calls native `cargo.exe` (not the WSL Linux
# cargo) so the produced installer is a real Windows `.exe`, then
# attaches it to the GitHub release created by
# `scripts/release-macos.sh`. Run after the macOS half so the tag
# and release already exist.
#
# Requires: cargo.exe on PATH (install Rust on the Windows side via
# https://rustup.rs), gh, and an authenticated GitHub CLI session.
# The repo must be checked out somewhere `cargo.exe` can read it —
# in practice that means a Windows-side path like `/mnt/c/...`,
# since cargo.exe can't build out of the WSL filesystem reliably.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# --- parse versions from Cargo.toml --------------------------------------

pkg_version=$(awk -F\" '/^version[[:space:]]*=/ { print $2; exit }' Cargo.toml)
truce_tag=$(sed -n 's/^truce[[:space:]]\{1,\}=.*tag[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -1)

if [[ -z "$pkg_version" || -z "$truce_tag" ]]; then
    echo "could not parse package version or truce tag from Cargo.toml" >&2
    exit 1
fi

release_tag="v$pkg_version"
echo "==> release tag: $release_tag (truce $truce_tag)"

# --- preflight -----------------------------------------------------------

if ! command -v cargo.exe >/dev/null; then
    echo "cargo.exe not on PATH — install Rust on the Windows side (https://rustup.rs)" >&2
    exit 1
fi
if ! command -v gh >/dev/null; then
    echo "gh CLI not installed (https://cli.github.com)" >&2
    exit 1
fi
gh auth status >/dev/null

git fetch --tags origin

if ! git rev-parse "refs/tags/$release_tag" >/dev/null 2>&1; then
    echo "tag $release_tag does not exist locally or upstream — run scripts/release-macos.sh first" >&2
    exit 1
fi
if ! gh release view "$release_tag" >/dev/null 2>&1; then
    echo "release $release_tag does not exist on GitHub — run scripts/release-macos.sh first" >&2
    exit 1
fi

# --- install cargo-truce (Windows toolchain) -----------------------------

# `cargo.exe install` writes to the Windows-side ~/.cargo/bin and
# `cargo.exe truce package` resolves the subcommand from there, so
# WSL's own cargo / cargo-truce (if any) is irrelevant.
echo "==> installing cargo-truce@$truce_tag via cargo.exe"
cargo.exe install cargo-truce \
    --git https://github.com/truce-audio/truce \
    --tag "$truce_tag" --locked

# --- build installer -----------------------------------------------------

mkdir -p target/dist
rm -f target/dist/*.exe

echo "==> packaging via cargo.exe"
cargo.exe truce package

exe_path=$(ls -1 target/dist/*.exe 2>/dev/null | head -1)
if [[ -z "${exe_path:-}" ]]; then
    echo "no .exe produced under target/dist/" >&2
    exit 1
fi
echo "==> built $exe_path"

# --- upload --------------------------------------------------------------

extras=()
for f in screenshots/analyzer_spectrum_windows.png screenshots/analyzer_diff_windows.png; do
    [[ -f "$f" ]] && extras+=("$f")
done

echo "==> uploading $(basename "$exe_path") to release $release_tag"
gh release upload "$release_tag" "$exe_path" "${extras[@]}" --clobber

echo
echo "==> Windows release done."

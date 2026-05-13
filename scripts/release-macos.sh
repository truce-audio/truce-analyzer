#!/usr/bin/env bash
# Release driver for the macOS half of a truce-analyzer release.
#
# Tags HEAD as v<package-version> (read from Cargo.toml), builds the
# signed `.pkg` installer, and creates / updates the matching GitHub
# release. Run this first; then run `scripts/release-windows.sh` from
# WSL on a Windows machine to attach the matching `.exe`.
#
# Requires: gh, cargo, and an authenticated GitHub CLI session. The
# script installs / upgrades cargo-truce to the tag pinned by
# `truce` in Cargo.toml. Notarization runs automatically when a
# `TRUCE_NOTARY` keychain profile is configured (see
# `xcrun notarytool store-credentials`); without it, the build falls
# back to `--no-notarize` and produces an installer that is signed
# but not notarized.

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

#if [[ -n "$(git status --porcelain)" ]]; then
#    echo "working tree is dirty — commit or stash first" >&2
#    exit 1
#fi

branch=$(git rev-parse --abbrev-ref HEAD)
if [[ "$branch" != "main" ]]; then
    echo "not on main (currently $branch) — refuse to release from a side branch" >&2
    exit 1
fi

git fetch --tags origin
if ! git diff --quiet "HEAD" "origin/$branch"; then
    echo "local main diverges from origin/$branch — push or pull first" >&2
    exit 1
fi

if ! command -v gh >/dev/null; then
    echo "gh CLI not installed (https://cli.github.com)" >&2
    exit 1
fi
gh auth status >/dev/null

# --- tag + push ----------------------------------------------------------

if git rev-parse "refs/tags/$release_tag" >/dev/null 2>&1; then
    echo "==> tag $release_tag already exists, skipping git tag"
else
    echo "==> tagging $release_tag"
    git tag -a "$release_tag" -m "$release_tag"
    git push origin "$release_tag"
fi

# --- install cargo-truce -------------------------------------------------

echo "==> installing cargo-truce@$truce_tag"
# `--force` so an already-installed `cargo-truce` (from a previous
# release run or local dev work) is replaced rather than silently
# kept at the old version. `cargo install` is a no-op when the
# binary already exists, regardless of the requested version.
cargo install cargo-truce --git https://github.com/truce-audio/truce \
    --tag "$truce_tag" --locked --force

# --- build installer -----------------------------------------------------

mkdir -p target/dist
rm -f target/dist/*.pkg

# `--formats` enumerates the release targets explicitly so AU v2
# gets built even though `au` is intentionally out of
# `[features].default` (keeping AU v3 out of unflagged installs).
# AU v3 / AAX stay outside the release pipeline until their
# signing setups are added.
formats="clap,vst3,au2,standalone"
if xcrun notarytool history --keychain-profile "TRUCE_NOTARY" >/dev/null 2>&1; then
    echo "==> packaging with notarization (formats: $formats)"
    cargo truce package --formats "$formats"
else
    echo "==> packaging without notarization (no TRUCE_NOTARY keychain profile; formats: $formats)"
    cargo truce package --no-notarize --formats "$formats"
fi

pkg_path=$(ls -1 target/dist/*.pkg 2>/dev/null | head -1)
if [[ -z "${pkg_path:-}" ]]; then
    echo "no .pkg produced under target/dist/" >&2
    exit 1
fi
echo "==> built $pkg_path"

# --- create or update release --------------------------------------------

prev_tag=$(git describe --tags --abbrev=0 "${release_tag}^" 2>/dev/null || true)
notes_file=$(mktemp)
trap 'rm -f "$notes_file"' EXIT

{
    echo "# $release_tag"
    echo
    echo "## Changes"
    echo
    if [[ -n "$prev_tag" ]]; then
        git log "$prev_tag..$release_tag" --pretty=format:"- %s"
    else
        git log --pretty=format:"- %s"
    fi
    echo
    echo
    echo "## Installers"
    echo
    echo "- macOS: \`$(basename "$pkg_path")\`"
    echo "- Windows: attached separately via \`scripts/release-windows.sh\` (WSL)"
} > "$notes_file"

# Optional extras — attach screenshots if the per-OS bake has run.
extras=()
for f in screenshots/analyzer_spectrum_macos.png screenshots/analyzer_diff_macos.png; do
    [[ -f "$f" ]] && extras+=("$f")
done

if gh release view "$release_tag" >/dev/null 2>&1; then
    echo "==> release $release_tag already exists — uploading .pkg"
    gh release upload "$release_tag" "$pkg_path" "${extras[@]}" --clobber
else
    echo "==> creating release $release_tag"
    gh release create "$release_tag" "$pkg_path" "${extras[@]}" \
        --title "$release_tag" \
        --notes-file "$notes_file"
fi

echo
echo "==> macOS release done."
echo "    Next, on a Windows machine (inside WSL):"
echo "      git fetch --tags origin"
echo "      ./scripts/release-windows.sh"
echo "    And on a Linux machine:"
echo "      git fetch --tags origin"
echo "      ./scripts/release-linux.sh"

#!/usr/bin/env bash
# Release driver for the Linux half of a truce-analyzer release.
# Builds the `.tar.gz` installer via `cargo truce package` and
# attaches it to the GitHub release created by
# `scripts/release-macos.sh`. Run after the macOS half so the tag
# and release already exist.
#
# Requires: cargo, gh, and an authenticated GitHub CLI session,
# plus the system libraries the framework links against (audio,
# windowing, GPU). On Debian/Ubuntu these are:
#   build-essential pkg-config
#   libx11-dev libx11-xcb-dev libxcb1-dev libxcb-dri2-0-dev
#   libxcb-icccm4-dev libxcursor-dev libxkbcommon-dev
#   libxkbcommon-x11-dev libxrandr-dev libgl1-mesa-dev libvulkan-dev
#   mesa-vulkan-drivers libasound2-dev libjack-jackd2-dev
#   libfontconfig1-dev libfreetype-dev
# No signing — Linux ships an unsigned tarball with an `install.sh`
# that drops the plugins into the user's plugin directories.

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

if ! command -v cargo >/dev/null; then
    echo "cargo not on PATH — install Rust (https://rustup.rs)" >&2
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

# --- install cargo-truce -------------------------------------------------

echo "==> installing cargo-truce@$truce_tag"
# `--force` so any stale `cargo-truce` (prior release, dev work)
# gets replaced rather than silently kept. `cargo install` is a
# no-op when the binary already exists at the same name.
cargo install cargo-truce --git https://github.com/truce-audio/truce \
    --tag "$truce_tag" --locked --force

# --- build installer -----------------------------------------------------

mkdir -p target/dist
rm -f target/dist/*.tar.gz

# Linux packaging uses the plugin's default features (CLAP + VST3 +
# LV2 + standalone) and produces a `.tar.gz` with an `install.sh`
# that copies the bundles into the user's plugin directories. No
# `--formats` flag, no signing.
echo "==> packaging"
cargo truce package

tarball_path=$(ls -1 target/dist/*.tar.gz 2>/dev/null | head -1)
if [[ -z "${tarball_path:-}" ]]; then
    echo "no .tar.gz produced under target/dist/" >&2
    exit 1
fi
echo "==> built $tarball_path"

# --- upload --------------------------------------------------------------

echo "==> uploading $(basename "$tarball_path") to release $release_tag"
gh release upload "$release_tag" "$tarball_path" --clobber

echo
echo "==> Linux release done."

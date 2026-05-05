# Release driver for the Windows half of a truce-analyzer release.
#
# Builds the .exe installer for the version recorded in Cargo.toml
# and uploads it to the matching GitHub release (created earlier by
# `scripts/release-macos.sh`). Run after the macOS half so the tag
# and release already exist.
#
# Requires: gh, cargo, and an authenticated GitHub CLI session.
# Pulls cargo-truce from the same git tag the plugin links against.

$ErrorActionPreference = 'Stop'

function Invoke-Checked {
    param([string]$What, [scriptblock]$Block)
    & $Block
    if ($LASTEXITCODE -ne 0) {
        throw "$What failed (exit $LASTEXITCODE)"
    }
}

Push-Location (git rev-parse --show-toplevel)
try {
    # --- parse versions from Cargo.toml ---------------------------------

    $cargoToml = Get-Content -Raw Cargo.toml
    $pkgVersion = ([regex]'(?m)^version\s*=\s*"([^"]+)"').Match($cargoToml).Groups[1].Value
    $truceTag   = ([regex]'(?m)^truce\s+=.*tag\s*=\s*"([^"]+)"').Match($cargoToml).Groups[1].Value

    if (-not $pkgVersion -or -not $truceTag) {
        throw "could not parse package version or truce tag from Cargo.toml"
    }

    $releaseTag = "v$pkgVersion"
    Write-Host "==> release tag: $releaseTag (truce $truceTag)"

    # --- preflight ------------------------------------------------------

    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
        throw "gh CLI not installed (https://cli.github.com)"
    }
    Invoke-Checked "gh auth status" { gh auth status | Out-Null }

    Invoke-Checked "git fetch" { git fetch --tags origin | Out-Null }

    & git rev-parse "refs/tags/$releaseTag" 2>$null | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "tag $releaseTag does not exist locally or upstream — run scripts/release-macos.sh first"
    }

    & gh release view $releaseTag 2>$null | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "release $releaseTag does not exist on GitHub — run scripts/release-macos.sh first"
    }

    # --- install cargo-truce --------------------------------------------

    Write-Host "==> installing cargo-truce@$truceTag"
    Invoke-Checked "cargo install cargo-truce" {
        cargo install cargo-truce `
            --git https://github.com/truce-audio/truce `
            --tag $truceTag --locked
    }

    # --- build installer ------------------------------------------------

    if (Test-Path target/dist) {
        Get-ChildItem target/dist/*.exe -ErrorAction SilentlyContinue | Remove-Item -Force
    } else {
        New-Item -ItemType Directory -Path target/dist -Force | Out-Null
    }

    Write-Host "==> packaging"
    Invoke-Checked "cargo truce package" { cargo truce package }

    $exe = Get-ChildItem target/dist/*.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $exe) {
        throw "no .exe produced under target/dist/"
    }
    Write-Host "==> built $($exe.FullName)"

    # --- upload ---------------------------------------------------------

    Write-Host "==> uploading $($exe.Name) to release $releaseTag"
    $extras = @()
    foreach ($f in @('screenshots/analyzer_spectrum_windows.png', 'screenshots/analyzer_diff_windows.png')) {
        if (Test-Path $f) { $extras += $f }
    }
    Invoke-Checked "gh release upload" {
        gh release upload $releaseTag $exe.FullName @extras --clobber
    }

    Write-Host ""
    Write-Host "==> Windows release done."
} finally {
    Pop-Location
}

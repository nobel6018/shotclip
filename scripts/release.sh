#!/usr/bin/env bash
# Builds, signs and publishes a release that installed copies pick up through the in-app updater
# (src/updater.rs reads releases/latest/download/latest.json).
#
#   scripts/release.sh [notes-file]
#
# Needs: GNU Rust toolchain, minisign (mise use -g minisign), gh, python.
# Secret key: $SHOTCLIP_UPDATER_KEY or ~/.shotclip/updater.key (its public key is updater.rs PUBKEY).
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
tag="v$version"
key=${SHOTCLIP_UPDATER_KEY:-$HOME/.shotclip/updater.key}
repo=$(gh repo view --json nameWithOwner -q .nameWithOwner)
[ -f "$key" ] || { echo "no updater key at $key"; exit 1; }
if gh release view "$tag" >/dev/null 2>&1; then echo "$tag already released"; exit 1; fi

tc="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-gnu/bin"
PATH="$tc:$PATH" "$tc/cargo" build --release

out=target/release-assets
rm -rf "$out" && mkdir -p "$out"
cp target/release/shotclip.exe "$out/"
minisign -S -s "$key" -m "$out/shotclip.exe" -t "shotclip $tag"

python - "$out" "$version" "$repo" "$tag" <<'PY'
import json, sys, datetime
out, version, repo, tag = sys.argv[1:]
sig = open(f"{out}/shotclip.exe.minisig", encoding="utf-8").read()
manifest = {
    "version": version,
    "notes": f"https://github.com/{repo}/releases/tag/{tag}",
    "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "platforms": {"windows-x86_64": {
        "url": f"https://github.com/{repo}/releases/download/{tag}/shotclip.exe",
        "signature": sig,
    }},
}
open(f"{out}/latest.json", "w", encoding="utf-8").write(json.dumps(manifest, indent=2))
PY
cat "$out/latest.json"

notes=(--generate-notes)
[ $# -ge 1 ] && notes=(--notes-file "$1")
gh release create "$tag" --title "ShotClip $tag" "${notes[@]}" \
  "$out/shotclip.exe" "$out/shotclip.exe.minisig" "$out/latest.json"

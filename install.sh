#!/bin/sh
# Install an official HeyCode executable without Rust or a repository checkout.
set -eu
repository=Naresh084/heycode
version=${HEYCODE_VERSION:-latest}
install_dir=${HEYCODE_INSTALL_DIR:-"$HOME/.local/bin"}
case "$install_dir" in /*) ;; *) echo 'HEYCODE_INSTALL_DIR must be absolute' >&2; exit 1 ;; esac
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) platform=macos-aarch64 ;;
  Darwin-x86_64) platform=macos-x86_64 ;;
  Linux-x86_64) platform=linux-x86_64 ;;
  *) echo 'This platform has no HeyCode binary. See GitHub Releases.' >&2; exit 1 ;;
esac
case "$version" in latest) ;; v[0-9]*|[0-9]*)
  case "$version" in *[!v0-9.]*) echo 'Invalid release version' >&2; exit 1 ;; esac
  version=v${version#v} ;;
  *) echo 'Invalid release version' >&2; exit 1 ;;
esac
command -v curl >/dev/null || { echo 'curl is required' >&2; exit 1; }
if command -v shasum >/dev/null; then checksum=shasum
elif command -v sha256sum >/dev/null; then checksum=sha256sum
else echo 'shasum or sha256sum is required' >&2; exit 1; fi
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
if [ "$version" = latest ]; then
  # Resolve once, so checksums and binary cannot come from different releases.
  resolved=$(curl --proto '=https' --tlsv1.2 -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$repository/releases/latest")
  version=${resolved##*/}
  case "$version" in v[0-9]*.[0-9]*.[0-9]*) ;; *) echo 'No stable release is available' >&2; exit 1 ;; esac
  case "$version" in *[!v0-9.]*) echo 'Invalid release version' >&2; exit 1 ;; esac
fi
asset=heycode-$platform
base=https://github.com/$repository/releases/download/$version
curl --proto '=https' --tlsv1.2 -fLsS "$base/$asset" -o "$scratch/$asset"
curl --proto '=https' --tlsv1.2 -fLsS "$base/SHA256SUMS" -o "$scratch/SHA256SUMS"
expected=$(awk -v file="$asset" '$2 == file {print $1}' "$scratch/SHA256SUMS")
[ ${#expected} -eq 64 ] || { echo 'Missing or invalid SHA-256 checksum' >&2; exit 1; }
if [ "$checksum" = shasum ]; then actual=$(shasum -a 256 "$scratch/$asset" | awk '{print $1}')
else actual=$(sha256sum "$scratch/$asset" | awk '{print $1}'); fi
[ "$actual" = "$expected" ] || { echo 'SHA-256 mismatch; nothing installed' >&2; exit 1; }
mkdir -p "$install_dir"
[ ! -L "$install_dir/heycode" ] || { echo 'Installation target is a symlink; choose a separate install directory' >&2; exit 1; }
staged=$(mktemp "$install_dir/.heycode-download.XXXXXX")
trap 'rm -rf "$scratch"; rm -f "$staged"' EXIT HUP INT TERM
cp "$scratch/$asset" "$staged"
chmod 755 "$staged"
# Running --version cannot connect to a provider or start an update.
"$staged" --version
if [ -f "$install_dir/heycode" ]; then cp -p "$install_dir/heycode" "$install_dir/heycode.previous"; fi
mv -f "$staged" "$install_dir/heycode"
printf '%s\n' "$repository" > "$install_dir/.heycode-install"
printf '\nInstalled HeyCode %s to %s/heycode\n' "$version" "$install_dir"
printf 'Automatic updates are enabled. Start with: heycode\n'
case ":$PATH:" in *":$install_dir:"*) ;; *) printf '\nAdd this directory to your shell PATH: %s\n' "$install_dir" ;; esac

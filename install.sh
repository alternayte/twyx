#!/bin/sh
# Installs twyx and the twyx-desktop app from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/alternayte/twyx/main/install.sh | sh
#
# TWYX_VERSION picks a release tag, such as v0.1.0; the latest one otherwise.
# TWYX_INSTALL_DIR picks the directory; ~/.local/bin otherwise.
set -eu

repo="alternayte/twyx"
dir="${TWYX_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
  printf 'twyx install: %s\n' "$1" >&2
  exit 1
}

case "$(uname -s)" in
  Darwin) os="apple-darwin" ;;
  Linux) os="unknown-linux-gnu" ;;
  *) fail "no release for $(uname -s); build from source" ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch="aarch64" ;;
  x86_64 | amd64) arch="x86_64" ;;
  *) fail "no release for $(uname -m); build from source" ;;
esac
target="$arch-$os"

if [ -n "${TWYX_VERSION:-}" ]; then
  base="https://github.com/$repo/releases/download/$TWYX_VERSION"
else
  base="https://github.com/$repo/releases/latest/download"
fi

command -v curl >/dev/null || fail "curl is not on PATH"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

archive="twyx-$target.tar.gz"
printf 'downloading %s\n' "$base/$archive"
curl -fsSL "$base/$archive" -o "$work/$archive" || fail "download $archive"
curl -fsSL "$base/$archive.sha256" -o "$work/$archive.sha256" || fail "download $archive.sha256"

expected="$(cut -d ' ' -f 1 "$work/$archive.sha256")"
if command -v sha256sum >/dev/null; then
  actual="$(sha256sum "$work/$archive" | cut -d ' ' -f 1)"
else
  actual="$(shasum -a 256 "$work/$archive" | cut -d ' ' -f 1)"
fi
[ "$expected" = "$actual" ] || fail "$archive does not match its checksum"

tar -xzf "$work/$archive" -C "$work"
mkdir -p "$dir"
# A new inode, so a running daemon keeps its own binary. The desktop app
# starts the daemon from the twyx beside it.
for binary in twyx twyx-desktop; do
  cp "$work/twyx-$target/$binary" "$dir/$binary.new"
  chmod 755 "$dir/$binary.new"
  mv -f "$dir/$binary.new" "$dir/$binary"
done
printf 'installed %s\n' "$("$dir/twyx" --version) in $dir"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) printf '%s is not on PATH; add it to your shell profile\n' "$dir" ;;
esac
found="$(command -v twyx || true)"
if [ -n "$found" ] && [ "$found" != "$dir/twyx" ]; then
  printf '%s comes first on PATH; remove it or put %s before it\n' "$found" "$dir"
fi
printf 'if a twyx daemon runs, run twyx daemon stop; your panes keep running, and the next twyx adopts them\n'

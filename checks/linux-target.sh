#!/usr/bin/env bash
# check: linux-target
# born: 2026-10-18
# failure: macOS-only code paths compile locally while the Linux build breaks in CI
# rule: the workspace type-checks for the Linux target before it is pushed
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

if [ "$(uname -s)" = "Linux" ]; then
  exit 0
fi

if ! rustup target list --installed | grep -q '^x86_64-unknown-linux-gnu$'; then
  printf 'install the cross-check target first:\n  rustup target add x86_64-unknown-linux-gnu\n' >&2
  exit 1
fi

# C sources (bundled SQLite) need a Linux C compiler. Zig is one, and every
# build already needs it.
if ! command -v x86_64-linux-gnu-gcc >/dev/null; then
  wrapper="$(mktemp -d)"
  trap 'rm -rf "$wrapper"' EXIT
  # cc-rs adds a --target that Zig does not read; Zig gets its own.
  cat > "$wrapper/cc" <<'WRAPPER'
#!/bin/sh
for arg do
  shift
  case "$arg" in --target=*) ;; *) set -- "$@" "$arg" ;; esac
done
exec zig cc -target x86_64-linux-gnu "$@"
WRAPPER
  printf '#!/bin/sh\nexec zig ar "$@"\n' > "$wrapper/ar"
  chmod +x "$wrapper/cc" "$wrapper/ar"
  export CC_x86_64_unknown_linux_gnu="$wrapper/cc"
  export AR_x86_64_unknown_linux_gnu="$wrapper/ar"
fi

# The desktop's Linux backends find fontconfig with pkg-config, which has no
# Linux sysroot here; loading it at run time needs none, and a check never
# links.
export RUST_FONTCONFIG_DLOPEN=on

cargo check --quiet --workspace --target x86_64-unknown-linux-gnu

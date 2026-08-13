#!/usr/bin/env sh
# One-line installer for `instagrep`.
#
#   curl -fsSL https://raw.githubusercontent.com/kvyaswanth/instagrep/main/install.sh | sh
#
# Downloads a prebuilt binary from GitHub Releases for your platform, falling
# back to `cargo install` from source if no binary is available.

set -eu

REPO="kvyaswanth/instagrep"
BIN="instagrep"
INSTALL_DIR="${INSTAGREP_INSTALL_DIR:-${HOME}/.cargo/bin}"

# --- detect platform --------------------------------------------------------
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
  Darwin) os=apple-darwin ;;
  Linux)  os=unknown-linux-gnu ;;
  *) echo "$BIN: unsupported OS: $OS" >&2; exit 1 ;;
esac
case "$ARCH" in
  arm64|aarch64) arch=aarch64 ;;
  x86_64|amd64)  arch=x86_64 ;;
  *) echo "$BIN: unsupported arch: $ARCH" >&2; exit 1 ;;
esac
TARGET="${arch}-${os}"

echo "$BIN: target ${TARGET}"

# --- find latest release tag ------------------------------------------------
TAG="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
  | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$TAG" ] || TAG=""

try_binary() {
  [ -z "$TAG" ] && return 1
  # Try tarball first, then a tag-prefixed name.
  for name in "${BIN}-${TARGET}.tar.gz" "${BIN}-${TAG}-${TARGET}.tar.gz"; do
    url="https://github.com/${REPO}/releases/download/${TAG}/${name}"
    tmpdir="$(mktemp -d)"
    if curl -fsSL "$url" -o "${tmpdir}/${BIN}.tar.gz"; then
      tar -xzf "${tmpdir}/${BIN}.tar.gz" -C "${tmpdir}"
      mkdir -p "$INSTALL_DIR"
      install -m 0755 "${tmpdir}/${BIN}" "${INSTALL_DIR}/${BIN}"
      rm -rf "$tmpdir"
      echo "$BIN: installed ${TAG} to ${INSTALL_DIR}/${BIN}"
      return 0
    fi
    rm -rf "$tmpdir"
  done
  return 1
}

try_cargo() {
  if command -v cargo >/dev/null 2>&1; then
    echo "$BIN: no prebuilt binary; installing from source via cargo (compiles, ~1 min)..."
    cargo install --git "https://github.com/${REPO}" --locked --force
    return 0
  fi
  echo "$BIN: no prebuilt binary and cargo not found." >&2
  echo "$BIN: install Rust from https://rustup.rs then re-run, or build manually." >&2
  return 1
}

if try_binary || try_cargo; then
  echo
  echo "$BIN: done. Make sure '${INSTALL_DIR}' is on your PATH."
  command -v "$BIN" >/dev/null 2>&1 && "$BIN" --version || true
else
  exit 1
fi

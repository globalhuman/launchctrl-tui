#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="launchctrl-tui"
REPO="${LAUNCHCTRL_TUI_REPO:-}"
INSTALL_DIR="${INSTALL_DIR:-${HOME}/.local/bin}"

usage() {
  cat <<'USAGE'
Install the latest launchctrl-tui release from GitHub.

Usage:
  scripts/install-latest.sh [--repo owner/repo] [--dir install-dir]

Environment:
  LAUNCHCTRL_TUI_REPO  GitHub repository in owner/repo form
  INSTALL_DIR          Install directory, defaults to ~/.local/bin

Examples:
  LAUNCHCTRL_TUI_REPO=owner/launchctrl-tui scripts/install-latest.sh
  scripts/install-latest.sh --repo owner/launchctrl-tui --dir /usr/local/bin
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      REPO="${2:-}"
      shift 2
      ;;
    --dir)
      INSTALL_DIR="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "${REPO}" ]]; then
  if git remote get-url origin >/dev/null 2>&1; then
    remote="$(git remote get-url origin)"
    REPO="$(printf '%s\n' "$remote" | sed -E 's#^git@github.com:##; s#^https://github.com/##; s#\.git$##')"
  fi
fi

if [[ -z "${REPO}" || "${REPO}" != */* ]]; then
  echo "Could not determine GitHub repo. Pass --repo owner/repo or set LAUNCHCTRL_TUI_REPO." >&2
  exit 1
fi

case "$(uname -s)" in
  Darwin) os="apple-darwin" ;;
  *) echo "launchctrl-tui currently publishes macOS release artifacts only." >&2; exit 1 ;;
esac

case "$(uname -m)" in
  arm64|aarch64) arch="aarch64" ;;
  x86_64|amd64) arch="x86_64" ;;
  *) echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

asset="${BIN_NAME}-${arch}-${os}.tar.gz"
base_url="https://github.com/${REPO}/releases/latest/download"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

command -v curl >/dev/null || { echo "curl is required" >&2; exit 1; }
command -v tar >/dev/null || { echo "tar is required" >&2; exit 1; }

printf 'Downloading %s from %s\n' "$asset" "$REPO"
curl -fL "${base_url}/${asset}" -o "${tmp_dir}/${asset}"

if curl -fsL "${base_url}/${asset}.sha256" -o "${tmp_dir}/${asset}.sha256"; then
  echo "Verifying checksum"
  (cd "$tmp_dir" && shasum -a 256 -c "${asset}.sha256")
else
  echo "Checksum file not found; continuing without verification" >&2
fi

tar -xzf "${tmp_dir}/${asset}" -C "$tmp_dir"
mkdir -p "$INSTALL_DIR"
install -m 0755 "${tmp_dir}/${BIN_NAME}" "${INSTALL_DIR}/${BIN_NAME}"

printf 'Installed %s to %s\n' "$BIN_NAME" "${INSTALL_DIR}/${BIN_NAME}"
if ! command -v "$BIN_NAME" >/dev/null 2>&1; then
  printf 'Note: %s is not on PATH. Add this to your shell profile:\n  export PATH="%s:$PATH"\n' "$INSTALL_DIR" "$INSTALL_DIR"
fi

#!/bin/sh
set -eu

repository="shichenghaoshu/openwork"
version=""
install_dir="${OPENWORK_INSTALL_DIR:-${HOME}/.local/bin}"
force=0

usage() {
  cat <<'EOF'
Install an official OpenWork release after verifying its SHA-256 checksum.

Usage: install.sh [--version vX.Y.Z] [--install-dir DIR] [--force]

If --version is omitted, the latest GitHub Release is selected. An existing
openwork binary is never replaced unless --force is supplied; forced installs
keep a timestamped backup in the same directory.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || { echo "--version requires a value" >&2; exit 2; }
      version=$2
      shift 2
      ;;
    --install-dir)
      [ "$#" -ge 2 ] || { echo "--install-dir requires a value" >&2; exit 2; }
      install_dir=$2
      shift 2
      ;;
    --force)
      force=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { echo "tar is required" >&2; exit 1; }

if [ -z "$version" ]; then
  latest_url=$(curl --proto '=https' --tlsv1.2 -fsSL -o /dev/null -w '%{url_effective}' \
    "https://github.com/${repository}/releases/latest")
  version=${latest_url##*/}
fi

printf '%s\n' "$version" | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?(\+[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?$' || {
  echo "invalid release version: $version" >&2
  exit 2
}

case $(uname -s) in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-gnu ;;
  *) echo "unsupported operating system: $(uname -s)" >&2; exit 3 ;;
esac

case $(uname -m) in
  arm64|aarch64) architecture=aarch64 ;;
  x86_64|amd64) architecture=x86_64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 3 ;;
esac

target="${architecture}-${os}"
asset="openwork-${version}-${target}.tar.gz"
base_url="https://github.com/${repository}/releases/download/${version}"
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/openwork-install.XXXXXXXX")
cleanup() { rm -rf -- "$temporary_dir"; }
trap cleanup EXIT HUP INT TERM

curl --proto '=https' --tlsv1.2 -fL --retry 3 --retry-delay 1 \
  -o "$temporary_dir/$asset" "$base_url/$asset"
curl --proto '=https' --tlsv1.2 -fL --retry 3 --retry-delay 1 \
  -o "$temporary_dir/$asset.sha256" "$base_url/$asset.sha256"

expected_hash=$(awk 'NR == 1 { print $1 }' "$temporary_dir/$asset.sha256")
case "$expected_hash" in
  *[!0-9A-Fa-f]*|'') echo "release checksum has an invalid format" >&2; exit 4 ;;
esac
[ "${#expected_hash}" -eq 64 ] || { echo "release checksum is not SHA-256" >&2; exit 4; }

if command -v sha256sum >/dev/null 2>&1; then
  actual_hash=$(sha256sum "$temporary_dir/$asset" | awk '{ print $1 }')
else
  command -v shasum >/dev/null 2>&1 || { echo "sha256sum or shasum is required" >&2; exit 1; }
  actual_hash=$(shasum -a 256 "$temporary_dir/$asset" | awk '{ print $1 }')
fi
[ "$actual_hash" = "$expected_hash" ] || { echo "SHA-256 verification failed" >&2; exit 4; }

tar -xzf "$temporary_dir/$asset" -C "$temporary_dir"
extracted="$temporary_dir/openwork-${version}-${target}/openwork"
[ -f "$extracted" ] || { echo "release archive does not contain openwork" >&2; exit 5; }

mkdir -p -- "$install_dir"
destination="$install_dir/openwork"
if [ -e "$destination" ] && [ "$force" -ne 1 ]; then
  echo "$destination already exists; rerun with --force to preserve a backup and replace it" >&2
  exit 6
fi

stage=$(mktemp "$install_dir/.openwork.install.XXXXXXXX")
install -m 0755 "$extracted" "$stage"
backup=""
if [ -e "$destination" ]; then
  backup="$destination.backup.$(date -u +%Y%m%dT%H%M%SZ).$$"
  mv -- "$destination" "$backup"
  echo "Preserved previous binary at $backup"
fi
if ! mv -- "$stage" "$destination"; then
  rm -f -- "$stage"
  if [ -n "$backup" ] && [ -e "$backup" ]; then
    mv -- "$backup" "$destination"
  fi
  echo "installation failed; the previous binary was restored" >&2
  exit 7
fi
trap - EXIT HUP INT TERM
rm -rf -- "$temporary_dir"
echo "Installed OpenWork $version to $destination"

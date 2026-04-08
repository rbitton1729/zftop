#!/bin/sh
# zftop install script.
#
# Detects OS/arch, fetches the matching binary from the GitLab release
# (latest by default, or --version X.Y.Z), verifies its SHA-256 against the
# checksum file published alongside it, and installs to /usr/local/bin by
# default. Uses sudo if the target directory isn't writable.
#
# Usage:
#   curl -fsSL https://git.skylantix.com/rbitton/zftop/-/raw/main/install.sh | sh
#   ./install.sh [--version X.Y.Z] [--dir /path/to/bin]
#
# POSIX sh — works on Linux /bin/sh, FreeBSD /bin/sh, busybox ash, etc.
# No bashisms, no jq, no python.

set -eu

REPO_HOST="git.skylantix.com"
REPO_PROJECT="rbitton%2Fzftop" # URL-encoded owner/repo for the GitLab API
API_BASE="https://${REPO_HOST}/api/v4/projects/${REPO_PROJECT}"

INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
VERSION="${VERSION:-}"
FORCE="${FORCE:-}"

# --- Arg parsing ---------------------------------------------------------

usage() {
    cat <<EOF
Usage: install.sh [--version X.Y.Z] [--dir DIR] [--force]

Installs zftop for the detected OS/arch from ${REPO_HOST}/rbitton/zftop.

Options:
  --version X.Y.Z   Install a specific version (default: latest release)
  --dir DIR         Install directory (default: /usr/local/bin)
  --force, -y       Skip the "ZFS not detected" confirmation prompt
  -h, --help        Show this help

Environment variables (override defaults):
  VERSION, INSTALL_DIR, FORCE (any non-empty value = --force)
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version)       VERSION="$2"; shift 2 ;;
        --version=*)     VERSION="${1#--version=}"; shift ;;
        --dir)           INSTALL_DIR="$2"; shift 2 ;;
        --dir=*)         INSTALL_DIR="${1#--dir=}"; shift ;;
        --force|-y|--yes) FORCE=1; shift ;;
        -h|--help)       usage; exit 0 ;;
        *)               echo "install.sh: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

# Strip any leading "v" the user may have passed.
VERSION="${VERSION#v}"

# --- Detect OS and architecture ------------------------------------------

uname_s=$(uname -s)
case "$uname_s" in
    Linux)   OS=linux ;;
    FreeBSD) OS=freebsd ;;
    Darwin)
        echo "install.sh: macOS is not supported — zftop needs ZFS on Linux or FreeBSD" >&2
        exit 1
        ;;
    *)
        echo "install.sh: unsupported OS: $uname_s" >&2
        exit 1
        ;;
esac

uname_m=$(uname -m)
case "$uname_m" in
    x86_64|amd64) ARCH=amd64 ;;
    aarch64|arm64)
        if [ "$OS" = "freebsd" ]; then
            echo "install.sh: no FreeBSD arm64 binary is published — build from source" >&2
            exit 1
        fi
        ARCH=arm64
        ;;
    *)
        echo "install.sh: unsupported architecture: $uname_m" >&2
        exit 1
        ;;
esac

ASSET="zftop-${OS}-${ARCH}"
echo "install.sh: detected ${OS}/${ARCH}, asset = ${ASSET}"

# --- Check that ZFS is actually present ---------------------------------

# A warning, not a hard failure. Someone might be pre-staging zftop on a host
# that isn't yet mounted with ZFS, or installing into a chroot/container
# that'll be flipped into a ZFS box later. We just want them to know.
zfs_present() {
    case "$OS" in
        linux)
            [ -r /proc/spl/kstat/zfs/arcstats ]
            ;;
        freebsd)
            # Any kstat.zfs.misc.arcstats.* sysctl exists exactly when the
            # ZFS module is loaded and reporting. `size` is the canonical one.
            sysctl -n kstat.zfs.misc.arcstats.size >/dev/null 2>&1
            ;;
        *)
            return 1
            ;;
    esac
}

if ! zfs_present; then
    case "$OS" in
        linux)
            zfs_reason="/proc/spl/kstat/zfs/arcstats is missing — the OpenZFS kernel module doesn't look loaded"
            ;;
        freebsd)
            zfs_reason="kstat.zfs.misc.arcstats sysctls are unavailable — OpenZFS doesn't look loaded"
            ;;
    esac
    echo ""
    echo "install.sh: WARNING — ZFS is not detected on this host."
    echo "  ${zfs_reason}."
    echo "  zftop will still install, but it'll exit with an error on launch"
    echo "  until ZFS is available."
    echo ""

    if [ -n "$FORCE" ]; then
        echo "install.sh: --force set, continuing anyway."
    else
        # Find a readable tty to prompt on. When the script is run as
        # `curl | sh`, stdin is the HTTP body, so we reopen /dev/tty to
        # talk to the user directly. If neither is available (e.g. inside
        # a non-interactive CI job), fall back to proceeding with a notice,
        # since the user explicitly invoked the installer.
        if [ -t 0 ]; then
            printf "Install anyway? [y/N] "
            read -r zfs_answer
        elif [ -r /dev/tty ]; then
            printf "Install anyway? [y/N] " > /dev/tty
            read -r zfs_answer < /dev/tty
        else
            echo "install.sh: no tty available for confirmation — proceeding."
            zfs_answer=y
        fi
        case "$zfs_answer" in
            y|Y|yes|YES|Yes) ;;
            *)
                echo "install.sh: aborted (no install performed)."
                exit 0
                ;;
        esac
    fi
    echo ""
fi

# --- Pick a downloader ---------------------------------------------------

if command -v curl >/dev/null 2>&1; then
    http_get()     { curl -fsSL "$1"; }
    http_get_to()  { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
    http_get()     { wget -qO- "$1"; }
    http_get_to()  { wget -qO "$2" "$1"; }
else
    echo "install.sh: need curl or wget on PATH" >&2
    exit 1
fi

# --- Resolve version -----------------------------------------------------

if [ -z "$VERSION" ]; then
    echo "install.sh: fetching latest release metadata..."
    # The release permalink endpoint returns JSON for the most recent release.
    # We only need tag_name; parse it without jq by splitting on { , } and
    # matching the one field we care about. This is fragile against JSON
    # with commas in strings, but tag names don't contain commas.
    release_json=$(http_get "${API_BASE}/releases/permalink/latest") || {
        echo "install.sh: failed to query latest release from ${API_BASE}" >&2
        exit 1
    }
    TAG=$(printf '%s' "$release_json" \
        | tr '{,}' '\n' \
        | grep -m1 '"tag_name"' \
        | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')
    if [ -z "$TAG" ]; then
        echo "install.sh: could not parse tag_name from release metadata" >&2
        exit 1
    fi
    VERSION="${TAG#v}"
fi

echo "install.sh: installing zftop ${VERSION}"

# The package-registry URL pattern the CI uploads to. Same pattern used by
# the release asset links in .gitlab-ci.yml's release: block.
PKG_BASE="${API_BASE}/packages/generic/zftop/${VERSION}"
BIN_URL="${PKG_BASE}/${ASSET}"
SHA_URL="${PKG_BASE}/${ASSET}.sha256"

# --- Download to a temp dir ----------------------------------------------

TMPDIR=$(mktemp -d 2>/dev/null || mktemp -d -t zftop-install)
trap 'rm -rf "$TMPDIR"' EXIT INT HUP TERM

echo "install.sh: downloading ${BIN_URL}"
http_get_to "$BIN_URL" "$TMPDIR/$ASSET" || {
    echo "install.sh: download failed — is version ${VERSION} published?" >&2
    exit 1
}

echo "install.sh: downloading ${SHA_URL}"
http_get_to "$SHA_URL" "$TMPDIR/$ASSET.sha256" || {
    echo "install.sh: could not fetch checksum file" >&2
    exit 1
}

# --- Verify SHA-256 ------------------------------------------------------

expected=$(tr -d '[:space:]' < "$TMPDIR/$ASSET.sha256")
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$TMPDIR/$ASSET" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$TMPDIR/$ASSET" | awk '{print $1}')
elif command -v sha256 >/dev/null 2>&1; then
    # FreeBSD base system ships `sha256`; output is "SHA256 (file) = <hash>"
    actual=$(sha256 -q "$TMPDIR/$ASSET")
else
    echo "install.sh: no sha256 tool found (need sha256sum, shasum, or sha256)" >&2
    exit 1
fi

if [ "$expected" != "$actual" ]; then
    echo "install.sh: SHA-256 mismatch" >&2
    echo "  expected: $expected" >&2
    echo "  got:      $actual" >&2
    exit 1
fi
echo "install.sh: sha256 verified"

# --- Install -------------------------------------------------------------

if [ ! -d "$INSTALL_DIR" ]; then
    echo "install.sh: install directory does not exist: $INSTALL_DIR" >&2
    exit 1
fi

chmod +x "$TMPDIR/$ASSET"
TARGET="${INSTALL_DIR}/zftop"

if [ -w "$INSTALL_DIR" ] || [ "$(id -u)" = "0" ]; then
    mv "$TMPDIR/$ASSET" "$TARGET"
else
    if ! command -v sudo >/dev/null 2>&1; then
        echo "install.sh: ${INSTALL_DIR} is not writable and sudo is not installed" >&2
        echo "  re-run with --dir pointing at a writable directory, or run as root" >&2
        exit 1
    fi
    echo "install.sh: ${INSTALL_DIR} is not writable — using sudo"
    sudo mv "$TMPDIR/$ASSET" "$TARGET"
fi

echo "install.sh: installed zftop ${VERSION} to ${TARGET}"

# Final confirmation: ask the binary its version, just to prove it runs.
if "$TARGET" --version >/dev/null 2>&1; then
    "$TARGET" --version
fi

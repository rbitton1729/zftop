#!/bin/sh
#
# One-shot setup for the FreeBSD CI build host (bsd-1).
#
# Run as root on a fresh FreeBSD VM that should host zfstop's CI builds:
#
#     scp scripts/setup-bsd-ci.sh root@10.10.10.109:/tmp/
#     ssh root@10.10.10.109 'sh /tmp/setup-bsd-ci.sh'
#
# This script is idempotent — re-running it on an already-set-up host is a
# no-op (it skips creating the user if it exists, skips installing rustup if
# it's already installed, etc.). Safe to re-run after any change.
#
# What it does:
#   1. Creates an unprivileged user `gitlab-ci` with home /home/gitlab-ci
#   2. Installs the CI's public SSH key into ~gitlab-ci/.ssh/authorized_keys
#   3. Installs Rust via rustup as that user (more current than `pkg install rust`,
#      and per-user so it can be upgraded without root)
#   4. Clones the zfstop repo into ~gitlab-ci/zfstop so the CI build job can
#      `git fetch && git checkout <tag>` instead of cloning each time
#
# Threat model: gitlab-ci has no sudo, no shell other than sh, and owns nothing
# of value. Compromising it costs an attacker your CPU but nothing else.

set -eu

USER_NAME="gitlab-ci"
USER_HOME="/home/${USER_NAME}"
REPO_URL="https://git.skylantix.com/rbitton/zfstop.git"
REPO_DIR="${USER_HOME}/zfstop"

# Public key for the CI → bsd-1 connection. The matching private key lives in
# GitLab as the BSD_SSH_PRIVATE_KEY file variable.
PUBKEY='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIN7gBuKsw9aTLXuOTwRAhEf67NmYo2dR9EVH1IP8vQ+7 gitlab-ci@bsd-1 (zfstop CI)'

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root" >&2
    exit 1
fi

# 1. Create the user if it doesn't exist.
if pw user show "$USER_NAME" >/dev/null 2>&1; then
    echo "[ok] user $USER_NAME already exists"
else
    echo "[+] creating user $USER_NAME"
    pw useradd "$USER_NAME" -m -s /bin/sh -c "GitLab CI build user for zfstop"
fi

# 2. Install the SSH public key.
SSH_DIR="${USER_HOME}/.ssh"
AUTHORIZED_KEYS="${SSH_DIR}/authorized_keys"
mkdir -p "$SSH_DIR"
chmod 700 "$SSH_DIR"
if [ -f "$AUTHORIZED_KEYS" ] && grep -qF "$PUBKEY" "$AUTHORIZED_KEYS"; then
    echo "[ok] CI public key already in authorized_keys"
else
    echo "[+] installing CI public key"
    echo "$PUBKEY" >> "$AUTHORIZED_KEYS"
fi
chmod 600 "$AUTHORIZED_KEYS"
chown -R "${USER_NAME}:${USER_NAME}" "$SSH_DIR"

# 3. Install rustup as the gitlab-ci user (skipping if already present).
if su - "$USER_NAME" -c 'test -x "$HOME/.cargo/bin/rustc"'; then
    echo "[ok] rust toolchain already installed for $USER_NAME"
else
    echo "[+] installing rustup for $USER_NAME (this takes a minute)"
    pkg install -y curl
    su - "$USER_NAME" -c 'fetch -o - https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal'
fi

# 4. Clone the repo if not already present.
if su - "$USER_NAME" -c "test -d '${REPO_DIR}/.git'"; then
    echo "[ok] repo already cloned at $REPO_DIR"
else
    echo "[+] cloning repo into $REPO_DIR"
    pkg install -y git
    su - "$USER_NAME" -c "git clone '${REPO_URL}' '${REPO_DIR}'"
fi

echo
echo "Setup complete. The CI job can now SSH as ${USER_NAME}@10.10.10.109"
echo "and run 'cd zfstop && git fetch && git checkout <tag> && cargo build --release'."

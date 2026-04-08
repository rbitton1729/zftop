#!/bin/sh
#
# Build script that runs ON bsd-1 (the FreeBSD CI host) inside a fresh SSH
# session. The GitLab CI job pipes this file to the remote shell:
#
#     ssh gitlab-ci@bsd-1 "REF='${CI_COMMIT_SHA}' VERSION='${CI_COMMIT_TAG#v}' sh -s" \
#         < scripts/bsd-build.sh
#
# Expected env vars:
#
#   REF      — the commit SHA to check out (always set, comes from $CI_COMMIT_SHA)
#   VERSION  — the release version without the "v" prefix, e.g. "0.2.0"
#              (empty on non-tag builds — in which case Cargo.toml is NOT
#              version-stamped, and the build keeps the in-tree "0.0.0-dev")
#
# Builds zfstop in release mode and leaves the binary at:
#
#     ~gitlab-ci/zfstop/target/release/zfstop
#
# The CI job then SCPs that file back into its workspace and continues.
#
# Idempotent — safe to re-run with the same or a different REF.

set -eu
: "${REF:?REF not set; pipe with ssh \"REF=<sha> VERSION=<ver-or-empty> sh -s\" < scripts/bsd-build.sh}"

cd "$HOME/zfstop"

# Pull anything new (branches and tags) so we can resolve REF below.
git fetch --tags --force origin

# Wipe any prior CI run's local edits (e.g. an earlier version-stamp of
# Cargo.toml) and check out the exact commit the CI is building.
git checkout --force "$REF"
git reset --hard "$REF"

# Stamp the version into Cargo.toml on tag builds, the same way the Linux
# jobs do. On branch builds (no VERSION) we leave Cargo.toml alone.
# Note: BSD sed requires '' as the empty backup extension argument.
if [ -n "${VERSION:-}" ]; then
    sed -i '' "s/^version = .*/version = \"${VERSION}\"/" Cargo.toml
fi

# Build. Cargo lives at $HOME/.cargo/bin/cargo because rustup installs
# per-user (see scripts/setup-bsd-ci.sh).
"$HOME/.cargo/bin/cargo" build --release

# Strip is optional on FreeBSD but matches what the Linux jobs do.
# /usr/bin/strip is in the FreeBSD base system.
strip target/release/zfstop

ls -la target/release/zfstop
file target/release/zfstop

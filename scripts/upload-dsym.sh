#!/bin/bash
# Creates the Sentry release for this version and uploads dist/mac-storage-cleaner.dSYM, so crash
# reports from the build made by scripts/bundle.sh have readable stack traces.
#
# Needs SENTRY_AUTH_TOKEN, SENTRY_ORG and SENTRY_PROJECT from the environment or .env, where the
# token may also be stored as sentry_api. The token reaches sentry-cli through the environment
# only and is never printed.
set -euo pipefail

cd "$(dirname "$0")/.."

read_env() {
    [[ -f .env ]] || return 0
    sed -n "s/^$1=//p" .env | head -n 1 | tr -d '"'"'"''
}

: "${SENTRY_AUTH_TOKEN:=$(read_env SENTRY_AUTH_TOKEN)}"
: "${SENTRY_AUTH_TOKEN:=$(read_env sentry_api)}"
: "${SENTRY_ORG:=$(read_env SENTRY_ORG)}"
: "${SENTRY_PROJECT:=$(read_env SENTRY_PROJECT)}"
for name in SENTRY_AUTH_TOKEN SENTRY_ORG SENTRY_PROJECT; do
    [[ -n "${!name}" ]] || { echo "$name is not set" >&2; exit 1; }
done
export SENTRY_AUTH_TOKEN SENTRY_ORG SENTRY_PROJECT

VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
RELEASE="mac-storage-cleaner@$VERSION"
DSYM="dist/mac-storage-cleaner.dSYM"
[[ -d "$DSYM" ]] || { echo "Missing $DSYM; run scripts/bundle.sh first" >&2; exit 1; }

if command -v sentry-cli >/dev/null; then
    cli=(sentry-cli)
else
    cli=(npx --yes @sentry/cli)
fi

"${cli[@]}" releases new "$RELEASE"
"${cli[@]}" debug-files upload --wait "$DSYM"
"${cli[@]}" releases finalize "$RELEASE"
echo "Uploaded debug symbols for $RELEASE"

#!/usr/bin/env bash
# Set the TestFlight secrets on the GitHub repo. Run once — OR skip the App Store Connect ones by
# defining ASC_KEY_ID / ASC_ISSUER_ID / ASC_KEY_CONTENT / APPLE_TEAM_ID as GitHub ORG secrets, which
# every repo in the org inherits.
#
# Two groups. The first four are always required. The signing five point CI at the encrypted signing
# bundle made by scripts/bootstrap-signing.py and MUST be the READ-ONLY, single-bucket key, never the
# bootstrap key.
#
# Usage:
#   ASC_KEY_ID=... ASC_ISSUER_ID=... APPLE_TEAM_ID=... \
#   ASC_KEY_P8=~/.appstoreconnect/private_keys/AuthKey_XXXX.p8 \
#   B2_SIGNING_KEY_ID=... B2_SIGNING_APP_KEY=... B2_SIGNING_ENDPOINT=... B2_SIGNING_BUCKET=... \
#   SIGNING_PASSPHRASE=... \
#   scripts/setup-secrets.sh <owner/repo>
set -euo pipefail
REPO="${1:?usage: setup-secrets.sh <owner/repo>}"
: "${ASC_KEY_ID:?set ASC_KEY_ID}"
: "${ASC_ISSUER_ID:?set ASC_ISSUER_ID}"
: "${APPLE_TEAM_ID:?set APPLE_TEAM_ID}"
: "${ASC_KEY_P8:?set ASC_KEY_P8 to the path of your AuthKey_*.p8}"
: "${B2_SIGNING_KEY_ID:?set B2_SIGNING_KEY_ID}"
: "${B2_SIGNING_APP_KEY:?set B2_SIGNING_APP_KEY (the secret half of the read-only key)}"
: "${B2_SIGNING_ENDPOINT:?set B2_SIGNING_ENDPOINT (e.g. s3.<region>.<host>)}"
: "${B2_SIGNING_BUCKET:?set B2_SIGNING_BUCKET}"
: "${SIGNING_PASSPHRASE:?set SIGNING_PASSPHRASE}"
[ -f "$ASC_KEY_P8" ] || { echo "no such .p8: $ASC_KEY_P8" >&2; exit 1; }

gh secret set ASC_KEY_ID          -R "$REPO" --body "$ASC_KEY_ID"
gh secret set ASC_ISSUER_ID       -R "$REPO" --body "$ASC_ISSUER_ID"
gh secret set APPLE_TEAM_ID       -R "$REPO" --body "$APPLE_TEAM_ID"
base64 -i "$ASC_KEY_P8" | tr -d '\n' | gh secret set ASC_KEY_CONTENT -R "$REPO"
gh secret set B2_SIGNING_KEY_ID   -R "$REPO" --body "$B2_SIGNING_KEY_ID"
gh secret set B2_SIGNING_ENDPOINT -R "$REPO" --body "$B2_SIGNING_ENDPOINT"
gh secret set B2_SIGNING_BUCKET   -R "$REPO" --body "$B2_SIGNING_BUCKET"
# The two real secrets go in on stdin, not as arguments, so they never show in the process list.
printf '%s' "$B2_SIGNING_APP_KEY" | gh secret set B2_SIGNING_APP_KEY -R "$REPO"
printf '%s' "$SIGNING_PASSPHRASE" | gh secret set SIGNING_PASSPHRASE -R "$REPO"

echo "Secrets set on $REPO:"
gh secret list -R "$REPO"

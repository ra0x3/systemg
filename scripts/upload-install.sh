#!/bin/sh

set -e

S3_BUCKET="${S3_BUCKET:-sh.sysg.dev}"
VERSION="${SYSG_VERSION:-${1:-}}"
CLOUDFRONT_DISTRIBUTION_ID="${CLOUDFRONT_DISTRIBUTION_ID:-${SH_SYSG_DEV_CLOUDFRONT_DISTRIBUTION_ID:-}}"

aws s3 cp scripts/index.sh "s3://${S3_BUCKET}/index.sh" \
  --content-type "text/x-shellscript"
aws s3 cp scripts/index.sh "s3://${S3_BUCKET}/index.html" \
  --content-type "text/x-shellscript"

if [ -n "$VERSION" ]; then
  tmp_version="$(mktemp)"
  trap 'rm -f "$tmp_version"' EXIT
  printf '%s\n' "${VERSION#v}" > "$tmp_version"
  aws s3 cp "$tmp_version" "s3://${S3_BUCKET}/latest/VERSION" \
    --content-type "text/plain"
fi

if [ -n "$CLOUDFRONT_DISTRIBUTION_ID" ]; then
  invalidation_id="$(aws cloudfront create-invalidation \
    --distribution-id "$CLOUDFRONT_DISTRIBUTION_ID" \
    --paths "/" "/index.sh" "/index.html" "/latest/*" \
    --query 'Invalidation.Id' \
    --output text)"
  aws cloudfront wait invalidation-completed \
    --distribution-id "$CLOUDFRONT_DISTRIBUTION_ID" \
    --id "$invalidation_id"
fi

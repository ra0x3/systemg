#!/bin/sh

set -e

aws s3 cp ./scripts/install.sh s3://sysg/install.sh \
  --endpoint-url https://53f6d0a9276394a412625c6c6576e474.r2.cloudflarestorage.com \
  --region auto \
  --content-type "text/x-sh" \
  --profile sysg \
  --acl public-read

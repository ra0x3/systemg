#!/bin/sh

set -e

aws s3 cp scripts/index.sh s3://sh.sysg.dev/index.sh --content-type "text/x-sh"
#!/usr/bin/env bash

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
FAILED=0

while IFS= read -r file; do
  issues="$(perl -ne '
    $line = $_;
    $line =~ s{\[`?(SG\d{4})`?\]\(([^)]*)\)}{
      $whole = $&;
      $code = lc($1);
      $url = $2;
      $url =~ m{^(?:https://sysg\.dev)?/how-it-works/dialog/codes#$code$}
        ? ""
        : $whole;
    }gex;
    $line =~ s{<a\b[^>]*href="([^"]*)"[^>]*>(SG\d{4})</a>}{
      $whole = $&;
      $url = $1;
      $code = lc($2);
      $url =~ m{^(?:https://sysg\.dev)?/how-it-works/dialog/codes#$code$}
        ? ""
        : $whole;
    }gex;
    print "$ARGV:$.:$_" if $line =~ /SG\d{4}/;
  ' "${REPO_ROOT}/${file}")"
  if [ -n "${issues}" ]; then
    printf '%s' "${issues}"
    FAILED=1
  fi
done < <(cd "${REPO_ROOT}" && git ls-files '*.md' '*.mdx')

if [ "${FAILED}" -ne 0 ]; then
  exit 1
fi

printf 'All SG code references link to their canonical anchors.\n'

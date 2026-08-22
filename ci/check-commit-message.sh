#!/usr/bin/env bash
# Enforces the repo commit-message convention:
#   - a title of the form `type: subject` or `type(scope): subject`
#   - compound scopes are comma-separated in one paren:
#       `type(scope1, scope2, ...): subject`
#   - the title ONLY: no body, no trailing paragraphs
#   - no AI/tool trailers (Co-Authored-By: Claude, Generated with, 🤖, etc.)
#
# Usage:
#   ci/check-commit-message.sh                 # checks HEAD's message
#   ci/check-commit-message.sh "<subject>" ["<body>"]
set -u

# The set of prefix operations this repo has actually used (derived from
# `git log`), not a generic conventional-commit list. Add a new type here when
# the project genuinely adopts one.
TYPES='feat|fix|docs|test|refactor|perf|build|ci|chore|revert|style|rfc|audit|enhancement|cve|spike|misc|infra|admin'

# `type: subject`, or `type(scope): subject` — the scope is optional, and a
# compound scope is comma-separated in one paren. An optional '!' marks a
# breaking change.
SCOPE='[a-z0-9._/-]+'
SUBJECT_RE="^(${TYPES})(\(${SCOPE}( *, *${SCOPE})*\))?!?: .+"

# `release` sits outside TYPES: `release: <subject>` (typically a version).
RELEASE_RE="^release!?: .+"

fail() { echo "commit-message: $1" >&2; return 1; }

# Checks one commit's subject + body. Returns non-zero on violation.
check_one() {
  local subject="$1" body="$2" rc=0

  if ! printf '%s' "$subject" | grep -qE "$SUBJECT_RE" \
    && ! printf '%s' "$subject" | grep -qE "$RELEASE_RE"; then
    fail "subject must be 'type: subject' or 'type(scope): subject': '$subject'"
    rc=1
  fi

  # No body: the convention is a single title line only.
  if printf '%s' "$body" | grep -q '[^[:space:]]'; then
    fail "commit must have NO body (title line only): body present for '$subject'"
    rc=1
  fi

  # No AI/tool trailers anywhere in subject or body.
  if printf '%s\n%s' "$subject" "$body" \
      | grep -qiE 'co-authored-by:.*(claude|noreply@anthropic)|generated with \[?claude|🤖'; then
    fail "commit must not contain AI/tool trailers: '$subject'"
    rc=1
  fi

  return $rc
}

# Checks the given subject/body, or HEAD's if none is passed.
if [ "$#" -ge 1 ]; then
  subject="$1"
  body="${2:-}"
else
  subject="$(git show -s --format='%s' HEAD)"
  body="$(git show -s --format='%b' HEAD)"
fi

check_one "$subject" "$body" && echo "commit-message: OK"

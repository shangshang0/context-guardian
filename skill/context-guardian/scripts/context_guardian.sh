#!/bin/sh
set -eu
skill_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)
binary=${CONTEXT_GUARDIAN_BIN:-$skill_dir/target/release/context-guardian}
exec "$binary" "$@"

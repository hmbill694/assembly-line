#!/usr/bin/env bash
# Runs one job in the real image, against a bare repository mounted into the
# container, with a shell one-liner standing in for the agent. Proves the
# image boots `assembly run`, runs mise, claims, commits, pushes, and prints
# frames. The runner itself is covered by `cargo test`'s fakes; this covers
# what the fakes cannot: the image.
set -euo pipefail

image="${1:?usage: smoke-docker.sh <image>}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git init --quiet --initial-branch=main "$work/repo"
mkdir -p "$work/repo/.assembly"
cat > "$work/repo/.assembly/config.toml" <<'TOML'
provider = "sh"
verify = "test -f smoke.txt"

[providers.sh]
cmd = "sh"
args = ["-c", "echo smoke > smoke.txt"]

[delivery]
mode = "none"
TOML
git -C "$work/repo" add -A
git -C "$work/repo" -c user.name=smoke -c user.email=smoke@localhost commit --quiet -m base
git init --quiet --bare --initial-branch=main "$work/origin.git"
git -C "$work/repo" push --quiet "$work/origin.git" main
# The container runs as uid 1000 and must be able to push.
chmod -R a+rwX "$work/origin.git"

# The mounted repository belongs to this host's uid, not the container's
# 1000, and git refuses a repository it does not own unless told it is safe.
status=0
out=$(docker run --rm \
  -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0='*' \
  -v "$work/origin.git:/origin.git" "$image" \
  assembly run --repo=/origin.git --ref=main --prompt=smoke --frames --provision-toolchain) || status=$?
# Printed before judging, so a failed round shows the frames that say why.
echo "$out"
[ "$status" -eq 0 ] || { echo "smoke: assembly run exited $status" >&2; exit 1; }

grep -q '"t":"round_passed"' <<<"$out" || { echo "smoke: no round_passed frame" >&2; exit 1; }
grep -q smoke <<<"$(git -C "$work/origin.git" show al/job-1:smoke.txt)" || { echo "smoke: branch not pushed" >&2; exit 1; }
echo "smoke: ok"

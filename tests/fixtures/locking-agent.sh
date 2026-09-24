#!/usr/bin/env bash
# Leaves its work inside a directory it has made unwritable, which nothing
# can empty until write permission is given back.
set -euo pipefail
echo "locking-agent: $1"
mkdir locked
echo kept > locked/work.txt
chmod 555 locked

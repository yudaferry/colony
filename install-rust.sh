#!/usr/bin/env bash
# Download Rust 1.99.1 + crates (first run), compile, and run all tests.
# CPU limited like the user's Node.js builds: 2 cores, 1 job. Output also saved to test.log.
set -o pipefail
cd "$(dirname "$0")" || exit 1

nix --extra-experimental-features 'nix-command flakes' \
  develop path:. --cores 2 --max-jobs 1 \
  --command cargo test -j 2 2>&1 | tee test.log

#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: fuzz/run-target.sh <target> <runs|seconds> <budget> <seed>" >&2
  exit 2
fi

target=$1
mode=$2
budget=$3
seed=$4
fuzz_toolchain=${HEYCODE_FUZZ_TOOLCHAIN:-nightly}

if [[ ! $budget =~ ^[1-9][0-9]{0,5}$ ]]; then
  echo "budget must be a positive bounded integer" >&2
  exit 2
fi
if [[ ! $seed =~ ^[0-9]{1,10}$ ]] || (( 10#$seed > 4294967295 )); then
  echo "seed must be an unsigned 32-bit integer" >&2
  exit 2
fi

case "$target" in
  session_event_parser|config_parser|mcp_protocol_parser)
    max_len=65536
    timeout_seconds=5
    ;;
  provider_stream_parser)
    max_len=32768
    timeout_seconds=5
    ;;
  render_parser)
    max_len=32768
    timeout_seconds=20
    ;;
  *)
    echo "unknown fuzz target" >&2
    exit 2
    ;;
esac

case "$mode" in
  runs)
    if (( 10#$budget > 100000 )); then
      echo "run budget exceeds 100000" >&2
      exit 2
    fi
    budget_flag="-runs=$budget"
    build_flag=--dev
    ;;
  seconds)
    if (( 10#$budget > 900 )); then
      echo "time budget exceeds 900 seconds" >&2
      exit 2
    fi
    budget_flag="-max_total_time=$budget"
    build_flag=--release
    ;;
  *)
    echo "mode must be runs or seconds" >&2
    exit 2
    ;;
esac

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
artifact_dir=$(mktemp -d "${TMPDIR:-/tmp}/heycode-fuzz-artifacts.XXXXXX")
corpus_dir="$artifact_dir/corpus"
mkdir -- "$corpus_dir"
cp -R -- "$repo_root/corpora/$target/." "$corpus_dir/"

cleanup() {
  case "$artifact_dir" in
    */heycode-fuzz-artifacts.*)
      rm -rf -- "$artifact_dir"
      ;;
  esac
}
trap cleanup EXIT

cd -- "$repo_root"
cargo +"$fuzz_toolchain" fuzz run \
  --fuzz-dir "$repo_root/fuzz" \
  "$build_flag" \
  --features "$target" \
  "$target" "$corpus_dir" -- \
  "$budget_flag" \
  "-seed=$seed" \
  "-max_len=$max_len" \
  "-timeout=$timeout_seconds" \
  -rss_limit_mb=2048 \
  -malloc_limit_mb=1024 \
  "-artifact_prefix=$artifact_dir/"

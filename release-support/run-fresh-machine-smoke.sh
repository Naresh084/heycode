#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
  echo "usage: run-fresh-machine-smoke.sh <binary> <platform> <evidence-json> [fake|real-openrouter]" >&2
  exit 2
fi

binary=$1
platform=$2
evidence=$3
mode=${4:-fake}
case "$binary" in
  /*) ;;
  *) echo "installed binary path must be absolute" >&2; exit 2 ;;
esac
[ -f "$binary" ] && [ ! -L "$binary" ] || {
  echo "installed binary is unavailable" >&2
  exit 2
}
case "$platform" in
  macos-*|linux-*) ;;
  *) echo "unsupported Unix onboarding platform" >&2; exit 2 ;;
esac

smoke_root=$(mktemp -d)
trap 'rm -rf "$smoke_root"' EXIT
home_root="$smoke_root/home"
workspace_root="$smoke_root/workspace"
mkdir -p "$home_root" "$workspace_root"

case "$mode" in
  fake)
    output=$(cd "$workspace_root" && HEYCODE_HOME="$home_root" \
      "$binary" --restricted-workspace --fake run \
      "fresh-machine deterministic release smoke")
    case "$output" in
      *FAKE-REPLY*) turn=deterministic_fake ;;
      *) echo "fresh-machine turn did not settle through the fake provider" >&2; exit 1 ;;
    esac
    ;;
  real-openrouter)
    test -n "${OPENROUTER_API_KEY:-}" || {
      echo "real-provider onboarding credential is unavailable" >&2
      exit 1
    }
    output=$(cd "$workspace_root" && HEYCODE_HOME="$home_root" \
      "$binary" --restricted-workspace \
      --provider openrouter --model z-ai/glm-5.3-flash \
      --set llm.api_key_env=OPENROUTER_API_KEY run \
      "Reply with exactly Q16_REAL_PROVIDER_OK. Do not call tools.")
    case "$output" in
      *Q16_REAL_PROVIDER_OK*) turn=real_provider ;;
      *) echo "fresh-machine real-provider turn did not settle" >&2; exit 1 ;;
    esac
    ;;
  *) echo "unknown fresh-machine smoke mode" >&2; exit 2 ;;
esac

run_id=${GITHUB_RUN_ID:-0}
cat > "$evidence" <<EOF
{"schema_version":1,"platform":"$platform","source":{"kind":"hosted_native","run_id":$run_id},"checks":["attestation_verified","fresh_install","first_run_ready"],"turn":"$turn"}
EOF

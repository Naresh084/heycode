#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: run-local-release-transaction.sh <built-heycode> <evidence-json>" >&2
  exit 2
fi

artifact=$1
evidence=$2
case "$artifact" in
  /*) ;;
  *) echo "built heycode path must be absolute" >&2; exit 2 ;;
esac
case "$evidence" in
  /*) ;;
  *) echo "evidence path must be absolute" >&2; exit 2 ;;
esac
[ -f "$artifact" ] && [ ! -L "$artifact" ] || {
  echo "built heycode artifact is unavailable" >&2
  exit 2
}
[ ! -e "$evidence" ] || {
  echo "evidence destination already exists" >&2
  exit 2
}

HEYCODE_LOCAL_RELEASE_ARTIFACT="$artifact" \
HEYCODE_LOCAL_RELEASE_EVIDENCE="$evidence" \
  cargo test -p heycode-install --features local-release-evidence --test main \
  it::manager::local_built_heycode_runs_the_complete_content_withheld_release_transaction \
  -- --exact

python3 - "$evidence" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    evidence = json.load(handle)

expected = {
    "schema_version": 1,
    "artifact": "local_built_heycode",
    "artifact_content": "withheld",
    "signature_evidence": "deterministic_fixture_not_github",
    "external_github_attestation_observed": False,
    "external_real_provider_turn_observed": False,
    "checks": [
        "fresh_install",
        "installed_artifact_fake_turn",
        "stable_preview_refusal",
        "plugin_api_update_refusal",
        "stable_update",
        "plugin_api_rollback_refusal",
        "directional_rollback",
    ],
}
if evidence != expected:
    raise SystemExit("local release evidence is not the exact content-withheld schema")
PY

echo "local release transaction evidence is valid"

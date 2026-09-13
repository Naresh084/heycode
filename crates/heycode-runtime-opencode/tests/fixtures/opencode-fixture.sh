#!/bin/sh
set -eu

if [ "${1:-}" = "--version" ]; then
  printf '%s\n' "${OPENCODE_FIXTURE_VERSION:-1.18.21}"
  exit 0
fi

if [ "${1:-}" != "acp" ]; then
  exit 64
fi

while IFS= read -r frame; do
  case "$frame" in
    *'"method":"initialize"'*)
      agent_version="${OPENCODE_FIXTURE_AGENT_VERSION:-1.18.21}"
      printf '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"sessionCapabilities":{"close":{},"resume":{}}},"authMethods":[],"agentInfo":{"name":"OpenCode","version":"%s"}}}\n' "$agent_version"
      ;;
    *'"method":"session/new"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"opencode-fixture","configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"opencode-go/glm-5.3-flash","options":[{"value":"opencode-go/glm-5.3-flash","name":"GLM-5.3-Flash"},{"value":"openrouter/z-ai/glm-5.3","name":"GLM-5.3"}]}]}}'
      ;;
    *'"method":"session/close"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{}}'
      ;;
    *)
      exit 65
      ;;
  esac
done

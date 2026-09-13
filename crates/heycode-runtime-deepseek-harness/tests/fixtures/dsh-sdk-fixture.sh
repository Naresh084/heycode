#!/bin/sh
set -eu

while IFS= read -r frame; do
  case "$frame" in
    *'"method":"initialize"'*)
      version="${DSH_FIXTURE_VERSION:-0.0.1}"
      printf '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"deepseek-harness-sdk-runtime","version":"%s"}}}\n' "$version"
      ;;
    *'"method":"session/prompt"'*)
      if [ "${DSH_FIXTURE_HANG:-}" = "1" ]; then
        continue
      fi
      if [ "${DSH_FIXTURE_INITIAL_IDLE:-}" = "1" ]; then
        printf '%s\n' '{"jsonrpc":"2.0","method":"session.status","params":{"sessionId":"dsh-fixture-session","status":"idle"}}'
      fi
      printf '%s\n' '{"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"dsh-fixture-session","event":{"type":"agent/inbox/spliced","seq":0,"time":1,"data":{"target":"next-turn","start":0,"inserted":[{"id":"user-1","role":"user","content":[],"source":{"kind":"user"}}]}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session.status","params":{"sessionId":"dsh-fixture-session","status":"running"}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"dsh-fixture-session","event":{"type":"turn/start","seq":1,"time":2,"data":{"turn":7}}}}'
      if [ "${DSH_FIXTURE_GAP:-}" = "1" ]; then
        seq=9
      else
        seq=2
      fi
      printf '{"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"dsh-fixture-session","event":{"type":"assistant/chunk","seq":%s,"time":3,"data":{"turn":7,"step":1,"chunk":{"type":"text-delta","index":0,"text":"working"}}}}}\n' "$seq"
      printf '%s\n' '{"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"dsh-fixture-session","event":{"type":"tool/call","seq":3,"time":4,"data":{"turn":7,"step":1,"callId":"call-1","name":"read","arguments":"{\"path\":\"README.md\"}"}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"dsh-fixture-session","event":{"type":"tool/result","seq":4,"time":5,"data":{"turn":7,"step":1,"message":{"role":"tool","toolCallId":"call-1","content":[{"type":"text","text":"fixture result"}]}}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"dsh-fixture-session","event":{"type":"assistant/message","seq":5,"time":6,"data":{"turn":7,"step":1,"message":{"id":"assistant-1","role":"assistant","content":[{"type":"text","text":"done"}],"source":{"kind":"model","provider":"fixture","model":"fixture"}},"usage":{"inputTokens":5,"outputTokens":2,"cacheReadTokens":3,"cacheWriteTokens":1}}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"dsh-fixture-session","event":{"type":"turn/end","seq":6,"time":7,"data":{"turn":7,"reason":{"kind":"completed"}}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session.status","params":{"sessionId":"dsh-fixture-session","status":"idle"}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"messageId":"user-1"}}'
      ;;
    *'"method":"shutdown"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{}}'
      exit 0
      ;;
    *)
      exit 65
      ;;
  esac
done

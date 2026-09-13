#!/usr/bin/env python3
"""Disposable stdio MCP fixture. Serves synthetic inline data without network or disk access."""
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    values = {
        "initialize": {"protocolVersion":"2024-11-05","capabilities":{"resources":{},"tools":{}},"serverInfo":{"name":"synthetic-terminal-reference","version":"1.0"}},
        "ping": {},
        "tools/list": {"tools":[{"name":"echo_fixture","description":"Return synthetic reference text","inputSchema":{"type":"object","properties":{}},"annotations":{"readOnlyHint":True,"destructiveHint":False,"openWorldHint":False}}]},
        "tools/call": {"content":[{"type":"text","text":"SYNTHETIC_MCP_TOOL_RESULT"}]},
        "resources/list": {"resources":[{"uri":"fixture://local/sample","name":"Synthetic fixture resource","mimeType":"text/plain"}]},
        "resources/templates/list": {"resourceTemplates":[]},
        "resources/read": {"contents":[{"uri":"fixture://local/sample","mimeType":"text/plain","text":"SYNTHETIC_MCP_RESOURCE_CONTENT"}]},
    }
    response = {"jsonrpc":"2.0","id":request["id"]}
    if method in values:
        response["result"] = values[method]
    else:
        response["error"] = {"code":-32601,"message":"Synthetic fixture method unavailable"}
    print(json.dumps(response),flush=True)

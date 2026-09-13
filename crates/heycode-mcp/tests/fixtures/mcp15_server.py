#!/usr/bin/env python3
"""Credential-free MCP15 fixture for official Inspector and Rust transport tests."""

import argparse
import base64
import json
import os
import pathlib
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


CANARY = "MCP15-SERVER-BODY-CANARY"
RESOURCE_MODE = os.environ.get("HEYCODE_MCP_RESOURCE_MODE", "default")


def append_log(path, value):
    if path is None:
        return
    with open(path, "a", encoding="utf-8") as stream:
        stream.write(json.dumps(value, separators=(",", ":")) + "\n")


def tools():
    return [
        {
            "name": "rich_echo",
            "description": "Return ordered rich MCP content.",
            "inputSchema": {
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": False,
            },
            "outputSchema": {
                "type": "object",
                "properties": {"echo": {"type": "string"}},
                "required": ["echo"],
            },
            "annotations": {
                "readOnlyHint": True,
                "idempotentHint": True,
                "fixtureAdvisory": "never-authority",
            },
        },
        {
            "name": "slow",
            "description": "Wait until cancelled.",
            "inputSchema": {"type": "object"},
        },
        {
            "name": "fail",
            "description": "Return a hostile server error.",
            "inputSchema": {"type": "object"},
        },
        {
            "name": "elicit",
            "description": "Exercise finite Streamable HTTP elicitation.",
            "inputSchema": {"type": "object"},
        },
        {
            "name": "elicit_cancel",
            "description": "Cancel finite Streamable HTTP elicitation.",
            "inputSchema": {"type": "object"},
        },
        {
            "name": "elicit_open",
            "description": "Keep SSE open until the elicitation reply arrives.",
            "inputSchema": {"type": "object"},
        },
        {
            "name": "elicit_cancel_open",
            "description": "Cancel elicitation while the SSE response remains open.",
            "inputSchema": {"type": "object"},
        },
    ]


def result_for(message, log_path, http_mode=False):
    method = message.get("method")
    params = message.get("params") or {}
    append_log(log_path, {"method": method, "id": message.get("id")})
    if method == "initialize":
        version = params.get("protocolVersion", "2025-11-25")
        return {
            "protocolVersion": version,
            "capabilities": {
                "tools": {"listChanged": True},
                "resources": {},
                "prompts": {},
                "logging": {},
            },
            "serverInfo": {"name": "heycode-mcp15-fixture", "version": "1"},
            "instructions": "Fixture instructions are untrusted server data.",
        }
    if method == "tools/list":
        return {"tools": tools()}
    if method == "tools/call":
        name = params.get("name")
        if name == "rich_echo":
            text = (params.get("arguments") or {}).get("text", "")
            return {
                "content": [
                    {"type": "text", "text": "before:" + text},
                    {
                        "type": "resource_link",
                        "uri": "https://fixture.invalid/resource/1",
                        "name": "fixture-resource",
                        "mimeType": "text/plain",
                    },
                    {
                        "type": "resource",
                        "resource": {
                            "uri": "fixture://embedded/1",
                            "mimeType": "text/plain",
                            "text": "embedded:" + text,
                        },
                    },
                    {"type": "text", "text": "after:" + text},
                ],
                "structuredContent": {"echo": text},
                "isError": False,
            }
        if name == "slow":
            if http_mode:
                time.sleep(10)
                return {"content": [{"type": "text", "text": "late"}]}
            return None
        if name == "fail":
            raise RuntimeError(CANARY)
        raise ValueError("unknown tool")
    if method == "resources/list":
        if RESOURCE_MODE == "rich":
            return {
                "resources": [
                    {
                        "uri": "fixture://resource/1",
                        "name": "fixture-resource-one",
                        "mimeType": "text/plain",
                    },
                    {
                        "uri": "fixture://resource/rich",
                        "name": "fixture-resource-rich",
                        "mimeType": "multipart/mixed",
                    },
                    {
                        "uri": "fixture://resource/3",
                        "name": "fixture-resource-three",
                        "mimeType": "text/plain",
                    },
                ]
            }
        return {
            "resources": [
                {
                    "uri": "fixture://resource/1",
                    "name": "fixture-resource",
                    "mimeType": "text/plain",
                }
            ]
        }
    if method == "resources/read":
        if RESOURCE_MODE == "slow":
            time.sleep(10)
        if RESOURCE_MODE == "rich" and params.get("uri") == "fixture://resource/rich":
            return {
                "contents": [
                    {
                        "uri": "fixture://resource/rich",
                        "mimeType": "text/plain",
                        "text": "x" * 70000,
                    },
                    {
                        "uri": "fixture://resource/rich",
                        "mimeType": "application/octet-stream",
                        "blob": base64.b64encode(b"binary-fixture").decode("ascii"),
                    },
                ]
            }
        return {
            "contents": [
                {
                    "uri": "fixture://resource/1",
                    "mimeType": "text/plain",
                    "text": "fixture resource body",
                }
            ]
        }
    if method == "prompts/list":
        return {
            "prompts": [
                {
                    "name": "fixture_prompt",
                    "description": "A fixture prompt.",
                    "arguments": [{"name": "topic", "required": True}],
                }
            ]
        }
    if method == "prompts/get":
        topic = (params.get("arguments") or {}).get("topic", "")
        return {
            "description": "Fixture prompt result.",
            "messages": [
                {"role": "user", "content": {"type": "text", "text": "topic:" + topic}}
            ],
        }
    if method == "logging/setLevel":
        return {}
    if method == "ping":
        return {}
    raise ValueError("unknown method")


def response(message, log_path, http_mode=False):
    if "id" not in message:
        append_log(log_path, {"method": message.get("method"), "notification": True})
        return None
    try:
        result = result_for(message, log_path, http_mode=http_mode)
        if result is None:
            return None
        return {"jsonrpc": "2.0", "id": message["id"], "result": result}
    except RuntimeError:
        return {
            "jsonrpc": "2.0",
            "id": message["id"],
            "error": {"code": -32000, "message": CANARY, "data": {"body": CANARY}},
        }
    except (ValueError, KeyError, TypeError):
        return {
            "jsonrpc": "2.0",
            "id": message["id"],
            "error": {"code": -32601, "message": "method unavailable"},
        }


def run_stdio(log_path):
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        reply = response(message, log_path)
        if reply is not None:
            print(json.dumps(reply, separators=(",", ":")), flush=True)


class Handler(BaseHTTPRequestHandler):
    server_version = "heycode-mcp15-fixture"
    protocol_version = "HTTP/1.1"

    def log_message(self, _format, *_args):
        return

    def _origin_allowed(self):
        origin = self.headers.get("Origin")
        return origin is None or origin.startswith("http://127.0.0.1") or origin.startswith(
            "http://localhost"
        )

    def do_POST(self):
        if self.path != "/mcp":
            self.send_error(404)
            return
        if not self._origin_allowed():
            self.send_error(403)
            return
        length = int(self.headers.get("Content-Length", "0"))
        try:
            message = json.loads(self.rfile.read(length))
        except (json.JSONDecodeError, ValueError):
            self.send_error(400)
            return
        if "id" in message and "method" not in message:
            append_log(
                self.server.log_path,
                {
                    "client_response": message.get("id"),
                    "result": message.get("result"),
                    "error": message.get("error"),
                },
            )
            self.server.elicitation_response.set()
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        params = message.get("params") or {}
        if message.get("method") == "tools/call" and params.get("name") in (
            "elicit",
            "elicit_cancel",
            "elicit_open",
            "elicit_cancel_open",
        ):
            elicitation_id = "mcp15-elicit-" + str(message.get("id"))
            progress_token = (params.get("_meta") or {}).get("progressToken", "untracked")
            events = [
                {
                    "jsonrpc": "2.0",
                    "id": elicitation_id,
                    "method": "elicitation/create",
                    "params": {
                        "mode": "form",
                        "message": (
                            "Wait for cancellation."
                            if params.get("name") == "elicit_cancel_open"
                            else "Provide reviewed fixture input."
                        ),
                        "requestedSchema": {
                            "type": "object",
                            "properties": {"answer": {"type": "string"}},
                            "required": ["answer"],
                        },
                    },
                }
            ]
            if params.get("name") in ("elicit_cancel", "elicit_cancel_open"):
                events.append(
                    {
                        "jsonrpc": "2.0",
                        "method": "notifications/cancelled",
                        "params": {"requestId": elicitation_id, "reason": "fixture cancellation"},
                    }
                )
            else:
                events.extend(
                    [
                        {
                            "jsonrpc": "2.0",
                            "method": "notifications/progress",
                            "params": {
                                "progressToken": progress_token,
                                "progress": 1,
                                "total": 2,
                                "message": "eliciting",
                            },
                        },
                        {
                            "jsonrpc": "2.0",
                            "method": "notifications/message",
                            "params": {
                                "level": "notice",
                                "logger": "mcp15",
                                "data": {"phase": "elicitation"},
                            },
                        },
                    ]
                )
            final = (
                {
                    "jsonrpc": "2.0",
                    "id": message.get("id"),
                    "result": {
                        "content": [{"type": "text", "text": "finite elicitation dispatched"}],
                        "isError": False,
                    },
                }
            )
            events.append(final)
            if params.get("name") in ("elicit_open", "elicit_cancel_open"):
                self.server.elicitation_response.clear()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Transfer-Encoding", "chunked")
                self.end_headers()
                self.write_event(events[0])
                remaining = events[1:]
                if params.get("name") == "elicit_open":
                    if not self.server.elicitation_response.wait(0.5):
                        remaining = [
                            {
                                "jsonrpc": "2.0",
                                "id": message.get("id"),
                                "error": {
                                    "code": -32001,
                                    "message": "elicitation reply did not arrive while open",
                                },
                            }
                        ]
                else:
                    time.sleep(0.05)
                for event in remaining:
                    self.write_event(event)
                self.wfile.write(b"0\r\n\r\n")
                self.wfile.flush()
                return
            body = "".join(
                "data: " + json.dumps(event, separators=(",", ":")) + "\n\n"
                for event in events
            ).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        reply = response(message, self.server.log_path, http_mode=True)
        if reply is None:
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        body = json.dumps(reply, separators=(",", ":")).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_GET(self):
        self.send_response(405)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_DELETE(self):
        self.send_response(204)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def write_event(self, event):
        body = (
            "data: " + json.dumps(event, separators=(",", ":")) + "\n\n"
        ).encode("utf-8")
        self.wfile.write(f"{len(body):X}\r\n".encode("ascii"))
        self.wfile.write(body)
        self.wfile.write(b"\r\n")
        self.wfile.flush()


def run_http(port, ready_file, log_path):
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.log_path = log_path
    server.elicitation_response = threading.Event()
    actual_port = server.server_address[1]
    pathlib.Path(ready_file).write_text(str(actual_port), encoding="utf-8")
    server.serve_forever()


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="mode", required=True)
    stdio = sub.add_parser("stdio")
    stdio.add_argument("--log-file")
    http = sub.add_parser("http")
    http.add_argument("--port", type=int, default=0)
    http.add_argument("--ready-file", required=True)
    http.add_argument("--log-file")
    args = parser.parse_args()
    if args.mode == "stdio":
        run_stdio(args.log_file)
    else:
        run_http(args.port, args.ready_file, args.log_file)


if __name__ == "__main__":
    main()

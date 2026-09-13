#!/usr/bin/env python3
"""Stateful local OAuth authorization/resource-server fixture for MCP15."""

import argparse
import base64
import hashlib
import json
import pathlib
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


CANARY = "MCP15-OAUTH-BODY-CANARY"
ACCESS_TOKEN = "mcp15-local-access"
REFRESH_TOKEN = "mcp15-local-refresh"
AUTH_CODE = "mcp15-local-code"


class State:
    def __init__(self, port, log_path):
        self.port = port
        self.log_path = log_path
        self.lock = threading.Lock()
        self.pending = None

    @property
    def issuer(self):
        return f"https://auth.fixture.test:{self.port}"

    @property
    def resource(self):
        return f"https://resource.fixture.test:{self.port}/mcp"

    def log(self, value):
        if self.log_path is None:
            return
        with self.lock:
            with open(self.log_path, "a", encoding="utf-8") as stream:
                stream.write(json.dumps(value, separators=(",", ":")) + "\n")


class Handler(BaseHTTPRequestHandler):
    server_version = "heycode-mcp15-oauth-fixture"

    def log_message(self, _format, *_args):
        return

    @property
    def state(self):
        return self.server.fixture_state

    def send_json(self, status, value, headers=None):
        body = json.dumps(value, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for name, header_value in (headers or {}).items():
            self.send_header(name, header_value)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        parsed = urllib.parse.urlsplit(self.path)
        self.state.log({"method": "GET", "path": parsed.path})
        if parsed.path == "/.well-known/oauth-protected-resource/mcp":
            self.send_json(
                200,
                {
                    "resource": self.state.resource,
                    "authorization_servers": [self.state.issuer],
                    "scopes_supported": ["mcp:read", "mcp:tools"],
                },
            )
            return
        if parsed.path == "/.well-known/oauth-protected-resource":
            self.send_error(404)
            return
        if parsed.path == "/.well-known/oauth-authorization-server":
            self.send_json(
                200,
                {
                    "issuer": self.state.issuer,
                    "authorization_endpoint": self.state.issuer + "/authorize",
                    "token_endpoint": self.state.issuer + "/token",
                    "code_challenge_methods_supported": ["S256"],
                    "authorization_response_iss_parameter_supported": True,
                },
            )
            return
        if parsed.path == "/authorize":
            query = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
            required = [
                "response_type",
                "client_id",
                "redirect_uri",
                "code_challenge",
                "code_challenge_method",
                "state",
                "resource",
            ]
            if any(key not in query for key in required):
                self.send_json(400, {"error": CANARY})
                return
            if (
                query["response_type"] != ["code"]
                or query["client_id"] != ["mcp15-client"]
                or query["code_challenge_method"] != ["S256"]
                or query["resource"] != [self.state.resource]
            ):
                self.send_json(400, {"error": CANARY})
                return
            with self.state.lock:
                self.state.pending = {
                    "challenge": query["code_challenge"][0],
                    "redirect_uri": query["redirect_uri"][0],
                    "state": query["state"][0],
                    "resource": query["resource"][0],
                }
            location = (
                query["redirect_uri"][0]
                + "?"
                + urllib.parse.urlencode(
                    {"code": AUTH_CODE, "state": query["state"][0], "iss": self.state.issuer}
                )
            )
            self.send_response(302)
            self.send_header("Location", location)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self.send_error(404)

    def do_POST(self):
        parsed = urllib.parse.urlsplit(self.path)
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        self.state.log({"method": "POST", "path": parsed.path, "body_len": len(body)})
        if parsed.path == "/token":
            form = urllib.parse.parse_qs(body.decode("utf-8"), keep_blank_values=True)
            grant = form.get("grant_type", [None])[0]
            if grant == "authorization_code":
                with self.state.lock:
                    pending = dict(self.state.pending or {})
                verifier = form.get("code_verifier", [""])[0]
                challenge = base64.urlsafe_b64encode(
                    hashlib.sha256(verifier.encode("utf-8")).digest()
                ).rstrip(b"=").decode("ascii")
                valid = (
                    form.get("code") == [AUTH_CODE]
                    and form.get("client_id") == ["mcp15-client"]
                    and form.get("redirect_uri") == [pending.get("redirect_uri")]
                    and form.get("resource") == [self.state.resource]
                    and challenge == pending.get("challenge")
                )
                if not valid:
                    self.send_json(400, {"error": CANARY, "detail": body.decode("utf-8")})
                    return
                self.send_json(
                    200,
                    {
                        "access_token": ACCESS_TOKEN,
                        "refresh_token": REFRESH_TOKEN,
                        "token_type": "Bearer",
                        "expires_in": 300,
                    },
                )
                return
            if grant == "refresh_token":
                valid = (
                    form.get("refresh_token") == [REFRESH_TOKEN]
                    and form.get("client_id") == ["mcp15-client"]
                    and form.get("resource") == [self.state.resource]
                )
                if not valid:
                    self.send_json(400, {"error": CANARY})
                    return
                self.send_json(
                    200,
                    {
                        "access_token": ACCESS_TOKEN + "-refreshed",
                        "refresh_token": REFRESH_TOKEN,
                        "token_type": "Bearer",
                        "expires_in": 300,
                    },
                )
                return
            self.send_json(400, {"error": CANARY})
            return
        if parsed.path == "/mcp":
            authorization = self.headers.get("Authorization")
            if authorization not in (
                "Bearer " + ACCESS_TOKEN,
                "Bearer " + ACCESS_TOKEN + "-refreshed",
            ):
                self.send_json(
                    401,
                    {"error": CANARY},
                    {
                        "WWW-Authenticate": (
                            f'Bearer resource_metadata="{self.state.resource.replace("/mcp", "/.well-known/oauth-protected-resource/mcp")}", '
                            'scope="mcp:read mcp:tools"'
                        )
                    },
                )
                return
            request = json.loads(body)
            self.send_json(
                200,
                {
                    "jsonrpc": "2.0",
                    "id": request.get("id"),
                    "result": {
                        "protocolVersion": request.get("params", {}).get(
                            "protocolVersion", "2025-11-25"
                        ),
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "oauth-fixture", "version": "1"},
                    },
                },
            )
            return
        self.send_error(404)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready-file", required=True)
    parser.add_argument("--log-file")
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = server.server_address[1]
    server.fixture_state = State(port, args.log_file)
    pathlib.Path(args.ready_file).write_text(str(port), encoding="utf-8")
    server.serve_forever()


if __name__ == "__main__":
    main()

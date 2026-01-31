#!/usr/bin/env python3
"""
Simple HTTP reverse proxy with Basic Auth.

Listens on PROXY_PORT (default 4097), requires Basic Auth, and forwards
requests to UPSTREAM_URL (default http://localhost:4096).

This is a workaround for https://github.com/anomalyco/opencode/issues/8458
where `opencode attach` doesn't support OPENCODE_SERVER_PASSWORD for auth.

Environment variables:
  OPENCODE_SERVER_PASSWORD - Required password for Basic Auth (username: opencode)
  AUTH_PROXY_PORT - Port to listen on (default: 4097)
  AUTH_PROXY_UPSTREAM - Upstream URL (default: http://localhost:4096)
"""

import base64
import http.client
import os
import sys
import urllib.parse
from http.server import HTTPServer, BaseHTTPRequestHandler

PASSWORD = os.environ.get("OPENCODE_SERVER_PASSWORD", "")
PROXY_PORT = int(os.environ.get("AUTH_PROXY_PORT", "4097"))
UPSTREAM = os.environ.get("AUTH_PROXY_UPSTREAM", "http://localhost:4096")

# Parse upstream URL
upstream_parsed = urllib.parse.urlparse(UPSTREAM)
UPSTREAM_HOST = upstream_parsed.hostname or "localhost"
UPSTREAM_PORT = upstream_parsed.port or 4096


class AuthProxyHandler(BaseHTTPRequestHandler):
    """HTTP handler that checks Basic Auth and proxies to upstream."""

    def check_auth(self) -> bool:
        """Verify Basic Auth credentials."""
        if not PASSWORD:
            return True  # No password configured, allow all

        auth_header = self.headers.get("Authorization", "")
        if not auth_header.startswith("Basic "):
            return False

        try:
            credentials = base64.b64decode(auth_header[6:]).decode("utf-8")
            username, password = credentials.split(":", 1)
            return username == "opencode" and password == PASSWORD
        except Exception:
            return False

    def proxy_request(self, method: str, body: bytes | None = None):
        """Forward request to upstream server."""
        if not self.check_auth():
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="opencode"')
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(b"Unauthorized\n")
            return

        # Connect to upstream
        try:
            conn = http.client.HTTPConnection(UPSTREAM_HOST, UPSTREAM_PORT, timeout=300)

            # Forward headers (except Host and Authorization)
            headers = {}
            for key, value in self.headers.items():
                if key.lower() not in ("host", "authorization"):
                    headers[key] = value

            conn.request(method, self.path, body=body, headers=headers)
            response = conn.getresponse()

            # Forward response
            self.send_response(response.status)
            for key, value in response.getheaders():
                if key.lower() != "transfer-encoding":
                    self.send_header(key, value)
            self.end_headers()

            # Stream response body
            while True:
                chunk = response.read(8192)
                if not chunk:
                    break
                self.wfile.write(chunk)

            conn.close()
        except Exception as e:
            self.send_response(502)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(f"Proxy error: {e}\n".encode())

    def do_GET(self):
        self.proxy_request("GET")

    def do_POST(self):
        content_length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_length) if content_length else None
        self.proxy_request("POST", body)

    def do_PUT(self):
        content_length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_length) if content_length else None
        self.proxy_request("PUT", body)

    def do_PATCH(self):
        content_length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_length) if content_length else None
        self.proxy_request("PATCH", body)

    def do_DELETE(self):
        self.proxy_request("DELETE")

    def do_OPTIONS(self):
        self.proxy_request("OPTIONS")

    def log_message(self, format, *args):
        """Suppress default logging."""
        pass


def main():
    if not PASSWORD:
        print("Warning: OPENCODE_SERVER_PASSWORD not set, proxy will allow all requests",
              file=sys.stderr)

    server = HTTPServer(("127.0.0.1", PROXY_PORT), AuthProxyHandler)
    print(f"Auth proxy listening on 127.0.0.1:{PROXY_PORT} -> {UPSTREAM}", file=sys.stderr)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    server.server_close()


if __name__ == "__main__":
    main()

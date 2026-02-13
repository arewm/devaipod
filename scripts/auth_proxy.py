#!/usr/bin/env python3
"""
Simple HTTP reverse proxy with Basic Auth and cookie-based sessions.

Listens on PROXY_PORT (default 4097), requires authentication, and forwards
requests to UPSTREAM_URL (default http://localhost:4096).

Authentication methods (checked in order):
1. Cookie: devaipod_session=PASSWORD (set after successful auth)
2. Query param: ?token=PASSWORD (sets session cookie on success)
3. Basic Auth: Authorization: Basic base64(opencode:PASSWORD)

The cookie/token approach allows browsers to access the opencode web UI,
which dynamically imports ES modules that can't include credentials in URLs.

This is a workaround for https://github.com/anomalyco/opencode/issues/8458
where `opencode attach` doesn't support password-based auth.

Environment variables:
  DEVAIPOD_PROXY_PASSWORD - Required password for auth (username: opencode)
  AUTH_PROXY_PORT - Port to listen on (default: 4097)
  AUTH_PROXY_UPSTREAM - Upstream URL (default: http://localhost:4096)
"""

import base64
import http.client
import os
import sys
import urllib.parse
from http.server import HTTPServer, BaseHTTPRequestHandler, ThreadingHTTPServer

PASSWORD = os.environ.get("DEVAIPOD_PROXY_PASSWORD", "")
PROXY_PORT = int(os.environ.get("AUTH_PROXY_PORT", "4097"))
UPSTREAM = os.environ.get("AUTH_PROXY_UPSTREAM", "http://localhost:4096")

# Parse upstream URL
upstream_parsed = urllib.parse.urlparse(UPSTREAM)
UPSTREAM_HOST = upstream_parsed.hostname or "localhost"
UPSTREAM_PORT = upstream_parsed.port or 4096


class AuthProxyHandler(BaseHTTPRequestHandler):
    """HTTP handler that checks auth (cookie, token, or Basic) and proxies to upstream."""

    # Track whether we should set a session cookie in the response
    _set_session_cookie = False

    def check_auth(self) -> bool:
        """Verify credentials via cookie, query param token, or Basic Auth."""
        self._set_session_cookie = False

        if not PASSWORD:
            return True  # No password configured, allow all

        # 1. Check session cookie first (most common for browser requests)
        cookie_header = self.headers.get("Cookie", "")
        for cookie in cookie_header.split(";"):
            cookie = cookie.strip()
            if cookie.startswith("devaipod_session="):
                token = cookie[len("devaipod_session="):]
                if token == PASSWORD:
                    return True

        # 2. Check query parameter token (for initial browser access)
        parsed = urllib.parse.urlparse(self.path)
        query_params = urllib.parse.parse_qs(parsed.query)
        if "token" in query_params:
            token = query_params["token"][0]
            if token == PASSWORD:
                # Set cookie so subsequent requests don't need token
                self._set_session_cookie = True
                return True

        # 3. Check Basic Auth header (for API/curl access)
        auth_header = self.headers.get("Authorization", "")
        if auth_header.startswith("Basic "):
            try:
                credentials = base64.b64decode(auth_header[6:]).decode("utf-8")
                username, password = credentials.split(":", 1)
                if username == "opencode" and password == PASSWORD:
                    return True
            except Exception:
                pass

        return False

    def proxy_request(self, method: str, body: bytes | None = None):
        """Forward request to upstream server."""
        if not self.check_auth():
            self.send_response(401)
            # Only send WWW-Authenticate for HTML requests, not API calls
            # This prevents browser signin dialogs for cross-origin API requests
            accept = self.headers.get("Accept", "")
            if "text/html" in accept and "application/json" not in accept:
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
            # Set session cookie if auth was via token param
            if self._set_session_cookie:
                # HttpOnly for security, SameSite=Lax for CSRF protection
                self.send_header(
                    "Set-Cookie",
                    f"devaipod_session={PASSWORD}; Path=/; HttpOnly; SameSite=Lax"
                )
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
        print("Warning: DEVAIPOD_PROXY_PASSWORD not set, proxy will allow all requests",
              file=sys.stderr)

    # Listen on all interfaces so podman port publishing can route traffic to us
    # Use ThreadingHTTPServer so long-polling/streaming requests don't block others
    server = ThreadingHTTPServer(("0.0.0.0", PROXY_PORT), AuthProxyHandler)
    print(f"Auth proxy listening on 0.0.0.0:{PROXY_PORT} -> {UPSTREAM}", file=sys.stderr)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    server.server_close()


if __name__ == "__main__":
    main()

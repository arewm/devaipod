#!/usr/bin/env python3
"""Unit tests for auth_proxy.py authentication logic."""

import base64
import unittest
from unittest.mock import MagicMock, patch
from http.server import BaseHTTPRequestHandler
from io import BytesIO


class TestAuthProxyAuth(unittest.TestCase):
    """Tests for AuthProxyHandler.check_auth method."""

    def _create_mock_handler(self, headers=None, path="/"):
        """Create a mock handler with specified headers and path."""
        # We need to import here to allow patching environment variables
        # Import fresh each time to get potentially patched PASSWORD
        import auth_proxy

        handler = object.__new__(auth_proxy.AuthProxyHandler)
        handler.headers = MagicMock()
        handler.headers.get = MagicMock(side_effect=lambda k, d="": (headers or {}).get(k, d))
        handler.path = path
        handler._set_session_cookie = False
        return handler

    def test_check_auth_no_password_configured(self):
        """When DEVAIPOD_PROXY_PASSWORD is empty, all requests allowed."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": ""}, clear=False):
            # Reimport to pick up new PASSWORD value
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            handler = self._create_mock_handler()
            result = handler.check_auth()

            self.assertTrue(result)
            self.assertFalse(handler._set_session_cookie)

    def test_check_auth_cookie_valid(self):
        """Valid devaipod_session cookie authenticates."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            handler = self._create_mock_handler(
                headers={"Cookie": "devaipod_session=secretpass"}
            )
            result = handler.check_auth()

            self.assertTrue(result)
            self.assertFalse(handler._set_session_cookie)

    def test_check_auth_cookie_invalid(self):
        """Invalid cookie value fails."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            handler = self._create_mock_handler(
                headers={"Cookie": "devaipod_session=wrongpass"}
            )
            result = handler.check_auth()

            self.assertFalse(result)

    def test_check_auth_cookie_multiple_cookies(self):
        """Valid session cookie found among multiple cookies."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            handler = self._create_mock_handler(
                headers={"Cookie": "other=foo; devaipod_session=secretpass; another=bar"}
            )
            result = handler.check_auth()

            self.assertTrue(result)

    def test_check_auth_token_param_valid(self):
        """Valid ?token= authenticates and sets cookie flag."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            handler = self._create_mock_handler(path="/?token=secretpass")
            result = handler.check_auth()

            self.assertTrue(result)
            self.assertTrue(handler._set_session_cookie)

    def test_check_auth_token_param_invalid(self):
        """Invalid token fails."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            handler = self._create_mock_handler(path="/?token=wrongpass")
            result = handler.check_auth()

            self.assertFalse(result)
            self.assertFalse(handler._set_session_cookie)

    def test_check_auth_token_in_path_with_other_params(self):
        """Token param works with other query parameters."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            handler = self._create_mock_handler(path="/api/test?foo=bar&token=secretpass&baz=qux")
            result = handler.check_auth()

            self.assertTrue(result)
            self.assertTrue(handler._set_session_cookie)

    def test_check_auth_basic_auth_valid(self):
        """Valid Basic auth with username=opencode works."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            credentials = base64.b64encode(b"opencode:secretpass").decode("utf-8")
            handler = self._create_mock_handler(
                headers={"Authorization": f"Basic {credentials}"}
            )
            result = handler.check_auth()

            self.assertTrue(result)
            self.assertFalse(handler._set_session_cookie)

    def test_check_auth_basic_auth_wrong_username(self):
        """Wrong username fails."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            credentials = base64.b64encode(b"admin:secretpass").decode("utf-8")
            handler = self._create_mock_handler(
                headers={"Authorization": f"Basic {credentials}"}
            )
            result = handler.check_auth()

            self.assertFalse(result)

    def test_check_auth_basic_auth_wrong_password(self):
        """Wrong password fails."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            credentials = base64.b64encode(b"opencode:wrongpass").decode("utf-8")
            handler = self._create_mock_handler(
                headers={"Authorization": f"Basic {credentials}"}
            )
            result = handler.check_auth()

            self.assertFalse(result)

    def test_check_auth_basic_auth_malformed(self):
        """Malformed Basic auth fails gracefully."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            # Invalid base64
            handler = self._create_mock_handler(
                headers={"Authorization": "Basic !!!invalid!!!"}
            )
            result = handler.check_auth()
            self.assertFalse(result)

            # Missing colon in credentials
            credentials_no_colon = base64.b64encode(b"opencodepassword").decode("utf-8")
            handler = self._create_mock_handler(
                headers={"Authorization": f"Basic {credentials_no_colon}"}
            )
            result = handler.check_auth()
            self.assertFalse(result)

    def test_check_auth_basic_auth_empty_string(self):
        """Empty Basic auth value fails gracefully."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            handler = self._create_mock_handler(
                headers={"Authorization": "Basic "}
            )
            result = handler.check_auth()
            self.assertFalse(result)

    def test_check_auth_no_credentials_provided(self):
        """Request with no credentials fails when password is set."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            handler = self._create_mock_handler()
            result = handler.check_auth()

            self.assertFalse(result)


class TestAuthProxy401Response(unittest.TestCase):
    """Tests for 401 response behavior."""

    def test_401_response_includes_www_authenticate_for_html(self):
        """401 response includes WWW-Authenticate header for HTML requests."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            # Create a more complete mock handler for proxy_request
            handler = object.__new__(auth_proxy.AuthProxyHandler)
            handler.headers = MagicMock()
            # Simulate browser HTML request
            handler.headers.get = MagicMock(
                side_effect=lambda k, d="": {"Accept": "text/html,application/xhtml+xml"}.get(k, d)
            )
            handler.headers.items = MagicMock(return_value=[])
            handler.path = "/"
            handler._set_session_cookie = False

            # Track what gets sent
            sent_headers = {}
            response_code = None

            def mock_send_response(code):
                nonlocal response_code
                response_code = code

            def mock_send_header(key, value):
                sent_headers[key] = value

            def mock_end_headers():
                pass

            handler.send_response = mock_send_response
            handler.send_header = mock_send_header
            handler.end_headers = mock_end_headers
            handler.wfile = BytesIO()

            handler.proxy_request("GET")

            self.assertEqual(response_code, 401)
            self.assertIn("WWW-Authenticate", sent_headers)
            self.assertEqual(sent_headers["WWW-Authenticate"], 'Basic realm="opencode"')

    def test_401_api_request_no_www_authenticate(self):
        """401 response does NOT include WWW-Authenticate for API (JSON) requests."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            handler = object.__new__(auth_proxy.AuthProxyHandler)
            handler.headers = MagicMock()
            # Simulate API request with Accept: application/json
            handler.headers.get = MagicMock(
                side_effect=lambda k, d="": {"Accept": "application/json"}.get(k, d)
            )
            handler.headers.items = MagicMock(return_value=[])
            handler.path = "/api/sessions"
            handler._set_session_cookie = False

            sent_headers = {}
            response_code = None

            def mock_send_response(code):
                nonlocal response_code
                response_code = code

            def mock_send_header(key, value):
                sent_headers[key] = value

            handler.send_response = mock_send_response
            handler.send_header = mock_send_header
            handler.end_headers = lambda: None
            handler.wfile = BytesIO()

            handler.proxy_request("GET")

            self.assertEqual(response_code, 401)
            # WWW-Authenticate should NOT be present for API requests
            self.assertNotIn("WWW-Authenticate", sent_headers)


class TestSetCookieAfterTokenAuth(unittest.TestCase):
    """Tests for Set-Cookie behavior after token auth."""

    def test_set_cookie_after_token_auth(self):
        """After ?token= auth, Set-Cookie header is included."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            # Create handler
            handler = object.__new__(auth_proxy.AuthProxyHandler)
            handler.headers = MagicMock()
            handler.headers.get = MagicMock(return_value="")
            handler.headers.items = MagicMock(return_value=[])
            handler.path = "/?token=secretpass"
            handler._set_session_cookie = False

            # Track what gets sent
            sent_headers = {}
            response_code = None

            def mock_send_response(code):
                nonlocal response_code
                response_code = code

            def mock_send_header(key, value):
                # Store all Set-Cookie headers (may be multiple)
                if key == "Set-Cookie":
                    sent_headers.setdefault("Set-Cookie", []).append(value)
                else:
                    sent_headers[key] = value

            def mock_end_headers():
                pass

            handler.send_response = mock_send_response
            handler.send_header = mock_send_header
            handler.end_headers = mock_end_headers
            handler.wfile = BytesIO()

            # Mock the upstream connection
            mock_response = MagicMock()
            mock_response.status = 200
            mock_response.getheaders = MagicMock(return_value=[("Content-Type", "text/html")])
            mock_response.read = MagicMock(side_effect=[b"OK", b""])

            mock_conn = MagicMock()
            mock_conn.request = MagicMock()
            mock_conn.getresponse = MagicMock(return_value=mock_response)
            mock_conn.close = MagicMock()

            with patch("http.client.HTTPConnection", return_value=mock_conn):
                handler.proxy_request("GET")

            self.assertEqual(response_code, 200)
            self.assertIn("Set-Cookie", sent_headers)
            cookie_values = sent_headers["Set-Cookie"]
            self.assertEqual(len(cookie_values), 1)
            self.assertIn("devaipod_session=secretpass", cookie_values[0])
            self.assertIn("HttpOnly", cookie_values[0])
            self.assertIn("SameSite=Lax", cookie_values[0])

    def test_no_cookie_set_for_basic_auth(self):
        """Basic auth does not set session cookie."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            credentials = base64.b64encode(b"opencode:secretpass").decode("utf-8")

            handler = object.__new__(auth_proxy.AuthProxyHandler)
            handler.headers = MagicMock()
            handler.headers.get = MagicMock(
                side_effect=lambda k, d="": {"Authorization": f"Basic {credentials}"}.get(k, d)
            )
            handler.headers.items = MagicMock(return_value=[("Authorization", f"Basic {credentials}")])
            handler.path = "/"
            handler._set_session_cookie = False

            sent_headers = {}
            response_code = None

            def mock_send_response(code):
                nonlocal response_code
                response_code = code

            def mock_send_header(key, value):
                if key == "Set-Cookie":
                    sent_headers.setdefault("Set-Cookie", []).append(value)
                else:
                    sent_headers[key] = value

            handler.send_response = mock_send_response
            handler.send_header = mock_send_header
            handler.end_headers = lambda: None
            handler.wfile = BytesIO()

            mock_response = MagicMock()
            mock_response.status = 200
            mock_response.getheaders = MagicMock(return_value=[])
            mock_response.read = MagicMock(side_effect=[b"OK", b""])

            mock_conn = MagicMock()
            mock_conn.request = MagicMock()
            mock_conn.getresponse = MagicMock(return_value=mock_response)
            mock_conn.close = MagicMock()

            with patch("http.client.HTTPConnection", return_value=mock_conn):
                handler.proxy_request("GET")

            self.assertEqual(response_code, 200)
            # Should NOT have Set-Cookie
            self.assertNotIn("Set-Cookie", sent_headers)


class TestAuthPriority(unittest.TestCase):
    """Tests for authentication method priority."""

    def test_cookie_takes_priority_over_token(self):
        """Valid cookie auth doesn't trigger Set-Cookie even with token param."""
        with patch.dict("os.environ", {"DEVAIPOD_PROXY_PASSWORD": "secretpass"}, clear=False):
            import importlib
            import auth_proxy
            importlib.reload(auth_proxy)

            handler = object.__new__(auth_proxy.AuthProxyHandler)
            handler.headers = MagicMock()
            handler.headers.get = MagicMock(
                side_effect=lambda k, d="": {"Cookie": "devaipod_session=secretpass"}.get(k, d)
            )
            handler.path = "/?token=secretpass"
            handler._set_session_cookie = False

            result = handler.check_auth()

            self.assertTrue(result)
            # Cookie auth should not set _set_session_cookie
            self.assertFalse(handler._set_session_cookie)


if __name__ == "__main__":
    unittest.main()

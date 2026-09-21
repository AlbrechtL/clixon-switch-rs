"""The RESTCONF client the tests drive the switch with."""

import json

import requests

YANG_JSON = "application/yang-data+json"


class Response:
    def __init__(self, response: requests.Response):
        self.status = response.status_code
        self.text = response.text

    def json(self):
        return json.loads(self.text)

    def __repr__(self) -> str:
        body = self.text.strip()
        if len(body) > 2000:
            body = body[:2000] + "\n... truncated"
        return f"<HTTP {self.status}>" + (f"\n{body}" if body else "")


class Restconf:
    """One switch's RESTCONF API.

    `get`/`put`/`patch`/`post`/`delete` return the response without judging
    it, so a test can assert on the status. `data()` is the common case: a
    GET that must succeed, returning the single value inside the YANG
    wrapper object.
    """

    def __init__(self, base: str, *, timeout: float = 10):
        self.base = base.rstrip("/")
        self.timeout = timeout
        self.session = requests.Session()
        self.session.headers.update({"Content-Type": YANG_JSON, "Accept": YANG_JSON})

    def url(self, path: str) -> str:
        if path.startswith("http"):
            return path
        return f"{self.base}/{path.lstrip('/')}"

    def request(self, method: str, path: str, body=None) -> Response:
        data = None
        if body is not None:
            data = body if isinstance(body, str) else json.dumps(body)
        return Response(
            self.session.request(
                method, self.url(path), data=data, timeout=self.timeout
            )
        )

    def get(self, path: str) -> Response:
        return self.request("GET", path)

    def put(self, path: str, body) -> Response:
        return self.request("PUT", path, body)

    def patch(self, path: str, body) -> Response:
        return self.request("PATCH", path, body)

    def post(self, path: str, body) -> Response:
        return self.request("POST", path, body)

    def delete(self, path: str) -> Response:
        return self.request("DELETE", path)

    def data(self, path: str):
        """GETs `path`, asserts 200, and unwraps the single top-level key."""
        response = self.get(path)
        assert response.status == 200, f"GET {path} {response!r}"
        body = response.json()
        assert len(body) == 1, f"GET {path}: expected one top-level key, got {body}"
        value = next(iter(body.values()))
        # A list entry selected by key comes back as a one-element array.
        if isinstance(value, list) and len(value) == 1:
            return value[0]
        return value

    def ok(self, method: str, path: str, body=None, *, expect=(200, 201, 204)) -> Response:
        response = self.request(method, path, body)
        assert response.status in expect, f"{method} {path} {response!r}"
        return response

    def rejected(self, method: str, path: str, body=None) -> Response:
        """The switch must refuse this: any 4xx or 5xx."""
        response = self.request(method, path, body)
        assert response.status >= 400, (
            f"{method} {path} was accepted with HTTP {response.status}, "
            f"expected a rejection"
        )
        return response

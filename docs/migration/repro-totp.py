#!/usr/bin/env python3
"""Reproduce: the secret from /api/auth/totp/setup yields codes that
/api/auth/totp/enable rejects.

Run a fresh server first, e.g.:

    APP_ADDR=127.0.0.1:18080 APP_BASE_URL=http://127.0.0.1:18080 \
    APP_DATA_DIR=/tmp/rv APP_WORK_DIR=/tmp/rv/work \
    ADMIN_USERNAME=admin ADMIN_PASSWORD=correct-horse-battery \
    ./target/debug/revaro &

    python3 docs/migration/repro-totp.py http://127.0.0.1:18080

The HOTP computation below is checked against the RFC 6238 Appendix B vectors
before it is used, so a failure is not an artefact of the test client.
"""
import base64, hashlib, hmac, json, struct, sys, time, urllib.error, urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18080"
PASSWORD = "correct-horse-battery"


def hotp(key: bytes, counter: int, digits: int = 6) -> str:
    mac = hmac.new(key, struct.pack(">Q", counter), hashlib.sha1).digest()
    offset = mac[-1] & 0x0F
    value = struct.unpack(">I", mac[offset : offset + 4])[0] & 0x7FFFFFFF
    return str(value % (10**digits)).zfill(digits)


def self_check() -> None:
    """RFC 6238 Appendix B, secret = ASCII '12345678901234567890'."""
    secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
    key = base64.b32decode(secret)
    for when, expected in [(59, "287082"), (1111111109, "081804"), (1111111111, "050471")]:
        assert hotp(key, when // 30) == expected, (when, hotp(key, when // 30), expected)


def call(method, path, body=None, cookie=None):
    headers = {"Origin": BASE, "Content-Type": "application/json"}
    if cookie:
        headers["Cookie"] = cookie
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request) as response:
            raw = response.read()
            return response.status, (json.loads(raw) if raw else None), response.headers
    except urllib.error.HTTPError as error:
        raw = error.read()
        try:
            return error.code, json.loads(raw), error.headers
        except Exception:
            return error.code, None, error.headers


def main() -> int:
    self_check()
    status, _, headers = call("POST", "/api/auth/login", {"username": "admin", "password": PASSWORD})
    assert status == 200, status
    cookie = headers.get("Set-Cookie").split(";")[0]

    status, setup, _ = call(
        "POST", "/api/auth/totp/setup", {"current_password": PASSWORD}, cookie
    )
    assert status == 201, (status, setup)
    secret = setup["secret"]
    key = base64.b32decode(secret)
    current = int(time.time()) // 30
    print(f"setup ok; secret={secret}")

    accepted = None
    for offset in (0, -1, 1):
        candidate = hotp(key, current + offset)
        status, body, _ = call(
            "POST",
            "/api/auth/totp/enable",
            {"current_password": PASSWORD, "code": candidate},
            cookie,
        )
        print(f"  enable with step{offset:+d} code {candidate} -> {status}")
        if status == 200:
            accepted = candidate
            break

    if accepted is None:
        print(
            "\nDEFECT REPRODUCED: no correctly computed code from the advertised\n"
            "secret is accepted, so two-factor authentication cannot be enabled."
        )
        return 1
    print("\nTOTP enable succeeded; no defect.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

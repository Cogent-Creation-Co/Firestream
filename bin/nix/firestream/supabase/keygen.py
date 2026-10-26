"""Generate a consistent key set for a self-hosted Supabase stack.

Reproduces upstream ``docker/utils/generate-keys.sh`` and
``docker/utils/add-new-auth-keys.sh`` (self-hosted/v0.8.3) in one program:

* the legacy HS256 ``ANON_KEY`` / ``SERVICE_ROLE_KEY`` signed with ``JWT_SECRET``;
* an EC P-256 pair as ``JWT_KEYS`` (private, for Auth) and ``JWT_JWKS``
  (public, for PostgREST, Realtime and Storage), each also holding
  ``JWT_SECRET`` as an ``oct`` key so legacy tokens still verify;
* the ES256 ``ANON_KEY_ASYMMETRIC`` / ``SERVICE_ROLE_KEY_ASYMMETRIC``;
* the opaque ``SUPABASE_PUBLISHABLE_KEY`` / ``SUPABASE_SECRET_KEY``;
* the service secrets (``SECRET_KEY_BASE``, ``VAULT_ENC_KEY``, ...).

``--verify FILE`` checks an env file instead: every token must verify against
the set's own secrets, and every value must have the length its consumer needs.
"""

import argparse
import base64
import hashlib
import hmac
import json
import secrets
import sys
import time
import uuid

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import (
    decode_dss_signature,
    encode_dss_signature,
)

# NOTE: upstream add-new-auth-keys.sh hashes every opaque key with this
# project ref; a key with any other checksum is not an upstream key.
PROJECT_REF = "supabase-self-hosted"
FIVE_YEARS = 5 * 365 * 24 * 3600

# NOTE: the lengths that upstream .env.example (self-hosted/v0.8.3) requires.
FIXED_LENGTHS = {"REALTIME_DB_ENC_KEY": 16, "VAULT_ENC_KEY": 32}
MIN_LENGTHS = {"JWT_SECRET": 32, "SECRET_KEY_BASE": 64, "PG_META_CRYPTO_KEY": 32}


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def b64url_decode(text: str) -> bytes:
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def int_b64url(value: int) -> str:
    return b64url(value.to_bytes(32, "big"))


def segment(obj: dict) -> str:
    return b64url(json.dumps(obj, separators=(",", ":")).encode())


def sign_hs256(payload: dict, secret: str) -> str:
    data = segment({"alg": "HS256", "typ": "JWT"}) + "." + segment(payload)
    mac = hmac.new(secret.encode(), data.encode(), hashlib.sha256).digest()
    return data + "." + b64url(mac)


def sign_es256(payload: dict, key: ec.EllipticCurvePrivateKey, kid: str) -> str:
    data = segment({"alg": "ES256", "typ": "JWT", "kid": kid}) + "." + segment(payload)
    r, s = decode_dss_signature(key.sign(data.encode(), ec.ECDSA(hashes.SHA256())))
    return data + "." + b64url(r.to_bytes(32, "big") + s.to_bytes(32, "big"))


def opaque_key(prefix: str) -> str:
    intermediate = prefix + b64url(secrets.token_bytes(17))[:22]
    digest = hashlib.sha256(f"{PROJECT_REF}|{intermediate}".encode()).digest()
    return intermediate + "_" + b64url(digest)[:8]


def generate(jwt_secret: str | None, now: int, lifetime: int) -> dict[str, str]:
    jwt_secret = jwt_secret or b64url(secrets.token_bytes(30))
    claims = {"iss": "supabase", "iat": now, "exp": now + lifetime}

    key = ec.generate_private_key(ec.SECP256R1())
    numbers = key.private_numbers()
    kid = str(uuid.uuid4())
    oct_key = {"kty": "oct", "k": b64url(jwt_secret.encode()), "alg": "HS256"}
    public = {
        "kty": "EC", "kid": kid, "use": "sig", "alg": "ES256", "ext": True, "crv": "P-256",
        "x": int_b64url(numbers.public_numbers.x), "y": int_b64url(numbers.public_numbers.y),
    }
    private = {**public, "key_ops": ["sign", "verify"], "d": int_b64url(numbers.private_value)}

    return {
        "POSTGRES_PASSWORD": secrets.token_hex(16),
        "JWT_SECRET": jwt_secret,
        "ANON_KEY": sign_hs256({"role": "anon", **claims}, jwt_secret),
        "SERVICE_ROLE_KEY": sign_hs256({"role": "service_role", **claims}, jwt_secret),
        "JWT_KEYS": json.dumps([private, oct_key], separators=(",", ":")),
        "JWT_JWKS": json.dumps(
            {"keys": [{**public, "key_ops": ["verify"]}, oct_key]}, separators=(",", ":")
        ),
        "ANON_KEY_ASYMMETRIC": sign_es256({"role": "anon", **claims}, key, kid),
        "SERVICE_ROLE_KEY_ASYMMETRIC": sign_es256({"role": "service_role", **claims}, key, kid),
        "SUPABASE_PUBLISHABLE_KEY": opaque_key("sb_publishable_"),
        "SUPABASE_SECRET_KEY": opaque_key("sb_secret_"),
        "SECRET_KEY_BASE": b64url(secrets.token_bytes(48)),
        "REALTIME_DB_ENC_KEY": secrets.token_hex(8),
        "VAULT_ENC_KEY": secrets.token_hex(16),
        "PG_META_CRYPTO_KEY": b64url(secrets.token_bytes(24)),
        "DASHBOARD_PASSWORD": secrets.token_hex(16),
    }


def verify_hs256(token: str, secret: str) -> dict:
    header, payload, signature = token.split(".")
    mac = hmac.new(secret.encode(), f"{header}.{payload}".encode(), hashlib.sha256).digest()
    if not hmac.compare_digest(b64url(mac), signature):
        raise ValueError("HS256 signature does not match JWT_SECRET")
    return json.loads(b64url_decode(payload))


def verify_es256(token: str, jwks: dict) -> dict:
    header, payload, signature = token.split(".")
    kid = json.loads(b64url_decode(header))["kid"]
    jwk = next(k for k in jwks["keys"] if k.get("kid") == kid)
    public = ec.EllipticCurvePublicNumbers(
        int.from_bytes(b64url_decode(jwk["x"]), "big"),
        int.from_bytes(b64url_decode(jwk["y"]), "big"),
        ec.SECP256R1(),
    ).public_key()
    raw = b64url_decode(signature)
    der = encode_dss_signature(int.from_bytes(raw[:32], "big"), int.from_bytes(raw[32:], "big"))
    public.verify(der, f"{header}.{payload}".encode(), ec.ECDSA(hashes.SHA256()))
    return json.loads(b64url_decode(payload))


def verify(values: dict[str, str], now: int) -> list[str]:
    errors = []
    secret = values["JWT_SECRET"]
    jwks = json.loads(values["JWT_JWKS"])
    checks = [
        ("ANON_KEY", "anon", lambda t: verify_hs256(t, secret)),
        ("SERVICE_ROLE_KEY", "service_role", lambda t: verify_hs256(t, secret)),
        ("ANON_KEY_ASYMMETRIC", "anon", lambda t: verify_es256(t, jwks)),
        ("SERVICE_ROLE_KEY_ASYMMETRIC", "service_role", lambda t: verify_es256(t, jwks)),
    ]
    for name, role, check in checks:
        try:
            claims = check(values[name])
        except Exception as exc:  # noqa: BLE001 - every failure is a finding
            errors.append(f"{name}: {exc}")
            continue
        if claims.get("role") != role:
            errors.append(f"{name}: role is {claims.get('role')!r}, not {role!r}")
        if claims.get("exp", 0) <= now:
            errors.append(f"{name}: expired")
    oct_keys = [k for k in jwks["keys"] if k["kty"] == "oct"]
    if not oct_keys or b64url_decode(oct_keys[0]["k"]).decode() != secret:
        errors.append("JWT_JWKS: the oct key is not JWT_SECRET")
    for name, prefix in (("SUPABASE_PUBLISHABLE_KEY", "sb_publishable_"), ("SUPABASE_SECRET_KEY", "sb_secret_")):
        # NOTE: the checksum is base64url and can itself contain "_", so the
        # split is by length, not at the last underscore.
        intermediate, separator, checksum = values[name][:-9], values[name][-9:-8], values[name][-8:]
        digest = hashlib.sha256(f"{PROJECT_REF}|{intermediate}".encode()).digest()
        if separator != "_" or not intermediate.startswith(prefix) or b64url(digest)[:8] != checksum:
            errors.append(f"{name}: bad prefix or checksum")
    for name, length in FIXED_LENGTHS.items():
        if len(values[name]) != length:
            errors.append(f"{name}: length {len(values[name])}, needs {length}")
    for name, length in MIN_LENGTHS.items():
        if len(values[name]) < length:
            errors.append(f"{name}: length {len(values[name])}, needs at least {length}")
    return errors


def read_env(path: str) -> dict[str, str]:
    values = {}
    with open(path) as handle:
        for line in handle:
            line = line.strip()
            if line and not line.startswith("#"):
                name, _, value = line.partition("=")
                values[name] = value
    return values


def render(values: dict[str, str], output: str, name: str) -> str:
    if output == "env":
        return "".join(f"{k}={v}\n" for k, v in values.items())
    if output == "json":
        return json.dumps(values, indent=2) + "\n"
    secret = {
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {"name": name},
        "type": "Opaque",
        "stringData": values,
    }
    return json.dumps(secret, indent=2) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--output", choices=["env", "json", "k8s-secret"], default="env")
    parser.add_argument("--name", default="supabase-secrets", help="Secret name for --output k8s-secret")
    parser.add_argument("--jwt-secret", help="Keep this JWT_SECRET; the tokens are signed with it")
    parser.add_argument("--lifetime", type=int, default=FIVE_YEARS, help="Token lifetime in seconds")
    parser.add_argument("--verify", metavar="FILE", help="Verify an env file instead of generating")
    args = parser.parse_args()

    now = int(time.time())
    if args.verify:
        errors = verify(read_env(args.verify), now)
        for error in errors:
            print(f"FAIL: {error}", file=sys.stderr)
        return 1 if errors else 0

    sys.stdout.write(render(generate(args.jwt_secret, now, args.lifetime), args.output, args.name))
    return 0


if __name__ == "__main__":
    sys.exit(main())

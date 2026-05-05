"""
FastAPI JWT authentication middleware.
Handles token issuance, refresh, revocation, and role-based access control.
"""

from __future__ import annotations

import hashlib
import hmac
import secrets
import time
from dataclasses import dataclass, field
from enum import Enum
from functools import wraps
from typing import Any, Callable, Optional

from fastapi import Depends, HTTPException, Request, status
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

JWT_ALGORITHM = "HS256"
ACCESS_TOKEN_TTL = 15 * 60       # 15 minutes
REFRESH_TOKEN_TTL = 7 * 24 * 3600  # 7 days
TOKEN_ISSUER = "api.example.com"

# ---------------------------------------------------------------------------
# Roles
# ---------------------------------------------------------------------------

class Role(str, Enum):
    GUEST = "guest"
    USER = "user"
    MODERATOR = "moderator"
    ADMIN = "admin"

ROLE_HIERARCHY: dict[Role, int] = {
    Role.GUEST: 0,
    Role.USER: 1,
    Role.MODERATOR: 2,
    Role.ADMIN: 3,
}

def role_gte(subject: Role, required: Role) -> bool:
    return ROLE_HIERARCHY.get(subject, -1) >= ROLE_HIERARCHY.get(required, 99)

# ---------------------------------------------------------------------------
# Token payload
# ---------------------------------------------------------------------------

@dataclass
class TokenPayload:
    sub: str                       # user id
    role: Role
    iat: int = field(default_factory=lambda: int(time.time()))
    exp: int = 0
    jti: str = field(default_factory=lambda: secrets.token_hex(16))
    iss: str = TOKEN_ISSUER

    def is_expired(self) -> bool:
        return time.time() > self.exp

    def to_dict(self) -> dict[str, Any]:
        return {
            "sub": self.sub,
            "role": self.role.value,
            "iat": self.iat,
            "exp": self.exp,
            "jti": self.jti,
            "iss": self.iss,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "TokenPayload":
        return cls(
            sub=data["sub"],
            role=Role(data["role"]),
            iat=data["iat"],
            exp=data["exp"],
            jti=data["jti"],
            iss=data.get("iss", TOKEN_ISSUER),
        )

# ---------------------------------------------------------------------------
# Simple HS256 JWT (no external dep)
# ---------------------------------------------------------------------------

import base64
import json


def _b64url_encode(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def _b64url_decode(s: str) -> bytes:
    padding = 4 - len(s) % 4
    return base64.urlsafe_b64decode(s + "=" * padding)


def encode_jwt(payload: TokenPayload, secret: str) -> str:
    header = _b64url_encode(json.dumps({"alg": JWT_ALGORITHM, "typ": "JWT"}).encode())
    body = _b64url_encode(json.dumps(payload.to_dict()).encode())
    signing_input = f"{header}.{body}"
    sig = hmac.new(
        secret.encode(),
        signing_input.encode(),
        hashlib.sha256,
    ).digest()
    return f"{signing_input}.{_b64url_encode(sig)}"


def decode_jwt(token: str, secret: str) -> TokenPayload:
    parts = token.split(".")
    if len(parts) != 3:
        raise ValueError("Malformed token")
    header_b64, body_b64, sig_b64 = parts
    signing_input = f"{header_b64}.{body_b64}"
    expected_sig = hmac.new(
        secret.encode(),
        signing_input.encode(),
        hashlib.sha256,
    ).digest()
    try:
        actual_sig = _b64url_decode(sig_b64)
    except Exception:
        raise ValueError("Invalid token signature encoding")
    if not hmac.compare_digest(expected_sig, actual_sig):
        raise ValueError("Signature mismatch")
    data = json.loads(_b64url_decode(body_b64))
    return TokenPayload.from_dict(data)

# ---------------------------------------------------------------------------
# Token store (revocation list backed by Redis or in-memory for tests)
# ---------------------------------------------------------------------------

class TokenStore:
    """Tracks revoked JTIs. Replace with Redis in production."""

    def __init__(self) -> None:
        self._revoked: dict[str, float] = {}  # jti -> expiry

    def revoke(self, jti: str, exp: float) -> None:
        self._revoked[jti] = exp

    def is_revoked(self, jti: str) -> bool:
        now = time.time()
        # Purge expired entries opportunistically
        expired = [k for k, v in self._revoked.items() if v < now]
        for k in expired:
            del self._revoked[k]
        return jti in self._revoked

    def revoke_all_for_user(self, sub: str, issued_before: float) -> None:
        # Requires iterating all tokens — only feasible with persistent store
        # This is a stub; real impl would write to Redis with a set per user
        pass


_token_store = TokenStore()

# ---------------------------------------------------------------------------
# Auth service
# ---------------------------------------------------------------------------

class AuthService:
    def __init__(self, secret: str, store: TokenStore = _token_store) -> None:
        self._secret = secret
        self._store = store

    def issue_access_token(self, user_id: str, role: Role) -> str:
        payload = TokenPayload(
            sub=user_id,
            role=role,
            exp=int(time.time()) + ACCESS_TOKEN_TTL,
        )
        return encode_jwt(payload, self._secret)

    def issue_refresh_token(self, user_id: str, role: Role) -> str:
        payload = TokenPayload(
            sub=user_id,
            role=role,
            exp=int(time.time()) + REFRESH_TOKEN_TTL,
        )
        return encode_jwt(payload, self._secret)

    def validate(self, token: str) -> TokenPayload:
        try:
            payload = decode_jwt(token, self._secret)
        except ValueError as exc:
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail=str(exc),
            )
        if payload.is_expired():
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="Token expired",
            )
        if self._store.is_revoked(payload.jti):
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="Token revoked",
            )
        return payload

    def refresh(self, refresh_token: str) -> tuple[str, str]:
        payload = self.validate(refresh_token)
        self._store.revoke(payload.jti, float(payload.exp))
        new_access = self.issue_access_token(payload.sub, payload.role)
        new_refresh = self.issue_refresh_token(payload.sub, payload.role)
        return new_access, new_refresh

    def revoke(self, token: str) -> None:
        try:
            payload = decode_jwt(token, self._secret)
            self._store.revoke(payload.jti, float(payload.exp))
        except ValueError:
            pass  # already invalid — nothing to do

# ---------------------------------------------------------------------------
# FastAPI dependency helpers
# ---------------------------------------------------------------------------

_bearer = HTTPBearer(auto_error=False)


def get_current_user(
    credentials: Optional[HTTPAuthorizationCredentials] = Depends(_bearer),
    auth: AuthService = Depends(lambda: AuthService(secret="changeme")),
) -> TokenPayload:
    if credentials is None:
        raise HTTPException(status_code=status.HTTP_401_UNAUTHORIZED, detail="Missing token")
    return auth.validate(credentials.credentials)


def require_role(minimum: Role) -> Callable:
    def dependency(current: TokenPayload = Depends(get_current_user)) -> TokenPayload:
        if not role_gte(current.role, minimum):
            raise HTTPException(
                status_code=status.HTTP_403_FORBIDDEN,
                detail=f"Requires role {minimum.value} or higher",
            )
        return current
    return Depends(dependency)


def admin_only(current: TokenPayload = Depends(require_role(Role.ADMIN))) -> TokenPayload:
    return current


def moderator_or_above(
    current: TokenPayload = Depends(require_role(Role.MODERATOR)),
) -> TokenPayload:
    return current

# ---------------------------------------------------------------------------
# Rate-limiting decorator (per-user, sliding window)
# ---------------------------------------------------------------------------

_rate_counters: dict[str, list[float]] = {}


def rate_limit(max_calls: int, window_secs: float):
    def decorator(func: Callable) -> Callable:
        @wraps(func)
        async def wrapper(request: Request, *args: Any, **kwargs: Any) -> Any:
            user_key = request.client.host if request.client else "anonymous"
            now = time.time()
            timestamps = _rate_counters.get(user_key, [])
            timestamps = [t for t in timestamps if now - t < window_secs]
            if len(timestamps) >= max_calls:
                raise HTTPException(
                    status_code=status.HTTP_429_TOO_MANY_REQUESTS,
                    detail=f"Rate limit: {max_calls} requests per {window_secs}s",
                )
            timestamps.append(now)
            _rate_counters[user_key] = timestamps
            return await func(request, *args, **kwargs)
        return wrapper
    return decorator

# Authentication

Oyster supports three authentication modes: **Bearer tokens** (API keys) for
data operations, **JWTs** for admin operations, and **public access** for blob
reads and infrastructure probes.

## Authentication Modes at a Glance

| Route pattern | Auth mode | Purpose |
|---|---|---|
| Bucket CRUD, blob write/list/delete, wallet | API Key | Data operations |
| `GET .../blobs/{key}`, `GET /blobs/by-blob-id/...` | Public | Blob reads |
| `POST /accounts`, key management under `/accounts/{id}/...` | JWT | Admin operations |
| `POST /apps/token-refresh` | JWT (with grace) | Token refresh |
| `/health`, `/ready`, `/metrics`, `/api/docs` | Public | Infrastructure |

> **How Oyster tells them apart:** JWTs contain exactly two `.` separators;
> hex-encoded API keys contain none. The server checks this before routing to
> the appropriate auth handler.

## Bearer Token (API Key) Authentication

Include your API key in the `Authorization` header:

```
Authorization: Bearer <api-key>
```

**Example:**

```bash
curl -s \
  -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/buckets"
```

### Key properties

| Property | Value |
|---|---|
| Size | 32 bytes, hex-encoded (64 characters) |
| Hash algorithm | BLAKE2s-256 (only the hash is stored) |
| Prefix | First 8 characters — used to identify keys without exposing the secret |

API keys are provisioned through the Admin API (see
[Admin](admin.md)). The full secret is shown **only once** at
creation time — a lost key cannot be recovered.

### Errors

| Status | Condition |
|---|---|
| `401 Unauthorized` | Missing, malformed, or invalid API key |

## JWT Authentication (for Apps)

Admin endpoints require a JWT issued by the server operator:

```
Authorization: Bearer <jwt>
```

JWTs are generated server-side with `oysterd app jwt <app_id>`. They are
**not** available through a public API.

### Token properties

| Property | Value |
|---|---|
| Algorithm | HS256 |
| Lifetime | 24 hours (86 400 s) |
| Issuer | `oyster` |

### Claims

| Claim | Description |
|---|---|
| `sub` | App ID (UUID) |
| `iat` | Issued-at (Unix timestamp) |
| `exp` | Expiration (Unix timestamp) |
| `iss` | Issuer — always `"oyster"` |
| `jti` | Unique token identifier (UUID, used for blacklisting) |

### Account ownership enforcement

An app can only manage accounts it created. Attempting to access another app's
accounts returns **403 Forbidden**:

```json
{ "error": "forbidden: account does not belong to this app" }
```

### Errors

| Status | Condition |
|---|---|
| `401 Unauthorized` | Missing, expired, or invalid JWT |
| `403 Forbidden` | Valid JWT but accessing another app's resources |

## Public Endpoints (No Authentication)

The following routes require no authentication:

- **Blob reads** — `GET /api/v1/buckets/{bucket_name}/blobs/{key}` and
  `GET /api/v1/blobs/by-blob-id/{blob_id}`
- **Infrastructure** — `/health`, `/ready`, `/metrics`, `/api/docs`

**Example:**

```bash
curl -s "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/hello.txt"
```

## Token Refresh

```
POST /api/v1/apps/token-refresh
```

Exchanges an expired JWT for a fresh one. The server accepts tokens up to
**48 hours** (172 800 s) past their expiry time.

### Requirements

- The app must have the `allow_refresh_jwt` flag enabled.
- The expired token's JTI is blacklisted upon successful refresh — it cannot
  be reused.

**Example:**

```bash
curl -s -X POST \
  -H "Authorization: Bearer $EXPIRED_JWT" \
  "$OYSTER_URL/api/v1/apps/token-refresh"
```

**Response** (`200 OK`):

```json
{
  "access_token": "<new-jwt>",
  "token_type": "Bearer",
  "expires_in": 86400
}
```

### Errors

| Status | Condition |
|---|---|
| `401 Unauthorized` | Token is more than 48 hours past expiry, or otherwise invalid |
| `403 Forbidden` | App does not have `allow_refresh_jwt` enabled |

## Security Notes

- **API keys** — Only the BLAKE2s-256 hash is stored. A lost key cannot be
  recovered; create a new one instead.
- **JWT secret** — Protect the `OYSTER_JWT_SECRET` environment variable. Anyone
  with this secret can mint valid tokens for any app.
- **JTI blacklisting** — Revoked token identifiers (via refresh or
  `oysterd app revoke-jwt`) are permanently rejected.
- **TLS** — Always terminate TLS in front of Oyster in production.

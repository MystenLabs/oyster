# S3 Access Keys

S3 access keys let you authenticate with Oyster's
[S3-compatible API](../s3-api/index.md) using standard AWS Signature
Version 4. Each access key consists of an **access key ID** (20 characters,
prefixed with `OYAK`) and a **secret access key** (40 hex characters).

You can have up to **3 active access keys** per account.

## Create Access Key

```
POST /api/v1/account/access-keys
```

Creates a new S3 access key pair. The secret is returned **only once** —
save it immediately.

**Example:**

```bash
curl -s -X POST \
  -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/account/access-keys" | jq
```

**Response** (`201 Created`):

```json
{
  "access_key_id": "OYAK1234567890ABCDEF",
  "secret_access_key": "abcdef1234567890abcdef1234567890abcdef12",
  "created_at": "2025-01-15T10:30:00Z"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `access_key_id` | string | 20-character key ID (starts with `OYAK`) |
| `secret_access_key` | string | 40-character hex secret — **shown only once** |
| `created_at` | string | ISO 8601 timestamp |

**Errors:**

| Status | Condition |
|--------|-----------|
| `401` | Missing or invalid API key |
| `409` | Access key limit reached (max 3 active keys) |

## List Access Keys

```
GET /api/v1/account/access-keys
```

Returns all S3 access keys for your account, including revoked ones.
Secrets are **never** included in list responses.

**Example:**

```bash
curl -s -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/account/access-keys" | jq
```

**Response** (`200 OK`):

```json
[
  {
    "access_key_id": "OYAK1234567890ABCDEF",
    "created_at": "2025-01-15T10:30:00Z",
    "revoked_at": null
  },
  {
    "access_key_id": "OYAKFEDCBA0987654321",
    "created_at": "2025-01-10T08:00:00Z",
    "revoked_at": "2025-01-14T12:00:00Z"
  }
]
```

| Field | Type | Description |
|-------|------|-------------|
| `access_key_id` | string | 20-character key ID |
| `created_at` | string | ISO 8601 timestamp |
| `revoked_at` | string or null | ISO 8601 timestamp if revoked, `null` if active |

## Delete Access Key

```
DELETE /api/v1/account/access-keys/{access_key_id}
```

Permanently deletes an S3 access key. Any S3 requests using this key will
immediately stop working.

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `access_key_id` | string | The 20-character access key ID to delete |

**Example:**

```bash
curl -s -X DELETE \
  -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/account/access-keys/OYAK1234567890ABCDEF"
```

**Response:** `204 No Content`

**Errors:**

| Status | Condition |
|--------|-----------|
| `401` | Missing or invalid API key |
| `404` | Access key not found or not owned by your account |

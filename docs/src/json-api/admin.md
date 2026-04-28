# Admin API

The Admin API lets app operators manage accounts, API keys, and S3 access
keys. All admin endpoints require **admin-key** authentication —
long-lived per-app Bearer tokens issued via `oysterd app issue-admin-key`
(see [Authentication](authentication.md)).

An app can only manage accounts it created. Attempting to access another
app's accounts returns **403 Forbidden**.

## Accounts

### Create Account

```
POST /api/v1/accounts
```

Creates a new account owned by the authenticated app. An initial API key
is generated automatically.

**Request body** (optional):

```json
{
  "name": "my-app-user"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | no | Human-readable account name; defaults to the account ID if omitted |

**Example:**

```bash
curl -s -X POST \
  -H "Authorization: Bearer $ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name": "my-app-user"}' \
  "$OYSTER_URL/api/v1/accounts" | jq
```

**Response** (`201 Created`):

```json
{
  "account_id": "550e8400-e29b-41d4-a716-446655440000",
  "api_key": {
    "id": "b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e",
    "prefix": "a1b2c3d4",
    "bearer_token": "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2",
    "created_at": "2025-01-15T10:30:00Z"
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `account_id` | string | UUID of the new account |
| `api_key.id` | string | Unique key identifier |
| `api_key.prefix` | string | First 8 characters of the raw key (for identification) |
| `api_key.bearer_token` | string | The full API key — **shown only once** |
| `api_key.created_at` | string | ISO 8601 timestamp |

> **Note:** The `bearer_token` is returned only at creation time. A lost
> key cannot be recovered — create a new one instead.

**Errors:**

| Status | Condition |
|--------|-----------|
| `401` | Missing or invalid admin key |

## API Keys

### Create API Key

```
POST /api/v1/accounts/{account_id}/api-keys
```

Creates a new API key for an existing account.

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `account_id` | string | UUID of the account |

**Example:**

```bash
curl -s -X POST \
  -H "Authorization: Bearer $ADMIN_KEY" \
  "$OYSTER_URL/api/v1/accounts/550e8400-e29b-41d4-a716-446655440000/api-keys" | jq
```

**Response** (`201 Created`):

```json
{
  "id": "b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e",
  "prefix": "a1b2c3d4",
  "bearer_token": "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2",
  "created_at": "2025-01-15T10:30:00Z"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique key identifier |
| `prefix` | string | First 8 characters of the raw key |
| `bearer_token` | string | The full API key — **shown only once** |
| `created_at` | string | ISO 8601 timestamp |

**Errors:**

| Status | Condition |
|--------|-----------|
| `401` | Missing or invalid admin key |
| `403` | Account does not belong to the authenticated app |
| `404` | Account not found |

### Revoke API Key

```
DELETE /api/v1/accounts/{account_id}/api-keys/{key_id}
```

Revokes an API key. The key immediately stops working for authentication.

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `account_id` | string | UUID of the account |
| `key_id` | string | ID of the API key to revoke |

**Example:**

```bash
curl -s -X DELETE \
  -H "Authorization: Bearer $ADMIN_KEY" \
  "$OYSTER_URL/api/v1/accounts/550e8400-e29b-41d4-a716-446655440000/api-keys/b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e"
```

**Response:** `204 No Content`

**Errors:**

| Status | Condition |
|--------|-----------|
| `401` | Missing or invalid admin key |
| `403` | Account does not belong to the authenticated app |
| `404` | API key not found or already revoked |

## S3 Access Keys

These endpoints manage S3-compatible access keys for accounts. See
[S3 Access Keys](access-keys.md) for key format details and limits.

### Create Access Key

```
POST /api/v1/accounts/{account_id}/access-keys
```

Creates a new S3 access key pair. The secret is returned **only once** —
save it immediately. Each account can have up to **3 active access keys**.

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `account_id` | string | UUID of the account |

**Example:**

```bash
curl -s -X POST \
  -H "Authorization: Bearer $ADMIN_KEY" \
  "$OYSTER_URL/api/v1/accounts/550e8400-e29b-41d4-a716-446655440000/access-keys" | jq
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
| `401` | Missing or invalid admin key |
| `403` | Account does not belong to the authenticated app |
| `404` | Account not found |
| `409` | Access key limit reached (max 3 active keys) |

### List Access Keys

```
GET /api/v1/accounts/{account_id}/access-keys
```

Returns all S3 access keys for the account, including revoked ones.
Secrets are **never** included in list responses.

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `account_id` | string | UUID of the account |

**Example:**

```bash
curl -s \
  -H "Authorization: Bearer $ADMIN_KEY" \
  "$OYSTER_URL/api/v1/accounts/550e8400-e29b-41d4-a716-446655440000/access-keys" | jq
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

**Errors:**

| Status | Condition |
|--------|-----------|
| `401` | Missing or invalid admin key |
| `403` | Account does not belong to the authenticated app |
| `404` | Account not found |

### Revoke Access Key

```
DELETE /api/v1/accounts/{account_id}/access-keys/{access_key_id}
```

Revokes an S3 access key. Any S3 requests using this key will
immediately stop working. Revoked keys no longer count toward the
3-key active limit.

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `account_id` | string | UUID of the account |
| `access_key_id` | string | The 20-character access key ID to revoke |

**Example:**

```bash
curl -s -X DELETE \
  -H "Authorization: Bearer $ADMIN_KEY" \
  "$OYSTER_URL/api/v1/accounts/550e8400-e29b-41d4-a716-446655440000/access-keys/OYAK1234567890ABCDEF"
```

**Response:** `204 No Content`

**Errors:**

| Status | Condition |
|--------|-----------|
| `401` | Missing or invalid admin key |
| `403` | Account does not belong to the authenticated app |
| `404` | Access key not found or already revoked |

## Server Commands

The `oysterd app` subcommands let server operators manage apps and admin
keys from the command line.

### Create App

```bash
oysterd app new --name <NAME> --contact_email <EMAIL> [--webhook_url <URL>] [--no-key]
```

Creates a new app, prints its UUID, and (by default) auto-issues a first
admin key alongside.

| Flag | Required | Description |
|------|----------|-------------|
| `--name` | yes | Human-readable app name |
| `--contact_email` | yes | Contact email for the app owner |
| `--webhook_url` | no | Webhook URL for extension failure notifications |
| `--no-key` | no | Skip the auto-issued first admin key |

**Example:**

```bash
oysterd app new --name "my-app" --contact_email "admin@example.com"
# 550e8400-e29b-41d4-a716-446655440000
# 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

### Issue Admin Key

```bash
oysterd app issue-admin-key <app_id>
```

Generates a fresh admin key for the given app. Multiple admin keys per
app are supported with no cap; use this for AWS-style two-key rotation.
The raw key prints to stdout (a single line); the key id and 8-char
prefix print to stderr.

**Example:**

```bash
oysterd app issue-admin-key 550e8400-e29b-41d4-a716-446655440000
# stdout: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
# stderr: issued admin key id=<key_id> prefix=01234567 for app 550e8400-...
```

The printed key can be used directly in the `Authorization` header:

```bash
curl -H "Authorization: Bearer $(oysterd app issue-admin-key $APP_ID)" ...
```

### List Admin Keys

```bash
oysterd app list-admin-keys <app_id>
```

Lists all admin keys for the given app in tab-separated format, including
revoked ones (so an operator can confirm what is currently live).

**Example output:**

```
ID	PREFIX	CREATED_AT	REVOKED_AT
b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e	01234567	2026-04-15T10:30:00Z	-
a1b2c3d4-e5f6-7890-abcd-ef0123456789	89abcdef	2026-03-01T08:00:00Z	2026-04-15T10:31:00Z
```

### Revoke Admin Key

```bash
oysterd app revoke-admin-key <key_id>
```

Marks an admin key as revoked. Subsequent requests using that key are
rejected with `401`. Revocation is by `key_id` (globally unique), not by
the raw key value.

**Example:**

```bash
oysterd app revoke-admin-key b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e
```

### List Apps

```bash
oysterd app list
```

Lists all registered apps in tab-separated format.

**Example output:**

```
ID	NAME	CONTACT_EMAIL
550e8400-e29b-41d4-a716-446655440000	my-app	admin@example.com
```

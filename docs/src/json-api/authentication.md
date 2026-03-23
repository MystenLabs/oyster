# Authentication

Oyster uses API keys (Bearer tokens) to authenticate requests. Your Oyster
administrator provides your initial API key. You can then create additional
keys and revoke them as needed.

## How It Works

Include your API key in the `Authorization` header of every authenticated
request:

```
Authorization: Bearer <your-api-key>
```

API keys are 32-byte random secrets. Oyster stores only a hash of the key,
so a lost key cannot be recovered — you'll need to create a new one.

Each key has a short **prefix** (the first 8 characters) that you can use
to identify which key is which without exposing the full secret.

## Create API Key

```
POST /api/v1/account/api-keys
```

Creates a new API key for your account. The full secret is returned
**only once** in the response — save it immediately.

**Example:**

```bash
curl -s -X POST \
  -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/account/api-keys" | jq
```

**Response** (`201 Created`):

```json
{
  "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "prefix": "a1b2c3d4",
  "secret": "a1b2c3d4e5f67890abcdef1234567890abcdef1234567890abcdef1234567890",
  "created_at": "2025-01-15T10:30:00Z"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique identifier for the key (used to revoke it) |
| `prefix` | string | First 8 characters, for identification |
| `secret` | string | Full API key — **shown only once** |
| `created_at` | string | ISO 8601 timestamp |

## Revoke API Key

```
DELETE /api/v1/account/api-keys/{key_id}
```

Permanently revokes an API key. Only the account that owns the key can
revoke it.

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `key_id` | string | The `id` returned when the key was created |

**Example:**

```bash
curl -s -X DELETE \
  -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/account/api-keys/a1b2c3d4-e5f6-7890-abcd-ef1234567890"
```

**Response:** `204 No Content`

**Errors:**

| Status | Condition |
|--------|-----------|
| `401` | Missing or invalid API key |
| `404` | Key not found or not owned by your account |

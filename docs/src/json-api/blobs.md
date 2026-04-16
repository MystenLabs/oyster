# Blobs

Blobs are binary objects stored inside buckets. Each blob is identified by a
user-chosen **key** (like a file path) and has a content-addressed **blob ID**
computed from its contents.

Key properties:
- **Reads are public** — no authentication needed to download a blob
- **Writes require auth** — uploading, updating, and deleting need a Bearer
  token
- **Overwrite semantics** — uploading to an existing key replaces the blob
- **Content-addressed** — identical content is stored only once
- **Reference-counted deletion** — underlying data is removed only when no
  keys reference it
- **30-day expiration** — blobs expire by default; an automatic extension
  service renews them

## Store Blob

```
PUT /api/v1/buckets/{bucket_name}/blobs/{key}
```

Uploads binary data to the specified bucket and key. If a blob already
exists at that key, it is replaced.

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `bucket_name` | string | Target bucket |
| `key` | string | Object key (e.g., `images/photo.png`) |

**Request headers:**

| Header | Default | Description |
|--------|---------|-------------|
| `Content-Type` | `application/octet-stream` | MIME type stored with the blob |
| `If-Match` | — | Only overwrite if the existing blob's ETag matches (412 otherwise) |
| `If-None-Match` | — | Set to `*` to create only if the key doesn't exist (412 otherwise) |

**Request body:** Raw binary data (max **1 GB**)

**Example — upload a string:**

```bash
curl -s -X PUT \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: text/plain" \
  --data-binary "Hello, Oyster!" \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/hello.txt" | jq
```

**Example — upload a file:**

```bash
curl -s -X PUT \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: image/png" \
  --data-binary @photo.png \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/images/photo.png" | jq
```

**Example — create only (fail if key exists):**

```bash
curl -s -X PUT \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: text/plain" \
  -H "If-None-Match: *" \
  --data-binary "Hello, Oyster!" \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/hello.txt" | jq
```

**Example — safe overwrite (only if ETag matches):**

```bash
curl -s -X PUT \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: text/plain" \
  -H 'If-Match: "9a0364b9e99bb480dd25e1f0284c8555"' \
  --data-binary "Updated content" \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/hello.txt" | jq
```

**Response** (`201 Created`):

```json
{
  "key": "hello.txt",
  "blob_id": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
  "size": 14,
  "md5": "9a0364b9e99bb480dd25e1f0284c8555",
  "sui_object_id": null,
  "created_at": "2025-01-15T10:31:00Z",
  "expires_at": "2025-02-14T10:31:00Z"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `key` | string | The object key |
| `blob_id` | string | Content-addressed hash of the blob data |
| `size` | integer | Size in bytes |
| `md5` | string | Hex-encoded MD5 digest (used as S3 ETag) |
| `sui_object_id` | string or null | On-chain Sui object ID (if stored on Walrus) |
| `created_at` | string | ISO 8601 timestamp |
| `expires_at` | string or null | ISO 8601 expiration (default: 30 days) |

The response includes an `ETag` header containing the quoted MD5 digest
(e.g., `"9a0364b9e99bb480dd25e1f0284c8555"`).

**Errors:**

| Status | Condition |
|--------|-----------|
| `401` | Missing or invalid API key |
| `404` | Bucket not found |
| `412` | `If-Match` or `If-None-Match` condition failed |
| `413` | Payload exceeds 1 GB |

## Read Blob by Key

```
GET /api/v1/buckets/{bucket_name}/blobs/{key}
```

Downloads a blob's contents. **No authentication required.**

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `bucket_name` | string | Bucket containing the blob |
| `key` | string | Object key |

**Example:**

```bash
curl -s "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/hello.txt"
```

**Conditional headers:**

| Header | Effect |
|--------|--------|
| `If-Match: "<etag>"` | Return the blob only if its ETag matches; otherwise `412` |
| `If-None-Match: "<etag>"` | Return the blob only if its ETag differs; otherwise `304` |

**Example — cache validation:**

```bash
curl -s -o /dev/null -w "%{http_code}" \
  -H 'If-None-Match: "9a0364b9e99bb480dd25e1f0284c8555"' \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/hello.txt"
# Returns 304 if unchanged, 200 with body if changed
```

**Response** (`200 OK`):
- **Body:** Raw binary blob data
- **Content-Type:** The MIME type set during upload
- **ETag:** Quoted MD5 digest (e.g., `"9a0364b9e99bb480dd25e1f0284c8555"`)
- **X-Content-Type-Options:** `nosniff` — prevents browsers from MIME-sniffing

**Errors:**

| Status | Condition |
|--------|-----------|
| `304` | `If-None-Match` matched — blob has not changed |
| `404` | Blob not found |
| `412` | `If-Match` condition failed |

## Read Blob by Blob ID

```
GET /api/v1/blobs/by-blob-id/{blob_id}
```

Downloads a blob by its content-addressed hash. Useful when you know the
blob ID but not which bucket or key it's stored under.
**No authentication required.**

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `blob_id` | string | Content-addressed blob hash |

**Example:**

```bash
curl -s "$OYSTER_URL/api/v1/blobs/by-blob-id/2cf24dba5fb0a30e..."
```

**Response** (`200 OK`):
- **Body:** Raw binary blob data
- **Content-Type:** `application/octet-stream`
- **X-Content-Type-Options:** `nosniff` — prevents browsers from MIME-sniffing

**Errors:**

| Status | Condition |
|--------|-----------|
| `404` | Blob ID not found |

## Duplicate Blob

```
POST /api/v1/buckets/{bucket_name}/blobs/{key}/duplicate
```

Reifies a second `(bucket, key)` reference to an already-stored blob without
re-uploading its bytes. The resulting blob shares the source's `blob_id`,
`size`, and `md5`, but receives a fresh `sui_object_id`, `created_at`, and
`expires_at`. For on-chain backends, the caller pays `reserve_space`,
`register_blob`, and `certify_blob` fees but no sliver bandwidth.

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `bucket_name` | string | Source bucket |
| `key` | string | Source object key |

**Auth:** Bearer token. The caller must own **both** the source bucket and
the destination bucket.

**Request headers:**

| Header | Default | Description |
|--------|---------|-------------|
| `If-Match` | — | Only overwrite the destination if its existing ETag matches (412 otherwise) |
| `If-None-Match` | — | Set to `*` to create only if the destination doesn't exist (412 otherwise) |

**Request body** (`DuplicateBlobRequest`):

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `destination_bucket` | string | yes | Target bucket (may equal the source bucket) |
| `destination_key` | string | yes | Target object key (must differ from source when bucket is the same) |
| `content_type` | string | no | MIME type for the new row. Defaults to the source's `content_type` |

**Example:**

```bash
curl -s -X POST \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "destination_bucket": "backup-bucket",
    "destination_key": "archive/photo.png"
  }' \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/images/photo.png/duplicate" | jq
```

**Response** (`201 Created`): Same shape as the [Store Blob](#store-blob)
response. The `blob_id` matches the source; `sui_object_id`, `created_at`,
and `expires_at` are fresh. The response includes an `ETag` header with the
quoted MD5 (identical to the source's ETag).

**Errors:**

| Status | Condition |
|--------|-----------|
| `400` | Malformed body, or source and destination are the same `(bucket, key)` |
| `401` | Missing or invalid API key |
| `402` | Insufficient on-chain balance for the requested operation |
| `404` | Source blob not found, destination bucket not found, or either not owned by your account |
| `412` | `If-Match` or `If-None-Match` condition failed on the destination |
| `501` | Blob-store backend does not support duplication |

## List Blobs

```
GET /api/v1/buckets/{bucket_name}/blobs
```

Returns a paginated list of blobs in a bucket.

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `bucket_name` | string | Bucket to list |

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `cursor` | string | — | Opaque cursor from a previous `next_cursor` |
| `limit` | integer | 20 | Items per page (max: 100) |

**Example:**

```bash
curl -s -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs?limit=50" | jq
```

**Response** (`200 OK`):

```json
{
  "data": [
    {
      "key": "hello.txt",
      "blob_id": "2cf24dba5fb0a30e...",
      "bucket_name": "my-bucket",
      "account_id": "550e8400-e29b-41d4-a716-446655440000",
      "content_type": "text/plain",
      "size": 14,
      "md5": "9a0364b9e99bb480...",
      "sui_object_id": null,
      "created_at": "2025-01-15T10:31:00Z",
      "expires_at": "2025-02-14T10:31:00Z"
    }
  ],
  "next_cursor": null
}
```

| Field | Type | Description |
|-------|------|-------------|
| `key` | string | Object key |
| `blob_id` | string | Content-addressed hash |
| `bucket_name` | string | Containing bucket |
| `account_id` | string | Owning account UUID |
| `content_type` | string | MIME type |
| `size` | integer | Size in bytes |
| `md5` | string | Hex-encoded MD5 digest |
| `sui_object_id` | string or null | On-chain Sui object ID |
| `created_at` | string | ISO 8601 timestamp |
| `expires_at` | string or null | ISO 8601 expiration |

## Update Blob Metadata

```
PATCH /api/v1/buckets/{bucket_name}/blobs/{key}/metadata
```

Updates metadata for an existing blob. Currently only `content_type` can be
changed.

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `bucket_name` | string | Bucket containing the blob |
| `key` | string | Object key |

**Request body:**

```json
{
  "content_type": "image/png"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `content_type` | string | yes | New MIME type for the blob |

**Example:**

```bash
curl -s -X PATCH \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"content_type": "image/png"}' \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/photo.png/metadata" | jq
```

**Response** (`200 OK`): Full blob metadata (same shape as items in
[List Blobs](#list-blobs)).

**Errors:**

| Status | Condition |
|--------|-----------|
| `400` | `content_type` not provided |
| `401` | Missing or invalid API key |
| `404` | Blob not found |

## Delete Blob

```
DELETE /api/v1/buckets/{bucket_name}/blobs/{key}
```

Deletes a blob by key. The underlying data is only removed from storage
when no other keys reference the same content (reference-counted deletion).

**Path parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `bucket_name` | string | Bucket containing the blob |
| `key` | string | Object key to delete |

**Conditional headers:**

| Header | Effect |
|--------|--------|
| `If-Match: "<etag>"` | Delete only if ETag matches; otherwise `412` |
| `If-None-Match: "<etag>"` | Delete only if ETag differs; otherwise `412` |

**Example:**

```bash
curl -s -X DELETE \
  -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/hello.txt"
```

**Example — delete only if ETag matches:**

```bash
curl -s -X DELETE \
  -H "Authorization: Bearer $API_KEY" \
  -H 'If-Match: "9a0364b9e99bb480dd25e1f0284c8555"' \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/hello.txt"
```

**Response:** `204 No Content`

**Errors:**

| Status | Condition |
|--------|-----------|
| `401` | Missing or invalid API key |
| `404` | Blob not found |
| `412` | `If-Match` or `If-None-Match` condition failed |

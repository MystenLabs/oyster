# JSON API Reference

The Oyster JSON API is served under `/api/v1/`. All requests and responses
use JSON (except blob content, which is raw binary). Authenticated endpoints
require a Bearer token in the `Authorization` header.

## Base URL

All API endpoints are prefixed with `/api/v1`:

```
$OYSTER_URL/api/v1/
```

Throughout this reference, we assume `$OYSTER_URL` is set to your Oyster
server address (e.g., `http://localhost:3000`).

## Authentication

Most endpoints require a Bearer token:

```
Authorization: Bearer <your-api-key>
```

Endpoints that **do not** require authentication:
- Reading blobs by key or blob ID
- Health, readiness, and metrics probes
- OpenAPI documentation

## Error Responses

All errors return a JSON body with a single `error` field:

```json
{
  "error": "human-readable error message"
}
```

### Status Codes

| Code | Meaning |
|------|---------|
| `200` | Success (GET, PATCH) |
| `201` | Created (POST, PUT) |
| `204` | No Content (DELETE) |
| `304` | Not Modified — `If-None-Match` matched on a GET request |
| `400` | Bad Request — invalid input or validation failure |
| `401` | Unauthorized — missing or invalid API key |
| `402` | Payment Required — insufficient on-chain balance for the requested operation |
| `404` | Not Found — resource doesn't exist or not owned by your account |
| `409` | Conflict — resource already exists or limit reached |
| `412` | Precondition Failed — `If-Match` or `If-None-Match` condition not met |
| `413` | Payload Too Large — blob exceeds 1 GB |
| `500` | Internal Server Error — unexpected error |
| `501` | Not Implemented — endpoint exists but isn't functional yet |
| `502` | Bad Gateway — upstream storage service unreachable or returned a server-side error |
| `503` | Service Unavailable — a dependent service is unreachable |

## Response Headers

Oyster sets these headers on responses (in addition to the usual
`Content-Type` / `Content-Length`):

| Header | Where it appears | Description |
|--------|------------------|-------------|
| `X-Oyster-Version` | Every response | The running Oyster server's crate version |
| `ETag` | Blob store, read, list, and duplicate responses | Quoted MD5 digest of the blob (e.g., `"9a0364b9e99bb480dd25e1f0284c8555"`) |
| `X-Content-Type-Options` | Blob read responses | Always `nosniff` — prevents browsers from MIME-sniffing |

## Pagination

List endpoints use **cursor-based pagination**:

**Query parameters:**
- `cursor` (optional) — opaque string from a previous response's `next_cursor`
- `limit` (optional) — number of items per page (default: 20, max: 100)

**Response format:**

```json
{
  "data": [ ... ],
  "next_cursor": "opaque-cursor-string"
}
```

When `next_cursor` is `null`, there are no more results. To fetch the next
page, pass the `next_cursor` value as the `cursor` query parameter.

**Example — paginating through buckets:**

```bash
# First page
curl -s -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/buckets?limit=10" | jq

# Next page (using next_cursor from previous response)
curl -s -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/buckets?limit=10&cursor=eyJjcmVhdGVk..." | jq
```

## Interactive Documentation

Oyster serves an interactive OpenAPI UI at:

```
$OYSTER_URL/api/docs
```

You can explore and test all endpoints directly from your browser.

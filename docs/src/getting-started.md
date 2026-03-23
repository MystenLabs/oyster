# Getting Started

This guide walks you through your first interactions with Oyster. By the end
you'll have created a bucket, uploaded a blob, and downloaded it back.

## Prerequisites

- **curl** — for making HTTP requests to the JSON API
- **AWS CLI** (optional) — for using the S3-compatible API
  ([install guide](https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html))

## Obtaining Credentials

Contact your **Oyster administrator** to receive an API key. This is a
Bearer token that authenticates your requests. It looks something like:

```
a1b2c3d4e5f67890abcdef1234567890abcdef1234567890abcdef1234567890
```

Store it somewhere safe — it cannot be retrieved again after initial
creation.

For the rest of this guide, we'll assume your API key is stored in an
environment variable:

```bash
export OYSTER_URL="http://localhost:3000"
export API_KEY="your-api-key-here"
```

## Create Your First Bucket

Buckets are named containers for your blobs. Create one called `my-bucket`:

```bash
curl -s -X POST \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name": "my-bucket"}' \
  "$OYSTER_URL/api/v1/buckets" | jq
```

Response:

```json
{
  "name": "my-bucket",
  "account_id": "550e8400-e29b-41d4-a716-446655440000",
  "created_at": "2025-01-15T10:30:00Z"
}
```

## Upload a Blob

Upload a text file to your bucket with the key `hello.txt`:

```bash
curl -s -X PUT \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: text/plain" \
  --data-binary "Hello, Oyster!" \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/hello.txt" | jq
```

Response:

```json
{
  "key": "hello.txt",
  "blob_id": "2cf24dba5fb0a30e...",
  "size": 14,
  "md5": "9a0364b9e99bb480...",
  "sui_object_id": null,
  "created_at": "2025-01-15T10:31:00Z",
  "expires_at": "2025-02-14T10:31:00Z"
}
```

You can also upload a file from disk:

```bash
curl -s -X PUT \
  -H "Authorization: Bearer $API_KEY" \
  --data-binary @photo.png \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/images/photo.png" | jq
```

## Download a Blob

Blob reads are **public** — no authentication needed:

```bash
curl -s "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/hello.txt"
```

Output:

```
Hello, Oyster!
```

## List Blobs in a Bucket

```bash
curl -s -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs" | jq
```

Response:

```json
{
  "data": [
    {
      "key": "hello.txt",
      "blob_id": "2cf24dba5fb0a30e...",
      "bucket_name": "my-bucket",
      "content_type": "text/plain",
      "size": 14,
      "md5": "9a0364b9e99bb480...",
      "created_at": "2025-01-15T10:31:00Z",
      "expires_at": "2025-02-14T10:31:00Z"
    }
  ],
  "next_cursor": null
}
```

## Delete a Blob

```bash
curl -s -X DELETE \
  -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/buckets/my-bucket/blobs/hello.txt"
```

Returns HTTP 204 (no content) on success.

## Create Additional API Keys

You can generate extra API keys for different applications or teammates:

```bash
curl -s -X POST \
  -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/account/api-keys" | jq
```

Response:

```json
{
  "id": "key-uuid-here",
  "prefix": "a1b2c3d4",
  "bearer_token": "a1b2c3d4e5f67890abcdef1234567890abcdef1234567890abcdef1234567890",
  "created_at": "2025-01-15T10:32:00Z"
}
```

> **Important:** The `bearer_token` field is only shown once. Save it immediately.

## Set Up S3 Access Keys

To use the AWS CLI or any S3-compatible SDK, create S3 access keys:

```bash
curl -s -X POST \
  -H "Authorization: Bearer $API_KEY" \
  "$OYSTER_URL/api/v1/account/access-keys" | jq
```

Response:

```json
{
  "access_key_id": "OYAK1234567890ABCDEF",
  "secret_access_key": "abcdef1234567890abcdef1234567890abcdef12",
  "created_at": "2025-01-15T10:33:00Z"
}
```

> **Important:** The `secret_access_key` is only shown once. Save it
> immediately. You can have up to 3 active S3 access keys per account.

Then configure the AWS CLI:

```bash
aws configure set aws_access_key_id "OYAK1234567890ABCDEF" --profile oyster
aws configure set aws_secret_access_key "abcdef1234567890..." --profile oyster
aws configure set region "us-east-1" --profile oyster
aws configure set endpoint_url "$OYSTER_URL" --profile oyster
```

Now you can use standard S3 commands:

```bash
# Create a bucket
aws --profile oyster s3api create-bucket --bucket my-s3-bucket

# Upload a file
aws --profile oyster s3api put-object \
  --bucket my-s3-bucket --key hello.txt --body hello.txt

# Download a file
aws --profile oyster s3api get-object \
  --bucket my-s3-bucket --key hello.txt downloaded.txt
```

For the full S3 API reference, see **[S3 API Reference](s3-api/README.md)**.

## What's Next

- **[JSON API Reference](json-api/README.md)** — detailed documentation of
  every endpoint.
- **[S3 API Reference](s3-api/README.md)** — complete S3-compatible
  operations and setup.
- **[Guides](guides/README.md)** — CLI quick start, SDK examples, and
  advanced topics.

# oyster-cli Quick Start

`oyster-cli` is a command-line tool for interacting with Oyster. It wraps
the JSON API and handles authentication, content-type detection, and
pagination for you.

## Configuration

The CLI looks for a config file in this order:

1. Path specified with `--config`
2. `./client.yaml` (current directory)
3. `$XDG_CONFIG_HOME/oyster/client.yaml`
4. `$HOME/.config/oyster/client.yaml`

**Example `client.yaml`:**

```yaml
url: "http://localhost:3000"
api_key: "your-api-key-here"
```

You can also pass these as flags or set them for individual commands:

```bash
oyster --url http://localhost:3000 --api-key "your-key" list-buckets
```

## Global Flags

| Flag | Description |
|------|-------------|
| `--config <PATH>` | Path to config file |
| `--url <URL>` | Oyster server URL |
| `--api-key <KEY>` | API key for authentication |
| `--json` | Output JSON instead of human-readable format |

## Bucket Management

### Create a Bucket

```bash
oyster create-bucket my-bucket
```

### List Buckets

```bash
oyster list-buckets
```

Limit results:

```bash
oyster list-buckets --limit 10
```

### Delete a Bucket

```bash
oyster delete-bucket my-bucket
```

This deletes all blobs in the bucket.

## Storing and Reading Blobs

### Upload a File

```bash
oyster store photo.png --bucket my-bucket
```

The key defaults to the filename (`photo.png`). Override it with `--key`:

```bash
oyster store photo.png --bucket my-bucket --key images/vacation/photo.png
```

Set a specific content type:

```bash
oyster store data.bin --bucket my-bucket --content-type application/x-custom
```

If `--content-type` is omitted, the CLI auto-detects it from the file
extension (see [Content-Type Detection](#content-type-detection) below).

### Download a File

```bash
oyster read hello.txt --bucket my-bucket
```

This prints the blob contents to stdout. Save to a file with `-o`:

```bash
oyster read hello.txt --bucket my-bucket -o downloaded.txt
```

> **Note:** Reading blobs does not require an API key — reads are public.

### List Blobs

```bash
oyster list-blobs --bucket my-bucket
```

Output (human-readable):

```
KEY            CONTENT_TYPE    SIZE    CREATED
hello.txt      text/plain      14      2025-01-15T10:31:00Z
images/cat.png image/png       204800  2025-01-15T11:00:00Z
```

### Delete a Blob

```bash
oyster delete hello.txt --bucket my-bucket
```

## API Key Management

### Create a New API Key

```bash
oyster create-api-key
```

Output:

```
Created API key:
  ID:     a1b2c3d4-e5f6-7890-abcd-ef1234567890
  Prefix: oysk_a1b
  Secret: oysk_a1b2c3d4e5f6789...

Save this secret — it cannot be retrieved again.
```

### Revoke an API Key

```bash
oyster revoke-api-key a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

## S3 Access Key Management

### Create an S3 Access Key

```bash
oyster create-access-key
```

Output:

```
Created S3 access key:
  Access Key ID:     OYAK1234567890ABCDEF
  Secret Access Key: abcdef1234567890abcdef1234567890abcdef12

Save this secret — it cannot be retrieved again.
```

### List S3 Access Keys

```bash
oyster list-access-keys
```

### Delete an S3 Access Key

```bash
oyster delete-access-key OYAK1234567890ABCDEF
```

## Other Commands

### View Wallet Address

```bash
oyster wallet
```

### View Resolved Configuration

```bash
oyster info
```

Shows which config file is loaded, the server URL, and the API key prefix.

## JSON Output

Add `--json` to any command for machine-readable output:

```bash
oyster --json list-blobs --bucket my-bucket
```

```json
{
  "data": [
    {
      "key": "hello.txt",
      "blob_id": "2cf24dba5fb0a30e...",
      "content_type": "text/plain",
      "size": 14,
      "created_at": "2025-01-15T10:31:00Z"
    }
  ],
  "next_cursor": null
}
```

## Content-Type Detection

When uploading without `--content-type`, the CLI guesses the MIME type from
the file extension:

| Extension | Content-Type |
|-----------|-------------|
| `.txt` | `text/plain` |
| `.html`, `.htm` | `text/html` |
| `.css` | `text/css` |
| `.csv` | `text/csv` |
| `.js` | `application/javascript` |
| `.json` | `application/json` |
| `.xml` | `application/xml` |
| `.yaml`, `.yml` | `application/yaml` |
| `.png` | `image/png` |
| `.jpg`, `.jpeg` | `image/jpeg` |
| `.gif` | `image/gif` |
| `.svg` | `image/svg+xml` |
| `.webp` | `image/webp` |
| `.pdf` | `application/pdf` |
| `.zip` | `application/zip` |
| `.gz`, `.gzip` | `application/gzip` |
| `.tar` | `application/x-tar` |
| `.wasm` | `application/wasm` |
| `.mp3` | `audio/mpeg` |
| `.mp4` | `video/mp4` |
| `.webm` | `video/webm` |
| (other) | `application/octet-stream` |

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

### Contexts

`client.yaml` holds a map of **named contexts**, each pointing at a
different Oyster deployment. The top-level `active_context` selects which
context is used by default.

**Example `client.yaml`:**

```yaml
active_context: testnet
contexts:
  testnet:
    url: "https://oyster.testnet.example/api/v1"
    api_key: "your-api-key-here"
    apps:
      my-app-1:
        admin_key: "<64-char hex admin key>"
      my-app-2:
        admin_key: "<64-char hex admin key>"
  devnet:
    url: "http://localhost:3000/api/v1"
    api_key: "dev-key"
```

> **Important:** The URL must include the `/api/v1` path. The CLI appends
> endpoint paths (like `/buckets`) directly to this URL.

> **Breaking change:** the pre-0.3 flat schema (top-level `url` /
> `api_key`) no longer parses. Migrate by wrapping your existing values in
> a named context under `contexts:`.

Precedence for the active-context name (highest first):

1. `--context <name>` flag
2. `OYSTER_CONTEXT` environment variable
3. `active_context` field in `client.yaml`

If none of the three is set and the file has exactly one context, that
context is used automatically. Ad-hoc `--url ... --api-key ...`
invocations without any context still work for one-off commands.

You can also override individual fields via flags:

```bash
oyster --url http://localhost:3000/api/v1 --api-key "your-key" list-buckets
```

## Global Flags

| Flag | Description |
|------|-------------|
| `--config <PATH>` | Path to config file |
| `--context <NAME>` | Named context to use (overrides `OYSTER_CONTEXT` / `active_context`) |
| `--url <URL>` | Oyster server URL (overrides the context's `url`) |
| `--api-key <KEY>` | API key (overrides the context's `api_key`) |
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

The bucket must be empty. Delete all blobs first, or the server returns
an error.

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

## API Key and Access Key Management

API keys and S3 access keys are managed by operators through the Admin API,
not through the CLI. See the [Admin API docs](../json-api/admin.md) for
details on creating, listing, and revoking keys.

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

## App Admin-Key Management

Apps — first-class principals that authenticate admin app-management calls
(creating accounts, issuing API keys / S3 access keys) independently of
end-user API keys — need a way to store the per-app admin key without
leaking it through shell history. The CLI persists admin keys under
`contexts.<ctx>.apps.<app_name>.admin_key`.

### Import an Admin Key

```bash
oyster app import my-app
```

Prompts for the admin key without echoing it (when stdin is a tty), then
writes it to the active context's `apps.my-app` entry. If stdin is a pipe,
the key is read as a line instead — useful for scripts. Requires that
`client.yaml` already exists; the CLI does not auto-create it.

### Rotation

Admin keys do not expire. Rotation is operator-driven, AWS-style two-key
overlap:

```bash
# operator
oysterd app issue-admin-key <APP_ID>
# prints: <new admin_key>           (to stdout)
# prints: <new key id>              (to stderr — needed later to revoke)

# user — replace the local entry with the new key
oyster app import my-app

# operator — after confirming nothing still uses the old key
oysterd app revoke-admin-key <OLD_KEY_ID>
```

`oysterd app list-admin-keys <APP_ID>` shows all keys (active and
revoked) so an operator can confirm what is live. Multiple admin keys per
app are supported with no cap.

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

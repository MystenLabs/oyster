#!/usr/bin/env python3
"""Generate an Oyster API key and print the bearer token and its Blake2s-256 hash.

Usage:
    python3 scripts/generate-api-key.py

The bearer token is the raw hex secret you pass as `Authorization: Bearer <token>`.
The hash is what gets stored in the `api_keys.key_hash` column.
"""

import hashlib
import os
import sys
import uuid

if len(sys.argv) < 2:
    print(f"usage: {sys.argv[0]} <account_id>", file=sys.stderr)
    print(f"", file=sys.stderr)
    print(f"To create a test account first:", file=sys.stderr)
    print(f"  PostgreSQL:  INSERT INTO accounts (id, name) VALUES (gen_random_uuid()::text, gen_random_uuid()::text) RETURNING id;", file=sys.stderr)
    print(f"  SQLite:      WITH u(v) AS (SELECT lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || substr(hex(randomblob(2)),2) || '-' || substr('89ab', abs(random()) % 4 + 1, 1) || substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6)))) INSERT INTO accounts (id, name) SELECT v, v FROM u RETURNING id;", file=sys.stderr)
    sys.exit(1)

account_id = sys.argv[1]
key_id = str(uuid.uuid4())
raw_bytes = os.urandom(32)
bearer_token = raw_bytes.hex()
key_hash = hashlib.new("blake2s", bearer_token.encode(), digest_size=32).hexdigest()
prefix = bearer_token[:8]

print(f"bearer_token: {bearer_token}")
print(f"key_hash:     {key_hash}")
print(f"prefix:       {prefix}")
print()
print(f"PostgreSQL / SQLite:")
print(f"  INSERT INTO api_keys (id, account_id, key_hash, prefix) VALUES ('{key_id}', '{account_id}', '{key_hash}', '{prefix}');")
print()
print(f"Then create an S3 access key:")
print(f"  curl -X POST http://localhost:3000/api/v1/account/access-keys -H 'Authorization: Bearer {bearer_token}'")

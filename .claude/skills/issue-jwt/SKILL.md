---
name: issue-jwt
description: Steps to issue a new admin key for an app
allowed-tools: ""
---

Do NOT run any commands. Simply print the following instructions for the admin to follow.

---

# Issue an Admin Key for an App

> Historical note: this skill was originally `/issue-jwt`; admin auth has
> moved from short-lived JWTs to long-lived per-app **admin keys**. The
> path stays so existing `/issue-jwt` invocations still work.

## Prerequisites

- Access to the Oyster server where `oysterd` and the database are available.
  No special env var is needed for issuance — admin keys are stored in the
  `app_admin_keys` table, hashed with BLAKE2s-256.

## Steps

1. Look up (or create) the app:
   - List apps: `oysterd app list`
   - Create one: `oysterd app new --name "<name>" --contact_email "<email>"`
     (auto-issues a first admin key by default; pass `--no-key` to opt out).
2. Issue an admin key: `oysterd app issue-admin-key <APP_ID>`
   - Stdout: the raw 64-char hex Bearer token (single line).
   - Stderr: the new key id and 8-char prefix — keep these for auditing /
     later revocation.
3. Hand the printed admin key to the user. It does not expire.

## Rotation

Admin keys are long-lived; rotation is voluntary. Use AWS-style two-key
overlap so no caller is interrupted:

1. `oysterd app issue-admin-key <APP_ID>` — issue a new key alongside the
   old one.
2. Have the user import it (`oyster app import <name>` or update their
   config) and confirm their workloads have switched.
3. `oysterd app revoke-admin-key <OLD_KEY_ID>` — revoke the old key.
   Revocation is immediate; there is no caching.

## Auditing

- `oysterd app list-admin-keys <APP_ID>` — TSV listing of every key for
  the app, including revoked ones (id, prefix, created_at, revoked_at).

## Notes

- Multiple admin keys per app are supported with no cap.
- Wire format: `Authorization: Bearer <64-char hex>`. Same shape as data
  plane API keys; routing decides which credential table the bearer is
  looked up in.
- Relevant code: `crates/oyster/src/app_admin.rs`,
  `crates/oyster/src/db/app_admin_keys.rs`, `crates/oyster/src/main.rs`.

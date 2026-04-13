---
name: issue-jwt
description: Steps to issue a new JWT for an app account
allowed-tools: ""
---

Do NOT run any commands. Simply print the following instructions for the admin to follow.

---

# Issue a JWT for an App Account

## Prerequisites

- Access to the Oyster server where `oysterd` and the database are available.
- `OYSTER_JWT_SECRET` env var set: `export OYSTER_JWT_SECRET=$(cat /secrets/oyster-jwt-secret)`

## Steps

1. Look up (or create) the app:
   - List apps: `oysterd app list`
   - Create one: `oysterd app new --name "<name>" --contact_email "<email>"`
2. Issue the JWT: `oysterd app jwt <APP_ID>`
3. Hand the printed JWT to the user. It is valid for 24 hours.

## Revoking

`oysterd app revoke-jwt <JTI>` (decode the token to extract the `jti` claim).

## Notes

- Token refresh is available at `POST /api/v1/apps/token-refresh` (48h grace window), but only if the app has `allow_refresh_jwt = true`.
- Relevant code: `crates/oyster/src/app_auth.rs`, `crates/oyster/src/main.rs`.

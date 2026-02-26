# Gap Analysis: Harbor Spec vs. Oyster API

## Summary

Oyster covers the **core data-plane primitives** well: accounts, API keys, buckets, blobs (CRUD + content-addressing), and wallet-backed on-chain storage via Pearl. However, Harbor is fundamentally a **multi-tenant, multi-user product** with identity, collaboration, billing, encryption, and sharing layers — none of which exist in Oyster today. A Harbor backend would need a substantial RDBMS (or equivalent) to hold all the objects and relationships below, plus new service layers for auth, billing, import, and encryption orchestration.

---

## Objects/Nouns Not in Oyster

| Harbor Object | Notes |
|---|---|
| **Space** (Personal / Team) | Oyster has `Account` but no concept of workspaces, personal vs. team distinction, or the 1-personal + N-team-spaces-per-user model |
| **User** (human identity) | Oyster has no user entity — only `Account` + `API Key`. No email, name, avatar, OAuth subject ID, linked wallet address |
| **Team membership** | No join table for users ↔ spaces, no role enum (Admin / Editor / Viewer) |
| **OAuth / zkLogin credentials** | No storage for Google/Microsoft identity tokens, zkLogin proofs, or wallet-linking records |
| **Session / auth tokens** | Oyster only has long-lived API keys — no short-lived session tokens for browser-based UI auth |
| **API key scopes** | Oyster API keys are all-or-nothing per account. Harbor needs per-key operation scopes (read-only, read/write) and space-scoping |
| **Billing plan / subscription** | No plan tier (Free/Basic/Premium), no subscription lifecycle, no payment method storage |
| **Plan limits / quotas** | No enforcement objects for max buckets, max storage, max users, max imports per plan |
| **Usage / metering records** | No per-space storage consumption tracking, no per-key attribution for billing |
| **Bucket encryption config** | Oyster buckets have no `public` vs `private` flag, no Seal policy binding |
| **Encryption key references** | No records linking Seal keys to user identity (personal) or team membership allowlists (team) |
| **Share links** | No share-link objects (token, target bucket, expiry, read-only flag) |
| **Visibility settings** | No per-bucket or per-blob visibility enum (private / shared / public) |
| **Import job** | No object for import source (Drive/OneDrive/S3), OAuth tokens/secrets, job state, progress, failure log |
| **Import source connection** | No OAuth credential storage for external providers |
| **Metrics / analytics events** | No time-to-first-upload, active-space counts, or private-data-percentage tracking |
| **Exportable metadata bundle** | No first-class "export" object packaging bucket IDs, blob IDs, encryption state, and key material references |
| **Invitation** | No invite objects for team onboarding flows |

---

## Actions/Verbs Not Satisfiable by Oyster

| Harbor Action | Gap |
|---|---|
| **Sign in with Google / Microsoft** | No OAuth / zkLogin flow — Oyster only has API key auth |
| **Sign in with wallet** | No wallet-connect or wallet-auth flow for end users |
| **Auto-create wallet-backed identity on sign-up** | Oyster can create Pearl accounts, but has no user-facing identity wiring or zkLogin binding |
| **Create / manage Spaces** | No space CRUD, no personal-space auto-provisioning on signup |
| **Invite / remove team members** | No membership management |
| **Assign / change roles** | No role assignment or enforcement |
| **Scope an API key** to a space + operation set | API keys are unscoped |
| **Set bucket as Public / Private (encrypted)** | No encryption config on bucket creation or update |
| **Encrypt blob client-side with Seal** | No Seal SDK integration, no key-binding orchestration |
| **Decrypt blob with Seal** | Same — no decryption orchestration |
| **Generate / revoke share links** | No sharing subsystem |
| **Enforce plan limits** (bucket count, storage cap, user cap) | No quota checks on any write path |
| **Subscribe / change billing plan** | No billing integration |
| **Process payments** | No payment provider integration |
| **Import from Google Drive / OneDrive / S3** | No import pipeline, no OAuth token exchange, no background job runner |
| **Track import progress** | No job-status polling or webhook mechanism |
| **Rename a file** | Oyster has no rename endpoint (Harbor spec notes "delete and create" workaround, but no atomic rename) |
| **Preview files** (image thumbnails, video thumbnails, PDF rendering) | No preview/thumbnail generation |
| **Export metadata bundle** for survivability/exit | No export endpoint |
| **Enforce data-plane / control-plane separation guarantees** | Oyster conflates them — blob reads are unauthenticated but metadata is in Oyster's SQLite, not independently recoverable |
| **Track and report metrics** (active spaces, time-to-first-upload, % private data, team utilization) | No analytics or metrics collection |
| **Restrict a user while preserving Walrus-level access** | No concept of "soft ban" that disables UI but leaves on-chain data intact |

---

## What Oyster Does Cover

For completeness, these Harbor needs map well to existing Oyster primitives:

- Create / list / delete **buckets** (1:1 with Walrus Buckets)
- Upload / download / list / delete **blobs**
- Blob metadata (size, content type, expiry)
- Content-addressed deduplication
- On-chain storage registration + certification (via Pearl)
- Background blob **extension** (epoch renewal)
- API key creation / revocation (basic auth)
- Wallet creation + balance tracking (via Pearl)

In short: Oyster is the **storage engine**. Harbor needs an entire **product backend** on top — identity, authorization, multi-tenancy, billing, encryption orchestration, import pipelines, sharing, and metrics.

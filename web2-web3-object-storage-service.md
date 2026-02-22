# Web2 → Web3 Object Storage Service

Will Bradley - 2026-02-18 - DRAFT

## Overview

This service is a first-party hosted solution for basic Walrus usage. The goal is to close the gap between “I need decentralized storage” and our current offerings, which require developers to manage Sui wallets, WAL tokens, and blob lifecycles themselves.

The thesis: Walrus adoption would grow if the last mile of usability were handled by a trusted party - Mysten Labs (“ML”) - while preserving the underlying decentralization of the storage layer.

Under this model, developers trust ML to manage Walrus blob registration and payment on their behalf, in exchange for a monthly usage-based bill denominated in fiat. The core value proposition: **blobs live indefinitely as long as the developer is paying.** The service automatically extends blob storage before it expires, and bills the developer for the extension - no manual epoch management required. The developer’s data lives on the Walrus network - not in ML-proprietary storage - and remains accessible via standard Walrus APIs at all times. If a developer wants to leave the service, they can transfer ownership of their on-chain blob objects to their own wallet via the `/account/transfer` endpoint.

The API aims to approximate S3’s basic operational model: buckets, blob CRUD, API key auth.

## Assumptions

- All developer-supplied **blob data** is stored in Walrus, not in ML-proprietary storage. The service itself maintains operational state (accounts, API keys, bucket metadata, billing records) in its own database.
- A caching layer (CDN, regional proxy, etc.) may sit in front of reads for performance, but is not the source of truth.
- All data remains accessible through standard Walrus APIs (by blob ID) regardless of whether the developer uses this service.
- The service runs on ML-owned or ML-leased infrastructure.
- All APIs described here could be exposed through a web dashboard (SPA) in addition to programmatic access.

## Non-Goals (v1)

- **Server-side encryption.** Developers who need encryption should encrypt client-side before upload.
- **Direct private key access.** During the lifetime of a hosted account, all blob management happens through the service’s API. The developer never holds private keys for the service-managed wallets. (They can transfer out via `/account/transfer`.)
- **Dedicated tenancy.** No per-customer isolated infrastructure in v1.
- **Quilt support.** Quilts (multi-blob packing) are not exposed in v1.
- **Blob naming or indexing.** Blobs are identified by object ID and blob ID only. A naming or
tagging layer could be added later as an extension of blob attributes.

## Key Concepts

Before the API, a few Walrus concepts that matter here:

- **Blob ID**: A content-derived hash (deterministic from the data). Two uploads of identical bytes produce the same blob ID. This is the Walrus-native identifier.
- **Object ID**: A Sui on-chain identifier for the blob registration object. This is unique per upload - even uploading the same data twice produces two distinct object IDs. The object ID is what controls ownership, deletability, and epoch lifecycle.
- **Deletable vs. Permanent**: Set at upload time, immutable afterward. A deletable blob can be explicitly destroyed before its storage expires. A permanent blob cannot be deleted by anyone - it lives until its storage epochs expire.
- **Epoch extension**: Paying to extend the end epoch of an existing blob’s storage without re-uploading data. In this service, epoch extension is handled automatically - developers don’t interact with it directly. They pay for it as a metered billing event.

In this service, the primary identifier exposed to developers is the **object ID**, since it represents their specific registration (ownership, metadata, lifecycle). Blob ID is available for content-addressing use cases but does not carry ownership semantics.

## API Surface

All mutating endpoints and authenticated reads require an API key, passed via an `Authorization` header (`Bearer <api_key>`). Query-parameter auth should not be supported - API keys in URLs leak into logs, referrer headers, and browser history.

### Account

### Signup (via website)

1. Authenticate (email/password, OAuth, or wallet-based - TBD by eng, as long as it’s secure).
2. Provide PII/KYC as required (scope TBD, legal).
3. Provide payment method (credit card via Stripe).
4. Agree to Terms of Service.
5. Receive an API key.

**Side effect**: The service creates one or more Sui wallets for the account, held by the service. Multiple wallets per account may be needed to support concurrent uploads (Sui transactions from a single wallet must be serialized). The developer does not have direct access to these wallets.

### Manage Account

- `POST /account/api-keys` - Generate a new API key.
- `DELETE /account/api-keys/<key_id>` - Revoke an API key.
- `PUT /account/billing` - Update payment method.
- `GET /account/report` - Usage and billing summary. Scope TBD, but should cover: storage used (bytes x epochs), upload/download counts, cost breakdown by bucket, and current month-to-date charges.

### Transfer Account Wallets

- `POST /account/transfer`
- Request body: `{ "target_wallet": "<sui_address>" }`
- Transfers all on-chain blob objects from all service-managed wallets to the developer-supplied wallet.
- **This is a one-way, destructive operation.** After transfer, blob extension and deletion are the developer’s responsibility. The service should require explicit confirmation (e.g., re-auth + confirmation token).
- **Open question**: Should transfer close the account, or should the account persist (empty) so the developer can continue uploading new blobs?
- What happens to blobs whose epochs are about to expire? The developer needs to understand they are inheriting epoch-extension responsibility.

### Buckets

Buckets are an organizational primitive. They exist for usage tracking, reporting, and bulk lifecycle management (deleting a bucket deletes its contents). There is work underway on a Walrus-native “Blob Manager” concept that overlaps with buckets, but this service should treat its bucket abstraction as its own - whether buckets are backed by Blob Manager on-chain or by service-layer metadata is an implementation detail. NB: in our initial prototype, we are implementing buckets support in our centralized service, not on chain.

### Create Bucket

- `POST /buckets`
- Body: `{ "name": "<name>" }`
- `name` is optional, must be unique within the account if provided. (Consider making it required - unnamed buckets produce opaque IDs in billing reports and dashboards.)
- Returns: `{ "bucket_id": "...", "name": "..." }`

### Delete Bucket

- `DELETE /buckets/<bucket_id>`
- Deletes all **deletable** blobs in the bucket.
- **Open question**: What happens to permanent (non-deletable) blobs in the bucket? Options: (a) reject the delete if any permanent blobs exist, (b) delete the bucket but orphan the permanent blobs, (c) require the developer to transfer permanent blobs out first. This needs a decision.
- This may be asynchronous for large buckets. If async, return `202 Accepted` with a status URL.

### List Buckets

- `GET /buckets`
- Returns all buckets in the account.
- Supports pagination (`cursor`, `limit`).

### Blobs

**Lifecycle**: Once stored, a blob is automatically extended by the service and billed to the developer’s account. A blob lives until the developer explicitly deletes it (if deletable) or transfers it out of the service. Deleting a blob stops future extension charges.

### Store Blob

- `PUT /buckets/<bucket_id>/blobs`
- Body: raw blob data (the request body *is* the blob).
- Headers:
  - `Content-Type` - stored as blob metadata, returned on read by object ID. Not available when reading by blob ID (blob ID is content-addressed and has no per-registration metadata).
  - `X-Walrus-Deletable: true|false` (default: `true`) - immutable after creation.
  - `X-Walrus-Duration: <duration>` - initial storage duration (e.g., `30d`, `6m`, `1y`). Also sets the auto-renewal increment for this blob. The service converts durations to epoch counts internally (Walrus epochs are ~2 weeks, so the service rounds up - minor over-reservation is expected and should be documented). If not specified, defaults to TBD.
- Returns: `{ "object_id": "...", "blob_id": "...", "expires": "<iso8601>", "auto_extend_duration": "<duration>" }`
- **Open question**: Maximum blob size? Walrus has a practical encoding limit based on committee size. The service should enforce and document this.
- **Multipart upload**: Planned but not in v1.

### List Blobs

- `GET /buckets/<bucket_id>/blobs`
- Returns blobs in the bucket with metadata (object ID, blob ID, size, content type, expiration, deletable, auto-extend duration).
- Supports pagination (`cursor`, `limit`).

### Read Blob

- `GET /blobs/<object_id>`
- Returns blob data with the stored `Content-Type` header.
- Unauthenticated - no API key required. Blob data is publicly readable on Walrus anyway, so auth on reads doesn’t add a privacy boundary.
- Subject to per-IP rate limiting. Could redirect to public Walrus aggregators to offload traffic and reduce DDoS surface.
- **Alternative read path**: `GET /blobs/by-blob-id/<blob_id>` - reads by content hash. Does not return stored metadata (content-type, etc.) since blob ID is not tied to a specific registration. This is essentially a passthrough to the Walrus network.
- Authenticated reads would be needed if/when read metering or access control is added (not v1).
- SLA targets (latency, throughput, availability) TBD.

### Update Blob Metadata

- `PATCH /blobs/<object_id>/metadata`
- Body: `{ "content_type": "...", "auto_extend_duration": "<duration>" }`
- `content_type` and `auto_extend_duration` are service-layer metadata and are mutable. On-chain properties (deletable, blob ID) are immutable.

### Delete Blob

- `DELETE /blobs/<object_id>`
- Fails if the blob is permanent (non-deletable).
- Returns `204 No Content` on success.

## Billing

### Model

Monthly prepayment model with top-ups if funds get too low. Developers pay for:

1. **Storage**: bytes x epochs. Priced per GB-epoch (or a human-friendly equivalent like GB-month).
2. **Writes**: per-upload fee (covers Sui gas + Walrus registration costs + margin).
3. **Reads**: per-read fee or per-GB-downloaded, if reads are metered. (v1 will start with unmetered reads to keep it simple and competitive with free Walrus reads.)
4. **Epoch extensions**: per-extension fee (covers gas + extension cost + margin).

### Pricing and FX

The service pays Walrus costs in WAL and Sui gas in SUI, but bills developers in USD (or other fiat). This creates FX exposure.

**Open question**: How do we handle WAL/SUI price volatility?

- Option A: Set fiat prices weekly/monthly based on a trailing average. Simple but creates margin risk.
- Option B: Bill at a fixed markup over real-time cost at time of operation. Accurate but makes costs unpredictable for developers.
- Option C: Set fixed fiat prices per billing period, absorb variance as margin. Simplest developer experience but highest risk to ML.

This needs a decision before launch. It significantly affects the billing pipeline design.

### Auto-Extension

The service manages blob epoch extension on behalf of developers. This is a key value proposition - developers don’t need to worry about their data expiring. The service should:

1. Automatically extend blobs before their epochs expire.
2. Bill the extension cost to the developer’s account.
3. Provide a way for developers to opt out of auto-extension (effectively letting the blob expire).
4. Handle the case where a developer’s payment method fails - grace period? Data loss? This needs policy.

### Payment Infrastructure

- **Stripe Billing**: Metered billing with usage reporting via API. Stripe calculates invoices, handles tax, receipts, dunning (failed payment retries), and supports credit cards, ACH, and SEPA.
- **Metronome** (metronome.com): Purpose-built for usage-based billing at scale. Worth evaluating if the pricing model becomes complex (tiered pricing, committed-use discounts, prepaid credits).
- **ACH / Plaid**: Not v1. Lower transaction fees than cards but adds onboarding complexity. Revisit once the customer base justifies it.

## Risks and Gaps — Legal, Finance, and Billing Model

*Cross-referenced against industry guidance on usage-based billing (Stripe, 2024). Items
below represent questions we are not yet asking or risks we are implicitly accepting.*

### R1. Billing Shock

The spec has no mechanism to protect developers from unexpected charges. Usage-based billing
without guardrails is a known cause of churn and support escalation.

**Implicit risk**: A developer's application goes viral or has a bug that triggers runaway
uploads. They receive a bill orders of magnitude higher than expected. We have no alerts, caps,
or projections to prevent this.

**Action items**:
- [ ] Design usage alert thresholds (e.g., 50%, 80%, 100% of trailing average).
- [ ] Decide whether to offer hard spending caps (reject operations above cap) vs. soft caps (alert only). Hard caps protect developers but complicate the "data lives forever" promise.
- [ ] Add a cost projection endpoint or dashboard widget showing month-to-date spend and projected month-end cost.

### R2. Permanent Blobs as Unbounded Financial Liability

Permanent (non-deletable) blobs cannot be destroyed by anyone. If a developer uploads permanent
blobs and then stops paying, ML must either (a) continue paying for epoch extensions indefinitely
with no revenue to offset the cost, or (b) let them expire, which contradicts the core value
proposition and may violate the ToS.

This is an **open-ended, uncapped financial obligation** and is the single largest implicit risk
in this spec.

**Implicit risk**: A bad actor or careless developer stores terabytes of permanent data, churns,
and ML is left holding a perpetual cost obligation with no recourse.

**Action items**:
- [ ] **Decide whether v1 supports permanent blobs at all.** If yes, permanent blobs need differentiated pricing (e.g., a large up-front prepayment covering N years of extensions) to bound ML's liability.
- [ ] Define what happens to permanent blobs on account closure or payment failure. Options: (a) require prepayment of extension costs for a minimum period at upload time, (b) let them expire (and document this clearly in the ToS), (c) cap the maximum size/count of permanent blobs per account.
- [ ] Legal review: can ML's ToS disclaim ongoing extension obligations for permanent blobs after account termination?

### R3. Compliance and Regulatory

The spec defers KYC and legal scope entirely. Usage-based billing in a multi-jurisdiction
context raises several regulatory concerns that should be scoped before architecture decisions
are finalized.

**Implicit risks**:
- Collecting usage metering data (who stored what, when, how much) may be subject to GDPR/CCPA data privacy requirements.
- Billing practices (advance notice of charges, right to dispute, refund obligations) are regulated in many jurisdictions.
- If the service ever offers prepaid credits or account balances, it may trigger money transmitter licensing requirements.
- Storing data on a decentralized network complicates jurisdiction — export controls and sanctions screening may apply.
- Permanent blobs containing illegal content on an immutable network — ML may be unable to comply with takedown orders, creating liability.

**Action items**:
- [ ] Engage legal to scope jurisdictions for v1 launch (US-only? US + EU? Global?).
- [ ] Get a legal opinion on content liability for permanent blobs stored on a decentralized network. Can ML comply with DMCA / EU Digital Services Act takedown obligations?
- [ ] Determine GDPR/CCPA obligations for usage metering data (retention periods, right to deletion, data processing agreements).
- [ ] Confirm tax nexus obligations — "Stripe handles tax" is necessary but not sufficient. ML needs to know which jurisdictions create tax obligations.
- [ ] Assess whether prepaid credits or account balances (if planned) trigger money transmitter regulations.
- [ ] Sanctions screening: does the signup flow need OFAC/SDN screening? Does storing data on Walrus (globally distributed) create sanctions exposure?

### R4. Dispute Resolution and Refund Policy

The spec has no billing dispute process. Usage-based billing is especially prone to disputes
because customers may not understand how charges were calculated.

**Implicit risk**: A developer disputes a charge, and we have no documented process. This
escalates to a Stripe chargeback, which costs $15+ per incident and damages our merchant
reputation.

**Action items**:
- [ ] Define a dispute resolution process (internal review → credit/adjustment → escalation path).
- [ ] Define a refund policy (full refund within N days? pro-rata? no refunds on consumed storage?).
- [ ] Determine whether developers can audit their own metering data (raw event logs vs. summary only).
- [ ] Define SLA credit policy for outages or service degradation.

### R5. Payment Failure and Data Destruction

Destroying customer data for non-payment is legally sensitive and reputationally dangerous.

**Implicit risk**: A developer's credit card expires, the dunning process fails, and we delete
their data. The developer claims they never received notice and sues for data loss. Or: a
jurisdiction's data retention laws prohibit destruction.

**Action items**:
- [ ] Define the dunning escalation timeline (e.g., Day 0: payment fails, retry. Day 3: email notification. Day 7: second notice. Day 14: service degradation (writes disabled). Day 30: account suspension. Day 60: data eligible for deletion).
- [ ] Legal review: what notification requirements exist before data destruction in target jurisdictions?
- [ ] Legal review: do any data retention laws in target jurisdictions prohibit destruction of customer data even after account termination?
- [ ] Define what "data deletion" means for blobs on Walrus — we stop extending epochs and blobs eventually expire. Clarify this in the ToS so developers understand the timeline.
- [ ] Special case: permanent blobs during payment failure (see R2).

### R6. SLA, Liability, and Indemnification

No SLA commitments or liability framework exists in the spec. Customers will ask about these
before adopting the service for production workloads.

**Implicit risk**: ML has unquantified liability exposure for data loss, service outages, or
auto-extension failures.

**Action items**:
- [ ] Define SLA targets for availability, durability, upload/download latency.
- [ ] Define liability caps in the ToS (standard practice: liability capped at fees paid in prior 12 months).
- [ ] Define indemnification terms — who bears liability if Walrus network issues cause data loss?
- [ ] Define force majeure provisions covering Walrus network outages, Sui chain congestion, etc.

### R7. Revenue Predictability

Usage-based billing creates inherently variable revenue. The spec acknowledges FX risk but
does not address demand-side revenue unpredictability.

**Implicit risk**: ML cannot reliably forecast revenue for financial planning, making it harder
to budget for infrastructure, staffing, and Walrus cost obligations.

**Action items**:
- [ ] Decide whether to offer committed-use pricing (e.g., "commit to $X/month for 12 months, get a discount"). This creates a baseline revenue floor.
- [ ] Decide whether to offer prepaid credit bundles (e.g., "$100 credit for $90"). Creates revenue predictability and reduces churn.
- [ ] Decide whether to require a minimum monthly spend per account.
- [ ] Model expected revenue variance under different adoption scenarios to set reserves for FX and cost obligations.

### R8. Anti-Abuse and Fraud

The spec mentions read rate-limiting but has no upload-side abuse protections.

**Implicit risk**: Stolen credit cards used to upload large volumes of permanent blobs. ML
cannot delete the blobs and eats the ongoing cost after the chargeback. Or: accounts used for
illegal content storage with no content moderation framework.

**Action items**:
- [ ] Define upload rate limits (per-account, per-bucket, per-hour).
- [ ] Implement spending velocity alerts (e.g., account spending 10x its trailing average in an hour).
- [ ] Evaluate fraud detection on payment methods (Stripe Radar or equivalent).
- [ ] Define acceptable use policy and content moderation framework. How does ML handle reports of illegal content on permanent blobs?
- [ ] Consider requiring identity verification (not just email) for accounts that upload permanent blobs or exceed a spending threshold.

### R9. Customer Retention

Pure usage-based billing creates minimal switching costs. The Stripe article notes this as a
retention risk.

**Implicit risk**: Developers treat the service as interchangeable with competitors (or with
running their own Walrus node) and churn freely.

**Action items**:
- [ ] Evaluate volume discounts or tiered pricing to reward scale.
- [ ] Evaluate committed-use discounts (overlaps with R7).
- [ ] Consider value-added features that increase switching costs (monitoring dashboards, analytics, CDN integration, team management).

## Open Questions Summary

*Original open questions, plus additions from the risk analysis above.*

1. **Transfer semantics**: Does `/account/transfer` close the account or leave it open?
2. **Permanent blobs in deleted buckets**: Reject, orphan, or require explicit handling?
3. **FX strategy**: Fixed prices, real-time markup, or periodic adjustment?
4. **Payment failure policy**: Grace period before data expiration? How long? (See R5 for expanded scope.)
5. **Unauthenticated read policy**: Rate-limit, require auth, or offer tiered access?
6. **Auto-extension opt-out**: Per-blob, per-bucket, or account-wide?
7. **Default storage duration**: What should the default `X-Walrus-Duration` be if not specified?
8. **Permanent blob financial liability**: Should v1 support permanent blobs? If yes, how is ML's ongoing cost obligation bounded? (See R2.)
9. **Content liability**: Can ML comply with takedown orders for permanent blobs on a decentralized network? (See R3.)
10. **Billing shock protection**: Hard spending caps, soft alerts, or both? (See R1.)
11. **Dispute and refund policy**: What is the process for billing disputes? (See R4.)
12. **SLA commitments**: What availability/durability/latency targets will ML commit to? (See R6.)
13. **Revenue floor**: Minimum spend, committed-use pricing, or prepaid credits? (See R7.)
14. **Fraud and abuse**: What upload-side protections exist against stolen cards and illegal content? (See R8.)

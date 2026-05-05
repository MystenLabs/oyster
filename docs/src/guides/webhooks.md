# Webhooks

Oyster posts a single webhook event today: `account.funding_required`.
It tells the owning app that an account's Pearl-derived wallet cannot
cover the next `extend_storage_pool` PTB. Top up the wallet and the
next extension cycle will succeed.

This guide covers the trigger condition, payload schema, retry
behavior, circuit-breaker semantics, and how to write a receiver.

## Overview

When the extension worker tries to extend an account's `StoragePool`
and Sui rejects the transaction with an insufficient-funds error,
Oyster POSTs a JSON event to the receiver URL configured for the
owning app. The receiver is expected to credit the wallet (or alert a
human to do so) and acknowledge with a `2xx` status.

Only `account.funding_required` is emitted today. Future events will
share the same envelope shape; receivers should switch on the `type`
field rather than assuming a single schema.

## Trigger condition

The webhook fires when **all** of the following hold during an
extension cycle:

- The extension worker claims an account row whose
  `pool_end_epoch < current_epoch + POOL_EXTEND_LOOKAHEAD_EPOCHS`.
- The `extend_storage_pool` PTB submission fails with an error whose
  lowercased message contains `insufficientgas`,
  `insufficientcoinbalance`, or `insufficient` (case-insensitive
  substring match — see `is_insufficient_funds_error` in
  `crates/oyster/src/webhook.rs`).
- The owning app has a webhook receiver URL configured.

Any other class of failure (Sui RPC down, network timeout, signing
error) is logged and metered but does **not** fire a webhook.

## Payload schema

```json
{
  "event_id": "8f2c5e1a-...-uuid-v4",
  "type": "account.funding_required",
  "account_id": "acc_...",
  "pearl_address": "0x...",
  "amount": {
    "wal_frost": "12345678900",
    "sui_mist": "100000000"
  },
  "timestamp": "2026-05-05T10:31:00Z"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `event_id` | UUID v4 string | Stable id for this delivery; reused across all retry attempts. Receivers MUST dedupe by this. |
| `type` | string | Event type discriminator. Always `"account.funding_required"` for this event. |
| `account_id` | string | Oyster account whose pool needs extension funding. |
| `pearl_address` | string | Sui wallet address derived by Pearl for this account — the address that needs funding. |
| `amount.wal_frost` | decimal string | WAL required, in FROST units (1 WAL = 10⁹ FROST). |
| `amount.sui_mist` | decimal string | SUI required, in MIST units (1 SUI = 10⁹ MIST). |
| `timestamp` | ISO-8601 UTC string | When Oyster emitted the event. |

`amount.*` are **decimal strings**, not numbers, to avoid `u64`
precision loss in JSON. The SUI amount is currently a fixed
`100_000_000` MIST (≈0.1 SUI) buffer — Oyster does not dry-run gas.
The WAL amount is computed from the planned extension's encoded
capacity × `POOL_EXTEND_EPOCHS` × the Walrus per-unit storage price.

## Authentication

**v0.6.0 sends an unsigned `POST`. There is no HMAC, no Bearer token,
no mTLS.** Restrict your receiver by source IP allow-list or run it
on a private network reachable only from the Oyster deployment.

HMAC signing is a tracked follow-up — it will be opt-in and won't
break existing receivers when introduced.

## Retry policy

Oyster retries the same delivery up to `MAX_RETRIES = 3` times with
exponential backoff:

- Attempt 1 — immediate.
- Attempt 2 — after 100 ms.
- Attempt 3 — after 200 ms.

(Backoff doubles each retry and is capped at 5 s, so the third sleep
would be 400 ms — well under the cap.)

Retry semantics by response:

| Outcome | Behavior |
|---------|----------|
| `2xx` | Recorded as success; circuit closes; no more attempts. |
| `4xx` | **Not retried.** Logged, counted as a failure, delivery dropped. |
| `5xx` | Retried up to 3 attempts total. |
| Connection / timeout error | Retried up to 3 attempts total. |
| All 3 attempts exhausted | Logged, counted as a failure, delivery dropped. |

The same delivery may re-emerge later — see [Idempotency](#idempotency).

## Circuit breaker

To prevent a misbehaving receiver from monopolizing the extension
worker, the webhook client wraps deliveries in a per-client circuit
breaker:

- **Closed** (normal): every event attempts delivery.
- **Opens** after `5` consecutive failed deliveries.
- **Stays open** for `60` seconds. While open, new events are
  **silently dropped** (logged, counted on
  `oyster_webhook_circuit_open_total`, but **not queued** for later
  delivery).
- **Half-open** after the 60 s cooldown: the next event is allowed
  through as a probe. On success the circuit closes; on failure it
  re-arms for another 60 s.

Because dropped events are not queued, recovery from a long receiver
outage relies on the next extension cycle re-claiming the account
once its `EXTENSION_CLAIM_COOLDOWN_SECS` elapses. Practically this
means: if your receiver is down, you will miss notifications for the
duration of the outage, but a healthy receiver will start receiving
events again on the next cycle after recovery.

## Idempotency

`event_id` is a fresh UUID v4 generated **once per delivery** in
`extension_task.rs`, then reused across every retry attempt the
webhook client makes for that delivery. **Receivers MUST dedupe by
`event_id`.**

A separate delivery for the same account in a later cycle will have a
**fresh** `event_id`, so dedup is per-delivery, not per-account. If
you want to suppress repeated notifications for the same underfunded
account, do so in your receiver based on `account_id` + your own
state.

## Setup

Webhook receiver URLs are configured per-app on the Oyster server.
Talk to your Oyster node operator to register your receiver URL
against your app — the admin endpoint is intentionally not documented
here because end users do not call it directly.

## Receiver examples

Both examples show the minimum viable receiver: dedupe by `event_id`,
acknowledge promptly with `200`, and return `5xx` on processing
failure so Oyster retries.

### Node.js / Express

```javascript
import express from "express";

const app = express();
app.use(express.json());

const seenEventIds = new Set();

app.post("/oyster/webhook", async (req, res) => {
  const { event_id, type, account_id, pearl_address, amount } = req.body;

  if (seenEventIds.has(event_id)) {
    return res.status(200).send();
  }
  seenEventIds.add(event_id);

  if (type !== "account.funding_required") {
    return res.status(200).send();
  }

  try {
    await topUpWallet(pearl_address, amount.wal_frost, amount.sui_mist);
    return res.status(200).send();
  } catch (err) {
    console.error("top-up failed for", account_id, err);
    seenEventIds.delete(event_id);
    return res.status(503).send();
  }
});

app.listen(8080);
```

### Python / Flask

```python
from flask import Flask, request

app = Flask(__name__)
seen_event_ids = set()

@app.post("/oyster/webhook")
def funding_required():
    payload = request.get_json()
    event_id = payload["event_id"]

    if event_id in seen_event_ids:
        return "", 200
    seen_event_ids.add(event_id)

    if payload["type"] != "account.funding_required":
        return "", 200

    try:
        top_up_wallet(
            payload["pearl_address"],
            int(payload["amount"]["wal_frost"]),
            int(payload["amount"]["sui_mist"]),
        )
        return "", 200
    except Exception as exc:
        app.logger.exception("top-up failed for %s", payload["account_id"])
        seen_event_ids.discard(event_id)
        return "", 503
```

> In production, persist the dedup set (e.g. Redis with a TTL of a
> few hours) so receiver restarts don't re-process events.

## Error semantics

| Situation | Server-side response | Oyster behavior |
|-----------|---------------------|-----------------|
| Receiver returned `2xx` | success | done; circuit resets |
| Receiver returned `4xx` | client error | **not retried**; logged, counted as failure |
| Receiver returned `5xx` | server error | retried (up to 3 attempts total) |
| Receiver-side processing failed | return `5xx` | Oyster retries this delivery |
| Connection refused / timeout | network failure | retried (up to 3 attempts total) |
| Circuit open | none | event silently dropped, not queued |

## Metrics

The Oyster server's Prometheus endpoint exposes four webhook
counters:

| Metric | Description |
|--------|-------------|
| `oyster_webhook_attempts_total` | Total webhook delivery attempts (one per delivery, not per retry). |
| `oyster_webhook_successes_total` | Deliveries that received `2xx` within the retry budget. |
| `oyster_webhook_failures_total` | Deliveries that exhausted retries or hit a non-retryable `4xx`. |
| `oyster_webhook_circuit_open_total` | Number of times the circuit breaker transitioned to open. |

Pair these with the extension worker counters
(`oyster_extension_pools_extended_total`,
`oyster_extension_errors_total{stage}`) to alert on chronic
under-funding without alerting on transient receiver failures.

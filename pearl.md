# Pearl — Wallet Custody & Transaction Signing Service

## Purpose

Pearl is a standalone service that holds private key material and signs Sui transactions on behalf of
user accounts. The goal is to isolate key custody and signing from the business logic in Oyster. Pearl
has a gRPC interface and its own database.

## Relationship to Oyster

- One Oyster account maps to one or more Pearl accounts (wallets).
- Oyster never holds private keys. When it needs a Sui transaction signed, it calls Pearl.
- Pearl does not know about buckets, blobs, or billing. It only knows about wallets, balances, and
  signing.

## Database

- Production: Postgres (or equivalent RDBMS with encryption-at-rest).
- Local prototyping: SQLite, in a separate database file from Oyster's database.

## Account Model

| Field              | Type     | Description                                                              |
|--------------------|----------|--------------------------------------------------------------------------|
| id                 | UUID     | Primary key                                                              |
| due_date           | datetime | Point after which this account is overdue (throttle, grace period, etc.) |
| min_sui_balance    | u64      | Balance threshold below which SUI should be topped up                    |
| min_wal_balance    | u64      | Balance threshold below which WAL should be topped up                    |
| top_up_target_sui  | u64      | Target SUI balance after a top-up                                        |
| top_up_target_wal  | u64      | Target WAL balance after a top-up                                        |
| address            | string   | Sui wallet address                                                       |
| private_key        | bytes    | Private key for the Sui address (see sui-sdk wallet_context.rs)          |
| credentials        | string   | API key for service-to-service auth (see Open Questions)                 |

## gRPC Interface

### sign_transaction

Signs a Sui transaction on behalf of an account.

```
sign_transaction(
    tx_data: bytes,         // BCS-encoded TransactionData (see sui-types/src/transaction.rs)
    account_id: AccountId,
    credentials: string,
) -> bytes                  // BCS-encoded signed Transaction
```

Error conditions:
- Account not found
- Invalid credentials
- Account overdue (past due_date)
- Balance below threshold (optional — may sign anyway and rely on the chain to reject)

### get_account_wallets

Returns wallet addresses for an account. This is the primary mechanism by which an external billing
service can look up where to send funds.

```
get_account_wallets(
    account_id: AccountId,
    credentials: string,
) -> Vec<WalletInfo>        // address, sui_balance, wal_balance
```

This supports **Scenario S1** (see Funding below): the billing service resolves account IDs to wallet
addresses, then sends funds directly on-chain.

### create_account

Creates a new Pearl account, generating a fresh Sui keypair.

```
create_account(
    credentials: string,
    min_sui_balance: u64,
    min_wal_balance: u64,
    top_up_target_sui: u64,
    top_up_target_wal: u64,
) -> AccountInfo            // id, address
```

## Funding

Two scenarios describe how funds flow into Pearl-managed wallets.

### S1: External Billing Service Pushes Funds (Primary)

A higher-level billing service (outside Pearl and Oyster) manages the developer-facing billing
relationship — Stripe subscriptions, internal ledger of "funded" vs. "distributed" amounts, etc.

Flow:
1. Billing service collects payment from the developer (Stripe, invoice, etc.).
2. Billing service credits the developer's internal ledger ("funded" increases).
3. When funded > distributed, billing service calls `get_account_wallets` to resolve the wallet
   address(es) for the developer's account.
4. Billing service sends SUI/WAL/USDC (TBD) to the wallet address on-chain.
5. Billing service records the transfer in its ledger ("distributed" increases).

Pearl's role here is limited: it exposes `get_account_wallets` so the billing service can find the
right addresses. Pearl does not initiate, request, or verify the funding — it simply holds the wallets
and signs transactions when asked.

### S2: Pearl Notifies Billing Service of Low Funds (Future)

When Pearl (or Oyster on Pearl's behalf) detects that a wallet's on-chain balance has dropped below
`min_sui_balance` or `min_wal_balance`, it needs to notify the billing service that a top-up is
required.

Options under consideration:
- **Webhook**: Pearl calls a configured callback URL to notify the billing service.
- **Persistent queue**: Pearl publishes a message to a shared queue (e.g., SQS, NATS) that the
  billing service consumes.

The exact mechanism is TBD. The interface should be designed to favor the billing service's
architecture, since Pearl is the lower-level dependency. For now, we note this as a requirement and
defer the protocol decision.

## Security Notes

- gRPC is assumed to be encrypted or behind a VPC. Credentials are sent in plaintext over the wire
  for now.
- Private keys must be encrypted at rest in production (Postgres column-level encryption or KMS).
- Pearl should never log private key material.

## Open Questions

1. **Credentials mechanism**: The current design uses a per-account API key for service-to-service
   auth. mTLS or a shared secret scoped to the calling service (not per-account) may be more
   appropriate. Decide before production.
2. **Funding denomination**: Does the billing service send SUI, WAL, USDC, or some combination? This
   affects whether Pearl needs swap/conversion logic or whether the billing service handles that
   before funding.
3. **Multi-wallet strategy**: When does an account need multiple wallets? Is it one wallet per account
   by default, with additional wallets created on demand for concurrency? What's the creation
   trigger?
4. **Balance monitoring ownership**: Does Pearl poll on-chain balances itself, or does Oyster (or the
   billing service) query balances and act on them? This determines where the S2 notification
   originates.
5. **S2 notification protocol**: Webhook vs. persistent queue vs. polling. Defer until the billing
   service architecture is clearer.
6. **Top-up funding source**: Where does the billing service acquire SUI/WAL to send to wallets? A
   treasury wallet? On-demand DEX swap? This is a billing-service concern but affects Pearl's
   balance assumptions.
7. **Key rotation**: Should Pearl support rotating a wallet's keypair (generating a new address and
   migrating on-chain objects)? Not v1, but worth noting.
8. **`sign_transaction` behavior when overdue**: Should Pearl refuse to sign, or sign with a warning
   and let the caller decide? Refusing gives Pearl policy authority it maybe shouldn't have.

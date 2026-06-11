# /// script
# requires-python = ">=3.10"
# dependencies = ["matplotlib", "numpy"]
# ///
"""Per-account Walrus storage cost per epoch, as a function of average blob size.

This "inverts" walrus_storage_efficiency_ratio.py into money. An Oyster
account at its full `max_unencoded_bytes` cap N, with blobs averaging `s`
bytes, reserves N*E(s) encoded bytes (E(x)=f(x)/x is the encoded expansion),
so its recurring cost is:

    cost_per_epoch = storage_units(N*E(s)) * STORAGE_PRICE_PER_UNIT   [FROST]

billed in whole 1 MiB units. Cost is LINEAR in the cap N, so the plot is
shown per the configured CAP but scales trivially (2x cap -> 2x cost).

Because Walrus charges a fixed per-blob metadata overhead (~64 MB encoded at
n_shards=1000), small average blob sizes are dramatically more expensive per
epoch for the same logical capacity. The `avg_blob_size` admission knob caps
an account's encoded reservation at N*E(avg_blob_size), so the worst-case
per-epoch cost Harbor can incur per account is the value at s = avg_blob_size.

Pricing (supplied; mainnet-ish):
  1 WAL = 1e9 FROST; 1 WAL = $0.03452
  storage: 60,752 FROST per encoded MiB-unit per epoch
  write:   0.0001 WAL (100,000 FROST) per encoded MiB-unit, one-time
"""

import matplotlib.pyplot as plt
import numpy as np

# ---- Walrus encoding (RS2), mirrors walrus_core ---------------------------
ALIGNMENT = 2
MAX_SYMBOL_SIZE = 65534
DIGEST_LEN = 32
BLOB_ID_LEN = 32
N_SHARDS = 1000

# ---- Pricing --------------------------------------------------------------
FROST_PER_WAL = 1_000_000_000
USD_PER_WAL = 0.03452
BYTES_PER_UNIT = 1 << 20                       # 1 MiB
STORAGE_PRICE_PER_UNIT_EPOCH = 60_752          # FROST / unit / epoch
WRITE_PRICE_PER_UNIT = 100_000                 # FROST / unit, one-time (0.0001 WAL)
EPOCHS_PER_YEAR = 26                           # ~14-day mainnet epochs

# ---- Scenario -------------------------------------------------------------
CAP = 5_000_000_000          # max_unencoded_bytes (Oyster default = 5 GB)
DEFAULT_AVG_BLOB_SIZE = 10_000_000  # the proposed knob default (10 MB)


def symbol_params(n_shards):
    fault = (n_shards - 1) // 3
    min_correct = n_shards - fault
    return min_correct - fault, min_correct, (min_correct - fault) * min_correct


def compute_symbol_size(length, n_symbols):
    length = max(length, 1)
    s = -(-length // n_symbols)
    s = ((s + ALIGNMENT - 1) // ALIGNMENT) * ALIGNMENT
    return s if s <= MAX_SYMBOL_SIZE else None


def encoded_blob_length(length, n_shards):
    primary, secondary, n_symbols = symbol_params(n_shards)
    sym = compute_symbol_size(length, n_symbols)
    if sym is None:
        return None
    slivers = n_shards * (primary + secondary) * sym
    metadata = n_shards * (n_shards * DIGEST_LEN * 2 + BLOB_ID_LEN)
    return metadata + slivers


def storage_units(encoded_bytes):
    """ceil to whole 1 MiB billing units."""
    return -(-int(encoded_bytes) // BYTES_PER_UNIT)


def account_cost(avg_blob_size, cap, n_shards):
    """Return (storage_frost_per_epoch, write_frost_one_time, n_blobs, encoded)
    for an account holding `cap` unencoded bytes as blobs of avg size s.
    Units are billed per blob (matches per-upload pool growth)."""
    enc_per_blob = encoded_blob_length(avg_blob_size, n_shards)
    if enc_per_blob is None:
        return None
    n_blobs = cap / avg_blob_size                       # continuous
    units_per_blob = storage_units(enc_per_blob)
    total_units = n_blobs * units_per_blob
    storage_epoch = total_units * STORAGE_PRICE_PER_UNIT_EPOCH
    write_once = total_units * WRITE_PRICE_PER_UNIT
    return storage_epoch, write_once, n_blobs, n_blobs * enc_per_blob


def usd_epoch(frost):
    return frost / FROST_PER_WAL * USD_PER_WAL


def fmt_bytes(n):
    for unit, div in (("GB", 1e9), ("MB", 1e6), ("KB", 1e3)):
        if abs(n) >= div:
            return f"{n / div:g} {unit}"
    return f"{n:g} B"


def main():
    sizes = np.unique(np.logspace(3, 9, 600).astype(np.int64))  # 1 KB .. 1 GB

    xs, usd, wal = [], [], []
    for s in sizes:
        s = int(s)
        r = account_cost(s, CAP, N_SHARDS)
        if r is None:
            continue
        storage_epoch = r[0]
        xs.append(s)
        usd.append(usd_epoch(storage_epoch))
        wal.append(storage_epoch / FROST_PER_WAL)

    fig, ax = plt.subplots(figsize=(11, 6.5))
    ax.plot(xs, usd, color="tab:green", linewidth=2.2,
            label=f"storage cost / epoch  (cap = {fmt_bytes(CAP)} unencoded)")
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_xlabel("average blob size (bytes)")
    ax.set_ylabel("USD per epoch per account  (storage)")
    ax.grid(True, which="both", ls=":", alpha=0.4)

    # Mark the proposed knob default and a couple of reference sizes.
    for s, note in ((1_000_000, ""), (DEFAULT_AVG_BLOB_SIZE, "  ← knob default"),
                    (100_000_000, "")):
        r = account_cost(s, CAP, N_SHARDS)
        u = usd_epoch(r[0])
        ax.plot([s], [u], "o", color="tab:green", ms=6)
        ax.annotate(
            f"{fmt_bytes(s)}{note}\n${u:.3f}/epoch  (${u * EPOCHS_PER_YEAR:.2f}/yr)",
            xy=(s, u), xytext=(s * 1.3, u * 1.8), fontsize=8.5, color="tab:green",
        )

    # Right axis: WAL per epoch (same curve, linear rescale of FROST).
    ax2 = ax.secondary_yaxis(
        "right",
        functions=(lambda d: d / USD_PER_WAL, lambda w: w * USD_PER_WAL),
    )
    ax2.set_ylabel("WAL per epoch per account")

    ax.set_title(
        "Per-account Walrus cost per epoch vs average blob size\n"
        f"cap = {fmt_bytes(CAP)} unencoded, n_shards={N_SHARDS}, "
        f"$ {USD_PER_WAL}/WAL, {STORAGE_PRICE_PER_UNIT_EPOCH:,} FROST/MiB-unit/epoch"
    )
    ax.legend(loc="upper right")
    ax.text(
        0.015, 0.04,
        "cost ∝ N·E(s); linear in the cap. Small blobs pay the fixed\n"
        "per-blob metadata overhead (~64 MB encoded) again and again.\n"
        f"(annualized at {EPOCHS_PER_YEAR} epochs/yr; write fee is one-time, not shown)",
        transform=ax.transAxes, ha="left", va="bottom", fontsize=8.5,
        bbox=dict(boxstyle="round", fc="white", ec="0.7", alpha=0.9),
    )
    fig.tight_layout()

    out = "walrus_account_cost.png"
    fig.savefig(out, dpi=130)
    print(f"wrote {out}")

    print(f"\nScenario: cap = {fmt_bytes(CAP)} unencoded, n_shards={N_SHARDS}, "
          f"${USD_PER_WAL}/WAL")
    print(f"{'avg blob':>12} {'encoded':>12} {'#blobs':>12} "
          f"{'WAL/epoch':>11} {'USD/epoch':>11} {'USD/yr':>10} {'write(WAL)':>11}")
    for s in (10_000, 100_000, 1_000_000, DEFAULT_AVG_BLOB_SIZE,
              100_000_000, 1_000_000_000):
        r = account_cost(s, CAP, N_SHARDS)
        if r is None:
            continue
        storage_epoch, write_once, n_blobs, encoded = r
        wal_e = storage_epoch / FROST_PER_WAL
        usd_e = usd_epoch(storage_epoch)
        tag = "  <- knob default" if s == DEFAULT_AVG_BLOB_SIZE else ""
        print(f"{fmt_bytes(s):>12} {fmt_bytes(encoded):>12} {n_blobs:>12,.0f} "
              f"{wal_e:>11,.3f} {usd_e:>11,.4f} {usd_e * EPOCHS_PER_YEAR:>10,.2f} "
              f"{write_once / FROST_PER_WAL:>11,.3f}{tag}")


if __name__ == "__main__":
    main()

# /// script
# requires-python = ">=3.10"
# dependencies = ["matplotlib", "numpy"]
# ///
"""Plot Walrus storage efficiency (encoded/unencoded) vs blob size.

For a given blob size `s` we assume we store as many *whole* blobs of that
size as fit in 1E9 bytes, i.e. exactly `1E9 - (1E9 mod s)` unencoded bytes,
then compute total_encoded / total_unencoded.

`f(L)` reimplements walrus_core::encoding::encoded_blob_length_for_n_shards
(RS2). Because every blob carries a fixed, size-independent metadata cost
(~n_shards^2 * 64 bytes), many small blobs are dramatically less efficient
than one large blob of the same total unencoded size.
"""

import math

import matplotlib.pyplot as plt
import numpy as np

ALIGNMENT = 2  # EncodingType::RS2::required_alignment()
MAX_SYMBOL_SIZE = 65534  # u16::MAX - 1
DIGEST_LEN = 32
BLOB_ID_LEN = 32
TARGET_BYTES = 1_000_000_000  # 1E9


def symbol_params(n_shards: int) -> tuple[int, int, int]:
    """(primary, secondary, source_symbols_per_blob) for n_shards (BFT)."""
    fault = (n_shards - 1) // 3          # max_n_faulty
    min_correct = n_shards - fault       # min_n_correct
    primary = min_correct - fault        # n - 2f
    secondary = min_correct              # n - f
    return primary, secondary, primary * secondary


def compute_symbol_size(length: int, n_symbols: int) -> int | None:
    length = max(length, 1)
    s = -(-length // n_symbols)                      # ceil_div
    s = ((s + ALIGNMENT - 1) // ALIGNMENT) * ALIGNMENT  # next multiple of alignment
    return s if s <= MAX_SYMBOL_SIZE else None       # must fit in u16


def encoded_blob_length(length: int, n_shards: int) -> int | None:
    """walrus encoded_blob_length_for_n_shards(n_shards, length, RS2)."""
    primary, secondary, n_symbols = symbol_params(n_shards)
    sym = compute_symbol_size(length, n_symbols)
    if sym is None:
        return None
    slivers = n_shards * (primary + secondary) * sym
    metadata = n_shards * (n_shards * DIGEST_LEN * 2 + BLOB_ID_LEN)
    return metadata + slivers


def efficiency(blob_size: int, n_shards: int) -> float | None:
    """encoded/unencoded for ~1E9 bytes stored as whole blobs of blob_size."""
    n_blobs = TARGET_BYTES // blob_size
    if n_blobs == 0:
        return None
    enc = encoded_blob_length(blob_size, n_shards)
    if enc is None:
        return None
    total_unencoded = n_blobs * blob_size            # == 1E9 - (1E9 mod blob_size)
    total_encoded = n_blobs * enc
    return total_encoded / total_unencoded


N_SHARDS = 1000  # Walrus mainnet/testnet


def required_encoded(avg_blob_size: int, n_shards: int) -> float | None:
    """Encoded bytes to reserve so that TARGET_BYTES of unencoded data is
    *guaranteed* storable when the average blob size is `avg_blob_size`.

    This is the lower-bound interpretation of the cap: reserve
    (TARGET_BYTES / s) * f(s) instead of today's f(TARGET_BYTES). Uses a
    continuous blob count (avg size need not divide TARGET_BYTES evenly),
    so required == TARGET_BYTES * expansion(s)."""
    enc = encoded_blob_length(avg_blob_size, n_shards)
    if enc is None:
        return None
    return (TARGET_BYTES / avg_blob_size) * enc


def main() -> None:
    sizes = np.unique(np.logspace(2, 9, 600).astype(np.int64))  # 100 B .. 1 GB

    # Today's reservation: cap treated as a single blob (upper-bound behavior,
    # equivalent to assuming avg blob size == max blob size).
    baseline = encoded_blob_length(TARGET_BYTES, N_SHARDS)

    xs, eff, extra = [], [], []
    for s in sizes:
        s = int(s)
        r = efficiency(s, N_SHARDS)
        req = required_encoded(s, N_SHARDS)
        if r is None or req is None:
            continue
        e = req - baseline
        xs.append(s)
        eff.append(r)
        extra.append(e if e > 0 else float("nan"))  # NaN at the tail (log axis)

    fig, ax = plt.subplots(figsize=(10, 6))

    # Left axis: storage expansion (encoded / unencoded).
    c1 = "tab:blue"
    ax.plot(xs, eff, color=c1, linewidth=2,
            label="storage expansion (encoded / unencoded)")
    ax.axhline(4.5, color="gray", ls="--", lw=1, alpha=0.7)
    ax.text(120, 4.7, "~4.5x asymptote (large blobs)", color="gray", fontsize=9)
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_xlabel("average blob size (bytes)")
    ax.set_ylabel("storage expansion  (encoded / unencoded)", color=c1)
    ax.tick_params(axis="y", labelcolor=c1)
    ax.grid(True, which="both", ls=":", alpha=0.4)

    # Right axis: extra encoded capacity to reserve for lower-bound guarantee.
    ax2 = ax.twinx()
    c2 = "tab:red"
    ax2.plot(xs, extra, color=c2, linewidth=2,
             label="extra encoded capacity to reserve (lower-bound guarantee)")
    ax2.set_yscale("log")
    ax2.set_ylabel("extra encoded bytes vs. f(cap) reservation", color=c2)
    ax2.tick_params(axis="y", labelcolor=c2)

    ax.set_title(
        f"Walrus storage efficiency vs blob size  (n_shards={N_SHARDS})\n"
        "storing 1E9 − (1E9 mod blob_size) bytes as whole blobs;  "
        "extra capacity = (1E9/s)·f(s) − f(1E9)"
    )
    lines1, labels1 = ax.get_legend_handles_labels()
    lines2, labels2 = ax2.get_legend_handles_labels()
    ax.legend(lines1 + lines2, labels1 + labels2, loc="upper right")
    fig.tight_layout()

    out = "walrus_storage_efficiency.png"
    fig.savefig(out, dpi=130)
    print(f"wrote {out}")

    print(f"\nbaseline f(1E9) reservation: {baseline:,} encoded bytes")
    print(f"\nn_shards={N_SHARDS} reference points:")
    for s in (1_000, 100_000, 1_000_000, 10_000_000, 100_000_000):
        r = efficiency(s, N_SHARDS)
        req = required_encoded(s, N_SHARDS)
        print(f"  blob={s:>12,} B  ->  expansion ~{r:>10,.1f}x  "
              f"reserve {req:>22,.0f} B  (+{req - baseline:>22,.0f} extra)")


if __name__ == "__main__":
    main()

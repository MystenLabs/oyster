# /// script
# requires-python = ">=3.10"
# dependencies = ["matplotlib", "numpy"]
# ///
"""Plot the per-account capacity SHORTFALL vs average blob size.

This is `red / blue` from walrus_storage_efficiency.py, which turns out to
have a clean meaning. With N = max_unencoded_bytes (here 1E9) and the
encoded expansion E(x) = f(x)/x:

    red(s)  = extra encoded capacity for a lower-bound guarantee
            = (N/s)*f(s) - f(N)                         [encoded bytes]
    blue(s) = E(s) = f(s)/s                             [encoded/unencoded]

    red/blue = [ (N/s)*f(s) - f(N) ] / (f(s)/s)
             = N * (1 - E(N)/E(s))                      [UNENCODED bytes]

Under today's cap the admission gate rejects when used_encoded > f(N), so an
account whose blobs average `s` bytes can only store

    storable(s) = f(N)/f(s) * s = N * E(N)/E(s)         [unencoded bytes]

before being capped. Therefore

    red/blue = N - storable(s) = the CAPACITY SHORTFALL:

the unencoded bytes by which the account falls short of its nominal cap N,
purely because of Walrus's fixed per-blob metadata overhead. It rises toward
N for tiny blobs (almost none of the promised capacity is usable) and falls
to 0 for blobs near the single-blob max (the cap is fully delivered).

The complement, storable(s)/N = E(N)/E(s), is the "cap-delivery efficiency".

`f(L)` reimplements walrus_core::encoding::encoded_blob_length_for_n_shards
(RS2).
"""

import matplotlib.pyplot as plt
import numpy as np

ALIGNMENT = 2  # EncodingType::RS2::required_alignment()
MAX_SYMBOL_SIZE = 65534  # u16::MAX - 1
DIGEST_LEN = 32
BLOB_ID_LEN = 32
TARGET_BYTES = 1_000_000_000  # 1E9 = nominal max_unencoded_bytes (the cap N)
N_SHARDS = 1000  # Walrus mainnet/testnet


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


def shortfall(blob_size: int, n_shards: int, baseline: int) -> float | None:
    """Capacity shortfall (unencoded bytes) = N - storable(s) = red/blue."""
    enc = encoded_blob_length(blob_size, n_shards)
    if enc is None:
        return None
    storable = baseline / enc * blob_size          # f(N)/f(s) * s = N*E(N)/E(s)
    return TARGET_BYTES - storable


def fmt_bytes(n: float) -> str:
    for unit, div in (("GB", 1e9), ("MB", 1e6), ("KB", 1e3)):
        if abs(n) >= div:
            return f"{n / div:.0f} {unit}"
    return f"{n:.0f} B"


def main() -> None:
    sizes = np.unique(np.logspace(2, 9, 600).astype(np.int64))  # 100 B .. 1 GB
    baseline = encoded_blob_length(TARGET_BYTES, N_SHARDS)       # f(N)

    xs, ys = [], []
    for s in sizes:
        s = int(s)
        v = shortfall(s, N_SHARDS, baseline)
        if v is None:
            continue
        xs.append(s)
        ys.append(v)

    fig, ax = plt.subplots(figsize=(11, 6.5))
    ax.plot(xs, ys, color="tab:purple", linewidth=2.2,
            label="capacity shortfall = cap − actually-storable")
    ax.axhline(TARGET_BYTES, color="gray", ls="--", lw=1, alpha=0.7)
    ax.text(110, TARGET_BYTES * 0.985,
            "entire 1E9 cap undeliverable (overhead dominates)",
            color="gray", fontsize=9, va="top")

    # Reference points at 1/10/100 MB.
    for s in (1_000_000, 10_000_000, 100_000_000):
        v = shortfall(s, N_SHARDS, baseline)
        ax.plot([s], [v], "o", color="tab:purple", ms=6)
        ax.annotate(
            f"{fmt_bytes(s)} blobs\n→ {v / TARGET_BYTES:.0%} of cap lost\n"
            f"   ({fmt_bytes(v)} short)",
            xy=(s, v), xytext=(s * 1.4, v + 0.06e9), fontsize=8.5,
            color="tab:purple",
        )

    ax.set_xscale("log")
    ax.set_xlabel("average blob size (bytes)")
    ax.set_ylabel("capacity shortfall  (unencoded bytes)")
    ax.set_ylim(0, TARGET_BYTES * 1.05)
    ax.set_title(
        "How much of a 1E9 max_unencoded_bytes cap is actually usable?\n"
        "Capacity shortfall  N·(1 − E(N)/E(s))  vs average blob size  "
        f"(n_shards={N_SHARDS})"
    )
    ax.grid(True, which="both", ls=":", alpha=0.4)
    ax.legend(loc="center left")

    # Right axis: same curve as a fraction of the nominal cap.
    ax2 = ax.secondary_yaxis(
        "right",
        functions=(lambda b: b / TARGET_BYTES, lambda f: f * TARGET_BYTES),
    )
    ax2.set_ylabel("shortfall as fraction of nominal cap\n"
                   "(1 − delivery efficiency)")

    # Explanatory box.
    ax.text(
        0.985, 0.04,
        "E(x) = f(x)/x  (encoded expansion)\n"
        "storable(s) = N·E(N)/E(s)   shortfall = N − storable(s)\n"
        "today the cap is an UPPER bound; the avg_blob_size knob\n"
        "inflates the budget to close this gap (lower bound).",
        transform=ax.transAxes, ha="right", va="bottom", fontsize=8.5,
        bbox=dict(boxstyle="round", fc="white", ec="0.7", alpha=0.9),
    )
    fig.tight_layout()

    out = "walrus_storage_efficiency_ratio.png"
    fig.savefig(out, dpi=130)
    print(f"wrote {out}")

    print(f"\nbaseline f(1E9) = {baseline:,} encoded bytes")
    print(f"\nn_shards={N_SHARDS} capacity shortfall (cap N=1E9):")
    for s in (1_000, 100_000, 1_000_000, 10_000_000, 100_000_000):
        v = shortfall(s, N_SHARDS, baseline)
        print(f"  blob={s:>12,} B  ->  shortfall {v:>16,.0f} B  "
              f"({v / TARGET_BYTES:6.1%} of cap lost; "
              f"{1 - v / TARGET_BYTES:5.1%} delivered)")


if __name__ == "__main__":
    main()

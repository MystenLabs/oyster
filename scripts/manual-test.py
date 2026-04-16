#!/usr/bin/env python3
"""Manual smoke-test script for the Oyster local testbed.

Run after starting the local testbed (procman oyster.procman -- --walrus_dir ../walrus).
Exercises core CRUD flows end-to-end against the live local stack.

Usage:
    python3 scripts/manual-test.py
"""

import base64
import json
import os
import sys
import time
import urllib.error
import urllib.request


# ANSI colors
GREEN = "\033[32m"
RED = "\033[31m"
YELLOW = "\033[33m"
BOLD = "\033[1m"
RESET = "\033[0m"


def ok(msg):
    print(f"  {GREEN}PASS{RESET} {msg}")


def fail(msg):
    print(f"  {RED}FAIL{RESET} {msg}")


def info(msg):
    print(f"  {YELLOW}INFO{RESET} {msg}")


def heading(label, title):
    print(f"\n{BOLD}=== Scenario {label}: {title} ==={RESET}")


def request(method, url, body=None, headers=None, expected_status=200):
    """Make an HTTP request and return (status, headers, body_bytes).

    Does NOT raise on HTTP errors — returns the error status instead.
    """
    hdrs = headers or {}
    data = None
    if body is not None:
        if isinstance(body, dict):
            data = json.dumps(body).encode()
            hdrs.setdefault("Content-Type", "application/json")
        elif isinstance(body, bytes):
            data = body
        else:
            data = body.encode()

    req = urllib.request.Request(url, data=data, headers=hdrs, method=method)
    try:
        resp = urllib.request.urlopen(req)
        resp_body = resp.read()
        return resp.status, dict(resp.headers), resp_body
    except urllib.error.HTTPError as e:
        resp_body = e.read()
        return e.code, dict(e.headers), resp_body


def json_body(raw):
    return json.loads(raw) if raw else None


# ---------------------------------------------------------------------------
# Scenarios
# ---------------------------------------------------------------------------


def scenario_1(base, auth):
    """Account & Wallet Info."""
    heading("1", "Account & Wallet Info")
    status, _, body = request("GET", f"{base}/api/v1/account/wallet", headers=auth)
    if status != 200:
        fail(f"GET /account/wallet returned {status}")
        return False
    data = json_body(body)
    address = data.get("address")
    if not address:
        fail("no wallet address returned")
        return False
    ok(f"wallet address={address}")
    return True


def scenario_2(base, auth, ctx):
    """Bucket CRUD."""
    heading("2", "Bucket CRUD")
    passed = True

    bucket_name = f"test-bucket-{int(time.time())}"

    # Create bucket
    status, _, body = request(
        "POST", f"{base}/api/v1/buckets", body={"name": bucket_name}, headers=auth
    )
    if status != 201:
        fail(f"POST /buckets returned {status} (expected 201)")
        return False
    data = json_body(body)
    bucket_name = data["name"]
    ctx["bucket_name"] = bucket_name
    ok(f"created bucket '{bucket_name}'")

    # List buckets
    status, _, body = request("GET", f"{base}/api/v1/buckets", headers=auth)
    if status != 200:
        fail(f"GET /buckets returned {status}")
        passed = False
    else:
        items = json_body(body).get("data", [])
        if not any(b["name"] == bucket_name for b in items):
            fail("new bucket not found in listing")
            passed = False
        else:
            ok("bucket appears in listing")

    # Duplicate name -> 409
    status, _, _ = request(
        "POST", f"{base}/api/v1/buckets", body={"name": bucket_name}, headers=auth
    )
    if status != 409:
        fail(f"duplicate POST /buckets returned {status} (expected 409)")
        passed = False
    else:
        ok("duplicate bucket name correctly rejected (409)")

    return passed


def scenario_3(base, auth, ctx):
    """Blob Store + Read."""
    heading("3", "Blob Store + Read")
    passed = True
    bucket = ctx["bucket_name"]
    key = "test-blob.txt"
    payload = b"hello oyster"

    # Store blob
    hdrs = {**auth, "Content-Type": "text/plain"}
    status, _, body = request(
        "PUT", f"{base}/api/v1/buckets/{bucket}/blobs/{key}", body=payload, headers=hdrs
    )
    if status != 201:
        fail(f"PUT blob returned {status} (expected 201)")
        return False
    data = json_body(body)
    blob_id = data["blob_id"]
    ctx["key"] = key
    ctx["blob_id"] = blob_id
    ok(f"stored blob key={key} blob_id={blob_id}")

    # Read by bucket/key (no auth)
    status, _, body = request("GET", f"{base}/api/v1/buckets/{bucket}/blobs/{key}")
    if status != 200:
        fail(f"GET /buckets/{bucket}/blobs/{key} returned {status}")
        passed = False
    elif body != payload:
        fail(f"body mismatch: got {body!r}")
        passed = False
    else:
        ok("read by key matches")

    # Read by blob_id (no auth)
    status, _, body = request("GET", f"{base}/api/v1/blobs/by-blob-id/{blob_id}")
    if status != 200:
        fail(f"GET /blobs/by-blob-id/{blob_id} returned {status}")
        passed = False
    elif body != payload:
        fail(f"body mismatch: got {body!r}")
        passed = False
    else:
        ok("read by blob_id matches")

    return passed


def scenario_4(base, auth, ctx):
    """Blob Listing."""
    heading("4", "Blob Listing")
    passed = True
    bucket = ctx["bucket_name"]

    # List -- should have 1 blob from scenario 3
    status, _, body = request(
        "GET", f"{base}/api/v1/buckets/{bucket}/blobs", headers=auth
    )
    if status != 200:
        fail(f"GET blobs listing returned {status}")
        return False
    items = json_body(body).get("data", [])
    if len(items) < 1:
        fail(f"expected >=1 blob, got {len(items)}")
        passed = False
    else:
        ok(f"listing shows {len(items)} blob(s)")

    # Store a second blob
    key2 = "second-blob.txt"
    hdrs = {**auth, "Content-Type": "text/plain"}
    status, _, body = request(
        "PUT",
        f"{base}/api/v1/buckets/{bucket}/blobs/{key2}",
        body=b"second blob",
        headers=hdrs,
    )
    if status != 201:
        fail(f"PUT second blob returned {status}")
        return False
    ctx["key_2"] = key2
    ok("stored second blob")

    # List again -- should have 2
    status, _, body = request(
        "GET", f"{base}/api/v1/buckets/{bucket}/blobs", headers=auth
    )
    if status != 200:
        fail(f"GET blobs listing returned {status}")
        passed = False
    else:
        items = json_body(body).get("data", [])
        if len(items) != 2:
            fail(f"expected 2 blobs, got {len(items)}")
            passed = False
        else:
            ok("listing shows 2 blobs")

    return passed


def scenario_5(base, auth, ctx):
    """Content-Addressed Dedup."""
    heading("5", "Content-Addressed Dedup")
    bucket = ctx["bucket_name"]
    original_blob_id = ctx["blob_id"]

    # Upload identical content as scenario 3 under a different key
    dedup_key = "dedup-blob.txt"
    hdrs = {**auth, "Content-Type": "text/plain"}
    status, _, body = request(
        "PUT",
        f"{base}/api/v1/buckets/{bucket}/blobs/{dedup_key}",
        body=b"hello oyster",
        headers=hdrs,
    )
    if status != 201:
        fail(f"PUT dedup blob returned {status}")
        return False
    data = json_body(body)
    new_blob_id = data["blob_id"]

    if new_blob_id != original_blob_id:
        fail(f"blob_id differs: {new_blob_id} != {original_blob_id}")
        return False

    ctx["dedup_key"] = dedup_key
    ok(f"same blob_id for different key '{dedup_key}'")
    return True


def scenario_duplicate(base, auth, ctx):
    """Blob Duplicate (Phase B: may 501 until Phase C lands)."""
    heading("5b", "Blob Duplicate")
    bucket = ctx["bucket_name"]
    src_key = ctx["key"]  # from scenario 3
    dst_key = "dup-of-first.txt"

    body = {"destination_bucket": bucket, "destination_key": dst_key}
    hdrs = {**auth, "Content-Type": "application/json"}
    status, _, resp_body = request(
        "POST",
        f"{base}/api/v1/buckets/{bucket}/blobs/{src_key}/duplicate",
        body=body,
        headers=hdrs,
    )
    if status == 501:
        info("backend returned 501 — Walrus duplicate not yet implemented (Phase C)")
        return True
    if status != 201:
        fail(f"POST duplicate returned {status} (expected 201)")
        return False
    data = json_body(resp_body)
    if data.get("blob_id") != ctx.get("blob_id"):
        fail(f"duplicate blob_id mismatch: {data.get('blob_id')} vs {ctx.get('blob_id')}")
        return False
    ok(f"duplicate created at '{dst_key}' with same blob_id")

    # GET the duplicate; bytes should match the original.
    status, _, body_bytes = request(
        "GET", f"{base}/api/v1/buckets/{bucket}/blobs/{dst_key}"
    )
    if status != 200 or body_bytes != b"hello oyster":
        fail(f"GET duplicate mismatch: status={status}, len={len(body_bytes)}")
        return False
    ok("GET duplicate returns original bytes")

    # Self-duplicate must 400.
    status, _, _ = request(
        "POST",
        f"{base}/api/v1/buckets/{bucket}/blobs/{src_key}/duplicate",
        body={"destination_bucket": bucket, "destination_key": src_key},
        headers=hdrs,
    )
    if status != 400:
        fail(f"self-duplicate returned {status} (expected 400)")
        return False
    ok("self-duplicate correctly rejected (400)")

    ctx["dup_key"] = dst_key
    return True


def scenario_6(base, auth, ctx):
    """Blob Metadata Update."""
    heading("6", "Blob Metadata Update")
    bucket = ctx["bucket_name"]
    key = ctx["key"]

    status, _, body = request(
        "PATCH",
        f"{base}/api/v1/buckets/{bucket}/blobs/{key}/metadata",
        body={"content_type": "text/markdown"},
        headers=auth,
    )
    if status != 200:
        fail(f"PATCH metadata returned {status}")
        return False
    data = json_body(body)
    if data.get("content_type") != "text/markdown":
        fail(f"content_type is {data.get('content_type')}, expected text/markdown")
        return False
    ok("content_type updated to text/markdown")
    return True


def scenario_7(base, auth, ctx):
    """Blob Delete."""
    heading("7", "Blob Delete")
    passed = True
    bucket = ctx["bucket_name"]
    key = ctx["key"]

    status, _, _ = request(
        "DELETE", f"{base}/api/v1/buckets/{bucket}/blobs/{key}", headers=auth
    )
    if status != 204:
        fail(f"DELETE blob returned {status} (expected 204)")
        return False
    ok("blob deleted")

    status, _, _ = request("GET", f"{base}/api/v1/buckets/{bucket}/blobs/{key}")
    if status != 404:
        fail(f"GET deleted blob returned {status} (expected 404)")
        passed = False
    else:
        ok("deleted blob returns 404")

    return passed


def scenario_8(base, auth, ctx):
    """Bucket Delete (drain then delete)."""
    heading("8", "Bucket Delete (drain then delete)")
    bucket = ctx["bucket_name"]

    # The DELETE /buckets/{name} endpoint does NOT cascade — it returns 409 if
    # the bucket is non-empty. Verify that, then drain the bucket and retry.
    status, _, _ = request(
        "DELETE", f"{base}/api/v1/buckets/{bucket}", headers=auth
    )
    if status != 409:
        fail(f"DELETE non-empty bucket returned {status} (expected 409)")
        return False
    ok("non-empty bucket correctly rejected (409)")

    # Drain: list and delete every remaining blob.
    status, _, body = request(
        "GET", f"{base}/api/v1/buckets/{bucket}/blobs", headers=auth
    )
    if status != 200:
        fail(f"GET blobs listing returned {status}")
        return False
    for blob in json_body(body).get("data", []):
        key = blob["key"]
        status, _, _ = request(
            "DELETE", f"{base}/api/v1/buckets/{bucket}/blobs/{key}", headers=auth
        )
        if status != 204:
            fail(f"DELETE blob {key} returned {status} (expected 204)")
            return False
    ok("drained remaining blobs from bucket")

    # Delete bucket (now empty)
    status, _, _ = request(
        "DELETE", f"{base}/api/v1/buckets/{bucket}", headers=auth
    )
    if status != 204:
        fail(f"DELETE bucket returned {status} (expected 204)")
        return False
    ok("bucket deleted")

    # Verify bucket is gone
    status, _, body = request("GET", f"{base}/api/v1/buckets", headers=auth)
    if status != 200:
        fail(f"GET /buckets returned {status}")
        return False
    items = json_body(body).get("data", [])
    if any(b["name"] == bucket for b in items):
        fail("deleted bucket still appears in listing")
        return False
    ok("bucket no longer in listing")
    return True


def scenario_9(base, auth, ctx):
    """API Key Management (skipped — now JWT-only)."""
    heading("9", "API Key Management (skipped)")
    info(
        "API key create/revoke moved to admin routes "
        "(POST/DELETE /api/v1/accounts/{account_id}/api-keys) which require "
        "an app JWT. Re-enable once the script has a --jwt bootstrap flow."
    )
    return True


def scenario_10(base, auth):
    """Error Cases."""
    heading("10", "Error Cases")
    passed = True

    # GET non-existent blob
    status, _, _ = request(
        "GET", f"{base}/api/v1/buckets/nonexistent-bucket/blobs/fake-key"
    )
    if status != 404:
        fail(f"GET nonexistent blob returned {status} (expected 404)")
        passed = False
    else:
        ok("GET non-existent blob -> 404")

    # GET malformed blob_id — aggregator can't decode it as a Walrus BlobId, so
    # it returns 400. Oyster should propagate that status so the caller can tell
    # "malformed ID" apart from "well-formed but missing".
    status, _, _ = request(
        "GET", f"{base}/api/v1/blobs/by-blob-id/FAKE_BLOB_ID_DOES_NOT_EXIST"
    )
    if status != 400:
        fail(f"GET malformed blob_id returned {status} (expected 400)")
        passed = False
    else:
        ok("GET malformed blob_id -> 400")

    # GET well-formed but nonexistent blob_id — Walrus aggregator returns 404
    # for a valid-but-unknown BlobId, which Oyster passes through.
    random_blob_id = (
        base64.urlsafe_b64encode(os.urandom(32)).rstrip(b"=").decode()
    )
    status, _, _ = request(
        "GET", f"{base}/api/v1/blobs/by-blob-id/{random_blob_id}"
    )
    if status != 404:
        fail(
            f"GET well-formed nonexistent blob_id returned {status} (expected 404)"
        )
        passed = False
    else:
        ok("GET well-formed nonexistent blob_id -> 404")

    # DELETE non-existent blob
    status, _, _ = request(
        "DELETE",
        f"{base}/api/v1/buckets/nonexistent-bucket/blobs/fake-key",
        headers=auth,
    )
    if status != 404:
        fail(f"DELETE nonexistent blob returned {status} (expected 404)")
        passed = False
    else:
        ok("DELETE non-existent blob -> 404")

    # No auth header
    status, _, _ = request("GET", f"{base}/api/v1/buckets")
    if status != 401:
        fail(f"GET /buckets without auth returned {status} (expected 401)")
        passed = False
    else:
        ok("missing auth -> 401")

    return passed


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main():
    print(f"{BOLD}Oyster Manual Smoke Test{RESET}\n")

    base = input("Oyster URL [http://127.0.0.1:3000]: ").strip()
    if not base:
        base = "http://127.0.0.1:3000"
    base = base.rstrip("/")

    api_key = input("API key: ").strip()
    if not api_key:
        print("API key is required.")
        sys.exit(1)

    auth = {"Authorization": f"Bearer {api_key}"}
    ctx = {}  # shared state between scenarios

    scenarios = [
        ("Account & Wallet Info", lambda: scenario_1(base, auth)),
        ("Bucket CRUD", lambda: scenario_2(base, auth, ctx)),
        ("Blob Store + Read", lambda: scenario_3(base, auth, ctx)),
        ("Blob Listing", lambda: scenario_4(base, auth, ctx)),
        ("Content-Addressed Dedup", lambda: scenario_5(base, auth, ctx)),
        ("Blob Duplicate", lambda: scenario_duplicate(base, auth, ctx)),
        ("Blob Metadata Update", lambda: scenario_6(base, auth, ctx)),
        ("Blob Delete", lambda: scenario_7(base, auth, ctx)),
        ("Bucket Delete (drain then delete)", lambda: scenario_8(base, auth, ctx)),
        ("API Key Management", lambda: scenario_9(base, auth, ctx)),
        ("Error Cases", lambda: scenario_10(base, auth)),
    ]

    results = []
    for i, (name, fn) in enumerate(scenarios):
        try:
            passed = fn()
        except Exception as e:
            fail(f"exception: {e}")
            passed = False
        results.append((name, passed))

        if not passed:
            if i < len(scenarios) - 1:
                input("\nPress Enter to continue (or Ctrl-C to abort)...")

    # Summary
    print(f"\n{BOLD}=== Summary ==={RESET}")
    all_passed = True
    for name, passed in results:
        status_str = f"{GREEN}PASS{RESET}" if passed else f"{RED}FAIL{RESET}"
        print(f"  {status_str} {name}")
        if not passed:
            all_passed = False

    print()
    if all_passed:
        print(f"{GREEN}All scenarios passed!{RESET}")
    else:
        print(f"{RED}Some scenarios failed.{RESET}")
    sys.exit(0 if all_passed else 1)


if __name__ == "__main__":
    main()

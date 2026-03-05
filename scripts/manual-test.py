#!/usr/bin/env python3
"""Manual smoke-test script for the Oyster local testbed.

Run after starting the local testbed (./scripts/local-testbed.sh).
Exercises core CRUD flows end-to-end against the live local stack.

Usage:
    python3 scripts/manual-test.py
"""

import json
import sys
import uuid
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


def heading(n, title):
    print(f"\n{BOLD}=== Scenario {n}: {title} ==={RESET}")


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
    heading(1, "Account & Wallet Info")
    status, _, body = request("GET", f"{base}/account/wallet", headers=auth)
    if status != 200:
        fail(f"GET /account/wallet returned {status}")
        return False
    data = json_body(body)
    if not data.get("provisioned"):
        fail("wallet not provisioned")
        return False
    wallet = data.get("wallet")
    if not wallet or not wallet.get("address"):
        fail("no wallet address returned")
        return False
    ok(f"wallet provisioned, address={wallet['address']}")
    return True


def scenario_2(base, auth, ctx):
    """Bucket CRUD."""
    heading(2, "Bucket CRUD")
    passed = True

    # Create bucket
    status, _, body = request(
        "POST", f"{base}/buckets", body={"name": "test-bucket"}, headers=auth
    )
    if status != 201:
        fail(f"POST /buckets returned {status} (expected 201)")
        return False
    data = json_body(body)
    bucket_id = data["id"]
    ctx["bucket_id"] = bucket_id
    ok(f"created bucket {bucket_id}")

    # List buckets
    status, _, body = request("GET", f"{base}/buckets", headers=auth)
    if status != 200:
        fail(f"GET /buckets returned {status}")
        passed = False
    else:
        items = json_body(body).get("data", [])
        if not any(b["id"] == bucket_id for b in items):
            fail("new bucket not found in listing")
            passed = False
        else:
            ok("bucket appears in listing")

    # Duplicate name → 409
    status, _, _ = request(
        "POST", f"{base}/buckets", body={"name": "test-bucket"}, headers=auth
    )
    if status != 409:
        fail(f"duplicate POST /buckets returned {status} (expected 409)")
        passed = False
    else:
        ok("duplicate bucket name correctly rejected (409)")

    return passed


def scenario_3(base, auth, ctx):
    """Blob Store + Read."""
    heading(3, "Blob Store + Read")
    passed = True
    bucket_id = ctx["bucket_id"]
    payload = b"hello oyster"

    # Store blob
    hdrs = {**auth, "Content-Type": "text/plain"}
    status, _, body = request(
        "PUT", f"{base}/buckets/{bucket_id}/blobs", body=payload, headers=hdrs
    )
    if status != 201:
        fail(f"PUT blob returned {status} (expected 201)")
        return False
    data = json_body(body)
    object_id = data["object_id"]
    blob_id = data["blob_id"]
    ctx["object_id"] = object_id
    ctx["blob_id"] = blob_id
    ok(f"stored blob object_id={object_id} blob_id={blob_id}")

    # Read by object_id (no auth)
    status, _, body = request("GET", f"{base}/blobs/{object_id}")
    if status != 200:
        fail(f"GET /blobs/{{object_id}} returned {status}")
        passed = False
    elif body != payload:
        fail(f"body mismatch: got {body!r}")
        passed = False
    else:
        ok("read by object_id matches")

    # Read by blob_id (no auth)
    status, _, body = request("GET", f"{base}/blobs/by-blob-id/{blob_id}")
    if status != 200:
        fail(f"GET /blobs/by-blob-id/{{blob_id}} returned {status}")
        passed = False
    elif body != payload:
        fail(f"body mismatch: got {body!r}")
        passed = False
    else:
        ok("read by blob_id matches")

    return passed


def scenario_4(base, auth, ctx):
    """Blob Listing."""
    heading(4, "Blob Listing")
    passed = True
    bucket_id = ctx["bucket_id"]

    # List — should have 1 blob from scenario 3
    status, _, body = request(
        "GET", f"{base}/buckets/{bucket_id}/blobs", headers=auth
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
    hdrs = {**auth, "Content-Type": "text/plain"}
    status, _, body = request(
        "PUT",
        f"{base}/buckets/{bucket_id}/blobs",
        body=b"second blob",
        headers=hdrs,
    )
    if status != 201:
        fail(f"PUT second blob returned {status}")
        return False
    ctx["object_id_2"] = json_body(body)["object_id"]
    ok("stored second blob")

    # List again — should have 2
    status, _, body = request(
        "GET", f"{base}/buckets/{bucket_id}/blobs", headers=auth
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
    heading(5, "Content-Addressed Dedup")
    bucket_id = ctx["bucket_id"]
    original_blob_id = ctx["blob_id"]

    # Upload identical content as scenario 3
    hdrs = {**auth, "Content-Type": "text/plain"}
    status, _, body = request(
        "PUT",
        f"{base}/buckets/{bucket_id}/blobs",
        body=b"hello oyster",
        headers=hdrs,
    )
    if status != 201:
        fail(f"PUT dedup blob returned {status}")
        return False
    data = json_body(body)
    new_object_id = data["object_id"]
    new_blob_id = data["blob_id"]

    if new_blob_id != original_blob_id:
        fail(f"blob_id differs: {new_blob_id} != {original_blob_id}")
        return False
    if new_object_id == ctx["object_id"]:
        fail("object_id should differ for dedup entry")
        return False

    ctx["object_id_dedup"] = new_object_id
    ok(f"same blob_id, different object_id={new_object_id}")
    return True


def scenario_6(base, auth, ctx):
    """Blob Metadata Update."""
    heading(6, "Blob Metadata Update")
    object_id = ctx["object_id"]

    status, _, body = request(
        "PATCH",
        f"{base}/blobs/{object_id}/metadata",
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
    heading(7, "Blob Delete")
    passed = True
    object_id = ctx["object_id"]

    status, _, _ = request("DELETE", f"{base}/blobs/{object_id}", headers=auth)
    if status != 204:
        fail(f"DELETE blob returned {status} (expected 204)")
        return False
    ok("blob deleted")

    status, _, _ = request("GET", f"{base}/blobs/{object_id}")
    if status != 404:
        fail(f"GET deleted blob returned {status} (expected 404)")
        passed = False
    else:
        ok("deleted blob returns 404")

    return passed


def scenario_8(base, auth, ctx):
    """Bucket Delete (cascade)."""
    heading(8, "Bucket Delete (cascade)")
    bucket_id = ctx["bucket_id"]

    # Store a fresh blob
    hdrs = {**auth, "Content-Type": "text/plain"}
    status, _, _ = request(
        "PUT",
        f"{base}/buckets/{bucket_id}/blobs",
        body=b"about to be cascaded",
        headers=hdrs,
    )
    if status != 201:
        fail(f"PUT fresh blob returned {status}")
        return False
    ok("stored fresh blob for cascade test")

    # Delete bucket
    status, _, _ = request("DELETE", f"{base}/buckets/{bucket_id}", headers=auth)
    if status != 204:
        fail(f"DELETE bucket returned {status} (expected 204)")
        return False
    ok("bucket deleted")

    # Verify bucket is gone
    status, _, body = request("GET", f"{base}/buckets", headers=auth)
    if status != 200:
        fail(f"GET /buckets returned {status}")
        return False
    items = json_body(body).get("data", [])
    if any(b["id"] == bucket_id for b in items):
        fail("deleted bucket still appears in listing")
        return False
    ok("bucket no longer in listing")
    return True


def scenario_9(base, auth, ctx):
    """API Key Management."""
    heading(9, "API Key Management")
    passed = True

    # Create new key
    status, _, body = request("POST", f"{base}/account/api-keys", headers=auth)
    if status != 201:
        fail(f"POST /account/api-keys returned {status}")
        return False
    data = json_body(body)
    new_key = data["secret"]
    key_id = data["id"]
    ok(f"created API key id={key_id}")

    # Use new key
    new_auth = {"Authorization": f"Bearer {new_key}"}
    status, _, _ = request("GET", f"{base}/buckets", headers=new_auth)
    if status != 200:
        fail(f"GET /buckets with new key returned {status}")
        passed = False
    else:
        ok("new key works for GET /buckets")

    # Revoke key
    status, _, _ = request(
        "DELETE", f"{base}/account/api-keys/{key_id}", headers=auth
    )
    if status != 204:
        fail(f"DELETE api-key returned {status} (expected 204)")
        passed = False
    else:
        ok("API key revoked")

    # Use revoked key
    status, _, _ = request("GET", f"{base}/buckets", headers=new_auth)
    if status != 401:
        fail(f"revoked key returned {status} (expected 401)")
        passed = False
    else:
        ok("revoked key correctly rejected (401)")

    return passed


def scenario_10(base, auth):
    """Error Cases."""
    heading(10, "Error Cases")
    passed = True
    fake_id = str(uuid.uuid4())

    # GET non-existent blob
    status, _, _ = request("GET", f"{base}/blobs/{fake_id}")
    if status != 404:
        fail(f"GET /blobs/{{random}} returned {status} (expected 404)")
        passed = False
    else:
        ok("GET non-existent blob → 404")

    # DELETE non-existent blob
    status, _, _ = request("DELETE", f"{base}/blobs/{fake_id}", headers=auth)
    if status != 404:
        fail(f"DELETE /blobs/{{random}} returned {status} (expected 404)")
        passed = False
    else:
        ok("DELETE non-existent blob → 404")

    # No auth header
    status, _, _ = request("GET", f"{base}/buckets")
    if status != 401:
        fail(f"GET /buckets without auth returned {status} (expected 401)")
        passed = False
    else:
        ok("missing auth → 401")

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
        ("Blob Metadata Update", lambda: scenario_6(base, auth, ctx)),
        ("Blob Delete", lambda: scenario_7(base, auth, ctx)),
        ("Bucket Delete (cascade)", lambda: scenario_8(base, auth, ctx)),
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

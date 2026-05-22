#!/usr/bin/env python3
"""Manual smoke-test script for the Oyster local testbed.

Run after starting the local testbed (procman oyster.procman -- --walrus_dir ../walrus).
Exercises core CRUD flows end-to-end against the live local stack:
account/wallet info, bucket CRUD, blob store/read/list/dedup, metadata,
blob tag CRUD, blob delete, bucket delete, API key management,
multi-account isolation, and error cases.

Usage:
    # Non-interactive (recommended): scrapes the bearer token from setup.log
    # and bootstraps additional accounts via an admin key for isolation
    # coverage.
    python3 scripts/manual-test.py --admin-key "$(./target/debug/oysterd app \\
        issue-admin-key 00000000-0000-0000-0000-000000000000)"

    # Single-account mode with an explicit pre-funded bearer token.
    python3 scripts/manual-test.py --bearer-token $BEARER

    # Interactive (legacy): prompts for URL and API key.
    python3 scripts/manual-test.py
"""

import argparse
import base64
import json
import os
import re
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
    heading(2, "Bucket CRUD")
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
    heading(3, "Blob Store + Read")
    passed = True
    bucket = ctx["bucket_name"]
    key = "test-blob.txt"
    payload = b"hello oyster"

    # Store blob with an initial tag via x-oyster-tag header.
    hdrs = {
        **auth,
        "Content-Type": "text/plain",
        "x-oyster-tag": "source=smoke",
    }
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

    # Initial x-oyster-tag header applied.
    status, _, body = request(
        "GET", f"{base}/api/v1/buckets/{bucket}/blobs/{key}/tags", headers=auth
    )
    if status != 200:
        fail(f"GET initial tags returned {status}")
        passed = False
    else:
        tags = (json_body(body) or {}).get("tags", {})
        if tags != {"source": "smoke"}:
            fail(f"initial tags mismatch: {tags!r}")
            passed = False
        else:
            ok("initial x-oyster-tag applied on PUT")

    return passed


def scenario_4(base, auth, ctx):
    """Blob Listing."""
    heading(4, "Blob Listing")
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
    heading(5, "Content-Addressed Dedup")
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


def scenario_6(base, auth, ctx):
    """Blob Metadata Update."""
    heading(6, "Blob Metadata Update")
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


def scenario_tags(base, auth, ctx):
    """Blob Tags CRUD + validation limits."""
    heading("tags", "Blob Tags CRUD")
    passed = True
    bucket = ctx["bucket_name"]
    key = ctx["key"]
    tags_url = f"{base}/api/v1/buckets/{bucket}/blobs/{key}/tags"
    json_hdrs = {**auth, "Content-Type": "application/json"}

    # Reset: scenario_3 left source=smoke on this blob. Start clean
    # so "GET empty" is deterministic regardless of upstream ordering.
    status, _, _ = request("DELETE", tags_url, headers=auth)
    if status != 204:
        fail(f"DELETE /tags (reset) returned {status} (expected 204)")
        return False

    # 1) GET empty
    status, _, body = request("GET", tags_url, headers=auth)
    if status != 200 or json_body(body) != {"tags": {}}:
        fail(f"GET empty tags returned {status} body={body!r}")
        passed = False
    else:
        ok("GET empty tags -> 200 with empty map")

    # 2) PUT full replace
    status, _, _ = request(
        "PUT", tags_url,
        body={"tags": {"project": "demo", "env": "dev"}},
        headers=json_hdrs,
    )
    if status != 204:
        fail(f"PUT tags (replace) returned {status} (expected 204)")
        passed = False
    else:
        status, _, body = request("GET", tags_url, headers=auth)
        got = (json_body(body) or {}).get("tags", {})
        if got != {"project": "demo", "env": "dev"}:
            fail(f"tags after PUT replace: {got!r}")
            passed = False
        else:
            ok("PUT replace persisted project+env")

    # 3) PATCH merge
    status, _, _ = request(
        "PATCH", tags_url,
        body={"tags": {"owner": "alice"}},
        headers=json_hdrs,
    )
    if status != 204:
        fail(f"PATCH tags returned {status} (expected 204)")
        passed = False
    else:
        status, _, body = request("GET", tags_url, headers=auth)
        got = (json_body(body) or {}).get("tags", {})
        want = {"project": "demo", "env": "dev", "owner": "alice"}
        if got != want:
            fail(f"tags after PATCH merge: {got!r}")
            passed = False
        else:
            ok("PATCH merge preserved prior tags + added owner")

    # 4) Single-key upsert (text/plain body)
    status, _, _ = request(
        "PUT", f"{tags_url}/project",
        body="new-demo",
        headers={**auth, "Content-Type": "text/plain"},
    )
    if status != 204:
        fail(f"PUT /tags/project returned {status} (expected 204)")
        passed = False
    else:
        status, _, body = request("GET", tags_url, headers=auth)
        if (json_body(body) or {}).get("tags", {}).get("project") != "new-demo":
            fail("single-key upsert did not persist")
            passed = False
        else:
            ok("single-key upsert persisted project=new-demo")

    # 5) Single-key delete
    status, _, _ = request("DELETE", f"{tags_url}/owner", headers=auth)
    if status != 204:
        fail(f"DELETE /tags/owner returned {status} (expected 204)")
        passed = False
    else:
        status, _, body = request("GET", tags_url, headers=auth)
        if "owner" in (json_body(body) or {}).get("tags", {}):
            fail("owner tag still present after single-key delete")
            passed = False
        else:
            ok("single-key delete removed owner")

    # 6) Clear all
    status, _, _ = request("DELETE", tags_url, headers=auth)
    if status != 204:
        fail(f"DELETE /tags returned {status} (expected 204)")
        passed = False
    else:
        status, _, body = request("GET", tags_url, headers=auth)
        if json_body(body) != {"tags": {}}:
            fail(f"tags not empty after clear-all: {body!r}")
            passed = False
        else:
            ok("clear-all left empty tag set")

    # Limit / validation subtests. Each must 4xx (not 5xx).
    def expect_4xx(label, method, url, body=None, headers=None):
        nonlocal passed
        status, _, resp = request(method, url, body=body, headers=headers)
        if 400 <= status < 500:
            ok(f"{label} -> {status}")
        elif status >= 500:
            fail(f"{label} returned 5xx {status}: {resp!r}")
            passed = False
        else:
            fail(f"{label} returned {status} (expected 4xx)")
            passed = False

    # 11 tags in one PUT
    too_many = {f"k{i}": "v" for i in range(11)}
    expect_4xx(
        "11 tags",
        "PUT", tags_url,
        body={"tags": too_many},
        headers=json_hdrs,
    )

    # Tag key length 129
    expect_4xx(
        "key length 129",
        "PUT", tags_url,
        body={"tags": {"k" * 129: "v"}},
        headers=json_hdrs,
    )

    # Tag value length 257
    expect_4xx(
        "value length 257",
        "PUT", tags_url,
        body={"tags": {"k": "v" * 257}},
        headers=json_hdrs,
    )

    # Disallowed charset
    expect_4xx(
        "disallowed char '!'",
        "PUT", tags_url,
        body={"tags": {"bad!key": "v"}},
        headers=json_hdrs,
    )

    # Duplicate-key-in-JSON: serde collapses to BTreeMap, so this
    # actually ends up as a valid single-key PUT. Assert only "not 5xx"
    # and log the observed status so regressions are obvious.
    raw_dup = b'{"tags": {"a": "1", "a": "2"}}'
    status, _, resp = request(
        "PUT", tags_url, body=raw_dup, headers=json_hdrs,
    )
    if status >= 500:
        fail(f"duplicate-JSON-key PUT returned 5xx {status}: {resp!r}")
        passed = False
    else:
        ok(f"duplicate-JSON-key PUT -> {status} (no 5xx)")

    # Auth subtest: GET /tags without Authorization -> 401
    status, _, _ = request("GET", tags_url)
    if status != 401:
        fail(f"GET /tags w/o auth returned {status} (expected 401)")
        passed = False
    else:
        ok("GET /tags without auth -> 401")

    # Cross-account isolation: B can't read/write A's tags. Only if
    # we have an admin key (so we can mint account B). Follows the
    # pattern in scenario_isolation.
    admin = ctx.get("admin")
    if not admin or not admin.get("admin_key"):
        info("no --admin-key; skipping cross-account isolation on tags")
    else:
        admin_auth = {"Authorization": f"Bearer {admin['admin_key']}"}
        status, _, body = request(
            "POST", f"{base}/api/v1/accounts", body={}, headers=admin_auth
        )
        if status != 201:
            fail(f"bootstrap account B returned {status}")
            passed = False
        else:
            b_token = json_body(body)["api_key"]["bearer_token"]
            auth_b = {"Authorization": f"Bearer {b_token}"}
            for method, url, body_val, hdrs in [
                ("GET", tags_url, None, auth_b),
                ("PUT", tags_url, {"tags": {"evil": "v"}},
                 {**auth_b, "Content-Type": "application/json"}),
                ("DELETE", tags_url, None, auth_b),
            ]:
                status, _, _ = request(method, url, body=body_val, headers=hdrs)
                if status != 404:
                    fail(f"B {method} A's tags returned {status} (expected 404)")
                    passed = False
                else:
                    ok(f"B {method} A's tags -> 404")

    # Leave the blob tagless so subsequent scenarios are unaffected.
    # clear-all already left us empty.
    return passed


def scenario_7(base, auth, ctx):
    """Blob Delete (also asserts tag rows cascade on delete)."""
    heading(7, "Blob Delete")
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

    status, _, _ = request(
        "GET", f"{base}/api/v1/buckets/{bucket}/blobs/{key}/tags",
        headers=auth,
    )
    if status != 404:
        fail(f"GET /tags on deleted blob returned {status} (expected 404)")
        passed = False
    else:
        ok("GET /tags on deleted blob -> 404 (tag rows cascaded)")

    return passed


def scenario_8(base, auth, ctx):
    """Bucket Delete (drain then delete)."""
    heading(8, "Bucket Delete (drain then delete)")
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


def scenario_9(base, auth, ctx, admin):
    """API Key Management (admin-key-bootstrapped admin flow)."""
    heading("9", "API Key Management")
    if not admin or not admin.get("admin_key") or not admin.get("account_id"):
        info(
            "no --admin-key / account_id bootstrap available — skipping. "
            "Re-run with --admin-key to exercise admin api-key create/revoke."
        )
        return True

    admin_key = admin["admin_key"]
    account_id = admin["account_id"]
    admin_auth = {"Authorization": f"Bearer {admin_key}"}

    # Mint a fresh API key via the admin route.
    status, _, body = request(
        "POST",
        f"{base}/api/v1/accounts/{account_id}/api-keys",
        body={},
        headers=admin_auth,
    )
    if status != 201:
        fail(f"POST admin api-keys returned {status} (expected 201)")
        return False
    data = json_body(body)
    key_id = data["id"]
    new_token = data["bearer_token"]
    ok(f"minted API key id={key_id}")

    # Confirm the new key authenticates.
    new_auth = {"Authorization": f"Bearer {new_token}"}
    status, _, _ = request("GET", f"{base}/api/v1/buckets", headers=new_auth)
    if status != 200:
        fail(f"GET /buckets with new key returned {status} (expected 200)")
        return False
    ok("new key authenticates (GET /buckets -> 200)")

    # Revoke the key.
    status, _, _ = request(
        "DELETE",
        f"{base}/api/v1/accounts/{account_id}/api-keys/{key_id}",
        headers=admin_auth,
    )
    if status != 204:
        fail(f"DELETE admin api-key returned {status} (expected 204)")
        return False
    ok("revoked API key")

    # The revoked key must fail auth.
    status, _, _ = request("GET", f"{base}/api/v1/buckets", headers=new_auth)
    if status != 401:
        fail(f"GET /buckets with revoked key returned {status} (expected 401)")
        return False
    ok("revoked key correctly rejected (401)")
    return True


def scenario_isolation(base, auth_a, admin):
    """Multi-account isolation: account B can't read/write account A's bucket."""
    heading("isolation", "Multi-account Isolation")
    if not admin or not admin.get("admin_key"):
        info(
            "no --admin-key available — skipping isolation scenario. "
            "Re-run with --admin-key to bootstrap a second account from the admin key."
        )
        return True

    admin_key = admin["admin_key"]
    admin_auth = {"Authorization": f"Bearer {admin_key}"}

    # Bootstrap account B (fresh, unfunded — read-only role in this scenario).
    status, _, body = request(
        "POST", f"{base}/api/v1/accounts", body={}, headers=admin_auth
    )
    if status != 201:
        fail(f"POST /accounts (B) returned {status} (expected 201)")
        return False
    data = json_body(body)
    b_token = data["api_key"]["bearer_token"]
    auth_b = {"Authorization": f"Bearer {b_token}"}
    ok(f"bootstrapped account B id={data['account_id']}")

    # Account A creates a bucket and stores a blob. auth_a is the pre-funded
    # testbed account so the on-chain PUT succeeds.
    bucket = f"iso-{int(time.time())}"
    status, _, _ = request(
        "POST", f"{base}/api/v1/buckets", body={"name": bucket}, headers=auth_a
    )
    if status != 201:
        fail(f"account A create bucket returned {status} (expected 201)")
        return False

    blob_key = "secret.txt"
    hdrs = {**auth_a, "Content-Type": "text/plain"}
    status, _, _ = request(
        "PUT",
        f"{base}/api/v1/buckets/{bucket}/blobs/{blob_key}",
        body=b"account A's secret",
        headers=hdrs,
    )
    if status != 201:
        fail(f"account A PUT blob returned {status} (expected 201)")
        return False
    ok(f"account A provisioned bucket '{bucket}' with blob '{blob_key}'")

    passed = True

    # B listing A's blobs must 404.
    status, _, _ = request(
        "GET", f"{base}/api/v1/buckets/{bucket}/blobs", headers=auth_b
    )
    if status != 404:
        fail(f"B GET A's bucket blobs returned {status} (expected 404)")
        passed = False
    else:
        ok("B can't list A's blobs (404)")

    # B DELETE blob must 404.
    status, _, _ = request(
        "DELETE",
        f"{base}/api/v1/buckets/{bucket}/blobs/{blob_key}",
        headers=auth_b,
    )
    if status != 404:
        fail(f"B DELETE A's blob returned {status} (expected 404)")
        passed = False
    else:
        ok("B can't delete A's blob (404)")

    # B DELETE bucket must 404 (not 403/500).
    status, _, _ = request(
        "DELETE", f"{base}/api/v1/buckets/{bucket}", headers=auth_b
    )
    if status != 404:
        fail(f"B DELETE A's bucket returned {status} (expected 404)")
        passed = False
    else:
        ok("B can't delete A's bucket (404)")

    # Cleanup: A drains and deletes the bucket.
    status, _, _ = request(
        "DELETE",
        f"{base}/api/v1/buckets/{bucket}/blobs/{blob_key}",
        headers=auth_a,
    )
    if status == 204:
        request("DELETE", f"{base}/api/v1/buckets/{bucket}", headers=auth_a)

    return passed


LOW_CAP = 500_000             # 500 KB (unencoded)
RAISED_CAP = 2_000_000        # 2 MB (unencoded)
RESTORE_CAP = 5_000_000_000   # 5 GB — matches CreateAccountRequest default
# 300 KB random per upload — sized to stay well under the local
# Walrus testbed's per-blob encoder limit (≈ 1.5 MiB on n_shards=10
# in RS2 mode; larger blobs would 500 on the encode step before the
# cap check runs). Two of these comfortably exceed `LOW_CAP`'s
# encoded threshold while one fits, so the cap fires on the 2nd or
# 3rd PUT.
CAP_BLOB_SIZE = 300_000
CAP_MAX_TRIES = 20            # hard ceiling so a regression can't hang


def scenario_storage_cap(base, auth, ctx):
    """Per-account max_unencoded_bytes cap (lower, hit, raise, recover)."""
    heading("cap", "Per-account Storage Cap")
    admin = ctx.get("admin") or {}
    admin_key = admin.get("admin_key")
    user_account_id = admin.get("user_account_id")
    if not admin_key or not user_account_id:
        info(
            "no --admin-key or no setup.log user account_id — skipping. "
            "Re-run with --admin-key against a fresh local testbed to "
            "exercise the cap enforcement / admin update."
        )
        return True
    admin_auth = {"Authorization": f"Bearer {admin_key}"}
    cap_url = f"{base}/api/v1/accounts/{user_account_id}/max-storage"
    passed = True
    bucket = f"cap-{int(time.time())}"
    created_keys = []
    json_hdrs = {**admin_auth, "Content-Type": "application/json"}

    def set_cap(new_cap, label):
        nonlocal passed
        status, _, body = request(
            "PUT", cap_url,
            body={"max_unencoded_bytes": new_cap},
            headers=json_hdrs,
        )
        if status != 200:
            fail(f"PUT max-storage ({label}) returned {status} body={body!r}")
            passed = False
            return None
        data = json_body(body) or {}
        if data.get("max_unencoded_bytes") != new_cap:
            fail(
                f"max-storage ({label}) response cap is "
                f"{data.get('max_unencoded_bytes')}, expected {new_cap}"
            )
            passed = False
        return data

    try:
        # 1) Lower cap to 50 MB. May submit a shrink PTB on the
        #    pre-funded account's pool — fine, wallet is funded.
        if set_cap(LOW_CAP, "lower to 50 MB") is None:
            return False
        ok(f"cap lowered to {LOW_CAP} bytes")

        # 2) Create a fresh bucket on the (pre-funded) account.
        status, _, body = request(
            "POST", f"{base}/api/v1/buckets",
            body={"name": bucket}, headers=auth,
        )
        if status != 201:
            fail(f"POST /buckets returned {status} (expected 201)")
            return False
        ok(f"created bucket '{bucket}'")

        # 3) Upload random 5 MiB blobs until one is rejected with 400 +
        #    cap_exceeded.
        rejected_payload = None
        rejected_body = None
        successes = 0
        put_hdrs = {**auth, "Content-Type": "application/octet-stream"}
        cap_fired = False
        for i in range(CAP_MAX_TRIES):
            key = f"cap-{i:02d}.bin"
            payload = os.urandom(CAP_BLOB_SIZE)
            status, _, body = request(
                "PUT",
                f"{base}/api/v1/buckets/{bucket}/blobs/{key}",
                body=payload, headers=put_hdrs,
            )
            if status == 201:
                successes += 1
                created_keys.append(key)
                continue
            if status == 400:
                parsed = json_body(body) or {}
                if "cap_exceeded" in parsed:
                    rejected_payload = payload
                    rejected_body = parsed
                    cap_fired = True
                    break
                fail(
                    f"PUT iteration {i} returned 400 without cap_exceeded: "
                    f"{parsed!r}"
                )
                return False
            fail(
                f"PUT iteration {i} returned unexpected status {status}: "
                f"{body!r}"
            )
            return False

        if not cap_fired:
            fail(f"cap never enforced after {CAP_MAX_TRIES} uploads")
            return False

        if successes < 1:
            fail("expected >=1 successful upload before cap kicked in")
            passed = False
        else:
            ok(f"{successes} upload(s) succeeded before cap fired")

        cap_block = rejected_body["cap_exceeded"]
        if cap_block.get("max_unencoded_bytes") != LOW_CAP:
            fail(
                f"cap_exceeded.max_unencoded_bytes is "
                f"{cap_block.get('max_unencoded_bytes')}, expected {LOW_CAP}"
            )
            passed = False
        if not isinstance(cap_block.get("used_encoded_bytes"), int):
            fail(f"cap_exceeded.used_encoded_bytes not int: {cap_block!r}")
            passed = False
        if not isinstance(cap_block.get("new_unencoded_bytes"), int):
            fail(f"cap_exceeded.new_unencoded_bytes not int: {cap_block!r}")
            passed = False
        else:
            ok(
                f"cap_exceeded block ok: "
                f"used={cap_block['used_encoded_bytes']} "
                f"new={cap_block['new_unencoded_bytes']}"
            )

        # 4) Raise cap to 100 MB. Raising never shrinks, so no PTB.
        if set_cap(RAISED_CAP, "raise to 100 MB") is None:
            return False
        ok(f"cap raised to {RAISED_CAP} bytes")

        # 5) Re-upload the previously-rejected payload under a new key.
        recover_key = "cap-recover.bin"
        status, _, body = request(
            "PUT",
            f"{base}/api/v1/buckets/{bucket}/blobs/{recover_key}",
            body=rejected_payload, headers=put_hdrs,
        )
        if status != 201:
            fail(f"re-upload after cap raise returned {status}: {body!r}")
            passed = False
        else:
            created_keys.append(recover_key)
            ok("previously-rejected payload accepted after cap raise")

        # 6) Two additional fresh random blobs of the same size, all
        #    expected 201.
        extra_ok = True
        for j in range(2):
            key = f"cap-extra-{j}.bin"
            payload = os.urandom(CAP_BLOB_SIZE)
            status, _, body = request(
                "PUT",
                f"{base}/api/v1/buckets/{bucket}/blobs/{key}",
                body=payload, headers=put_hdrs,
            )
            if status != 201:
                fail(f"post-raise extra upload {j} returned {status}: {body!r}")
                passed = False
                extra_ok = False
                break
            created_keys.append(key)
        if extra_ok:
            ok("two additional uploads under raised cap also accepted")
    finally:
        # 7) Cleanup: drain blobs, delete bucket, restore cap.
        for key in created_keys:
            request(
                "DELETE",
                f"{base}/api/v1/buckets/{bucket}/blobs/{key}",
                headers=auth,
            )
        request("DELETE", f"{base}/api/v1/buckets/{bucket}", headers=auth)
        # Restore the default cap. This is always a raise so it just
        # updates the DB row; no on-chain action.
        status, _, body = request(
            "PUT", cap_url,
            body={"max_unencoded_bytes": RESTORE_CAP},
            headers=json_hdrs,
        )
        if status != 200:
            # Don't flip the test fail bit on a cleanup-only failure —
            # but warn loudly so the next run sees it.
            info(
                f"WARNING: failed to restore cap to {RESTORE_CAP}: "
                f"status={status} body={body!r}"
            )

    return passed


def scenario_10(base, auth):
    """Error Cases."""
    heading(10, "Error Cases")
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

    # Bogus /api/v1/... path should 404 cleanly, not fall through to the S3
    # handler (which would choke on the Bearer header and return a confusing 400).
    status, _, body = request(
        "GET",
        f"{base}/api/v1/definitely-not-a-real-route",
        headers=auth,
    )
    if status != 404:
        fail(
            f"GET unmatched /api/v1 route returned {status} (expected 404)"
        )
        passed = False
    else:
        ok("unmatched /api/v1 path -> 404")

    return passed


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


SETUP_LOG_BEARER_RE = re.compile(r"^\s*Bearer Token:\s*(\S+)\s*$")
SETUP_LOG_ACCOUNT_ID_RE = re.compile(r"^\s*account_id:\s*([0-9a-f-]{36})\s*$")


def scrape_bearer_from_setup_log(path):
    """Return the last 'Bearer Token: <val>' from setup.log, or None."""
    try:
        with open(path, "r") as f:
            content = f.read()
    except OSError:
        return None
    last = None
    for line in content.splitlines():
        m = SETUP_LOG_BEARER_RE.match(line)
        if m:
            last = m.group(1)
    return last


def scrape_user_account_id_from_setup_log(path):
    """Return the LAST 'account_id: <uuid>' line from setup.log, or None.

    The setup script prints two such lines — the operator first, then
    the test user. We want the user, hence 'last'.
    """
    try:
        with open(path, "r") as f:
            content = f.read()
    except OSError:
        return None
    last = None
    for line in content.splitlines():
        m = SETUP_LOG_ACCOUNT_ID_RE.match(line)
        if m:
            last = m.group(1)
    return last


def bootstrap_account(base, admin_key):
    """Create a new account via the admin API. Returns (account_id, bearer_token)."""
    admin_auth = {"Authorization": f"Bearer {admin_key}"}
    status, _, body = request(
        "POST", f"{base}/api/v1/accounts", body={}, headers=admin_auth
    )
    if status != 201:
        raise SystemExit(
            f"bootstrap POST /accounts failed: {status} {body!r}"
        )
    data = json_body(body)
    return data["account_id"], data["api_key"]["bearer_token"]


def resolve_auth(args):
    """Pick the primary bearer token and (optionally) an admin-key bootstrap handle.

    Returns (base_url, primary_auth_headers, admin_dict).
    admin_dict = {'admin_key': <str>, 'account_id': <str>} when admin-key
    bootstrap is in effect; None otherwise.
    """
    base = args.url.rstrip("/")

    if args.bearer_token:
        return base, {"Authorization": f"Bearer {args.bearer_token}"}, None

    if args.admin_key:
        # Prefer the pre-funded testbed bearer for PUT-heavy scenarios, fall
        # back to a freshly-bootstrapped account if no setup.log is present
        # (reads will pass but PUTs will fail against an unfunded wallet).
        primary_token = scrape_bearer_from_setup_log(args.setup_log)
        if primary_token:
            print(f"  using pre-funded bearer scraped from {args.setup_log}")
        else:
            account_id, primary_token = bootstrap_account(base, args.admin_key)
            print(
                f"  no setup.log bearer found; bootstrapped account "
                f"{account_id} (PUT scenarios may fail against unfunded wallet)"
            )

        # Also resolve an account_id we own for scenario 9. If we can bootstrap
        # a fresh account, use its ID (so api-key mint/revoke doesn't touch the
        # pre-funded testbed account).
        admin_account_id, _ = bootstrap_account(base, args.admin_key)
        user_account_id = scrape_user_account_id_from_setup_log(args.setup_log)
        return (
            base,
            {"Authorization": f"Bearer {primary_token}"},
            {
                "admin_key": args.admin_key,
                "account_id": admin_account_id,
                "user_account_id": user_account_id,
            },
        )

    # Interactive fallback.
    interactive_base = input(f"Oyster URL [{base}]: ").strip() or base
    bearer_token = input("Bearer Token: ").strip()
    if not bearer_token:
        print("Bearer Token is required.")
        sys.exit(1)
    return interactive_base.rstrip("/"), {"Authorization": f"Bearer {bearer_token}"}, None


def parse_args():
    p = argparse.ArgumentParser(
        description="Manual smoke-test for the Oyster local testbed."
    )
    p.add_argument("--url", default="http://127.0.0.1:3000", help="Oyster base URL")
    auth_group = p.add_mutually_exclusive_group()
    auth_group.add_argument(
        "--bearer-token", help="API bearer token (skips admin-key bootstrap)"
    )
    auth_group.add_argument(
        "--admin-key",
        help="App admin key for bootstrapping accounts via POST /api/v1/accounts",
    )
    p.add_argument(
        "--setup-log",
        default="logs/procman/setup.log",
        help="Path to testbed setup.log to scrape the pre-funded bearer token",
    )
    p.add_argument(
        "--non-interactive",
        action="store_true",
        help="Fail instead of prompting on missing auth; also suppresses the "
        "between-scenarios Press-Enter prompt on failure.",
    )
    return p.parse_args()


def main():
    args = parse_args()
    print(f"{BOLD}Oyster Manual Smoke Test{RESET}\n")
    base, auth, admin = resolve_auth(args)
    ctx = {}  # shared state between scenarios
    if admin:
        ctx["admin"] = admin

    scenarios = [
        ("Account & Wallet Info", lambda: scenario_1(base, auth)),
        ("Bucket CRUD", lambda: scenario_2(base, auth, ctx)),
        ("Blob Store + Read", lambda: scenario_3(base, auth, ctx)),
        ("Blob Listing", lambda: scenario_4(base, auth, ctx)),
        ("Content-Addressed Dedup", lambda: scenario_5(base, auth, ctx)),
        ("Blob Metadata Update", lambda: scenario_6(base, auth, ctx)),
        ("Blob Tags CRUD", lambda: scenario_tags(base, auth, ctx)),
        ("Blob Delete", lambda: scenario_7(base, auth, ctx)),
        ("Bucket Delete (drain then delete)", lambda: scenario_8(base, auth, ctx)),
        ("API Key Management", lambda: scenario_9(base, auth, ctx, admin)),
        ("Multi-account Isolation", lambda: scenario_isolation(base, auth, admin)),
        ("Per-account Storage Cap", lambda: scenario_storage_cap(base, auth, ctx)),
        ("Error Cases", lambda: scenario_10(base, auth)),
    ]

    non_interactive = args.non_interactive or args.bearer_token or args.admin_key
    results = []
    for i, (name, fn) in enumerate(scenarios):
        try:
            passed = fn()
        except Exception as e:
            fail(f"exception: {e}")
            passed = False
        results.append((name, passed))

        if not passed and not non_interactive and i < len(scenarios) - 1:
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

# Limitations

Oyster implements the most commonly used S3 operations. This page documents
what's different from a full AWS S3 deployment.

## Supported vs. Not Supported

| Feature | Status | Notes |
|---------|--------|-------|
| CreateBucket | Supported | |
| HeadBucket | Supported | |
| ListBuckets | Supported | Max 1000, no pagination |
| DeleteBucket | Supported | Cascades — deletes all objects (unlike AWS S3) |
| PutObject | Supported | Single-part only |
| GetObject | Supported | |
| HeadObject | Supported | |
| DeleteObject | Supported | |
| ListObjectsV2 | Supported | Prefix, delimiter, pagination |
| Multipart Upload | Not supported | Use single PutObject (max 1 GB) |
| CopyObject | Not supported | Download and re-upload instead |
| DeleteObjects (batch) | Not supported | Delete one at a time |
| Object Versioning | Not supported | Overwrite replaces the object |
| Bucket Policies | Not supported | |
| ACLs | Not supported | |
| CORS | Not supported | |
| Server-Side Encryption | Not supported | Data is stored unencrypted |
| Object Tagging | Not supported | |
| Custom Metadata Headers | Not supported | Only Content-Type is stored |
| Website Hosting | Not supported | |
| S3 Select | Not supported | |
| Storage Classes | Not supported | All objects are `STANDARD` |
| Transfer Acceleration | Not supported | |
| Inventory / Analytics | Not supported | |
| Object Lock / Legal Hold | Not supported | |
| Lifecycle Rules | Not supported | See automatic expiration below |

## Behavioral Differences

### DeleteBucket Cascades

In AWS S3, you must empty a bucket before deleting it. In Oyster,
`DeleteBucket` automatically deletes all objects inside the bucket.
Underlying blob data is cleaned up when no other references exist.

### Object Expiration

All objects expire after **30 days** by default. Oyster runs a background
extension service that automatically renews objects before they expire, so
in practice objects persist indefinitely as long as the service is running.

There is no way to set a custom expiration per object.

### Bucket Naming

Oyster's bucket naming rules are slightly **stricter** than AWS S3:

| Rule | AWS S3 | Oyster |
|------|--------|--------|
| Dots (`.`) in names | Allowed | Not allowed |
| Underscores (`_`) in names | Allowed | Not allowed |
| Consecutive hyphens (`--`) | Allowed | Not allowed |
| Reserved names | None | `health`, `ready`, `metrics`, `api` |

### ListBuckets Limit

`ListBuckets` returns a maximum of 1000 buckets with no pagination support.

### Path-Style URLs Only

Oyster only supports **path-style** S3 URLs:

```
http://endpoint/bucket-name/key
```

Virtual-hosted-style URLs (`bucket-name.endpoint/key`) are **not**
supported. Always set `force_path_style: true` in your SDK configuration.

### No Region Semantics

Oyster ignores the region in S3 requests. All data is stored in the same
location. You still need to specify a region for SigV4 signing to work —
use any valid region string (e.g., `us-east-1`).

### ETag Format

ETags are always the MD5 digest of the object content, even for large
objects. There is no multipart ETag format (since multipart upload is not
supported).

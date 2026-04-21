DELETE FROM blobs;
ALTER TABLE accounts ADD COLUMN storage_pool_object_id TEXT;
ALTER TABLE blobs ADD COLUMN pooled_blob_object_id TEXT;

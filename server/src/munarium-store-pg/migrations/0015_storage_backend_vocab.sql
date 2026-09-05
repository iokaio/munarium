-- The storage_backend vocabulary widened when the object_store adapter
-- (mmesh-store-objects) replaced the hand-rolled Azure crate and added the
-- S3 / GCS / local-filesystem backends. The column stays free TEXT; this
-- comment is the one place the vocabulary is recorded in the schema.
-- (0006 could not be edited in place: sqlx checksums applied migrations.)
COMMENT ON COLUMN sources.storage_backend IS 'az | pg | mem | s3 | gcs | file';

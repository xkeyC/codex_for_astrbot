-- Fork-reserved version (9000+): keeps upstream free to add 0003.. without
-- sqlx checksum conflicts; the runtime migrator ignores unknown versions and
-- applies missing lower versions out of order.
-- Fork addition: per-chat memory scopes. Rows default to the global partition,
-- so databases without scoped threads behave exactly as before.
CREATE TABLE thread_memory_scope (
    thread_id TEXT PRIMARY KEY,
    scope_key TEXT,
    may_write_global INTEGER NOT NULL DEFAULT 1
);

ALTER TABLE stage1_outputs ADD COLUMN memory_partition TEXT NOT NULL DEFAULT 'global';

CREATE INDEX idx_stage1_outputs_memory_partition ON stage1_outputs(memory_partition);

-- Query/index optimization for admin audit pages and chain tx lookup.

BEGIN;

-- Audit request/behavior lookup
CREATE INDEX IF NOT EXISTS idx_audit_behavior_events_related_tx_hash_time
    ON audit_behavior_events(related_tx_hash, occurred_at DESC)
    WHERE related_tx_hash IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_audit_behavior_events_room_time
    ON audit_behavior_events(room_id, occurred_at DESC);

-- Hand replay / pagination (hand_id,event_seq) already exists; add created_at sort helper.
CREATE INDEX IF NOT EXISTS idx_hand_events_hand_created_at
    ON hand_events(hand_id, created_at DESC);

-- Chain tx and exception credits admin queries
CREATE INDEX IF NOT EXISTS idx_tx_verifications_status_verified_at
    ON tx_verifications(verification_status, verified_at DESC);

CREATE INDEX IF NOT EXISTS idx_exception_credits_room_updated_at
    ON exception_credits(room_id, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_exception_credits_tx_hash
    ON exception_credits(tx_hash)
    WHERE tx_hash IS NOT NULL;

-- Ledger pagination/read model
CREATE INDEX IF NOT EXISTS idx_ledger_entries_room_created_at
    ON ledger_entries(room_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_ledger_entries_hand_created_at
    ON ledger_entries(hand_id, created_at DESC)
    WHERE hand_id IS NOT NULL;

COMMIT;

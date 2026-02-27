BEGIN;

-- Required by PostgresChainTxRepository::upsert_exception_credit
-- which uses: ON CONFLICT (tx_hash) DO UPDATE
CREATE UNIQUE INDEX IF NOT EXISTS uq_exception_credits_tx_hash
    ON exception_credits(tx_hash);

COMMIT;

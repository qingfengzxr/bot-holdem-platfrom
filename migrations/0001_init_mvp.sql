-- MVP initial schema (draft)
-- Focus: clear boundaries for state, audit, ledger, and key custody metadata.

BEGIN;

-- ===== Identity & Session =====

CREATE TABLE IF NOT EXISTS agents (
    agent_id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS agent_sessions (
    session_id UUID PRIMARY KEY,
    agent_id UUID NOT NULL REFERENCES agents(agent_id),
    auth_method TEXT NOT NULL,
    session_status TEXT NOT NULL DEFAULT 'active',
    issued_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    client_fingerprint TEXT
);

CREATE INDEX IF NOT EXISTS idx_agent_sessions_agent_id
    ON agent_sessions(agent_id);

CREATE TABLE IF NOT EXISTS seat_session_keys (
    seat_session_key_id UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES agent_sessions(session_id),
    agent_id UUID NOT NULL REFERENCES agents(agent_id),
    room_id UUID NOT NULL,
    seat_id SMALLINT NOT NULL,
    seat_address TEXT NOT NULL,
    card_encrypt_pubkey TEXT NOT NULL,
    request_verify_pubkey TEXT NOT NULL,
    key_algo TEXT NOT NULL,
    proof_signature TEXT NOT NULL,
    status TEXT NOT NULL,
    bound_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_seat_session_keys_active_seat
    ON seat_session_keys(room_id, seat_id)
    WHERE status = 'active';

CREATE UNIQUE INDEX IF NOT EXISTS uq_seat_session_keys_session_pubkey
    ON seat_session_keys(session_id, room_id, seat_id, request_verify_pubkey);

-- Optional replay nonce persistence fallback (Redis recommended in production)
CREATE TABLE IF NOT EXISTS replay_nonces (
    replay_nonce_id BIGSERIAL PRIMARY KEY,
    agent_id UUID NOT NULL REFERENCES agents(agent_id),
    request_id UUID NOT NULL,
    request_nonce TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_replay_nonces_request_id
    ON replay_nonces(request_id);

CREATE UNIQUE INDEX IF NOT EXISTS uq_replay_nonces_agent_nonce
    ON replay_nonces(agent_id, request_nonce);

-- ===== Room & Seat =====

CREATE TABLE IF NOT EXISTS rooms (
    room_id UUID PRIMARY KEY,
    room_status TEXT NOT NULL,
    dealer_address TEXT NOT NULL,
    small_blind NUMERIC(39,0) NOT NULL,
    big_blind NUMERIC(39,0) NOT NULL,
    min_buy_in NUMERIC(39,0) NOT NULL,
    max_players SMALLINT NOT NULL,
    step_timeout_ms BIGINT NOT NULL,
    rake_bps INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- MVP temporary scheme: encrypted room private keys stored in DB.
CREATE TABLE IF NOT EXISTS room_signing_keys (
    room_signing_key_id UUID PRIMARY KEY,
    room_id UUID NOT NULL REFERENCES rooms(room_id),
    address TEXT NOT NULL,
    encrypted_private_key BYTEA NOT NULL,
    cipher_alg TEXT NOT NULL,
    key_version INTEGER NOT NULL,
    kek_id TEXT NOT NULL,
    nonce BYTEA NOT NULL,
    aad BYTEA NOT NULL,
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    rotated_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_room_signing_keys_room_version
    ON room_signing_keys(room_id, key_version);

CREATE UNIQUE INDEX IF NOT EXISTS uq_room_signing_keys_active_address
    ON room_signing_keys(address)
    WHERE status = 'active';

CREATE TABLE IF NOT EXISTS room_seats (
    room_id UUID NOT NULL REFERENCES rooms(room_id),
    seat_id SMALLINT NOT NULL,
    agent_id UUID REFERENCES agents(agent_id),
    seat_status TEXT NOT NULL,
    bound_address TEXT,
    current_stack_virtual NUMERIC(39,0) NOT NULL DEFAULT 0,
    pending_credit NUMERIC(39,0) NOT NULL DEFAULT 0,
    withdrawable_credit NUMERIC(39,0) NOT NULL DEFAULT 0,
    unmatched_credit NUMERIC(39,0) NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (room_id, seat_id)
);

-- ===== Hand State Snapshots =====

CREATE TABLE IF NOT EXISTS hands (
    hand_id UUID PRIMARY KEY,
    room_id UUID NOT NULL REFERENCES rooms(room_id),
    hand_no BIGINT NOT NULL,
    button_seat_id SMALLINT NOT NULL,
    hand_status TEXT NOT NULL,
    street TEXT NOT NULL,
    acting_seat_id SMALLINT,
    action_seq_next INTEGER NOT NULL,
    pot_total NUMERIC(39,0) NOT NULL DEFAULT 0,
    rake_accrued NUMERIC(39,0) NOT NULL DEFAULT 0,
    incoming_total NUMERIC(39,0) NOT NULL DEFAULT 0,
    settlement_pending_total NUMERIC(39,0) NOT NULL DEFAULT 0,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_hands_room_hand_no
    ON hands(room_id, hand_no);

CREATE TABLE IF NOT EXISTS hand_seats (
    hand_id UUID NOT NULL REFERENCES hands(hand_id),
    seat_id SMALLINT NOT NULL,
    agent_id UUID REFERENCES agents(agent_id),
    seat_address TEXT NOT NULL,
    status_in_hand TEXT NOT NULL,
    committed_this_hand NUMERIC(39,0) NOT NULL DEFAULT 0,
    stack_virtual_start NUMERIC(39,0) NOT NULL DEFAULT 0,
    stack_virtual_end NUMERIC(39,0),
    hole_cards_cipher_ref TEXT,
    showdown_cards_masked JSONB,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (hand_id, seat_id)
);

-- ===== Audit & Domain Events =====

CREATE TABLE IF NOT EXISTS audit_action_attempts (
    attempt_id UUID PRIMARY KEY,
    request_id UUID NOT NULL,
    request_nonce TEXT NOT NULL,
    agent_id UUID REFERENCES agents(agent_id),
    session_id UUID REFERENCES agent_sessions(session_id),
    room_id UUID,
    hand_id UUID REFERENCES hands(hand_id),
    seat_id SMALLINT,
    action_seq INTEGER,
    method TEXT NOT NULL,
    action_type TEXT,
    request_payload_json JSONB NOT NULL,
    canonical_params_hash TEXT,
    signature_pubkey_id TEXT,
    signature_verify_result TEXT NOT NULL,
    replay_check_result TEXT NOT NULL,
    idempotency_check_result TEXT NOT NULL,
    validation_result TEXT NOT NULL,
    router_result TEXT NOT NULL,
    business_result_code TEXT,
    error_detail TEXT,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    trace_id UUID NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_audit_action_attempts_request_id
    ON audit_action_attempts(request_id);

CREATE INDEX IF NOT EXISTS idx_audit_action_attempts_room_hand_action
    ON audit_action_attempts(room_id, hand_id, action_seq, received_at DESC);

CREATE TABLE IF NOT EXISTS audit_behavior_events (
    behavior_event_id UUID PRIMARY KEY,
    event_kind TEXT NOT NULL,
    event_source TEXT NOT NULL,
    room_id UUID,
    hand_id UUID REFERENCES hands(hand_id),
    seat_id SMALLINT,
    action_seq INTEGER,
    related_attempt_id UUID REFERENCES audit_action_attempts(attempt_id),
    related_tx_hash TEXT,
    severity TEXT NOT NULL,
    payload_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    trace_id UUID NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_audit_behavior_events_room_hand_time
    ON audit_behavior_events(room_id, hand_id, occurred_at DESC);

CREATE TABLE IF NOT EXISTS hand_events (
    hand_event_id UUID PRIMARY KEY,
    room_id UUID NOT NULL,
    hand_id UUID NOT NULL REFERENCES hands(hand_id),
    event_seq INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    event_payload_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    trace_id UUID NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_hand_events_hand_seq
    ON hand_events(hand_id, event_seq);

CREATE INDEX IF NOT EXISTS idx_hand_events_room_hand_seq
    ON hand_events(room_id, hand_id, event_seq);

CREATE TABLE IF NOT EXISTS outbox_events (
    outbox_event_id UUID PRIMARY KEY,
    topic TEXT NOT NULL,
    partition_key TEXT NOT NULL,
    payload_json JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    delivered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_outbox_events_pending
    ON outbox_events(status, available_at, created_at);

-- ===== Chain / Settlement =====

CREATE TABLE IF NOT EXISTS tx_verifications (
    tx_verification_id UUID PRIMARY KEY,
    tx_hash TEXT NOT NULL,
    chain_id TEXT NOT NULL,
    from_address TEXT,
    to_address TEXT,
    value NUMERIC(39,0),
    tx_status TEXT NOT NULL,
    confirmations BIGINT NOT NULL DEFAULT 0,
    verification_status TEXT NOT NULL,
    failure_reason TEXT,
    verified_at TIMESTAMPTZ,
    raw_tx_json JSONB
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_tx_verifications_tx_hash
    ON tx_verifications(tx_hash);

CREATE TABLE IF NOT EXISTS tx_bindings (
    tx_binding_id UUID PRIMARY KEY,
    room_id UUID NOT NULL,
    hand_id UUID NOT NULL REFERENCES hands(hand_id),
    action_seq INTEGER NOT NULL,
    seat_id SMALLINT NOT NULL,
    expected_amount NUMERIC(39,0) NOT NULL,
    tx_hash TEXT NOT NULL,
    binding_status TEXT NOT NULL,
    bound_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_tx_bindings_hand_action_seq
    ON tx_bindings(hand_id, action_seq);

CREATE UNIQUE INDEX IF NOT EXISTS uq_tx_bindings_tx_hash
    ON tx_bindings(tx_hash);

CREATE TABLE IF NOT EXISTS exception_credits (
    exception_credit_id UUID PRIMARY KEY,
    room_id UUID NOT NULL REFERENCES rooms(room_id),
    seat_id SMALLINT NOT NULL,
    hand_id UUID REFERENCES hands(hand_id),
    tx_hash TEXT,
    credit_kind TEXT NOT NULL,
    amount NUMERIC(39,0) NOT NULL,
    status TEXT NOT NULL,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS settlement_plans (
    settlement_plan_id UUID PRIMARY KEY,
    room_id UUID NOT NULL REFERENCES rooms(room_id),
    hand_id UUID NOT NULL REFERENCES hands(hand_id),
    status TEXT NOT NULL,
    rake_amount NUMERIC(39,0) NOT NULL,
    payout_total NUMERIC(39,0) NOT NULL,
    payload_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finalized_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS settlement_records (
    settlement_record_id UUID PRIMARY KEY,
    settlement_plan_id UUID NOT NULL REFERENCES settlement_plans(settlement_plan_id),
    room_id UUID NOT NULL REFERENCES rooms(room_id),
    hand_id UUID NOT NULL REFERENCES hands(hand_id),
    tx_hash TEXT,
    settlement_status TEXT NOT NULL,
    retry_count INTEGER NOT NULL DEFAULT 0,
    error_detail TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ===== Ledger =====

CREATE TABLE IF NOT EXISTS ledger_entries (
    ledger_entry_id UUID PRIMARY KEY,
    entry_type TEXT NOT NULL,
    room_id UUID,
    hand_id UUID REFERENCES hands(hand_id),
    seat_id SMALLINT,
    agent_id UUID REFERENCES agents(agent_id),
    asset_type TEXT NOT NULL,
    amount NUMERIC(39,0) NOT NULL,
    direction TEXT NOT NULL,
    account_scope TEXT NOT NULL,
    related_tx_hash TEXT,
    related_event_id UUID,
    related_attempt_id UUID REFERENCES audit_action_attempts(attempt_id),
    remark TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    trace_id UUID NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_ledger_entries_room_hand_time
    ON ledger_entries(room_id, hand_id, created_at DESC);

COMMIT;

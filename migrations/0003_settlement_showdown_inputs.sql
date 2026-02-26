BEGIN;

CREATE TABLE IF NOT EXISTS settlement_showdown_inputs (
    settlement_showdown_input_id UUID PRIMARY KEY,
    room_id UUID NOT NULL REFERENCES rooms(room_id),
    hand_id UUID NOT NULL REFERENCES hands(hand_id),
    board_cards TEXT NOT NULL,
    revealed_hole_cards_json JSONB NOT NULL,
    source TEXT NOT NULL DEFAULT 'admin_ops_http',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    trace_id UUID NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_settlement_showdown_inputs_hand
    ON settlement_showdown_inputs(hand_id);

CREATE INDEX IF NOT EXISTS idx_settlement_showdown_inputs_room_updated
    ON settlement_showdown_inputs(room_id, updated_at DESC);

COMMIT;

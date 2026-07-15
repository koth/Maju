-- pairing_codes: pairing_code (PK) <-> pc_device_id <-> created_at <-> expires_at <-> used
CREATE TABLE IF NOT EXISTS pairing_codes (
    pairing_code TEXT PRIMARY KEY,
    pc_device_id TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    used         INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (pc_device_id) REFERENCES devices(device_id)
);

CREATE INDEX IF NOT EXISTS idx_pairing_codes_pc_device_id ON pairing_codes(pc_device_id);
CREATE INDEX IF NOT EXISTS idx_pairing_codes_expires_at ON pairing_codes(expires_at);

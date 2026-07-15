-- pairings: pairing_id <-> pc_device_id <-> phone_device_id <-> created_at <-> bound <-> account_id (nullable)
CREATE TABLE IF NOT EXISTS pairings (
    pairing_id      TEXT PRIMARY KEY,
    pc_device_id    TEXT NOT NULL,
    phone_device_id TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    bound           INTEGER NOT NULL DEFAULT 0,
    account_id      TEXT,
    FOREIGN KEY (pc_device_id) REFERENCES devices(device_id),
    FOREIGN KEY (phone_device_id) REFERENCES devices(device_id)
);

CREATE INDEX IF NOT EXISTS idx_pairings_pc_device_id ON pairings(pc_device_id);
CREATE INDEX IF NOT EXISTS idx_pairings_phone_device_id ON pairings(phone_device_id);

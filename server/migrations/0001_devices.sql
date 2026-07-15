-- devices: device_id (PK) <-> public_key (base64) <-> registered_at
CREATE TABLE IF NOT EXISTS devices (
    device_id     TEXT PRIMARY KEY,
    public_key    TEXT NOT NULL,
    registered_at INTEGER NOT NULL
);

-- accounts: account_id <-> credentials <-> auth_token
CREATE TABLE IF NOT EXISTS accounts (
    account_id  TEXT PRIMARY KEY,
    credentials TEXT NOT NULL,
    auth_token   TEXT NOT NULL UNIQUE
);

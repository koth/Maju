-- subscriptions: account_id <-> plan <-> active <-> expires_at
CREATE TABLE IF NOT EXISTS subscriptions (
    account_id TEXT PRIMARY KEY,
    plan       TEXT NOT NULL,
    active     INTEGER NOT NULL DEFAULT 0,
    expires_at INTEGER NOT NULL,
    FOREIGN KEY (account_id) REFERENCES accounts(account_id)
);

CREATE TABLE repositories (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    full_name TEXT NOT NULL COLLATE NOCASE CHECK (length(trim(full_name)) > 0),
    description TEXT,
    language TEXT,
    html_url TEXT NOT NULL,
    private BOOLEAN NOT NULL,
    starred BOOLEAN NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    tags TEXT NOT NULL DEFAULT '[]'
);

CREATE INDEX repositories_by_name ON repositories (full_name, starred DESC, id DESC);

CREATE TABLE repository_readmes (
    repository_id INTEGER PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    source_url TEXT NOT NULL
);

CREATE TRIGGER expire_repository_readme
    AFTER UPDATE OF full_name, private ON repositories
    WHEN old.full_name <> new.full_name OR old.private <> new.private
BEGIN
    DELETE FROM repository_readmes WHERE repository_id = old.id;
END;

CREATE TABLE library_sync_status (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    running BOOLEAN NOT NULL DEFAULT FALSE,
    last_attempt_at TEXT,
    last_successful_at TEXT,
    last_result TEXT,
    last_error TEXT
);

INSERT INTO library_sync_status (id) VALUES (1);

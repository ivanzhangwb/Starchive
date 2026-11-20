CREATE TABLE IF NOT EXISTS agent_sessions (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    revision INTEGER NOT NULL DEFAULT 0,
    data TEXT NOT NULL
);

CREATE TABLE candidate_readmes (
    session_id INTEGER NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
    repository_id INTEGER NOT NULL,
    content TEXT NOT NULL,
    identity_verified BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (session_id, repository_id)
);

INSERT INTO candidate_readmes (session_id, repository_id, content)
SELECT session.id, readme.key, readme.value
FROM agent_sessions AS session, json_each(session.data, '$.external_readmes') AS readme
WHERE readme.type = 'text';

UPDATE agent_sessions SET data = json_remove(data, '$.external_readmes')
WHERE json_type(data, '$.external_readmes') IS NOT NULL;

ALTER TABLE repository_readmes ADD COLUMN identity_verified BOOLEAN NOT NULL DEFAULT FALSE;

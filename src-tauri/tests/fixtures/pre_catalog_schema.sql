-- Owned pre-catalog schema fixture, reduced to historical provider/model/chat tables.
-- Deliberately contains no catalog tables, triggers or migration calls.
CREATE TABLE providers (
 id TEXT PRIMARY KEY, name TEXT NOT NULL, dialect TEXT NOT NULL DEFAULT 'openai',
 base_url TEXT NOT NULL, api_key_enc TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
 priority INTEGER NOT NULL DEFAULT 100, rpm_limit INTEGER NOT NULL DEFAULT 0,
 intelligence INTEGER NOT NULL DEFAULT 50, note TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE models (
 id TEXT PRIMARY KEY, provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
 alias TEXT NOT NULL, upstream TEXT NOT NULL, context_window INTEGER NOT NULL DEFAULT 8192,
 supports_tools INTEGER NOT NULL DEFAULT 0, supports_vision INTEGER NOT NULL DEFAULT 0,
 supports_audio INTEGER NOT NULL DEFAULT 0, supports_video INTEGER NOT NULL DEFAULT 0,
 supports_stream INTEGER NOT NULL DEFAULT 1, model_type TEXT NOT NULL DEFAULT 'chat',
 upstream_path TEXT, enabled INTEGER NOT NULL DEFAULT 1, price_json TEXT, overrides_json TEXT
);
CREATE TABLE sessions (
 id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE session_messages (
 id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 role TEXT NOT NULL, content TEXT NOT NULL, created_at TEXT NOT NULL
);
INSERT INTO providers(id,name,base_url,api_key_enc,created_at,updated_at)
 VALUES('owned','Owned legacy fixture','http://127.0.0.1:9/v1','','2026-10-17T00:00:00Z','2026-10-17T00:00:00Z');
INSERT INTO models(id,provider_id,alias,upstream,enabled,price_json) VALUES
 ('owned:custom-alias','owned','custom-alias','vendor/exact:free',0,'{"prompt":4,"completion":5,"currency":"usd"}'),
 ('owned:manual-zero','owned','manual-zero','vendor/user-zero',1,'{"prompt":0,"completion":0,"currency":"usd"}');
INSERT INTO sessions(id,title,created_at,updated_at)
 VALUES('legacy-history','Owned legacy chat','2026-10-17T00:00:00Z','2026-10-17T00:00:00Z');
INSERT INTO session_messages(session_id,role,content,created_at)
 VALUES('legacy-history','assistant','owned pre-catalog history','2026-10-17T00:00:00Z');

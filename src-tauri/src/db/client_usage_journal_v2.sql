-- CLIENT v2 isolated-development draft. No total-count/capacity admission gate.
-- Each object marker is followed by exactly one CREATE statement. The root
-- integrator must compare canonical sqlite_master SQL, not only object names.
-- Initialize meta in the same creation transaction using externally bound UUID
-- and activation UTC seconds; initialization SQL is in initialize-meta.sql.
-- Tombstones survive explicit business-row deletion. All counters are SQLite
-- INTEGERs; overflow aborts a real write rather than wrapping or becoming REAL.
-- RAISE messages are fixed literals compatible with SQLite 3.46.0.
-- object: client_usage_journal_meta_v2
CREATE TABLE client_usage_journal_meta_v2 (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    contract_version INTEGER NOT NULL CHECK(typeof(contract_version)='integer' AND contract_version=2),
    db_generation TEXT NOT NULL CHECK(typeof(db_generation)='text' AND length(db_generation)=36),
    last_entry_generation INTEGER NOT NULL CHECK(typeof(last_entry_generation)='integer' AND last_entry_generation>=0),
    activated_at INTEGER NOT NULL CHECK(typeof(activated_at)='integer' AND activated_at>=0)
);
-- object: usage_attempt_journal_v2
CREATE TABLE usage_attempt_journal_v2 (
    attempt_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(attempt_id)='text' AND length(attempt_id)=36),
    scope_id TEXT NOT NULL CHECK(typeof(scope_id)='text' AND length(scope_id)=36),
    kind TEXT NOT NULL CHECK(kind IN ('buffered','stream','auxiliary','probe','passthrough','compaction','summary','agent_intent','native_exact')),
    created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at>=0),
    known_not_sent INTEGER NOT NULL DEFAULT 0 CHECK(typeof(known_not_sent)='integer' AND known_not_sent IN (0,1)),
    agent_gate_reason TEXT CHECK(agent_gate_reason IS NULL OR agent_gate_reason IN ('quota','concurrency')),
    reviewed_at INTEGER CHECK(reviewed_at IS NULL OR (typeof(reviewed_at)='integer' AND reviewed_at>=0)),
    audit_request_id INTEGER CHECK(audit_request_id IS NULL OR (typeof(audit_request_id)='integer' AND audit_request_id>0)),
    background_scope_id TEXT CHECK(background_scope_id IS NULL OR typeof(background_scope_id)='text'),
    CHECK(audit_request_id IS NULL OR background_scope_id IS NULL),
    CHECK(reviewed_at IS NULL OR (kind='agent_intent' AND known_not_sent=1)),
    CHECK(agent_gate_reason IS NULL OR (kind='agent_intent' AND known_not_sent=1))
);
-- object: usage_attempt_versions_v2
CREATE TABLE usage_attempt_versions_v2 (
    attempt_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(attempt_id)='text' AND length(attempt_id)=36),
    entry_generation INTEGER NOT NULL UNIQUE CHECK(typeof(entry_generation)='integer' AND entry_generation>0),
    revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
    present INTEGER NOT NULL CHECK(typeof(present)='integer' AND present IN (0,1))
);
-- object: idx_usage_attempt_journal_v2_unlinked
CREATE INDEX idx_usage_attempt_journal_v2_unlinked ON usage_attempt_journal_v2(audit_request_id,created_at);
-- object: idx_usage_agent_intent_state_v2
CREATE INDEX idx_usage_agent_intent_state_v2 ON usage_attempt_journal_v2(kind,audit_request_id,background_scope_id,known_not_sent,reviewed_at);
-- object: client_usage_meta_v2_insert_guard
CREATE TRIGGER client_usage_meta_v2_insert_guard BEFORE INSERT ON client_usage_journal_meta_v2 BEGIN
    SELECT CASE WHEN EXISTS(SELECT 1 FROM client_usage_journal_meta_v2)
        THEN RAISE(ABORT,'client_usage_meta_exists') END;
    SELECT CASE WHEN NEW.singleton IS NOT 1 OR NEW.contract_version IS NOT 2
        OR typeof(NEW.db_generation)<>'text' OR length(NEW.db_generation)<>36
        OR NEW.last_entry_generation IS NOT 0 OR typeof(NEW.activated_at)<>'integer' OR NEW.activated_at<0
        THEN RAISE(ABORT,'client_usage_meta_initialization_invalid') END;
END;
-- object: client_usage_meta_v2_update_guard
CREATE TRIGGER client_usage_meta_v2_update_guard BEFORE UPDATE ON client_usage_journal_meta_v2 BEGIN
    SELECT CASE WHEN NEW.singleton IS NOT OLD.singleton OR NEW.contract_version IS NOT OLD.contract_version
        OR NEW.db_generation IS NOT OLD.db_generation OR NEW.activated_at IS NOT OLD.activated_at
        OR typeof(OLD.last_entry_generation)<>'integer' OR OLD.last_entry_generation<0
        OR OLD.last_entry_generation>=9223372036854775807
        OR typeof(NEW.last_entry_generation)<>'integer' OR NEW.last_entry_generation<>OLD.last_entry_generation+1
        THEN RAISE(ABORT,'client_usage_meta_update_invalid') END;
END;
-- object: client_usage_meta_v2_delete_guard
CREATE TRIGGER client_usage_meta_v2_delete_guard BEFORE DELETE ON client_usage_journal_meta_v2 BEGIN
    SELECT RAISE(ABORT,'client_usage_meta_delete_denied');
END;
-- object: usage_attempt_versions_v2_insert_guard
CREATE TRIGGER usage_attempt_versions_v2_insert_guard BEFORE INSERT ON usage_attempt_versions_v2 BEGIN
    SELECT CASE WHEN EXISTS(SELECT 1 FROM usage_attempt_versions_v2 WHERE attempt_id=NEW.attempt_id OR entry_generation=NEW.entry_generation)
        THEN RAISE(ABORT,'client_usage_version_duplicate') END;
    SELECT CASE WHEN (SELECT COUNT(*) FROM client_usage_journal_meta_v2)<>1
        OR NOT EXISTS(SELECT 1 FROM client_usage_journal_meta_v2 WHERE singleton=1 AND contract_version=2
            AND typeof(db_generation)='text' AND length(db_generation)=36 AND typeof(activated_at)='integer' AND activated_at>=0
            AND typeof(last_entry_generation)='integer' AND last_entry_generation>0 AND last_entry_generation=NEW.entry_generation)
        OR NOT EXISTS(SELECT 1 FROM usage_attempt_journal_v2 WHERE attempt_id=NEW.attempt_id)
        OR NEW.revision IS NOT 1 OR NEW.present IS NOT 1
        THEN RAISE(ABORT,'client_usage_version_insert_invalid') END;
END;
-- object: usage_attempt_versions_v2_update_guard
CREATE TRIGGER usage_attempt_versions_v2_update_guard BEFORE UPDATE ON usage_attempt_versions_v2 BEGIN
    SELECT CASE WHEN NEW.attempt_id IS NOT OLD.attempt_id OR typeof(OLD.revision)<>'integer' OR OLD.revision<1
        OR OLD.revision>=9223372036854775807 OR typeof(NEW.revision)<>'integer' OR NEW.revision<>OLD.revision+1
        OR typeof(NEW.entry_generation)<>'integer' OR NEW.entry_generation<1
        OR typeof(NEW.present)<>'integer' OR NEW.present NOT IN (0,1)
        OR EXISTS(SELECT 1 FROM usage_attempt_versions_v2 WHERE attempt_id<>OLD.attempt_id AND entry_generation=NEW.entry_generation)
        THEN RAISE(ABORT,'client_usage_version_update_invalid') END;
    SELECT CASE WHEN (SELECT COUNT(*) FROM client_usage_journal_meta_v2)<>1
        OR NOT EXISTS(SELECT 1 FROM client_usage_journal_meta_v2 WHERE singleton=1 AND contract_version=2
            AND typeof(db_generation)='text' AND length(db_generation)=36 AND typeof(activated_at)='integer' AND activated_at>=0
            AND typeof(last_entry_generation)='integer' AND last_entry_generation>=NEW.entry_generation)
        OR NOT (
            (OLD.present=1 AND NEW.present=1 AND NEW.entry_generation=OLD.entry_generation
                AND EXISTS(SELECT 1 FROM usage_attempt_journal_v2 WHERE attempt_id=OLD.attempt_id))
            OR (OLD.present=1 AND NEW.present=0 AND NEW.entry_generation=OLD.entry_generation
                AND NOT EXISTS(SELECT 1 FROM usage_attempt_journal_v2 WHERE attempt_id=OLD.attempt_id))
            OR (OLD.present=0 AND NEW.present=1 AND NEW.entry_generation>OLD.entry_generation
                AND NEW.entry_generation=(SELECT last_entry_generation FROM client_usage_journal_meta_v2 WHERE singleton=1)
                AND EXISTS(SELECT 1 FROM usage_attempt_journal_v2 WHERE attempt_id=OLD.attempt_id))
        ) THEN RAISE(ABORT,'client_usage_version_transition_invalid') END;
END;
-- object: usage_attempt_versions_v2_delete_guard
CREATE TRIGGER usage_attempt_versions_v2_delete_guard BEFORE DELETE ON usage_attempt_versions_v2 BEGIN
    SELECT RAISE(ABORT,'client_usage_tombstone_delete_denied');
END;
-- object: usage_attempt_journal_v2_insert_guard
CREATE TRIGGER usage_attempt_journal_v2_insert_guard BEFORE INSERT ON usage_attempt_journal_v2 BEGIN
    SELECT CASE WHEN EXISTS(SELECT 1 FROM usage_attempt_journal_v2 WHERE attempt_id=NEW.attempt_id)
        OR EXISTS(SELECT 1 FROM usage_attempt_versions_v2 WHERE attempt_id=NEW.attempt_id AND present=1)
        THEN RAISE(ABORT,'client_usage_duplicate_live_id') END;
    SELECT CASE WHEN (SELECT COUNT(*) FROM client_usage_journal_meta_v2)<>1
        OR NOT EXISTS(SELECT 1 FROM client_usage_journal_meta_v2 WHERE singleton=1 AND contract_version=2
            AND typeof(db_generation)='text' AND length(db_generation)=36 AND typeof(activated_at)='integer' AND activated_at>=0
            AND typeof(last_entry_generation)='integer' AND last_entry_generation>=0)
        THEN RAISE(ABORT,'client_usage_contract_invalid') END;
    SELECT CASE WHEN (SELECT last_entry_generation FROM client_usage_journal_meta_v2 WHERE singleton=1)>=9223372036854775807
        THEN RAISE(ABORT,'client_usage_generation_overflow') END;
    SELECT CASE WHEN EXISTS(SELECT 1 FROM usage_attempt_versions_v2 WHERE attempt_id=NEW.attempt_id
            AND (present<>0 OR typeof(revision)<>'integer' OR revision<1 OR revision>=9223372036854775807
                OR typeof(entry_generation)<>'integer' OR entry_generation<1
                OR entry_generation>(SELECT last_entry_generation FROM client_usage_journal_meta_v2 WHERE singleton=1)))
        THEN RAISE(ABORT,'client_usage_tombstone_invalid') END;
END;
-- object: usage_attempt_journal_v2_update_guard
CREATE TRIGGER usage_attempt_journal_v2_update_guard BEFORE UPDATE ON usage_attempt_journal_v2 BEGIN
    SELECT CASE WHEN NEW.attempt_id IS NOT OLD.attempt_id THEN RAISE(ABORT,'client_usage_id_update_denied') END;
    SELECT CASE WHEN (SELECT COUNT(*) FROM client_usage_journal_meta_v2)<>1
        OR NOT EXISTS(SELECT 1 FROM client_usage_journal_meta_v2 WHERE singleton=1 AND contract_version=2
            AND typeof(db_generation)='text' AND length(db_generation)=36 AND typeof(activated_at)='integer' AND activated_at>=0
            AND typeof(last_entry_generation)='integer' AND last_entry_generation>=0)
        OR NOT EXISTS(SELECT 1 FROM usage_attempt_versions_v2 WHERE attempt_id=OLD.attempt_id AND present=1
            AND typeof(revision)='integer' AND revision>=1 AND typeof(entry_generation)='integer' AND entry_generation>0
            AND entry_generation<=(SELECT last_entry_generation FROM client_usage_journal_meta_v2 WHERE singleton=1))
        THEN RAISE(ABORT,'client_usage_contract_invalid') END;
    SELECT CASE WHEN (SELECT revision FROM usage_attempt_versions_v2 WHERE attempt_id=OLD.attempt_id)>=9223372036854775807
        THEN RAISE(ABORT,'client_usage_revision_overflow') END;
END;
-- object: usage_attempt_journal_v2_delete_guard
CREATE TRIGGER usage_attempt_journal_v2_delete_guard BEFORE DELETE ON usage_attempt_journal_v2 BEGIN
    SELECT CASE WHEN (SELECT COUNT(*) FROM client_usage_journal_meta_v2)<>1
        OR NOT EXISTS(SELECT 1 FROM client_usage_journal_meta_v2 WHERE singleton=1 AND contract_version=2
            AND typeof(db_generation)='text' AND length(db_generation)=36 AND typeof(activated_at)='integer' AND activated_at>=0
            AND typeof(last_entry_generation)='integer' AND last_entry_generation>=0)
        OR NOT EXISTS(SELECT 1 FROM usage_attempt_versions_v2 WHERE attempt_id=OLD.attempt_id AND present=1
            AND typeof(revision)='integer' AND revision>=1 AND typeof(entry_generation)='integer' AND entry_generation>0
            AND entry_generation<=(SELECT last_entry_generation FROM client_usage_journal_meta_v2 WHERE singleton=1))
        THEN RAISE(ABORT,'client_usage_contract_invalid') END;
    SELECT CASE WHEN (SELECT revision FROM usage_attempt_versions_v2 WHERE attempt_id=OLD.attempt_id)>=9223372036854775807
        THEN RAISE(ABORT,'client_usage_revision_overflow') END;
END;
-- object: usage_attempt_journal_v2_insert_version
CREATE TRIGGER usage_attempt_journal_v2_insert_version AFTER INSERT ON usage_attempt_journal_v2 BEGIN
    UPDATE client_usage_journal_meta_v2 SET last_entry_generation=last_entry_generation+1 WHERE singleton=1;
    SELECT CASE WHEN changes()<>1 THEN RAISE(ABORT,'client_usage_generation_write_failed') END;
    UPDATE usage_attempt_versions_v2 SET entry_generation=(SELECT last_entry_generation FROM client_usage_journal_meta_v2 WHERE singleton=1),revision=revision+1,present=1 WHERE attempt_id=NEW.attempt_id;
    INSERT INTO usage_attempt_versions_v2(attempt_id,entry_generation,revision,present)
        SELECT NEW.attempt_id,last_entry_generation,1,1 FROM client_usage_journal_meta_v2
        WHERE singleton=1 AND NOT EXISTS(SELECT 1 FROM usage_attempt_versions_v2 WHERE attempt_id=NEW.attempt_id);
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM usage_attempt_versions_v2 WHERE attempt_id=NEW.attempt_id AND present=1
            AND typeof(revision)='integer' AND revision>=1
            AND entry_generation=(SELECT last_entry_generation FROM client_usage_journal_meta_v2 WHERE singleton=1))
        THEN RAISE(ABORT,'client_usage_version_write_failed') END;
END;
-- object: usage_attempt_journal_v2_update_version
CREATE TRIGGER usage_attempt_journal_v2_update_version AFTER UPDATE ON usage_attempt_journal_v2 BEGIN
    UPDATE usage_attempt_versions_v2 SET revision=revision+1 WHERE attempt_id=OLD.attempt_id AND present=1;
    SELECT CASE WHEN changes()<>1 OR NOT EXISTS(SELECT 1 FROM usage_attempt_versions_v2 WHERE attempt_id=OLD.attempt_id AND present=1)
        THEN RAISE(ABORT,'client_usage_version_write_failed') END;
END;
-- object: usage_attempt_journal_v2_delete_version
CREATE TRIGGER usage_attempt_journal_v2_delete_version AFTER DELETE ON usage_attempt_journal_v2 BEGIN
    UPDATE usage_attempt_versions_v2 SET revision=revision+1,present=0 WHERE attempt_id=OLD.attempt_id AND present=1;
    SELECT CASE WHEN changes()<>1 OR NOT EXISTS(SELECT 1 FROM usage_attempt_versions_v2 WHERE attempt_id=OLD.attempt_id AND present=0)
        THEN RAISE(ABORT,'client_usage_version_write_failed') END;
END;

-- Values are the server-held complete snapshot, never caller-supplied versions.
-- BEGIN IMMEDIATE plus full contract inspect belongs to the Rust wrapper.
-- Each statement must affect one top-level row; any miss/error rolls back the
-- entire batch. Re-read versions after UPDATE: RETURNING omits AFTER effects.
-- Parameter order: reviewed_at, attempt_id, scope_id, created_at, gate_reason,
-- entry_generation, revision, db_generation.
UPDATE usage_attempt_journal_v2 SET reviewed_at=?
WHERE attempt_id=? AND scope_id=? AND created_at=? AND agent_gate_reason=?
  AND kind='agent_intent' AND known_not_sent=1 AND reviewed_at IS NULL
  AND audit_request_id IS NULL AND background_scope_id IS NULL
  AND EXISTS(
      SELECT 1 FROM usage_attempt_versions_v2 v
      JOIN client_usage_journal_meta_v2 m ON m.singleton=1
      WHERE v.attempt_id=usage_attempt_journal_v2.attempt_id
        AND v.entry_generation=? AND v.revision=? AND v.present=1
        AND m.db_generation=? AND m.contract_version=2
  );

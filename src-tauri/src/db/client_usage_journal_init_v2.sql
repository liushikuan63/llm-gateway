-- Bind a newly generated canonical UUID and activation UTC seconds externally.
-- This must run only while atomically creating a completely absent contract.
-- Existing meta is rejected by its BEFORE INSERT guard, even under OR REPLACE.
INSERT INTO client_usage_journal_meta_v2
    (singleton,contract_version,db_generation,last_entry_generation,activated_at)
VALUES(1,2,?,0,?);

-- What the most recent failed extension attempt looked like, so the
-- worker can publish "stuck for a non-funding reason" gauges from one
-- aggregate query instead of re-checking every wallet each cycle.
--
-- extend_last_failure_reason: the `reason` label of the failed attempt
--   (insufficient_funds / ptb_build / on_chain_abort / sign_or_submit /
--   invalid_object_id).
-- extend_last_failure_wallet: the wallet's verified funding state at the
--   time of that failure — 'funded' (WAL and SUI balances covered the
--   extension, so the failure is Oyster's problem), 'unfunded', or
--   'unknown' (balance read failed).
--
-- Both are NULL while extend_failure_count = 0 and are cleared together
-- with it on success, on an expired-pool reset, and on a user-requested
-- retry. Additive and nullable so pre-026 code keeps running during
-- rollout.
ALTER TABLE accounts ADD COLUMN extend_last_failure_reason TEXT;
ALTER TABLE accounts ADD COLUMN extend_last_failure_wallet TEXT;

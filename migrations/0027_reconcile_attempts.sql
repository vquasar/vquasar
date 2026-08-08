-- Make a reconcile that cannot succeed visible (issue #35).
--
-- `reconcile_ensure` retried forever, every tick, at WARN. A VM whose create
-- was interrupted at the wrong moment therefore sat in `Scheduling` for as long
-- as anyone left it — observed on the lab at ~130 attempts over 400 seconds —
-- with `message` NULL and nothing an operator would ever see in the API.
--
-- Two columns are enough to fix both halves of that: count the consecutive
-- failures so the loop can give up and say so, and remember when the last
-- attempt was so it can back off instead of hammering an agent that is failing
-- for a reason retrying will not change.
--
-- Reset to 0 on any success, so this counts *consecutive* failures. A VM that
-- fails, recovers and fails again months later has not exhausted anything.

ALTER TABLE virtual_machines
    ADD COLUMN reconcile_failures INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN last_reconcile_at  TIMESTAMPTZ;

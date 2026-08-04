-- Cross-CPU live migration (design M15): record each host's CPU vendor and
-- curated guest-visible ISA feature flags so the control plane can refuse a
-- migration to a host that lacks a feature the source guest could be using
-- (Cloud Hypervisor cannot mask CPUID). Populated on each heartbeat from the
-- agent's inventory; existing rows fill in on their next heartbeat.
ALTER TABLE hosts ADD COLUMN cpu_vendor   TEXT;
ALTER TABLE hosts ADD COLUMN cpu_features TEXT[] NOT NULL DEFAULT '{}';

-- microVMs (design M15): a template can pin the machine profile for VMs it
-- creates. "standard" is the full device model; "microvm" is the minimal,
-- fast-booting profile (direct-kernel boot, no cloud-init seed, pvpanic,
-- single PCI segment). Existing templates default to standard.
ALTER TABLE templates ADD COLUMN machine_type TEXT NOT NULL DEFAULT 'standard';

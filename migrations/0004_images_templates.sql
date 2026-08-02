-- Base images and VM templates (design M9: disk provisioning + templates).
--
-- An image is a read-only golden base disk on shared storage plus the boot
-- recipe used by VMs created from it. A template is a reusable VM preset
-- (sizing + image + network + cloud-init defaults) that the API instantiates
-- into a full VirtualMachineSpec.

CREATE TABLE images (
    id                 UUID PRIMARY KEY,
    name               TEXT NOT NULL UNIQUE,
    -- read-only golden base disk on shared storage
    source_path        TEXT NOT NULL,
    -- on-disk format of the base image ('raw' | 'qcow2')
    format             TEXT NOT NULL,
    -- boot recipe (JSON BootSpec) applied to VMs created from this image
    boot               JSONB NOT NULL,
    -- default provisioned volume size in bytes (NULL = keep base image size)
    default_size_bytes BIGINT,
    -- whether VMs from this image expect a cloud-init NoCloud seed
    cloud_init         BOOLEAN NOT NULL DEFAULT TRUE,
    -- free-form OS label for the UI (e.g. "ubuntu-26.04")
    os                 TEXT,
    created_at         TIMESTAMPTZ NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL
);

CREATE TABLE templates (
    id                 UUID PRIMARY KEY,
    name               TEXT NOT NULL UNIQUE,
    image_id           UUID NOT NULL REFERENCES images(id) ON DELETE RESTRICT,
    boot_vcpus         INTEGER NOT NULL,
    max_vcpus          INTEGER NOT NULL,
    memory_mib         BIGINT NOT NULL,
    -- provisioned volume size in bytes (NULL = image default)
    disk_size_bytes    BIGINT,
    -- volume format for VMs from this template ('qcow2' | 'raw')
    disk_format        TEXT NOT NULL DEFAULT 'qcow2',
    -- optional default network for the VM's primary NIC
    network_id         UUID REFERENCES networks(id) ON DELETE SET NULL,
    -- cloud-init defaults (JSON CloudInitSpec), applied unless overridden
    cloud_init         JSONB,
    created_at         TIMESTAMPTZ NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL
);

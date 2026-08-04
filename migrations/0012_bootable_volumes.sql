-- Bootable / image-backed volumes (design M14d): a volume can be cloned from an
-- image, making it a persistent root disk a VM boots from (and which survives
-- the VM). NULL ⇒ a plain blank data volume (M14a).
ALTER TABLE volumes ADD COLUMN source_image_id UUID REFERENCES images(id);

//! Host storage provisioning: per-VM volumes and cloud-init seeds (design M9).
//!
//! The agent owns all host storage operations (ADR-001/ADR-010): the control
//! plane records *what* a disk should be (a base image, a size, a cloud-init
//! config) in the VM spec, and the agent materialises it here before launch.
//!
//! Volumes and seeds are placed on **shared storage** so a live migration can
//! reuse the exact same files on the destination without re-provisioning
//! (sections 20, 28).

use std::path::{Path, PathBuf};

use ch_model::{CloudInitSpec, DiskImageType, DiskSpec, VirtualMachineSpec, VmId};
use tokio::process::Command;
use tracing::info;

/// A failure provisioning host storage.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("`{cmd}` failed: {stderr}")]
    Command { cmd: String, stderr: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("qemu-img produced unparseable output: {0}")]
    Parse(String),
}

type Result<T> = std::result::Result<T, StorageError>;

/// Materialises per-VM volumes and cloud-init seeds on shared storage.
pub struct StorageProvisioner {
    /// Root of shared storage; cloud-init seeds live under `<shared_dir>/seeds`.
    shared_dir: PathBuf,
}

impl StorageProvisioner {
    pub fn new(shared_dir: impl Into<PathBuf>) -> Self {
        Self {
            shared_dir: shared_dir.into(),
        }
    }

    /// Provision any disks that carry a base image, generate a cloud-init seed
    /// when requested, and return the spec with the seed disk appended. Fully
    /// idempotent: existing volumes/seeds are reused untouched, so this is safe
    /// to call on every reconcile.
    pub async fn prepare(
        &self,
        id: VmId,
        name: &str,
        mut spec: VirtualMachineSpec,
    ) -> Result<VirtualMachineSpec> {
        for disk in &spec.disks {
            if disk.needs_provisioning() {
                self.provision_disk(disk).await?;
            }
        }
        if let Some(ci) = spec.cloud_init.clone() {
            let seed = self.ensure_seed(id, name, &ci).await?;
            if !spec.disks.iter().any(|d| d.path == seed.path) {
                spec.disks.push(seed);
            }
        }
        Ok(spec)
    }

    /// Create a disk's backing file from its base image if it does not exist.
    async fn provision_disk(&self, disk: &DiskSpec) -> Result<()> {
        if tokio::fs::try_exists(&disk.path).await? {
            // Already provisioned. Grow it if the desired size increased (design
            // M10 disk expansion). CH cannot resize an attached virtio-blk
            // online, so the guest sees the extra space after a restart (and an
            // in-guest partition/filesystem grow).
            if let Some(target) = disk.size_bytes {
                let path = path_str(&disk.path)?;
                if virtual_size(&disk.path).await? < target {
                    match disk.image_type {
                        DiskImageType::Raw => {
                            run(
                                "qemu-img",
                                &["resize", "-f", "raw", path, &target.to_string()],
                            )
                            .await?
                        }
                        DiskImageType::Qcow2 => {
                            run("qemu-img", &["resize", path, &target.to_string()]).await?
                        }
                    }
                    info!(disk = %disk.path.display(), bytes = target, "grew volume");
                }
            }
            return Ok(()); // idempotent: reuse an already-provisioned volume
        }
        if let Some(parent) = disk.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let path = path_str(&disk.path)?;

        // No source: create a blank data disk of the requested size (design M10
        // "add a secondary disk").
        let Some(source) = disk.source.as_ref() else {
            let size = disk
                .size_bytes
                .ok_or_else(|| StorageError::Parse("blank disk requires size_bytes".into()))?;
            let fmt = match disk.image_type {
                DiskImageType::Raw => "raw",
                DiskImageType::Qcow2 => "qcow2",
            };
            run("qemu-img", &["create", "-f", fmt, path, &size.to_string()]).await?;
            info!(disk = %disk.path.display(), bytes = size, fmt, "created blank data disk");
            return Ok(());
        };

        let src = path_str(source)?;
        match disk.image_type {
            DiskImageType::Qcow2 => {
                // Standalone qcow2: Cloud Hypervisor's native qcow2 driver
                // rejects backing-file overlays ("maximum disk nesting depth
                // exceeded"), so convert the base into a self-contained image.
                // `convert` copies only used blocks, so it is still thinner and
                // faster than a full raw copy and needs no base at run time.
                let src_fmt = detect_format(source).await?;
                run(
                    "qemu-img",
                    &["convert", "-f", &src_fmt, "-O", "qcow2", src, path],
                )
                .await?;
                if let Some(size) = disk.size_bytes {
                    run("qemu-img", &["resize", path, &size.to_string()]).await?;
                }
            }
            DiskImageType::Raw => {
                // Full copy (reflink where the filesystem supports it).
                run("cp", &["--reflink=auto", "-f", src, path]).await?;
                if let Some(size) = disk.size_bytes {
                    run(
                        "qemu-img",
                        &["resize", "-f", "raw", path, &size.to_string()],
                    )
                    .await?;
                }
            }
        }
        info!(disk = %disk.path.display(), source = %source.display(), fmt = ?disk.image_type, "provisioned volume");
        Ok(())
    }

    /// Generate a NoCloud seed ISO for `ci` and return the disk that mounts it.
    /// Idempotent: an existing seed on shared storage is reused (so both ends of
    /// a migration reference an identical file).
    async fn ensure_seed(&self, id: VmId, name: &str, ci: &CloudInitSpec) -> Result<DiskSpec> {
        let seed_path = self.shared_dir.join("seeds").join(format!("{id}.iso"));
        if !tokio::fs::try_exists(&seed_path).await? {
            if let Some(parent) = seed_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let hostname = ci.hostname.clone().unwrap_or_else(|| name.to_string());
            let meta_data = format!("instance-id: {id}\nlocal-hostname: {hostname}\n");
            let user_data = render_user_data(ci, &hostname);

            // Stage the two files in a temp dir, then pack them into an ISO
            // labelled `cidata` (what cloud-init's NoCloud datasource looks for).
            let stage = std::env::temp_dir().join(format!("ch-seed-{id}"));
            tokio::fs::create_dir_all(&stage).await?;
            tokio::fs::write(stage.join("meta-data"), meta_data).await?;
            tokio::fs::write(stage.join("user-data"), user_data).await?;
            let out = path_str(&seed_path)?;
            let stage_s = path_str(&stage)?;
            let result = run(
                "xorriso",
                &[
                    "-as", "mkisofs", "-o", out, "-V", "cidata", "-J", "-r", stage_s,
                ],
            )
            .await;
            let _ = tokio::fs::remove_dir_all(&stage).await;
            result?;
            info!(seed = %seed_path.display(), %hostname, "generated cloud-init seed");
        }
        Ok(DiskSpec {
            path: seed_path,
            readonly: true,
            image_type: DiskImageType::Raw,
            source: None,
            size_bytes: None,
        })
    }
}

/// Render `#cloud-config` user-data from a [`CloudInitSpec`].
fn render_user_data(ci: &CloudInitSpec, hostname: &str) -> String {
    if let Some(raw) = &ci.user_data {
        return raw.clone();
    }
    let mut s = format!("#cloud-config\nhostname: {hostname}\n");
    if let Some(pw) = &ci.password {
        s.push_str(&format!(
            "password: {pw}\nchpasswd:\n  expire: false\nssh_pwauth: true\n"
        ));
    }
    if !ci.ssh_authorized_keys.is_empty() {
        s.push_str("ssh_authorized_keys:\n");
        for key in &ci.ssh_authorized_keys {
            s.push_str(&format!("  - {key}\n"));
        }
    }
    s
}

/// The virtual (guest-visible) size of a disk image in bytes.
async fn virtual_size(path: &Path) -> Result<u64> {
    let value = qemu_img_info(path).await?;
    value
        .get("virtual-size")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| StorageError::Parse("no `virtual-size` field".into()))
}

/// Detect a disk image's format via `qemu-img info` (e.g. "raw", "qcow2").
async fn detect_format(path: &Path) -> Result<String> {
    qemu_img_info(path)
        .await?
        .get("format")
        .and_then(|f| f.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| StorageError::Parse("no `format` field".into()))
}

/// Run `qemu-img info --output=json` and return the parsed object.
async fn qemu_img_info(path: &Path) -> Result<serde_json::Value> {
    let p = path_str(path)?;
    let output = Command::new("qemu-img")
        .args(["info", "--output=json", p])
        .output()
        .await?;
    if !output.status.success() {
        return Err(StorageError::Command {
            cmd: format!("qemu-img info {p}"),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|e| StorageError::Parse(e.to_string()))
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .ok_or_else(|| StorageError::Parse(format!("non-UTF-8 path: {}", p.display())))
}

async fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(cmd).args(args).output().await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(StorageError::Command {
            cmd: format!("{cmd} {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_data_password_and_keys() {
        let ci = CloudInitSpec {
            hostname: Some("h".into()),
            ssh_authorized_keys: vec!["ssh-ed25519 AAAA".into()],
            password: Some("pw".into()),
            user_data: None,
        };
        let s = render_user_data(&ci, "h");
        assert!(s.starts_with("#cloud-config"));
        assert!(s.contains("password: pw"));
        assert!(s.contains("ssh-ed25519 AAAA"));
        assert!(s.contains("ssh_pwauth: true"));
    }

    #[test]
    fn user_data_raw_passthrough() {
        let ci = CloudInitSpec {
            hostname: None,
            ssh_authorized_keys: vec![],
            password: None,
            user_data: Some("#cloud-config\nruncmd: [echo hi]\n".into()),
        };
        assert_eq!(
            render_user_data(&ci, "h"),
            "#cloud-config\nruncmd: [echo hi]\n"
        );
    }
}

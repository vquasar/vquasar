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
use tracing::{info, warn};

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
    /// Control-plane base URL for cloud-init phone_home (design M13e); `None`
    /// disables it.
    phone_home_url: Option<String>,
    /// PEM CA to trust in the guest (so an internal-CA HTTPS control endpoint is
    /// reachable for phone_home).
    trusted_ca: Option<String>,
}

impl StorageProvisioner {
    pub fn new(shared_dir: impl Into<PathBuf>) -> Self {
        Self {
            shared_dir: shared_dir.into(),
            phone_home_url: None,
            trusted_ca: None,
        }
    }

    /// Configure the cloud-init phone_home fallback (design M13e).
    pub fn with_phone_home(mut self, url: Option<String>, trusted_ca: Option<String>) -> Self {
        self.phone_home_url = url.filter(|u| !u.is_empty());
        self.trusted_ca = trusted_ca;
        self
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
        network_config: Option<&str>,
    ) -> Result<VirtualMachineSpec> {
        for disk in &spec.disks {
            if disk.needs_provisioning() {
                self.provision_disk(disk).await?;
            }
        }
        if let Some(ci) = spec.cloud_init.clone() {
            let seed = self.ensure_seed(id, name, &ci, network_config).await?;
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
                    // Best-effort: a running (or guest-stopped) VM's VMM holds an
                    // exclusive lock on the file, so qemu-img resize fails until
                    // the VM is fully powered off. Don't fail the whole reconcile
                    // over it — the larger size stays desired and applies once
                    // the lock is released.
                    let res = match disk.image_type {
                        DiskImageType::Raw => {
                            run(
                                "qemu-img",
                                &["resize", "-f", "raw", path, &target.to_string()],
                            )
                            .await
                        }
                        DiskImageType::Qcow2 => {
                            run("qemu-img", &["resize", path, &target.to_string()]).await
                        }
                    };
                    match res {
                        Ok(()) => {
                            info!(disk = %disk.path.display(), bytes = target, "grew volume")
                        }
                        Err(e) => warn!(disk = %disk.path.display(), error = %e,
                            "disk grow deferred (volume in use); power off the VM to apply"),
                    }
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
    async fn ensure_seed(
        &self,
        id: VmId,
        name: &str,
        ci: &CloudInitSpec,
        network_config: Option<&str>,
    ) -> Result<DiskSpec> {
        let seed_path = self.shared_dir.join("seeds").join(format!("{id}.iso"));
        if !tokio::fs::try_exists(&seed_path).await? {
            if let Some(parent) = seed_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let hostname = ci.hostname.clone().unwrap_or_else(|| name.to_string());
            let meta_data = format!("instance-id: {id}\nlocal-hostname: {hostname}\n");
            let user_data = render_user_data(
                ci,
                &hostname,
                &id.to_string(),
                self.phone_home_url.as_deref(),
                self.trusted_ca.as_deref(),
            );

            // Stage the files in a temp dir, then pack them into an ISO labelled
            // `cidata` (what cloud-init's NoCloud datasource looks for). A
            // `network-config` (netplan v2) is added only when the control plane
            // supplied one — i.e. the VM is on a managed/static network (M13a);
            // otherwise cloud-init falls back to DHCP as before.
            let stage = std::env::temp_dir().join(format!("ch-seed-{id}"));
            tokio::fs::create_dir_all(&stage).await?;
            tokio::fs::write(stage.join("meta-data"), meta_data).await?;
            tokio::fs::write(stage.join("user-data"), user_data).await?;
            if let Some(nc) = network_config.filter(|s| !s.is_empty()) {
                tokio::fs::write(stage.join("network-config"), nc).await?;
            }
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

/// Render `#cloud-config` user-data from a [`CloudInitSpec`]. `phone_home_url`
/// and `trusted_ca` add the M13e IP-discovery fallback to *generated* user-data
/// (operator-supplied raw user-data is returned verbatim).
fn render_user_data(
    ci: &CloudInitSpec,
    hostname: &str,
    instance_id: &str,
    phone_home_url: Option<&str>,
    trusted_ca: Option<&str>,
) -> String {
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
    // Auto-online hot-plugged vCPUs so CPU hot-plug is seamless (design M10).
    // Memory blocks online automatically; CPUs do not on stock Ubuntu.
    s.push_str(concat!(
        "write_files:\n",
        "  - path: /etc/udev/rules.d/80-hotplug-cpu.rules\n",
        "    content: |\n",
        "      SUBSYSTEM==\"cpu\", ACTION==\"add\", TEST==\"online\", ATTR{online}==\"0\", ATTR{online}=\"1\"\n",
    ));

    if let Some(url) = phone_home_url {
        // Trust our internal CA first (ca_certs runs early, before runcmd) so an
        // HTTPS control endpoint is reachable (design M13e).
        if let Some(ca) = trusted_ca.filter(|c| !c.trim().is_empty()) {
            s.push_str("ca_certs:\n  trusted:\n    - |\n");
            for line in ca.trim().lines() {
                s.push_str(&format!("      {line}\n"));
            }
        }
        // Use curl rather than cloud-init's phone_home module: the module posts
        // via Python requests, which trusts certifi's bundle and ignores the
        // ca_certs-injected system store, so an internal-CA HTTPS endpoint fails.
        // curl uses the system CA store (which ca_certs updates via
        // update-ca-certificates / update-ca-trust) — no explicit --cacert, so it
        // works across Debian and RHEL guests whose bundle paths differ. The POST
        // carries no body; control records the request's source IP.
        let base = url.trim_end_matches('/');
        s.push_str(&format!(
            "runcmd:\n  - [\"sh\", \"-c\", \"for i in $(seq 1 12); do curl -fsS -X POST {base}/api/v1/phone-home/{instance_id} && break; sleep 5; done\"]\n"
        ));
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
    // `-U` (force-share) reads metadata without taking a lock, so this is safe
    // on a disk a running VMM already holds open (design M10).
    let output = Command::new("qemu-img")
        .args(["info", "-U", "--output=json", p])
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
        let s = render_user_data(&ci, "h", "vm-1", None, None);
        assert!(s.starts_with("#cloud-config"));
        assert!(s.contains("password: pw"));
        assert!(s.contains("ssh-ed25519 AAAA"));
        assert!(s.contains("ssh_pwauth: true"));
        assert!(!s.contains("phone-home"));
    }

    #[test]
    fn user_data_raw_passthrough() {
        let ci = CloudInitSpec {
            hostname: None,
            ssh_authorized_keys: vec![],
            password: None,
            user_data: Some("#cloud-config\nruncmd: [echo hi]\n".into()),
        };
        // Raw user-data is returned verbatim even with phone_home configured.
        assert_eq!(
            render_user_data(&ci, "h", "vm-1", Some("https://c:8080"), Some("CA")),
            "#cloud-config\nruncmd: [echo hi]\n"
        );
    }

    #[test]
    fn user_data_injects_phone_home_and_ca() {
        let ci = CloudInitSpec {
            hostname: Some("h".into()),
            ssh_authorized_keys: vec![],
            password: None,
            user_data: None,
        };
        let ca = "-----BEGIN CERTIFICATE-----\nABCD\n-----END CERTIFICATE-----";
        let s = render_user_data(&ci, "h", "vm-42", Some("https://172.16.56.8:8080/"), Some(ca));
        // curl-based phone home to the vm-id URL (trailing slash trimmed);
        // no explicit --cacert so it works across Debian/RHEL guests.
        assert!(s.contains("curl -fsS -X POST"));
        assert!(!s.contains("--cacert"));
        assert!(s.contains("https://172.16.56.8:8080/api/v1/phone-home/vm-42"));
        assert!(s.contains("ca_certs:"));
        assert!(s.contains("-----BEGIN CERTIFICATE-----"));
    }
}

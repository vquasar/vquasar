//! Redaction of secret-bearing fields on the way out of the API (design M12c).
//!
//! Cloud-init passwords, raw user-data and SSH keys are sealed at rest and
//! unsealed at the store boundary, so the reconcile loop can hand plaintext to
//! the agent to build the seed ISO. Nothing about that requires showing them to
//! an API caller — and `vm:read`, which the built-in `viewer` role holds, would
//! otherwise return every VM's guest password in plaintext.
//!
//! Responses therefore carry [`REDACTED`] in place of each secret value. The
//! marker is deliberately visible rather than dropped: a caller can still tell
//! that a password is set, and an update that echoes the marker back is
//! resolved against the stored value (see [`merge_cloud_init`]) instead of
//! overwriting the secret with the marker.

use vquasar_model::CloudInitSpec;

use crate::store::{Template, Vm};

/// Stands in for a secret value the caller may not read.
pub const REDACTED: &str = "__redacted__";

/// Replace every secret in a cloud-init block with [`REDACTED`], keeping
/// non-secret fields (hostname) and the shape of the value.
fn redact_cloud_init(ci: &mut CloudInitSpec) {
    if ci.password.is_some() {
        ci.password = Some(REDACTED.to_string());
    }
    if ci.user_data.is_some() {
        ci.user_data = Some(REDACTED.to_string());
    }
    for key in ci.ssh_authorized_keys.iter_mut() {
        *key = REDACTED.to_string();
    }
}

/// Redact a VM before it is serialized into a response.
pub fn vm(mut vm: Vm) -> Vm {
    if let Some(ci) = vm.spec.0.cloud_init.as_mut() {
        redact_cloud_init(ci);
    }
    vm
}

pub fn vms(vms: Vec<Vm>) -> Vec<Vm> {
    vms.into_iter().map(vm).collect()
}

/// Redact a template before it is serialized into a response.
pub fn template(mut t: Template) -> Template {
    if let Some(ci) = t.cloud_init.as_mut() {
        redact_cloud_init(&mut ci.0);
    }
    t
}

pub fn templates(ts: Vec<Template>) -> Vec<Template> {
    ts.into_iter().map(template).collect()
}

/// Resolve a submitted cloud-init block against what is already stored.
///
/// A client that read a template back sees [`REDACTED`] where the secrets are;
/// echoing that value on update must mean "leave it as it is", not "set the
/// password to the literal marker". Any other value — including `None` — is
/// taken at face value, so secrets can still be changed or cleared.
pub fn merge_cloud_init(
    submitted: Option<CloudInitSpec>,
    stored: Option<&CloudInitSpec>,
) -> Option<CloudInitSpec> {
    let mut submitted = submitted?;
    if let Some(stored) = stored {
        if submitted.password.as_deref() == Some(REDACTED) {
            submitted.password = stored.password.clone();
        }
        if submitted.user_data.as_deref() == Some(REDACTED) {
            submitted.user_data = stored.user_data.clone();
        }
        // SSH keys are all-or-nothing: a list that is exactly the redacted
        // marker(s) means the caller did not touch the field.
        if !submitted.ssh_authorized_keys.is_empty()
            && submitted.ssh_authorized_keys.iter().all(|k| k == REDACTED)
        {
            submitted.ssh_authorized_keys = stored.ssh_authorized_keys.clone();
        }
    }
    Some(submitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> CloudInitSpec {
        CloudInitSpec {
            hostname: Some("guest".into()),
            ssh_authorized_keys: vec!["ssh-ed25519 AAAA".into()],
            password: Some("hunter2".into()),
            user_data: Some("#cloud-config".into()),
        }
    }

    #[test]
    fn every_secret_is_replaced_and_hostname_survives() {
        let mut ci = spec();
        redact_cloud_init(&mut ci);
        assert_eq!(ci.password.as_deref(), Some(REDACTED));
        assert_eq!(ci.user_data.as_deref(), Some(REDACTED));
        assert_eq!(ci.ssh_authorized_keys, vec![REDACTED.to_string()]);
        assert_eq!(ci.hostname.as_deref(), Some("guest"));
    }

    /// The whole point: no plaintext secret may survive serialization.
    #[test]
    fn redacted_json_contains_no_plaintext_secret() {
        let mut ci = spec();
        redact_cloud_init(&mut ci);
        let json = serde_json::to_string(&ci).unwrap();
        assert!(!json.contains("hunter2"), "{json}");
        assert!(!json.contains("#cloud-config"), "{json}");
        assert!(!json.contains("ssh-ed25519 AAAA"), "{json}");
    }

    #[test]
    fn absent_secrets_stay_absent() {
        let mut ci = CloudInitSpec {
            hostname: None,
            ssh_authorized_keys: vec![],
            password: None,
            user_data: None,
        };
        redact_cloud_init(&mut ci);
        assert!(ci.password.is_none());
        assert!(ci.user_data.is_none());
        assert!(ci.ssh_authorized_keys.is_empty());
    }

    #[test]
    fn echoing_the_marker_keeps_the_stored_secret() {
        let stored = spec();
        let mut submitted = spec();
        redact_cloud_init(&mut submitted);
        let merged = merge_cloud_init(Some(submitted), Some(&stored)).unwrap();
        assert_eq!(merged.password.as_deref(), Some("hunter2"));
        assert_eq!(merged.user_data.as_deref(), Some("#cloud-config"));
        assert_eq!(merged.ssh_authorized_keys, vec!["ssh-ed25519 AAAA"]);
    }

    #[test]
    fn a_real_value_still_overwrites() {
        let stored = spec();
        let submitted = CloudInitSpec {
            password: Some("newpass".into()),
            user_data: None,
            ssh_authorized_keys: vec!["ssh-ed25519 BBBB".into()],
            hostname: None,
        };
        let merged = merge_cloud_init(Some(submitted), Some(&stored)).unwrap();
        assert_eq!(merged.password.as_deref(), Some("newpass"));
        assert!(merged.user_data.is_none(), "an omitted secret is cleared");
        assert_eq!(merged.ssh_authorized_keys, vec!["ssh-ed25519 BBBB"]);
    }

    #[test]
    fn no_stored_template_means_take_the_submission_verbatim() {
        let submitted = spec();
        let merged = merge_cloud_init(Some(submitted), None).unwrap();
        assert_eq!(merged.password.as_deref(), Some("hunter2"));
    }
}

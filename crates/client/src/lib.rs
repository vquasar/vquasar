//! A direct client for a single Cloud Hypervisor instance.
//!
//! The crate is split along the three concerns called out in the design
//! document (section 43), each in its own module so they can evolve
//! independently:
//!
//! * [`process`] — launching and supervising the `cloud-hypervisor` process.
//! * [`socket`] — the HTTP-over-Unix-socket API client.
//! * [`config`] — translation between the domain model and CH's wire types.
//!
//! [`hypervisor::Hypervisor`] is the abstraction the rest of the system depends
//! on; [`cloud_hypervisor::CloudHypervisor`] is the production implementation
//! and [`fake::FakeHypervisor`] the test double.

pub mod cloud_hypervisor;
pub mod config;
pub mod error;
pub mod fake;
pub mod hypervisor;
pub mod process;
pub mod socket;

pub use cloud_hypervisor::{CloudHypervisor, LaunchConfig};
pub use config::{SerialTarget, TapBinding, TranslateOptions};
pub use error::{ChError, Result};
pub use fake::FakeHypervisor;
pub use hypervisor::{Hypervisor, HypervisorState, HypervisorVmInfo};
pub use process::{ChProcess, ProcessConfig};
pub use socket::ApiClient;

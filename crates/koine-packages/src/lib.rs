//! `koine-packages`: shipped surfaces and packages
//! (docs/plan-koine-research-os.md B-D, P-D).
//!
//! Omarchy keeps what the system ships (`/usr/share/omarchy`) apart from what
//! the person owns (`~/.config`), and can put the shipped one back. Koine
//! does the same per surface: [`shipped`] keeps a baseline beside every
//! shipped surface, upgrades three-way, and resets and diffs on request.
//! [`package`] reads a package directory, answers the review a person reads
//! before installing, and installs exactly what was reviewed (its digest).
//! [`kit`] is the package every host ships: the Notebook, the Agent desk and
//! the Packages surface.

pub mod digest;
pub mod dto;
pub mod kit;
pub mod package;
pub mod registry;
pub mod service;
pub mod shipped;

pub use service::{DefaultPackageService, PackageService};

/// Reference the service so the linker keeps its registrations.
pub fn ensure_linked() {
    std::hint::black_box(DefaultPackageService::new as *const ());
}

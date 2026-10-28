//! Explicitly refusing protected-runtime surface for hosts other than Linux.
//! The real module provisions root-owned OCI runtimes and needs Linux kernel
//! facilities; on other targets the shared types exist so cross-platform code
//! compiles, and every entry point refuses.

/// The closed caller identity type. On this host no caller can be admitted.
#[derive(Clone, Debug)]
pub struct RuntimeCaller;

pub mod console {
    /// The owned OCI console type. On this host no console can exist.
    pub struct OwnerConsole;
}

pub mod qualified {
    /// The live qualified reader. On this host no reader can be issued.
    pub struct CurrentReader<'a>(std::marker::PhantomData<&'a ()>);
}

/// No reviewed protected profile can be active on this host.
pub fn protected_profile_active() -> bool {
    false
}

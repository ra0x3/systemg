//! Failures a service's child process reports to the supervisor.
//!
//! Between `fork` and `exec` a child cannot return a message: Rust's exec
//! handshake carries only an errno, so the SG identity of a failure would be
//! lost and every refusal would surface as a generic start error. `ChildFault`
//! therefore has a one-byte wire form the child writes to a dedicated pipe
//! before it fails; the parent reads it back and rebuilds the typed diagnostic.
//!
//! The byte is the last two digits of the SG code, so the mapping stays readable
//! and does not depend on the `SgCode` declaration order.

use crate::diag::SgCode;

/// A failure raised in the child, in a form that survives `fork`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildFault {
    /// SG0721 - a security key was accepted but cannot be enforced.
    KeyUnenforceable,
    /// SG0722 - a seccomp filter could not be built, compiled, or applied.
    SeccompFilter,
    /// SG0723 - `prctl(PR_SET_NO_NEW_PRIVS)` failed.
    NoNewPrivs,
    /// SG0724 - Landlock is unavailable or could not be fully enforced.
    Landlock,
    /// SG0725 - the manifest named a seccomp profile that does not exist.
    SeccompProfile,
    /// SG0726 - seccomp is unsupported on this CPU architecture.
    SeccompArch,
    /// SG0727 - a requested namespace could not be unshared.
    NamespaceUnshare,
    /// SG0741 - a resource limit could not be set.
    ResourceLimit,
    /// SG0742 - the scheduling priority could not be set.
    Priority,
    /// SG0743 - the CPU affinity could not be applied.
    CpuAffinity,
    /// SG0744 - the supplementary group list could not be set.
    SupplementaryGroups,
    /// SG0745 - the primary group could not be switched.
    PrimaryGid,
    /// SG0746 - the user could not be switched.
    UidSwitch,
    /// SG0747 - capabilities could not be retained across the identity switch.
    CapabilityRetention,
    /// SG0748 - capabilities could not be fully dropped.
    CapabilityReduction,
}

impl ChildFault {
    /// The SG code this fault reports as.
    pub fn code(self) -> SgCode {
        match self {
            Self::KeyUnenforceable => SgCode::SandboxKeyUnenforceable,
            Self::SeccompFilter => SgCode::SeccompFilterFailed,
            Self::NoNewPrivs => SgCode::NoNewPrivsFailed,
            Self::Landlock => SgCode::LandlockUnavailable,
            Self::SeccompProfile => SgCode::SeccompProfileUnknown,
            Self::SeccompArch => SgCode::SeccompArchUnsupported,
            Self::NamespaceUnshare => SgCode::NamespaceUnshareFailed,
            Self::ResourceLimit => SgCode::ResourceLimitFailed,
            Self::Priority => SgCode::PriorityFailed,
            Self::CpuAffinity => SgCode::CpuAffinityFailed,
            Self::SupplementaryGroups => SgCode::SupplementaryGroupsFailed,
            Self::PrimaryGid => SgCode::PrimaryGidFailed,
            Self::UidSwitch => SgCode::UidSwitchFailed,
            Self::CapabilityRetention => SgCode::CapabilityRetentionFailed,
            Self::CapabilityReduction => SgCode::CapabilityReductionIncomplete,
        }
    }

    /// The wire byte the child writes.
    pub fn as_byte(self) -> u8 {
        match self {
            Self::KeyUnenforceable => 21,
            Self::SeccompFilter => 22,
            Self::NoNewPrivs => 23,
            Self::Landlock => 24,
            Self::SeccompProfile => 25,
            Self::SeccompArch => 26,
            Self::NamespaceUnshare => 27,
            Self::ResourceLimit => 41,
            Self::Priority => 42,
            Self::CpuAffinity => 43,
            Self::SupplementaryGroups => 44,
            Self::PrimaryGid => 45,
            Self::UidSwitch => 46,
            Self::CapabilityRetention => 47,
            Self::CapabilityReduction => 48,
        }
    }

    /// Reads a fault back from its wire byte.
    pub fn from_byte(byte: u8) -> Option<Self> {
        let fault = match byte {
            21 => Self::KeyUnenforceable,
            22 => Self::SeccompFilter,
            23 => Self::NoNewPrivs,
            24 => Self::Landlock,
            25 => Self::SeccompProfile,
            26 => Self::SeccompArch,
            27 => Self::NamespaceUnshare,
            41 => Self::ResourceLimit,
            42 => Self::Priority,
            43 => Self::CpuAffinity,
            44 => Self::SupplementaryGroups,
            45 => Self::PrimaryGid,
            46 => Self::UidSwitch,
            47 => Self::CapabilityRetention,
            48 => Self::CapabilityReduction,
            _ => return None,
        };
        Some(fault)
    }
}

/// A child-side failure. Holds no allocation: it is built after `fork`, where
/// allocating is not async-signal-safe.
#[derive(Debug, Clone, Copy)]
pub struct ApplyFault {
    /// Which control failed.
    pub fault: ChildFault,
    /// `errno` at the point of failure, or 0 when the cause was not a syscall.
    pub errno: i32,
}

impl ApplyFault {
    /// Captures the current `errno` for `fault`. Reads a thread-local int; no
    /// allocation, safe between `fork` and `exec`.
    pub fn last(fault: ChildFault) -> Self {
        Self {
            fault,
            errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        }
    }

    /// A failure that was not a syscall error, so `errno` would be stale.
    pub fn bare(fault: ChildFault) -> Self {
        Self { fault, errno: 0 }
    }
}

/// A parent-side failure, raised before any fork, so it can carry a message.
#[derive(Debug)]
pub struct PrepareFault {
    /// Which control could not be prepared.
    pub fault: ChildFault,
    /// Operator-facing explanation.
    pub message: String,
}

impl PrepareFault {
    /// Builds a prepare-time failure.
    pub fn new(fault: ChildFault, message: impl Into<String>) -> Self {
        Self {
            fault,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PrepareFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PrepareFault {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fault_round_trips_through_its_byte() {
        let all = [
            ChildFault::KeyUnenforceable,
            ChildFault::SeccompFilter,
            ChildFault::NoNewPrivs,
            ChildFault::Landlock,
            ChildFault::SeccompProfile,
            ChildFault::SeccompArch,
            ChildFault::NamespaceUnshare,
            ChildFault::ResourceLimit,
            ChildFault::Priority,
            ChildFault::CpuAffinity,
            ChildFault::SupplementaryGroups,
            ChildFault::PrimaryGid,
            ChildFault::UidSwitch,
            ChildFault::CapabilityRetention,
            ChildFault::CapabilityReduction,
        ];
        let mut seen = std::collections::HashSet::new();
        for fault in all {
            assert!(seen.insert(fault.as_byte()), "duplicate wire byte");
            assert_eq!(ChildFault::from_byte(fault.as_byte()), Some(fault));
            // The byte must match the code it claims to be.
            let digits = &fault.code().as_str()[4..];
            assert_eq!(digits.parse::<u8>().unwrap(), fault.as_byte());
        }
    }

    #[test]
    fn unknown_byte_is_rejected() {
        assert_eq!(ChildFault::from_byte(0), None);
        assert_eq!(ChildFault::from_byte(99), None);
    }
}

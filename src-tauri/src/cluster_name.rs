//! Pure logic for the shared cluster-name register `(name, version, origin)`.
//!
//! The cluster name is replicated across a leaderless set of peers with
//! last-write-wins semantics: a Lamport-style `version` counter decides the
//! winner, and `origin` (the device_id that set the name) breaks ties so two
//! concurrent renames at the same version converge deterministically. See
//! `docs/superpowers/specs/2026-06-06-cluster-name-convergence-design.md`.

/// Returns true if an incoming register should replace the local one.
///
/// Incoming wins iff its version is strictly higher, or the versions are equal
/// and its origin sorts strictly after the local origin (string comparison).
/// Equal version AND equal origin is NOT a win (idempotent — used for gossip
/// de-duplication).
pub(crate) fn incoming_register_wins(
    local_version: u64,
    local_origin: &str,
    incoming_version: u64,
    incoming_origin: &str,
) -> bool {
    if incoming_version != local_version {
        return incoming_version > local_version;
    }
    incoming_origin > local_origin
}

/// The version a local rename should claim: one past the highest version we
/// currently know. Because every accepted incoming register overwrites the
/// local version, the local version always tracks the max seen, so `+1` beats
/// everything currently known.
pub(crate) fn next_local_version(local_version: u64) -> u64 {
    local_version + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_version_wins() {
        assert!(incoming_register_wins(1, "dev-a", 2, "dev-a"));
    }

    #[test]
    fn lower_version_loses() {
        assert!(!incoming_register_wins(2, "dev-a", 1, "dev-z"));
    }

    #[test]
    fn equal_version_higher_origin_wins() {
        // Tie broken by origin: "dev-z" > "dev-a".
        assert!(incoming_register_wins(3, "dev-a", 3, "dev-z"));
    }

    #[test]
    fn equal_version_lower_origin_loses() {
        assert!(!incoming_register_wins(3, "dev-z", 3, "dev-a"));
    }

    #[test]
    fn identical_register_is_not_a_win() {
        // Idempotent: same version + same origin → no adoption, no re-gossip.
        assert!(!incoming_register_wins(5, "dev-a", 5, "dev-a"));
    }

    #[test]
    fn upgrade_zero_version_converges_by_origin() {
        // Two pre-feature peers both at version 0 converge to higher origin.
        assert!(incoming_register_wins(0, "dev-aaa", 0, "dev-bbb"));
        assert!(!incoming_register_wins(0, "dev-bbb", 0, "dev-aaa"));
    }

    #[test]
    fn next_version_is_one_past_local() {
        assert_eq!(next_local_version(0), 1);
        assert_eq!(next_local_version(41), 42);
    }
}

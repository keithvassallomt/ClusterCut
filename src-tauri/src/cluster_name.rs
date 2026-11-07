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

/// Hostnames of nearby devices already advertising `name` for a *different*
/// cluster. Two clusters sharing a name can't be told apart by name alone,
/// which is how a re-created cluster leaves a device behind that still thinks
/// it's a member. Used to warn (never block) when choosing a name.
///
/// "Different cluster" compares cluster tags when the device advertises one.
/// Older builds don't, so for them it falls back to "not one of our known
/// peers": a device that pairs into our cluster is added to `known_peers`, so
/// a same-name device outside it belongs to someone else's cluster.
pub(crate) fn name_conflicts(
    name: &str,
    local_id: &str,
    local_tag: &str,
    known: &std::collections::HashMap<String, crate::peer::Peer>,
    runtime: &std::collections::HashMap<String, crate::peer::Peer>,
) -> Vec<String> {
    let mut hosts: Vec<String> = runtime
        .values()
        .filter(|p| p.id != local_id && p.network_name.as_deref() == Some(name))
        .filter(|p| match crate::membership::advertised_tag(p) {
            Some(tag) if !local_tag.is_empty() => tag != local_tag,
            _ => !known.contains_key(&p.id),
        })
        .map(|p| p.hostname.clone())
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, host: &str, name: &str) -> (String, crate::peer::Peer) {
        (
            id.to_string(),
            crate::peer::Peer {
                id: id.to_string(),
                ip: "192.168.96.7".parse().unwrap(),
                port: 4654,
                hostname: host.to_string(),
                last_seen: 0,
                is_trusted: false,
                is_manual: false,
                network_name: Some(name.to_string()),
                signature: None,
                fingerprint: None,
                protocol_version: None,
                cluster_tag: None,
            },
        )
    }

    #[test]
    fn same_name_outside_our_cluster_conflicts() {
        use std::collections::HashMap;
        let runtime: HashMap<_, _> = [
            device("eir", "eir.vassallo.cloud", "vassallo.cloud"),
            device("mimir", "Mimir.local", "vassallo.cloud"),
            device("ubuntu", "UbuntuVM", "vassallo.cloud"),
            device("other", "Elsewhere", "abundant-peace"),
            device("me", "thor", "vassallo.cloud"),
        ]
        .into_iter()
        .collect();
        // UbuntuVM is one of ours; the rest named vassallo.cloud are not.
        let known: HashMap<_, _> = [device("ubuntu", "UbuntuVM", "vassallo.cloud")].into_iter().collect();
        // No tags advertised (older builds): falls back to known_peers.
        assert_eq!(
            name_conflicts("vassallo.cloud", "me", "aaaa", &known, &runtime),
            ["Mimir.local", "eir.vassallo.cloud"]
        );
        assert!(name_conflicts("unused-name", "me", "aaaa", &known, &runtime).is_empty());
    }

    #[test]
    fn advertised_tags_decide_over_known_peers() {
        use std::collections::HashMap;
        // eir is still pinned from before it left, but advertises another
        // cluster; UbuntuVM isn't pinned yet but advertises ours.
        let mut eir = device("eir", "eir.vassallo.cloud", "vassallo.cloud");
        eir.1.cluster_tag = Some("bbbb".into());
        let mut ubuntu = device("ubuntu", "UbuntuVM", "vassallo.cloud");
        ubuntu.1.cluster_tag = Some("aaaa".into());
        let runtime: HashMap<_, _> = [eir.clone(), ubuntu].into_iter().collect();
        let known: HashMap<_, _> = [eir].into_iter().collect();
        assert_eq!(
            name_conflicts("vassallo.cloud", "me", "aaaa", &known, &runtime),
            ["eir.vassallo.cloud"]
        );
    }

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

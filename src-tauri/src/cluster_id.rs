//! The cluster's identity: a UUID (`cluster_id`) that every member should
//! share, and the short tag derived from it that devices advertise in mDNS so
//! "same name, different cluster" can be seen directly.

use crate::state::AppState;
use sha2::{Digest, Sha256};
use tauri::Emitter;

/// Two authenticated members holding different cluster_ids are one cluster
/// regardless, so both converge on the smaller id: each side reaches the same
/// answer on its own, with no coordination.
pub(crate) fn should_adopt(local: &str, remote: &str) -> bool {
    !remote.is_empty() && (local.is_empty() || remote < local)
}

/// The cluster tag advertised in the mDNS `c` property: a SHA-256 prefix of
/// the id. Plenty to tell clusters apart, without broadcasting the id itself.
pub(crate) fn tag(cluster_id: &str) -> String {
    if cluster_id.is_empty() {
        return String::new();
    }
    Sha256::digest(cluster_id.as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Adopt `cluster_id` from another member: persist it and re-advertise under
/// the new tag.
pub(crate) fn adopt(app: &tauri::AppHandle, state: &AppState, cluster_id: &str) {
    let previous = std::mem::replace(&mut *state.cluster_id.lock().unwrap(), cluster_id.to_string());
    crate::storage::save_cluster_id(app, cluster_id);
    tracing::info!(
        "[Cluster] Converged cluster id {} -> {} (a member held the other id)",
        previous, cluster_id
    );
    crate::discovery::readvertise(state);
    let _ = app.emit("network-update", ());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_converge_on_the_smaller_id() {
        let (a, b) = ("1a2b-old", "7f00-new");
        assert!(should_adopt(b, a)); // the holder of b adopts a
        assert!(!should_adopt(a, b)); // the holder of a keeps it
        assert!(!should_adopt(a, a));
    }

    #[test]
    fn never_adopts_an_empty_id() {
        assert!(!should_adopt("1a2b", ""));
        assert!(should_adopt("", "1a2b"));
    }

    #[test]
    fn tag_is_stable_short_and_distinct() {
        let t = tag("dc774de7-9371-485e-93ff-d3d28000a5c4");
        assert_eq!(t.len(), 16);
        assert_eq!(t, tag("dc774de7-9371-485e-93ff-d3d28000a5c4"));
        assert_ne!(t, tag("4ffb1491-f4cc-4d03-a814-e483b99e8290"));
        assert_eq!(tag(""), "");
    }
}

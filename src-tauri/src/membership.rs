//! Cluster membership health from this device's side: is it still a member of
//! the cluster it thinks it is in, and are its paired devices still in it?
//!
//! The evidence is mTLS refusal (`transport::PeerRefused`). A refusal can only
//! come from a device whose certificate matches one we pinned, so it means "a
//! device we paired with no longer recognises us", never a guess. What it
//! implies depends on whether anyone still accepts us:
//!
//! - **Dropped** (nobody accepts us): the cluster was re-created without us,
//!   or we were removed while offline. Only a re-pair fixes that, so it is
//!   surfaced as a notification (once per cluster) and an in-app banner, and
//!   peer join/leave notifications are muted until it clears.
//! - **Departed** (others still accept us): that one device moved to another
//!   cluster. It no longer counts us as a member, so we stop counting it.
//!
//! A device that simply hasn't learned about us *yet* refuses us too: a member
//! that was offline when we paired only hears of us through membership sync,
//! normally within a minute. The windows below sit past that. A refusal from
//! a device advertising our own cluster is the most likely to be that, so it
//! needs longer to count, but it does count: that is what being removed while
//! offline looks like. "Departed" (which deletes data) needs the device to
//! advertise a different cluster, and a much longer window.

use crate::peer::Peer;
use crate::state::AppState;
use serde::Serialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tauri::Emitter;

/// How long refusals must persist, with nobody accepting us, before we count
/// as dropped.
pub(crate) const DROP_AFTER: Duration = Duration::from_secs(60);
/// The same, for refusals from devices advertising our own cluster, which are
/// more likely to be members still catching up with us.
pub(crate) const SAME_CLUSTER_DROP_AFTER: Duration = Duration::from_secs(180);
/// How long a device advertising another cluster must keep refusing us, while
/// others accept us, before we forget it.
pub(crate) const DEPART_AFTER: Duration = Duration::from_secs(300);
/// Refusal evidence that hasn't been refreshed for this long is discarded
/// (the device went offline, or we stopped heartbeating it).
const REFUSAL_TTL: Duration = Duration::from_secs(180);

/// Which cluster a refusing device advertises, relative to ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Affiliation {
    /// Advertises our cluster: a member that hasn't learned about us yet.
    Ours,
    /// Advertises a different cluster.
    Other,
    /// Can't tell: advertises our cluster's name, which another cluster may
    /// share.
    Unknown,
}

/// Classify a device by what it advertises: its cluster tag when both sides
/// have one, otherwise its cluster name (which only proves "different" when
/// it differs).
pub(crate) fn affiliation(peer: &Peer, local_name: &str, local_tag: &str) -> Affiliation {
    match advertised_tag(peer) {
        Some(tag) if !local_tag.is_empty() => {
            if tag == local_tag {
                Affiliation::Ours
            } else {
                Affiliation::Other
            }
        }
        _ => match peer.network_name.as_deref() {
            Some(name) if name != local_name => Affiliation::Other,
            _ => Affiliation::Unknown,
        },
    }
}

/// The peer's cluster tag, if it advertises a usable one.
pub(crate) fn advertised_tag(peer: &Peer) -> Option<&str> {
    peer.cluster_tag.as_deref().filter(|t| !t.is_empty())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRef {
    pub id: String,
    pub hostname: String,
}

/// Payload of the `cluster-dropped` event and `get_cluster_dropped`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DroppedInfo {
    pub cluster_name: String,
    /// Devices that refuse us; the UI offers them as rejoin targets.
    pub devices: Vec<DeviceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Departed {
    pub id: String,
    pub hostname: String,
    pub fingerprint: Option<Vec<u8>>,
}

/// What changed in one evaluation.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    /// Set when we just became dropped.
    pub dropped: Option<DroppedInfo>,
    /// Set when a dropped state just cleared because someone accepted us.
    pub rejoined: bool,
    pub departed: Vec<Departed>,
}

#[derive(Debug)]
struct Refusal {
    hostname: String,
    fingerprint: Option<Vec<u8>>,
    affiliation: Affiliation,
    first: Instant,
    last: Instant,
}

#[derive(Debug, Default)]
pub struct MembershipTracker {
    /// Runtime peer id -> ongoing refusal.
    refusing: HashMap<String, Refusal>,
    /// Last time any device accepted us (successful send or inbound mTLS).
    last_accepted: Option<Instant>,
    /// Current dropped state and when it was declared.
    dropped: Option<(DroppedInfo, Instant)>,
}

impl MembershipTracker {
    /// Record that `id` refused us. Returns true if it wasn't already
    /// refusing, so callers can log the start of a refusal once.
    pub(crate) fn record_refusal(
        &mut self,
        id: &str,
        hostname: &str,
        fingerprint: Option<Vec<u8>>,
        affiliation: Affiliation,
        now: Instant,
    ) -> bool {
        match self.refusing.get_mut(id) {
            Some(r) => {
                r.last = now;
                r.hostname = hostname.to_string();
                r.affiliation = affiliation;
                if fingerprint.is_some() {
                    r.fingerprint = fingerprint;
                }
                false
            }
            None => {
                self.refusing.insert(
                    id.to_string(),
                    Refusal {
                        hostname: hostname.to_string(),
                        fingerprint,
                        affiliation,
                        first: now,
                        last: now,
                    },
                );
                true
            }
        }
    }

    /// Record that a device accepted us (`id` when we know which). Returns
    /// true if that device had been refusing.
    pub(crate) fn record_accepted(&mut self, id: Option<&str>, now: Instant) -> bool {
        self.last_accepted = Some(now);
        id.map_or(false, |id| self.refusing.remove(id).is_some())
    }

    pub(crate) fn is_dropped(&self) -> bool {
        self.dropped.is_some()
    }

    pub(crate) fn dropped(&self) -> Option<DroppedInfo> {
        self.dropped.as_ref().map(|(info, _)| info.clone())
    }

    /// Forget everything (we just paired into a cluster, or left ours).
    /// Returns true if we had been dropped.
    pub(crate) fn reset(&mut self) -> bool {
        let was_dropped = self.dropped.is_some();
        *self = Self::default();
        was_dropped
    }

    pub(crate) fn evaluate(&mut self, cluster_name: &str, now: Instant) -> Changes {
        self.refusing
            .retain(|_, r| now.saturating_duration_since(r.last) < REFUSAL_TTL);
        let mut changes = Changes::default();

        if let Some(since) = self.dropped.as_ref().map(|(_, since)| *since) {
            if self.last_accepted.map_or(false, |t| t > since) {
                self.dropped = None;
                changes.rejoined = true;
            } else {
                // Keep the rejoin targets current.
                let devices = refusing_devices(&self.refusing);
                if let (false, Some((info, _))) = (devices.is_empty(), self.dropped.as_mut()) {
                    info.devices = devices;
                }
            }
            return changes;
        }

        let accepted_recently = self
            .last_accepted
            .map_or(false, |t| now.saturating_duration_since(t) < DROP_AFTER);

        if !accepted_recently {
            if self.refusing.values().any(|r| counts_against_us(r, now)) {
                let info = DroppedInfo {
                    cluster_name: cluster_name.to_string(),
                    devices: refusing_devices(&self.refusing),
                };
                self.dropped = Some((info.clone(), now));
                changes.dropped = Some(info);
            }
            return changes;
        }

        // Others accept us, so a device that has long refused us while
        // advertising another cluster is the one that left.
        let departed: Vec<String> = self
            .refusing
            .iter()
            .filter(|(_, r)| {
                r.affiliation == Affiliation::Other
                    && now.saturating_duration_since(r.first) >= DEPART_AFTER
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in departed {
            if let Some(r) = self.refusing.remove(&id) {
                changes.departed.push(Departed {
                    id,
                    hostname: r.hostname,
                    fingerprint: r.fingerprint,
                });
            }
        }
        changes
    }
}

/// Whether a refusal has lasted long enough to count against our membership.
fn counts_against_us(r: &Refusal, now: Instant) -> bool {
    let age = now.saturating_duration_since(r.first);
    match r.affiliation {
        Affiliation::Ours => age >= SAME_CLUSTER_DROP_AFTER,
        Affiliation::Other | Affiliation::Unknown => age >= DROP_AFTER,
    }
}

/// Every device currently refusing us, one entry per hostname (a device can
/// be refusing under both its current id and an older pinned one).
fn refusing_devices(refusing: &HashMap<String, Refusal>) -> Vec<DeviceRef> {
    let mut devices: Vec<DeviceRef> = refusing
        .iter()
        .map(|(id, r)| DeviceRef { id: id.clone(), hostname: r.hostname.clone() })
        .collect();
    devices.sort_by(|a, b| a.hostname.cmp(&b.hostname).then(a.id.cmp(&b.id)));
    devices.dedup_by(|a, b| a.hostname == b.hostname);
    devices
}

/// Known-peer ids that belong to a departed device: its runtime id, plus any
/// entry pinned to the same cert (older ids from before it was reset).
pub(crate) fn known_entries_for(departed: &[Departed], known: &HashMap<String, Peer>) -> Vec<String> {
    let mut ids: Vec<String> = known
        .iter()
        .filter(|(id, p)| {
            departed.iter().any(|d| {
                d.id == **id
                    || (d.fingerprint.is_some() && p.fingerprint.as_ref() == d.fingerprint.as_ref())
            })
        })
        .map(|(id, _)| id.clone())
        .collect();
    ids.sort();
    ids
}

/// Known-peer ids to forget when pairing moves this device into a different
/// cluster: everything from the old cluster except the device we just paired
/// with. `manual-<ip>` placeholders are reachability hints, not members, so
/// they stay.
pub(crate) fn stale_on_cluster_switch(known: &HashMap<String, Peer>, keep_id: &str) -> Vec<String> {
    let mut ids: Vec<String> = known
        .keys()
        .filter(|id| id.as_str() != keep_id && !id.starts_with("manual-"))
        .cloned()
        .collect();
    ids.sort();
    ids
}

/// "A", "A and B", "A, B and C".
pub(crate) fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {}", init.join(", "), last),
    }
}

/// Record a refusal seen by a probe of a known peer (startup, recovery and
/// anti-entropy re-probes). Those reach known peers that aren't in the runtime
/// map, so evidence keeps flowing after the stale-peer prune stops the
/// heartbeat to a device that refuses us.
pub(crate) fn record_probe_refusal(
    state: &AppState,
    addr: std::net::SocketAddr,
    refused: &crate::transport::PeerRefused,
) {
    let local_name = state.network_name.lock().unwrap().clone();
    let local_tag = state.cluster_tag();
    // The cert it presented matched one of our pins, which names the entry
    // exactly; the address is the fallback.
    let entry = {
        let kp = state.known_peers.lock().unwrap();
        let by_cert = refused
            .server_fingerprint
            .as_ref()
            .and_then(|fp| kp.values().find(|p| p.fingerprint.as_ref() == Some(fp)));
        by_cert
            .or_else(|| kp.values().find(|p| p.ip == addr.ip() && p.port == addr.port()))
            .cloned()
    };
    let Some(peer) = entry else { return };
    let started = state.membership.lock().unwrap().record_refusal(
        &peer.id,
        &peer.hostname,
        refused.server_fingerprint.clone(),
        affiliation(&peer, &local_name, &local_tag),
        Instant::now(),
    );
    if started {
        tracing::warn!(
            "[Membership] {} ({}) refuses our connection: this device is not paired on its side. {}",
            peer.hostname, addr, refused
        );
    }
}

/// Evaluate the tracker and act on what changed. Called from the heartbeat
/// loop every round. Skipped while presence is paused (offline, suspended,
/// or just resumed), when neither refusals nor acceptances can arrive.
pub(crate) fn tick(app: &tauri::AppHandle, state: &AppState) {
    if crate::presence::presence_paused(state) {
        return;
    }
    let cluster_name = state.network_name.lock().unwrap().clone();
    let changes = state
        .membership
        .lock()
        .unwrap()
        .evaluate(&cluster_name, Instant::now());

    if let Some(info) = changes.dropped {
        on_dropped(app, state, info);
    }
    if changes.rejoined {
        tracing::info!("[Membership] A cluster member accepts this device again; no longer dropped");
        let _ = app.emit("cluster-rejoined", ());
    }
    if !changes.departed.is_empty() {
        forget_departed(app, state, &changes.departed);
    }
}

/// Clear membership state after pairing into a cluster or leaving ours, and
/// tell the UI if that ended a dropped state.
pub(crate) fn clear(app: &tauri::AppHandle, state: &AppState) {
    if state.membership.lock().unwrap().reset() {
        let _ = app.emit("cluster-rejoined", ());
    }
}

fn on_dropped(app: &tauri::AppHandle, state: &AppState, info: DroppedInfo) {
    let names: Vec<String> = info.devices.iter().map(|d| d.hostname.clone()).collect();
    let verb = if names.len() == 1 { "recognises" } else { "recognise" };
    let who = join_names(&names);
    tracing::warn!(
        "[Membership] Dropped from cluster '{}': {} no longer {} this device. Re-pair to rejoin.",
        info.cluster_name, who, verb
    );
    let _ = app.emit("cluster-dropped", &info);

    // Notify once per cluster: the banner covers every later restart.
    let cluster_id = state.cluster_id.lock().unwrap().clone();
    if crate::storage::load_dropped_notice(app).as_deref() == Some(cluster_id.as_str()) {
        tracing::debug!("[Membership] Already notified about being dropped from this cluster");
        return;
    }
    crate::storage::save_dropped_notice(app, &cluster_id);
    crate::send_notification(
        app,
        &format!("Dropped from {}", info.cluster_name),
        &format!(
            "{} no longer {} this device. Open ClusterCut to rejoin {}.",
            who, verb, info.cluster_name
        ),
        false,
        None,
        "devices",
        crate::NotificationPayload::None,
    );
}

fn forget_departed(app: &tauri::AppHandle, state: &AppState, departed: &[Departed]) {
    // Lock order: known_peers before peers (matches prune/reset).
    let forgotten = {
        let mut kp = state.known_peers.lock().unwrap();
        let ids = known_entries_for(departed, &kp);
        for id in &ids {
            kp.remove(id);
        }
        if !ids.is_empty() {
            crate::storage::save_known_peers(app, &kp);
        }
        ids
    };
    // So a member that still lists it can't gossip it back via membership sync.
    state
        .removed_peer_tombstones
        .lock()
        .unwrap()
        .extend(forgotten.iter().cloned());

    let mut updated = Vec::new();
    {
        let mut peers = state.peers.lock().unwrap();
        for id in forgotten.iter().chain(departed.iter().map(|d| &d.id)) {
            if let Some(p) = peers.get_mut(id).filter(|p| p.is_trusted) {
                p.is_trusted = false;
                updated.push(crate::peer::PeerView::from_peer(p));
            }
        }
    }
    for view in updated {
        let _ = app.emit("peer-update", view);
    }
    for d in departed {
        tracing::info!(
            "[Membership] {} ({}) moved to another cluster and no longer recognises this device; forgot it",
            d.hostname, d.id
        );
    }
    if !forgotten.is_empty() {
        tracing::info!("[Membership] Removed known-peer entries: {:?}", forgotten);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn refuse(t: &mut MembershipTracker, id: &str, aff: Affiliation, at: Instant) {
        t.record_refusal(id, &format!("host-{id}"), Some(vec![id.len() as u8]), aff, at);
    }

    #[test]
    fn dropped_once_refusals_outlast_the_window_with_nobody_accepting() {
        let t0 = Instant::now();
        let mut t = MembershipTracker::default();
        refuse(&mut t, "eir", Affiliation::Unknown, t0);
        refuse(&mut t, "mimir", Affiliation::Unknown, t0);

        // Not yet: a member that missed our pairing may still be catching up.
        refuse(&mut t, "eir", Affiliation::Unknown, t0 + secs(30));
        assert_eq!(t.evaluate("vassallo.cloud", t0 + secs(30)), Changes::default());

        refuse(&mut t, "eir", Affiliation::Unknown, t0 + DROP_AFTER);
        let changes = t.evaluate("vassallo.cloud", t0 + DROP_AFTER);
        let info = changes.dropped.expect("should be dropped");
        assert_eq!(info.cluster_name, "vassallo.cloud");
        let hosts: Vec<_> = info.devices.iter().map(|d| d.hostname.as_str()).collect();
        assert_eq!(hosts, ["host-eir", "host-mimir"]);
        assert!(t.is_dropped());

        // Declared once, not on every later round.
        assert_eq!(t.evaluate("vassallo.cloud", t0 + DROP_AFTER + secs(5)).dropped, None);
    }

    #[test]
    fn any_acceptance_keeps_us_in_the_cluster() {
        let t0 = Instant::now();
        let mut t = MembershipTracker::default();
        refuse(&mut t, "eir", Affiliation::Unknown, t0);
        t.record_accepted(Some("ubuntu"), t0 + secs(50));
        refuse(&mut t, "eir", Affiliation::Unknown, t0 + secs(70));
        assert!(t.evaluate("c", t0 + secs(70)).dropped.is_none());
        assert!(!t.is_dropped());
    }

    #[test]
    fn refusals_from_our_own_cluster_take_longer_to_count() {
        let t0 = Instant::now();
        let mut t = MembershipTracker::default();
        // Probably a member that hasn't heard of us yet...
        refuse(&mut t, "eir", Affiliation::Ours, t0);
        refuse(&mut t, "eir", Affiliation::Ours, t0 + DROP_AFTER);
        assert!(t.evaluate("c", t0 + DROP_AFTER).dropped.is_none());
        // ...but still refusing, with nobody accepting us, means we were
        // removed while offline.
        let at = t0 + SAME_CLUSTER_DROP_AFTER;
        refuse(&mut t, "eir", Affiliation::Ours, at);
        assert!(t.evaluate("c", at).dropped.is_some());
    }

    #[test]
    fn a_device_refusing_under_two_ids_is_listed_once() {
        let t0 = Instant::now();
        let mut t = MembershipTracker::default();
        t.record_refusal("eir-new", "eir", None, Affiliation::Other, t0);
        t.record_refusal("eir-old", "eir", None, Affiliation::Unknown, t0);
        let at = t0 + DROP_AFTER;
        t.record_refusal("eir-new", "eir", None, Affiliation::Other, at);
        let info = t.evaluate("c", at).dropped.expect("dropped");
        assert_eq!(info.devices.len(), 1);
    }

    #[test]
    fn acceptance_after_dropping_rejoins() {
        let t0 = Instant::now();
        let mut t = MembershipTracker::default();
        refuse(&mut t, "eir", Affiliation::Unknown, t0);
        refuse(&mut t, "eir", Affiliation::Unknown, t0 + DROP_AFTER);
        assert!(t.evaluate("c", t0 + DROP_AFTER).dropped.is_some());

        assert!(t.record_accepted(Some("eir"), t0 + DROP_AFTER + secs(5)));
        let changes = t.evaluate("c", t0 + DROP_AFTER + secs(10));
        assert!(changes.rejoined);
        assert!(!t.is_dropped());
    }

    #[test]
    fn reset_reports_whether_we_were_dropped() {
        let t0 = Instant::now();
        let mut t = MembershipTracker::default();
        assert!(!t.reset());
        refuse(&mut t, "eir", Affiliation::Unknown, t0);
        refuse(&mut t, "eir", Affiliation::Unknown, t0 + DROP_AFTER);
        t.evaluate("c", t0 + DROP_AFTER);
        assert!(t.reset());
        assert!(!t.is_dropped());
    }

    #[test]
    fn stale_refusals_expire() {
        let t0 = Instant::now();
        let mut t = MembershipTracker::default();
        refuse(&mut t, "eir", Affiliation::Unknown, t0);
        // Went quiet long ago: no longer evidence.
        assert!(t.evaluate("c", t0 + REFUSAL_TTL + secs(1)).dropped.is_none());
    }

    #[test]
    fn device_advertising_another_cluster_departs_while_others_accept_us() {
        let t0 = Instant::now();
        let mut t = MembershipTracker::default();
        refuse(&mut t, "eir", Affiliation::Other, t0);
        refuse(&mut t, "mimir", Affiliation::Unknown, t0);

        let at = t0 + DEPART_AFTER;
        refuse(&mut t, "eir", Affiliation::Other, at);
        refuse(&mut t, "mimir", Affiliation::Unknown, at);
        t.record_accepted(Some("ubuntu"), at);
        let changes = t.evaluate("c", at);

        // Only the one advertising another cluster; a same-name refuser may
        // be our own member catching up.
        assert_eq!(changes.departed.len(), 1);
        assert_eq!(changes.departed[0].id, "eir");
        assert!(changes.dropped.is_none());
    }

    #[test]
    fn nothing_departs_before_the_window() {
        let t0 = Instant::now();
        let mut t = MembershipTracker::default();
        refuse(&mut t, "eir", Affiliation::Other, t0);
        let at = t0 + DEPART_AFTER - secs(5);
        refuse(&mut t, "eir", Affiliation::Other, at);
        t.record_accepted(None, at);
        assert!(t.evaluate("c", at).departed.is_empty());
    }

    fn known(id: &str, fp: u8) -> (String, Peer) {
        (
            id.to_string(),
            Peer {
                id: id.to_string(),
                ip: "192.168.96.108".parse().unwrap(),
                port: 4654,
                hostname: id.to_string(),
                last_seen: 0,
                is_trusted: true,
                is_manual: false,
                network_name: None,
                signature: None,
                fingerprint: Some(vec![fp]),
                protocol_version: None,
                cluster_tag: None,
            },
        )
    }

    #[test]
    fn affiliation_prefers_tags_over_names() {
        let (_, mut p) = known("x", 1);
        p.network_name = Some("vassallo.cloud".into());
        // No tag: a shared name proves nothing either way.
        assert_eq!(affiliation(&p, "vassallo.cloud", "aaaa"), Affiliation::Unknown);
        p.cluster_tag = Some("aaaa".into());
        assert_eq!(affiliation(&p, "vassallo.cloud", "aaaa"), Affiliation::Ours);
        // The re-created cluster: same name, different tag.
        p.cluster_tag = Some("bbbb".into());
        assert_eq!(affiliation(&p, "vassallo.cloud", "aaaa"), Affiliation::Other);
        p.cluster_tag = None;
        p.network_name = Some("abundant-peace".into());
        assert_eq!(affiliation(&p, "vassallo.cloud", "aaaa"), Affiliation::Other);
    }

    #[test]
    fn departed_device_matches_old_ids_by_cert() {
        let kp: HashMap<String, Peer> =
            [known("eir-old", 7), known("eir-new", 7), known("mimir", 9)].into_iter().collect();
        let departed = [Departed { id: "eir-new".into(), hostname: "eir".into(), fingerprint: Some(vec![7]) }];
        assert_eq!(known_entries_for(&departed, &kp), ["eir-new", "eir-old"]);
    }

    #[test]
    fn cluster_switch_keeps_only_the_new_responder_and_placeholders() {
        let kp: HashMap<String, Peer> = [
            known("responder", 1),
            known("old-a", 2),
            known("old-b", 3),
            known("manual-192.168.1.5", 4),
        ]
        .into_iter()
        .collect();
        assert_eq!(stale_on_cluster_switch(&kp, "responder"), ["old-a", "old-b"]);
    }

    #[test]
    fn names_read_naturally() {
        let n = |v: &[&str]| join_names(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(n(&["eir"]), "eir");
        assert_eq!(n(&["eir", "mimir"]), "eir and mimir");
        assert_eq!(n(&["eir", "mimir", "ubuntu"]), "eir, mimir and ubuntu");
    }
}

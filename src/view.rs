//! Pure view reconciliation: folds the four independent sources of truth — CLI
//! rows, Health probe results, Host state, and the optimistic create/delete sets —
//! into a flat list of per-group views with nested per-port views.
//!
//! This module is deliberately free of any Slint, channel, or `Rc<RefCell>`
//! dependency: it takes plain references in and returns plain data out, so the
//! reconciliation invariants ("why does this port show this badge?") can be
//! unit-tested in isolation. The thin mapping from [`GroupViewData`] /
//! [`PortViewData`] onto the Slint structs, plus the tray-menu rebuild, stays in
//! `main.rs`.

use crate::devtunnel::Row;
use std::collections::{HashMap, HashSet};

/// Status id assigned to optimistic placeholder rows. Drives the
/// "Provisioning…" badge and disables the row's action buttons.
pub const PROVISIONING_STATUS: &str = "provisioning";

/// Per-port health status id, keyed by `(tunnel_id, port)`.
pub type ProbeMap = HashMap<(String, i32), String>;
/// Per-group host-state id ("host"/"hosting"/""), keyed by `tunnel_id`.
pub type HostMap = HashMap<String, String>;
/// Optimistic hidden-delete keys: `(tunnel_id, None)` hides a whole group;
/// `(tunnel_id, Some(port))` hides one port.
pub type HiddenSet = HashSet<(String, Option<i32>)>;

/// An optimistic placeholder inserted immediately when a create-group / add-port
/// operation is dispatched. Replaced by the real row when the op's refresh lands.
pub struct Placeholder {
    pub id: u64,
    pub group: String,
    pub port: i32,
    pub protocol: String,
}

/// Plain-data mirror of the Slint `PortView` struct (no Slint types).
#[derive(Debug, Clone, PartialEq)]
pub struct PortViewData {
    pub port: i32,
    pub protocol: String,
    pub url: String,
    /// "idle" | "ok" | "warn" | "down" | "host" | "provisioning".
    pub status: String,
    /// Stable index into the flat visible-row space (keys the detail panel);
    /// -1 for inert placeholder rows.
    pub row_index: i32,
}

/// Plain-data mirror of the Slint `GroupView` struct (no Slint types).
#[derive(Debug, Clone, PartialEq)]
pub struct GroupViewData {
    pub group: String,
    pub tunnel_id: String,
    pub expiration: String,
    pub hosting: bool,
    /// "" | "hosting" (this session) | "external" (another session).
    pub host_state: String,
    pub provisioning: bool,
    pub has_port: bool,
    pub ports: Vec<PortViewData>,
}

/// The four sources of truth fed into [`fold`], plus the expanded-port key.
pub struct FoldInput<'a> {
    /// Latest CLI data load (Real Tunnel ID order preserved).
    pub rows: &'a [Row],
    pub probe: &'a ProbeMap,
    pub host: &'a HostMap,
    pub hidden: &'a HiddenSet,
    pub placeholders: &'a [Placeholder],
    /// The currently-expanded port, keyed by `(tunnel_id, port)` (`None` = none).
    pub detail: Option<&'a (String, i32)>,
}

/// The reconciled result: the group list plus the few scalars `main.rs` needs to
/// drive the header chip and detail-panel selection.
pub struct FoldOutput {
    pub groups: Vec<GroupViewData>,
    /// Count of real service ports actually rendered into the cards (excludes
    /// portless groups, optimistically-hidden ports, and placeholders). Drives
    /// the header chip so it can never disagree with the cards.
    pub rendered_ports: usize,
    /// Flat index of the expanded port, recomputed against the visible rows
    /// (-1 = none).
    pub selected_index: i32,
    /// True when the expanded port no longer exists (deleted elsewhere): the
    /// caller collapses the panel so the metrics poll stops issuing CLI calls.
    pub stale_detail: bool,
}

/// Derives a port's `status` id from the latest probe + host state.
/// Probe result wins (it is the most specific); otherwise fall back to the
/// group's host state ("host" = hosting but not yet probed), then to the
/// service-reported `host_connections` count, then "idle".
pub fn derive_status(
    probe: &ProbeMap,
    host: &HostMap,
    tunnel_id: &str,
    port: i32,
    host_connections: i64,
) -> String {
    if let Some(s) = probe.get(&(tunnel_id.to_string(), port)) {
        return s.clone();
    }
    match host.get(tunnel_id).map(String::as_str) {
        Some("hosting") | Some("host") => "host".to_string(),
        _ if host_connections > 0 => "host".to_string(),
        _ => "idle".to_string(),
    }
}

/// Derives the group toggle / pill state:
/// - `"hosting"` when this session is actively hosting the group,
/// - `"external"` when the service reports active connections but this session is not hosting,
/// - `""` otherwise.
pub fn derive_host_state(host: &HostMap, tunnel_id: &str, host_connections: i64) -> String {
    match host.get(tunnel_id).map(String::as_str) {
        Some("hosting") | Some("host") => "hosting".to_string(),
        _ if host_connections > 0 => "external".to_string(),
        _ => String::new(),
    }
}

/// Folds the flat CLI rows (plus probe/host/hidden/placeholder state) into
/// per-group views. **Zero behavior change** from the original inline
/// `rebuild_rows` body — only Slint construction and the tray rebuild stay in
/// `main.rs`.
pub fn fold(input: &FoldInput) -> FoldOutput {
    let FoldInput {
        rows,
        probe,
        host,
        hidden,
        placeholders,
        detail,
    } = *input;

    let mut rendered_ports = 0usize;

    // Build a flat index space first: every visible (non-group-hidden) real port
    // gets a stable `row_index` used to key the expandable detail panel (#17).
    // A group-level delete (`(id, None)`) drops the whole card here; a port-level
    // delete (`(id, Some(port))`) keeps the row in the index space and is skipped
    // below when attaching ports, so deleting a group's last port leaves the card
    // standing (as portless) instead of flickering out and back.
    let visible_rows: Vec<&Row> = rows
        .iter()
        .filter(|r| !hidden.contains(&(r.tunnel_id.clone(), None)))
        .collect();

    // Fold the flat rows into groups (Real Tunnel ID order preserved).
    let mut groups: Vec<GroupViewData> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (flat_idx, r) in visible_rows.iter().enumerate() {
        let gi = match index.get(&r.tunnel_id) {
            Some(&i) => i,
            None => {
                index.insert(r.tunnel_id.clone(), groups.len());
                let host_state = derive_host_state(host, &r.tunnel_id, r.host_connections);
                groups.push(GroupViewData {
                    group: r.group.clone(),
                    tunnel_id: r.tunnel_id.clone(),
                    expiration: r.expiration.clone(),
                    hosting: host_state == "hosting",
                    // "Hosted elsewhere" pill: service reports connections but
                    // this session is not hosting the group (#15).
                    host_state,
                    provisioning: false,
                    has_port: false,
                    ports: Vec::new(),
                });
                groups.len() - 1
            }
        };
        // A port==0 row is a portless group: keep the card, skip the port row.
        // A port hidden by an optimistic delete (#13) likewise keeps its card but
        // drops the port row until the reflush refresh confirms the deletion.
        if r.port != 0 && !hidden.contains(&(r.tunnel_id.clone(), Some(r.port))) {
            groups[gi].has_port = true;
            rendered_ports += 1;
            groups[gi].ports.push(PortViewData {
                port: r.port,
                protocol: r.protocol.clone(),
                url: r.url.clone(),
                status: derive_status(probe, host, &r.tunnel_id, r.port, r.host_connections),
                row_index: flat_idx as i32,
            });
        }
    }

    // Optimistic placeholders for in-flight creates: attach the provisioning port
    // to its existing group (matched by friendly name) when possible, otherwise
    // add a whole provisioning card. Placeholders are inert, so they carry
    // row-index -1 (not expandable).
    for p in placeholders {
        match groups.iter().position(|g| g.group == p.group) {
            Some(gi) if p.port != 0 => groups[gi].ports.push(PortViewData {
                port: p.port,
                protocol: p.protocol.clone(),
                url: String::new(),
                status: PROVISIONING_STATUS.to_string(),
                row_index: -1,
            }),
            _ => {
                let ports = if p.port != 0 {
                    vec![PortViewData {
                        port: p.port,
                        protocol: p.protocol.clone(),
                        url: String::new(),
                        status: PROVISIONING_STATUS.to_string(),
                        row_index: -1,
                    }]
                } else {
                    Vec::new()
                };
                groups.push(GroupViewData {
                    group: p.group.clone(),
                    tunnel_id: String::new(),
                    expiration: String::new(),
                    hosting: false,
                    host_state: String::new(),
                    provisioning: true,
                    has_port: p.port != 0,
                    ports,
                });
            }
        }
    }

    // Recompute the expanded port's flat index: rows can reorder or disappear
    // across reloads, so the selection is keyed by (tunnel_id, port), not index.
    let mut selected_index = -1;
    let mut stale_detail = false;
    if let Some((tid, port)) = detail {
        // A port hidden by an optimistic delete is still in `visible_rows` (to
        // keep its group card alive), so check the hidden set too: deleting the
        // expanded port must collapse the panel rather than point at a gone row.
        let deleting =
            hidden.contains(&(tid.clone(), Some(*port))) || hidden.contains(&(tid.clone(), None));
        match visible_rows
            .iter()
            .position(|r| r.tunnel_id == tid.as_str() && r.port == *port)
        {
            Some(i) if !deleting => selected_index = i as i32,
            _ => stale_detail = true,
        }
    }

    FoldOutput {
        groups,
        rendered_ports,
        selected_index,
        stale_detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(tunnel_id: &str, port: i32) -> Row {
        Row {
            group: tunnel_id.to_string(),
            tunnel_id: tunnel_id.to_string(),
            port,
            protocol: "http".into(),
            url: "https://example.com".into(),
            expiration: "30d".into(),
            host_connections: 0,
        }
    }

    fn fold_rows(
        rows: &[Row],
        probe: &ProbeMap,
        host: &HostMap,
        hidden: &HiddenSet,
        placeholders: &[Placeholder],
        detail: Option<&(String, i32)>,
    ) -> FoldOutput {
        fold(&FoldInput {
            rows,
            probe,
            host,
            hidden,
            placeholders,
            detail,
        })
    }

    // ---- derive_status: badge mapping for the 3 probe states + fallbacks -----

    #[test]
    fn derive_status_maps_each_probe_state() {
        let host = HostMap::new();
        for (probe_id, expected) in [("ok", "ok"), ("warn", "warn"), ("down", "down")] {
            let mut probe = ProbeMap::new();
            probe.insert(("t1".into(), 3000), probe_id.to_string());
            assert_eq!(
                derive_status(&probe, &host, "t1", 3000, 0),
                expected,
                "probe state {probe_id} should win"
            );
        }
    }

    #[test]
    fn derive_status_probe_wins_over_host_and_connections() {
        let mut probe = ProbeMap::new();
        probe.insert(("t1".into(), 3000), "down".into());
        let mut host = HostMap::new();
        host.insert("t1".into(), "hosting".into());
        // Probe is most specific: it wins even while hosting with connections.
        assert_eq!(derive_status(&probe, &host, "t1", 3000, 5), "down");
    }

    #[test]
    fn derive_status_host_then_connections_then_idle() {
        let probe = ProbeMap::new();
        let mut host = HostMap::new();
        host.insert("t1".into(), "hosting".into());
        assert_eq!(derive_status(&probe, &host, "t1", 3000, 0), "host");
        host.insert("t1".into(), "host".into());
        assert_eq!(derive_status(&probe, &host, "t1", 3000, 0), "host");

        let empty = HostMap::new();
        // No host entry, but the service reports connections.
        assert_eq!(derive_status(&probe, &empty, "t1", 3000, 1), "host");
        // Nothing at all → idle.
        assert_eq!(derive_status(&probe, &empty, "t1", 3000, 0), "idle");
    }

    // ---- derive_host_state: hosting pill ------------------------------------

    #[test]
    fn derive_host_state_session_wins_over_service_count() {
        let mut host = HostMap::new();
        host.insert("t1".into(), "hosting".into());
        assert_eq!(derive_host_state(&host, "t1", 3), "hosting");
        host.insert("t1".into(), "host".into());
        assert_eq!(derive_host_state(&host, "t1", 1), "hosting");
    }

    #[test]
    fn derive_host_state_external_then_empty() {
        let host = HostMap::new();
        assert_eq!(derive_host_state(&host, "t1", 2), "external");
        assert_eq!(derive_host_state(&host, "t1", 0), "");
    }

    // ---- fold: host state → hosting pill ------------------------------------

    #[test]
    fn fold_sets_group_hosting_pill_from_host_state() {
        let rows = vec![row("t1", 3000)];
        let mut host = HostMap::new();
        host.insert("t1".into(), "hosting".into());
        let out = fold_rows(&rows, &ProbeMap::new(), &host, &HiddenSet::new(), &[], None);
        assert_eq!(out.groups.len(), 1);
        assert!(out.groups[0].hosting);
        assert_eq!(out.groups[0].host_state, "hosting");
        assert_eq!(out.groups[0].ports[0].status, "host");
    }

    #[test]
    fn fold_external_pill_not_hosting() {
        let mut rows = vec![row("t1", 3000)];
        rows[0].host_connections = 2;
        let out = fold_rows(
            &rows,
            &ProbeMap::new(),
            &HostMap::new(),
            &HiddenSet::new(),
            &[],
            None,
        );
        assert!(!out.groups[0].hosting);
        assert_eq!(out.groups[0].host_state, "external");
    }

    // ---- fold: optimistic-delete hiding -------------------------------------

    #[test]
    fn fold_hides_single_port_keeps_card() {
        let rows = vec![row("t1", 3000), row("t1", 8080)];
        let mut hidden = HiddenSet::new();
        hidden.insert(("t1".into(), Some(3000)));
        let out = fold_rows(&rows, &ProbeMap::new(), &HostMap::new(), &hidden, &[], None);
        // One group card, only the un-hidden port rendered.
        assert_eq!(out.groups.len(), 1);
        assert_eq!(out.groups[0].ports.len(), 1);
        assert_eq!(out.groups[0].ports[0].port, 8080);
        assert_eq!(out.rendered_ports, 1);
    }

    #[test]
    fn fold_hides_whole_group() {
        let rows = vec![row("t1", 3000), row("t2", 9000)];
        let mut hidden = HiddenSet::new();
        hidden.insert(("t1".into(), None));
        let out = fold_rows(&rows, &ProbeMap::new(), &HostMap::new(), &hidden, &[], None);
        assert_eq!(out.groups.len(), 1);
        assert_eq!(out.groups[0].tunnel_id, "t2");
        assert_eq!(out.rendered_ports, 1);
    }

    #[test]
    fn fold_hiding_last_port_leaves_portless_card() {
        let rows = vec![row("t1", 3000)];
        let mut hidden = HiddenSet::new();
        hidden.insert(("t1".into(), Some(3000)));
        let out = fold_rows(&rows, &ProbeMap::new(), &HostMap::new(), &hidden, &[], None);
        // Card stands, but has no port and is excluded from the header count.
        assert_eq!(out.groups.len(), 1);
        assert!(!out.groups[0].has_port);
        assert!(out.groups[0].ports.is_empty());
        assert_eq!(out.rendered_ports, 0);
    }

    // ---- fold: placeholder folding ------------------------------------------

    #[test]
    fn fold_attaches_placeholder_port_to_existing_group() {
        let rows = vec![row("t1", 3000)];
        let placeholders = vec![Placeholder {
            id: 1,
            group: "t1".into(), // matches the friendly name of the existing group
            port: 4000,
            protocol: "tcp".into(),
        }];
        let out = fold_rows(
            &rows,
            &ProbeMap::new(),
            &HostMap::new(),
            &HiddenSet::new(),
            &placeholders,
            None,
        );
        assert_eq!(out.groups.len(), 1);
        assert_eq!(out.groups[0].ports.len(), 2);
        let prov = &out.groups[0].ports[1];
        assert_eq!(prov.port, 4000);
        assert_eq!(prov.status, PROVISIONING_STATUS);
        assert_eq!(prov.row_index, -1);
        // Placeholders never inflate the real-port header count.
        assert_eq!(out.rendered_ports, 1);
    }

    #[test]
    fn fold_adds_new_provisioning_card_for_new_group() {
        let placeholders = vec![Placeholder {
            id: 1,
            group: "brand-new".into(),
            port: 5000,
            protocol: "http".into(),
        }];
        let out = fold_rows(
            &[],
            &ProbeMap::new(),
            &HostMap::new(),
            &HiddenSet::new(),
            &placeholders,
            None,
        );
        assert_eq!(out.groups.len(), 1);
        assert!(out.groups[0].provisioning);
        assert!(out.groups[0].tunnel_id.is_empty());
        assert_eq!(out.groups[0].ports[0].status, PROVISIONING_STATUS);
        assert_eq!(out.rendered_ports, 0);
    }

    #[test]
    fn fold_portless_placeholder_group_has_no_port() {
        let placeholders = vec![Placeholder {
            id: 1,
            group: "new-group".into(),
            port: 0,
            protocol: String::new(),
        }];
        let out = fold_rows(
            &[],
            &ProbeMap::new(),
            &HostMap::new(),
            &HiddenSet::new(),
            &placeholders,
            None,
        );
        assert_eq!(out.groups.len(), 1);
        assert!(out.groups[0].provisioning);
        assert!(!out.groups[0].has_port);
        assert!(out.groups[0].ports.is_empty());
    }

    // ---- fold: detail-panel selection reconciliation ------------------------

    #[test]
    fn fold_selects_expanded_port_by_key() {
        let rows = vec![row("t1", 3000), row("t1", 8080)];
        let detail = ("t1".to_string(), 8080);
        let out = fold_rows(
            &rows,
            &ProbeMap::new(),
            &HostMap::new(),
            &HiddenSet::new(),
            &[],
            Some(&detail),
        );
        assert_eq!(out.selected_index, 1);
        assert!(!out.stale_detail);
    }

    #[test]
    fn fold_collapses_when_expanded_port_deleted() {
        let rows = vec![row("t1", 3000)];
        let mut hidden = HiddenSet::new();
        hidden.insert(("t1".into(), Some(3000)));
        let detail = ("t1".to_string(), 3000);
        let out = fold_rows(
            &rows,
            &ProbeMap::new(),
            &HostMap::new(),
            &hidden,
            &[],
            Some(&detail),
        );
        assert_eq!(out.selected_index, -1);
        assert!(out.stale_detail);
    }

    #[test]
    fn fold_collapses_when_expanded_port_absent() {
        let rows = vec![row("t1", 3000)];
        let detail = ("t1".to_string(), 9999); // never existed
        let out = fold_rows(
            &rows,
            &ProbeMap::new(),
            &HostMap::new(),
            &HiddenSet::new(),
            &[],
            Some(&detail),
        );
        assert_eq!(out.selected_index, -1);
        assert!(out.stale_detail);
    }
}

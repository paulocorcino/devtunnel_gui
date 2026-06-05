//! Types that map the JSON (`-j`) output of the `devtunnel` CLI.
//! The service is the source of truth; these structs only deserialize what it returns.
//! Some fields are not yet read by the UI but reflect the CLI contract and
//! will be used in upcoming slices (badges, port counts, etc.).
#![allow(dead_code)]

use serde::Deserialize;

/// Output of `devtunnel list -j`.
#[derive(Debug, Deserialize)]
pub struct TunnelList {
    #[serde(default)]
    pub tunnels: Vec<TunnelSummary>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelSummary {
    pub tunnel_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub host_connections: i64,
    #[serde(default)]
    pub port_count: i64,
    #[serde(default)]
    pub tunnel_expiration: String,
    #[serde(default)]
    pub description: String,
}

/// Output of `devtunnel show <id> -j` (richer: includes portUri and accessControl).
#[derive(Debug, Deserialize)]
pub struct ShowResult {
    pub tunnel: TunnelDetail,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelDetail {
    pub tunnel_id: String,
    #[serde(default)]
    pub tunnel_expiration: String,
    #[serde(default)]
    pub ports: Vec<PortDetail>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortDetail {
    pub port_number: i32,
    #[serde(default)]
    pub protocol: String,
    /// The real Public URL. Comes from `show`; cannot be constructed manually.
    #[serde(default)]
    pub port_uri: Option<String>,
}

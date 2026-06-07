//! Types that map the JSON (`-j`) output of the `devtunnel` CLI.
//! The service is the source of truth; these structs only deserialize what it returns.
//! Some fields are not yet read by the UI but reflect the CLI contract and
//! will be used in upcoming slices (badges, port counts, etc.).
#![allow(dead_code)]

use serde::{Deserialize, Deserializer};

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
    /// Live traffic/connection metrics. Only present (as an object) while the
    /// port is hosted; when idle the CLI returns `status` as a plain summary
    /// string (e.g. `"0 client connections"`), so [`flex_status`] degrades any
    /// non-object shape to `None` instead of failing the whole row.
    #[serde(default, deserialize_with = "flex_status")]
    pub status: Option<PortStatus>,
}

/// Tolerates the two shapes `status` takes in `show -j` output: an object of
/// metrics (while hosting) or a summary string / null (while idle). Only the
/// object shape is mapped to [`PortStatus`]; anything else becomes `None`.
/// Without this, a string `status` would fail the entire `ShowResult` parse and
/// drop every port from the row.
fn flex_status<'de, D>(deserializer: D) -> Result<Option<PortStatus>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        Some(serde_json::Value::Object(_)) => {
            serde_json::from_value(value.unwrap()).map_err(serde::de::Error::custom)
        }
        _ => Ok(None),
    }
}

/// The `status` block of a port in `show -j` output. Every field is optional:
/// the CLI omits metrics it does not track, and some appear either as a plain
/// number or as an object (`{ "current": …, "rateBySeconds": … }`) — both
/// shapes are accepted via [`flex_num`].
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortStatus {
    #[serde(default, deserialize_with = "flex_num")]
    pub current_upload_rate: Option<f64>,
    #[serde(default, deserialize_with = "flex_num")]
    pub current_download_rate: Option<f64>,
    #[serde(default, deserialize_with = "flex_num")]
    pub upload_total: Option<f64>,
    #[serde(default, deserialize_with = "flex_num")]
    pub download_total: Option<f64>,
    #[serde(default, deserialize_with = "flex_num")]
    pub client_connection_count: Option<f64>,
}

/// Accepts a metric that is either a bare number or an object exposing a
/// `current` / `count` field (the service's `RateStatus` / `ResourceStatus`
/// shapes). Anything else deserializes to `None` instead of failing the row.
fn flex_num<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.as_ref().and_then(|v| match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::Object(m) => m
            .get("current")
            .or_else(|| m.get("count"))
            .and_then(|x| x.as_f64()),
        _ => None,
    }))
}

#[cfg(test)]
mod tests {
    use super::PortDetail;

    #[test]
    fn deserializes_port_without_status() {
        let json = r#"{ "portNumber": 3000, "protocol": "http",
                        "portUri": "https://x.devtunnels.ms/" }"#;
        let p: PortDetail = serde_json::from_str(json).unwrap();
        assert_eq!(p.port_number, 3000);
        assert!(p.status.is_none());
    }

    #[test]
    fn deserializes_port_with_numeric_status() {
        let json = r#"{ "portNumber": 3000, "protocol": "http",
                        "status": {
                            "currentUploadRate": 12.5,
                            "currentDownloadRate": 40,
                            "uploadTotal": 1024,
                            "downloadTotal": 2048,
                            "clientConnectionCount": 3
                        } }"#;
        let p: PortDetail = serde_json::from_str(json).unwrap();
        let s = p.status.unwrap();
        assert_eq!(s.current_upload_rate, Some(12.5));
        assert_eq!(s.current_download_rate, Some(40.0));
        assert_eq!(s.upload_total, Some(1024.0));
        assert_eq!(s.download_total, Some(2048.0));
        assert_eq!(s.client_connection_count, Some(3.0));
    }

    #[test]
    fn deserializes_port_with_object_shaped_metrics() {
        // The service can wrap rates/counts in RateStatus / ResourceStatus objects.
        let json = r#"{ "portNumber": 8080,
                        "status": {
                            "currentUploadRate": { "current": 99 },
                            "clientConnectionCount": { "count": 2 }
                        } }"#;
        let p: PortDetail = serde_json::from_str(json).unwrap();
        let s = p.status.unwrap();
        assert_eq!(s.current_upload_rate, Some(99.0));
        assert_eq!(s.client_connection_count, Some(2.0));
        // Absent fields degrade to None, not an error.
        assert!(s.upload_total.is_none());
        assert!(s.download_total.is_none());
        assert!(s.current_download_rate.is_none());
    }

    #[test]
    fn deserializes_port_with_string_status() {
        // The real CLI returns `status` as a summary STRING when the port is
        // idle (not hosting). This must not fail the parse — regression for the
        // "ports disappear" bug where a string status dropped every port.
        let json = r#"{ "portNumber": 3000, "protocol": "http",
                        "portUri": "https://x.devtunnels.ms/",
                        "status": "0 client connections" }"#;
        let p: PortDetail = serde_json::from_str(json).unwrap();
        assert_eq!(p.port_number, 3000);
        assert!(p.status.is_none());
    }

    #[test]
    fn deserializes_port_with_null_status() {
        let json = r#"{ "portNumber": 8080, "status": null }"#;
        let p: PortDetail = serde_json::from_str(json).unwrap();
        assert!(p.status.is_none());
    }

    #[test]
    fn unexpected_metric_shape_degrades_to_none() {
        let json = r#"{ "portNumber": 1, "status": { "uploadTotal": "fast" } }"#;
        let p: PortDetail = serde_json::from_str(json).unwrap();
        assert!(p.status.unwrap().upload_total.is_none());
    }
}

//! Tipos que mapeiam a saída JSON (`-j`) do CLI `devtunnel`.
//! O serviço é a fonte da verdade; estes structs só desserializam o que ele retorna.
//! Alguns campos ainda não são lidos pela UI, mas refletem o contrato do CLI e
//! serão usados nas próximas fatias (badges, contagem de portas, etc.).
#![allow(dead_code)]

use serde::Deserialize;

/// Saída de `devtunnel list -j`.
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

/// Saída de `devtunnel show <id> -j` (mais rica: traz portUri e accessControl).
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
    /// A Public URL real. Vem do `show`, não dá para construir manualmente.
    #[serde(default)]
    pub port_uri: Option<String>,
}

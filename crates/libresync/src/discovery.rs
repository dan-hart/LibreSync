use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};
use std::{env, process::Command};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Deserialize;

use crate::{Error, Identity, Result};

/// mDNS service type advertised and browsed for peers.
pub const SERVICE_TYPE: &str = "_libresync._tcp.local.";
pub const DEFAULT_SYNC_PORT: u16 = 52345;
const OVERLAY_PEERS_ENV: &str = "LIBRESYNC_OVERLAY_PEERS";
/// Overrides the Tailscale local API socket path (feature `tailscale-local-api`).
pub const TAILSCALE_SOCKET_ENV: &str = "LIBRESYNC_TAILSCALE_SOCKET";
/// Default Tailscale local API socket on Linux.
pub const DEFAULT_TAILSCALE_SOCKET: &str = "/var/run/tailscale/tailscaled.sock";

const TAILSCALE_CLI_UNKNOWN: u8 = 0;
const TAILSCALE_CLI_AVAILABLE: u8 = 1;
const TAILSCALE_CLI_UNAVAILABLE: u8 = 2;

/// Remembers whether the `tailscale` CLI is usable in this process. Inside a
/// Flatpak sandbox (or on machines without Tailscale) the first probe fails and
/// no further attempts are made, so discovery does not spawn a failing process
/// every few seconds.
static TAILSCALE_CLI_STATE: AtomicU8 = AtomicU8::new(TAILSCALE_CLI_UNKNOWN);

/// Availability of the `tailscale` CLI as observed by discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailscaleCliState {
    /// Not probed yet.
    Unknown,
    Available,
    /// The CLI is missing or failed; discovery no longer tries it. Call
    /// [`reset_tailscale_probe`] to try again (e.g. after the user installed it).
    Unavailable,
}

pub fn tailscale_cli_state() -> TailscaleCliState {
    match TAILSCALE_CLI_STATE.load(Ordering::SeqCst) {
        TAILSCALE_CLI_AVAILABLE => TailscaleCliState::Available,
        TAILSCALE_CLI_UNAVAILABLE => TailscaleCliState::Unavailable,
        _ => TailscaleCliState::Unknown,
    }
}

/// Forgets a failed `tailscale` CLI probe so the next discovery tries again.
pub fn reset_tailscale_probe() {
    TAILSCALE_CLI_STATE.store(TAILSCALE_CLI_UNKNOWN, Ordering::SeqCst);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoverySource {
    Mdns,
    Tailscale,
    StaticOverlay,
}

#[derive(Clone, Debug)]
pub struct DiscoveredDevice {
    pub identity: Identity,
    pub address: SocketAddr,
    pub source: DiscoverySource,
}

pub struct MdnsAdvertiser {
    mdns: ServiceDaemon,
}

impl MdnsAdvertiser {
    pub fn shutdown(self) -> Result<()> {
        self.mdns
            .shutdown()
            .map_err(|error| Error::Protocol(error.to_string()))?;
        Ok(())
    }
}

impl Drop for MdnsAdvertiser {
    fn drop(&mut self) {
        let _ = self.mdns.shutdown();
    }
}

pub fn register_mdns(identity: &Identity, listen: SocketAddr) -> Result<MdnsAdvertiser> {
    let mdns = ServiceDaemon::new().map_err(|error| Error::Protocol(error.to_string()))?;
    let mut properties = HashMap::new();
    properties.insert("app_id".to_string(), identity.app_id.clone());
    properties.insert("device_id".to_string(), identity.device_id.clone());
    properties.insert("user_id".to_string(), identity.user_id.clone());

    let ips = local_ips(listen.ip())?;
    let host_name = format!("{}.local.", identity.device_id);
    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &identity.device_id,
        &host_name,
        ips.as_slice(),
        listen.port(),
        properties,
    )
    .map_err(|error| Error::Protocol(error.to_string()))?;

    mdns.register(service)
        .map_err(|error| Error::Protocol(error.to_string()))?;

    Ok(MdnsAdvertiser { mdns })
}

pub fn browse_mdns(app_id: &str, timeout: Duration) -> Result<Vec<DiscoveredDevice>> {
    let mdns = ServiceDaemon::new().map_err(|error| Error::Protocol(error.to_string()))?;
    let receiver = mdns
        .browse(SERVICE_TYPE)
        .map_err(|error| Error::Protocol(error.to_string()))?;
    let start = Instant::now();
    let mut devices: BTreeMap<String, DiscoveredDevice> = BTreeMap::new();

    while start.elapsed() < timeout {
        let remaining = timeout.saturating_sub(start.elapsed());
        let event = receiver.recv_timeout(remaining.min(Duration::from_millis(200)));
        let event = match event {
            Ok(event) => event,
            Err(flume::RecvTimeoutError::Timeout) => continue,
            Err(error) => return Err(Error::Protocol(error.to_string())),
        };

        if let ServiceEvent::ServiceResolved(info) = event {
            if let Some(device) = parse_service_info(&info, app_id) {
                devices.insert(device.identity.device_id.clone(), device);
            }
        }
    }

    mdns.shutdown()
        .map_err(|error| Error::Protocol(error.to_string()))?;
    Ok(devices.into_values().collect())
}

pub fn discover_devices(
    app_id: &str,
    timeout: Duration,
    overlay_port: u16,
) -> Result<Vec<DiscoveredDevice>> {
    let mut devices: BTreeMap<SocketAddr, DiscoveredDevice> = BTreeMap::new();
    let mut mdns_error = None;
    match browse_mdns(app_id, timeout) {
        Ok(found) => {
            for device in found {
                devices.insert(device.address, device);
            }
        }
        Err(error) => {
            mdns_error = Some(error);
        }
    }

    for device in browse_private_overlays(app_id, overlay_port) {
        devices.entry(device.address).or_insert(device);
    }

    if devices.is_empty() {
        if let Some(error) = mdns_error {
            return Err(error);
        }
    }

    Ok(devices.into_values().collect())
}

pub fn browse_private_overlays(app_id: &str, overlay_port: u16) -> Vec<DiscoveredDevice> {
    let mut devices: BTreeMap<SocketAddr, DiscoveredDevice> = BTreeMap::new();
    let mut tailscale_found = false;
    if tailscale_cli_state() != TailscaleCliState::Unavailable {
        if let Ok(found) = browse_tailscale_overlay(app_id, overlay_port) {
            tailscale_found = true;
            for device in found {
                devices.insert(device.address, device);
            }
        }
    }
    #[cfg(all(unix, feature = "tailscale-local-api"))]
    if !tailscale_found {
        let socket =
            env::var(TAILSCALE_SOCKET_ENV).unwrap_or_else(|_| DEFAULT_TAILSCALE_SOCKET.to_string());
        match browse_tailscale_local_api(app_id, overlay_port, std::path::Path::new(&socket)) {
            Ok(found) => {
                for device in found {
                    devices.entry(device.address).or_insert(device);
                }
            }
            Err(error) => log::debug!("tailscale local api unavailable at {socket}: {error}"),
        }
    }
    #[cfg(not(all(unix, feature = "tailscale-local-api")))]
    let _ = tailscale_found;

    if let Ok(raw) = env::var(OVERLAY_PEERS_ENV) {
        for device in parse_static_overlay_peers(app_id, overlay_port, &raw) {
            devices.entry(device.address).or_insert(device);
        }
    }

    devices.into_values().collect()
}

#[derive(Clone, Debug, Default, Deserialize)]
struct TailscaleStatus {
    #[serde(rename = "Self", default)]
    this_node: Option<TailscalePeer>,
    #[serde(rename = "Peer", default)]
    peers: HashMap<String, TailscalePeer>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct TailscalePeer {
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
    #[serde(rename = "Online", default)]
    online: Option<bool>,
}

fn browse_tailscale_overlay(app_id: &str, overlay_port: u16) -> Result<Vec<DiscoveredDevice>> {
    let output = match Command::new("tailscale")
        .args(["status", "--json"])
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            mark_tailscale_cli_unavailable(&format!("cannot run `tailscale`: {error}"));
            return Err(Error::Protocol(format!("tailscale status failed: {error}")));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        mark_tailscale_cli_unavailable(&format!(
            "`tailscale status` exited with {}: {}",
            output.status,
            stderr.trim()
        ));
        return Err(Error::Protocol("tailscale status failed".to_string()));
    }
    TAILSCALE_CLI_STATE.store(TAILSCALE_CLI_AVAILABLE, Ordering::SeqCst);

    let json = String::from_utf8(output.stdout)
        .map_err(|error| Error::Protocol(format!("invalid tailscale status output: {error}")))?;
    parse_tailscale_status(app_id, overlay_port, &json)
}

fn mark_tailscale_cli_unavailable(reason: &str) {
    let previous = TAILSCALE_CLI_STATE.swap(TAILSCALE_CLI_UNAVAILABLE, Ordering::SeqCst);
    if previous != TAILSCALE_CLI_UNAVAILABLE {
        log::info!(
            "tailscale overlay discovery disabled for this process ({reason}); \
             LAN mDNS and LIBRESYNC_OVERLAY_PEERS still work"
        );
    }
}

/// Reads peers from the Tailscale local API over its Unix socket. Used when
/// the `tailscale` CLI is not available, e.g. inside a Flatpak sandbox that
/// was granted `--filesystem=/var/run/tailscale`.
#[cfg(all(unix, feature = "tailscale-local-api"))]
pub fn browse_tailscale_local_api(
    app_id: &str,
    overlay_port: u16,
    socket_path: &std::path::Path,
) -> Result<Vec<DiscoveredDevice>> {
    let json = tailscale_local_api_get(socket_path, "/localapi/v0/status")?;
    parse_tailscale_status(app_id, overlay_port, &json)
}

#[cfg(all(unix, feature = "tailscale-local-api"))]
fn tailscale_local_api_get(socket_path: &std::path::Path, path: &str) -> Result<String> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    // HTTP/1.0 so the daemon closes the connection after the response and no
    // chunked decoding is needed. The Sec-Tailscale header is what the CLI
    // sends; it is required by newer daemons for non-browser clients.
    let request = format!(
        "GET {path} HTTP/1.0\r\nHost: local-tailscaled.sock\r\nSec-Tailscale: localapi\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    parse_http_response(&response)
}

#[cfg(all(unix, feature = "tailscale-local-api"))]
fn parse_http_response(response: &[u8]) -> Result<String> {
    let text = String::from_utf8_lossy(response);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| Error::Protocol("malformed local api response".to_string()))?;
    let status_line = head.lines().next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| Error::Protocol(format!("bad status line: {status_line}")))?;
    if status != 200 {
        return Err(Error::Protocol(format!(
            "local api returned {status}: {}",
            body.trim()
        )));
    }
    Ok(body.to_string())
}

fn parse_tailscale_status(
    app_id: &str,
    overlay_port: u16,
    json: &str,
) -> Result<Vec<DiscoveredDevice>> {
    let status: TailscaleStatus = serde_json::from_str(json)?;
    let mut self_ips = Vec::new();
    if let Some(node) = status.this_node {
        for value in node.tailscale_ips {
            if let Ok(ip) = value.parse::<IpAddr>() {
                self_ips.push(ip);
            }
        }
    }

    let mut devices: BTreeMap<SocketAddr, DiscoveredDevice> = BTreeMap::new();
    for peer in status.peers.into_values() {
        if peer.online == Some(false) {
            continue;
        }
        let ip = match pick_overlay_ip(&peer.tailscale_ips) {
            Some(ip) => ip,
            None => continue,
        };
        if self_ips.contains(&ip) {
            continue;
        }

        let label = overlay_label_for_peer(&peer);
        let identity = overlay_identity(app_id, &label, "tailnet-user", devices.len() + 1);
        let address = SocketAddr::new(ip, overlay_port);
        devices.insert(
            address,
            DiscoveredDevice {
                identity,
                address,
                source: DiscoverySource::Tailscale,
            },
        );
    }

    Ok(devices.into_values().collect())
}

fn pick_overlay_ip(values: &[String]) -> Option<IpAddr> {
    let mut ipv6 = None;
    for value in values {
        let ip = match value.parse::<IpAddr>() {
            Ok(ip) => ip,
            Err(_) => continue,
        };
        match ip {
            IpAddr::V4(_) => return Some(ip),
            IpAddr::V6(addr) => {
                if !addr.is_unicast_link_local() && ipv6.is_none() {
                    ipv6 = Some(IpAddr::V6(addr));
                }
            }
        }
    }
    ipv6
}

fn overlay_label_for_peer(peer: &TailscalePeer) -> String {
    if !peer.host_name.is_empty() {
        return peer.host_name.clone();
    }
    if !peer.dns_name.is_empty() {
        let trimmed = peer.dns_name.trim_end_matches('.');
        if let Some((label, _)) = trimmed.split_once('.') {
            if !label.is_empty() {
                return label.to_string();
            }
        }
        return trimmed.to_string();
    }
    "overlay-peer".to_string()
}

fn parse_static_overlay_peers(
    app_id: &str,
    overlay_port: u16,
    value: &str,
) -> Vec<DiscoveredDevice> {
    let mut devices = Vec::new();
    let mut seen = BTreeMap::<SocketAddr, ()>::new();
    for (index, token) in value
        .split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
        .enumerate()
    {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let address = match parse_overlay_address(token, overlay_port) {
            Some(address) => address,
            None => continue,
        };
        if seen.contains_key(&address) {
            continue;
        }
        seen.insert(address, ());
        let label = overlay_label_from_token(token);
        devices.push(DiscoveredDevice {
            identity: overlay_identity(app_id, &label, "overlay-user", index + 1),
            address,
            source: DiscoverySource::StaticOverlay,
        });
    }
    devices
}

fn parse_overlay_address(token: &str, default_port: u16) -> Option<SocketAddr> {
    if let Ok(address) = token.parse::<SocketAddr>() {
        return Some(address);
    }
    if let Ok(ip) = token.parse::<IpAddr>() {
        return Some(SocketAddr::new(ip, default_port));
    }
    None
}

fn overlay_label_from_token(token: &str) -> String {
    token
        .split(':')
        .next()
        .unwrap_or(token)
        .trim_matches(|ch: char| ch == '[' || ch == ']')
        .to_string()
}

fn overlay_identity(app_id: &str, label: &str, user: &str, fallback_index: usize) -> Identity {
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in label.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            previous_dash = false;
        } else if !previous_dash {
            slug.push('-');
            previous_dash = true;
        }
    }
    slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        slug = format!("peer-{fallback_index}");
    }
    Identity::new(format!("overlay-{slug}"), app_id, user)
}

fn parse_service_info(info: &ServiceInfo, expected_app_id: &str) -> Option<DiscoveredDevice> {
    let properties = info.get_properties();
    let app_id = properties.get("app_id")?.val_str();
    if app_id != expected_app_id {
        return None;
    }
    let device_id = properties.get("device_id")?.val_str();
    let user_id = properties.get("user_id")?.val_str();
    let address = pick_address(info, info.get_port())?;

    Some(DiscoveredDevice {
        identity: Identity::new(device_id, app_id, user_id),
        address,
        source: DiscoverySource::Mdns,
    })
}

fn pick_address(info: &ServiceInfo, port: u16) -> Option<SocketAddr> {
    let addresses = info.get_addresses();
    if let Some(ip) = addresses.iter().find_map(|addr| match addr {
        IpAddr::V4(ip) => Some(IpAddr::V4(*ip)),
        _ => None,
    }) {
        return Some(SocketAddr::new(ip, port));
    }

    if let Some(ip) = addresses.iter().find_map(|addr| match addr {
        IpAddr::V6(ip) if !ip.is_unicast_link_local() => Some(IpAddr::V6(*ip)),
        _ => None,
    }) {
        return Some(SocketAddr::new(ip, port));
    }

    let ip = addresses.iter().find_map(|addr| match addr {
        IpAddr::V6(ip) => Some(IpAddr::V6(*ip)),
        _ => None,
    })?;
    Some(SocketAddr::new(ip, port))
}

fn local_ips(listen_ip: IpAddr) -> Result<Vec<IpAddr>> {
    if !listen_ip.is_unspecified() {
        return Ok(vec![listen_ip]);
    }

    let mut ips = Vec::new();
    for iface in if_addrs::get_if_addrs()? {
        if iface.ip().is_loopback() {
            continue;
        }
        ips.push(iface.ip());
    }

    if ips.is_empty() {
        ips.push(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    }

    Ok(ips)
}

#[cfg(test)]
mod tests {
    use super::{
        browse_mdns, local_ips, parse_service_info, parse_static_overlay_peers,
        parse_tailscale_status, pick_address, DiscoverySource,
    };
    use crate::Identity;
    use mdns_sd::ServiceInfo;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::time::Duration;

    #[test]
    fn browse_mdns_returns_empty_for_short_timeout() {
        let devices = browse_mdns("com.example.app", Duration::from_millis(10)).expect("browse");
        assert!(devices.is_empty());
    }

    #[test]
    fn parse_service_info_filters_by_app_id() {
        let mut props = HashMap::new();
        props.insert("app_id".to_string(), "com.example.app".to_string());
        props.insert("device_id".to_string(), "device-a".to_string());
        props.insert("user_id".to_string(), "user-a".to_string());
        let addresses = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
        let info = ServiceInfo::new(
            "_libresync._tcp.local.",
            "device-a",
            "device-a.local.",
            addresses.as_slice(),
            1234,
            props,
        )
        .expect("service info");

        assert!(parse_service_info(&info, "com.other.app").is_none());
        let parsed = parse_service_info(&info, "com.example.app").expect("parsed");
        assert_eq!(parsed.identity.device_id, "device-a");
        assert_eq!(
            parsed.address,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234)
        );
        assert_eq!(parsed.source, DiscoverySource::Mdns);
    }

    #[test]
    fn pick_address_prefers_ipv4_then_ipv6() {
        let mut props = HashMap::new();
        props.insert("app_id".to_string(), "com.example.app".to_string());
        props.insert("device_id".to_string(), "device-a".to_string());
        props.insert("user_id".to_string(), "user-a".to_string());
        let addresses = vec![
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        ];
        let info = ServiceInfo::new(
            "_libresync._tcp.local.",
            "device-a",
            "device-a.local.",
            addresses.as_slice(),
            4321,
            props,
        )
        .expect("service info");

        let addr = pick_address(&info, 4321).expect("addr");
        assert_eq!(addr, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4321));
    }

    #[test]
    fn local_ips_returns_listen_ip_when_specified() {
        let ips = local_ips(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))).expect("ips");
        assert_eq!(ips, vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))]);
    }

    #[test]
    fn parse_tailscale_status_extracts_online_peers() {
        let json = r#"{
          "Self": {
            "HostName": "device-a",
            "TailscaleIPs": ["198.51.100.1"]
          },
          "Peer": {
            "peer1": {
              "HostName": "device-b",
              "TailscaleIPs": ["198.51.100.2"],
              "Online": true
            },
            "peer2": {
              "HostName": "device-c",
              "TailscaleIPs": ["198.51.100.3"],
              "Online": false
            }
          }
        }"#;

        let devices =
            parse_tailscale_status("com.example.app", 52345, json).expect("parse tailscale");
        assert_eq!(devices.len(), 1);
        let device = &devices[0];
        assert_eq!(device.address, "198.51.100.2:52345".parse().expect("addr"));
        assert_eq!(device.source, DiscoverySource::Tailscale);
        assert_eq!(device.identity.app_id, "com.example.app");
    }

    #[test]
    fn tailscale_cli_probe_is_remembered() {
        super::reset_tailscale_probe();
        assert_eq!(
            super::tailscale_cli_state(),
            super::TailscaleCliState::Unknown
        );
        super::mark_tailscale_cli_unavailable("test");
        assert_eq!(
            super::tailscale_cli_state(),
            super::TailscaleCliState::Unavailable
        );
        // Marked unavailable: discovery skips the CLI entirely and the state
        // stays unavailable (only the local API socket may still add peers).
        let _ = super::browse_private_overlays("com.example.app", 52345);
        assert_eq!(
            super::tailscale_cli_state(),
            super::TailscaleCliState::Unavailable
        );
        super::reset_tailscale_probe();
        assert_eq!(
            super::tailscale_cli_state(),
            super::TailscaleCliState::Unknown
        );
    }

    #[cfg(all(unix, feature = "tailscale-local-api"))]
    #[test]
    fn tailscale_local_api_reads_status_over_unix_socket() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("tailscaled.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0u8; 1024];
                let n = stream.read(&mut request).expect("read");
                let request = String::from_utf8_lossy(&request[..n]).to_string();
                let body = if request.starts_with("GET /localapi/v0/status ") {
                    r#"{"Self":{"HostName":"me","TailscaleIPs":["198.51.100.1"]},"Peer":{"p":{"HostName":"laptop","TailscaleIPs":["198.51.100.9"],"Online":true}}}"#
                } else {
                    "nope"
                };
                let status = if body == "nope" {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                let response =
                    format!("HTTP/1.0 {status}\r\nContent-Type: application/json\r\n\r\n{body}");
                stream.write_all(response.as_bytes()).expect("write");
                seen.push(request);
            }
            seen
        });

        let devices = super::browse_tailscale_local_api("com.example.app", 52345, &socket_path)
            .expect("local api");
        assert_eq!(devices.len(), 1);
        assert_eq!(
            devices[0].address,
            "198.51.100.9:52345".parse().expect("addr")
        );
        assert_eq!(devices[0].source, DiscoverySource::Tailscale);

        let error = super::tailscale_local_api_get(&socket_path, "/other").expect_err("404");
        assert!(error.to_string().contains("404"));
        let requests = server.join().expect("join");
        assert!(requests[0].contains("Sec-Tailscale: localapi"));

        assert!(super::browse_tailscale_local_api(
            "com.example.app",
            52345,
            &dir.path().join("missing.sock")
        )
        .is_err());
        assert!(super::parse_http_response(b"garbage").is_err());
        assert!(super::parse_http_response(b"HTTP/1.0 abc\r\n\r\n").is_err());
    }

    #[test]
    #[ignore = "needs a LAN interface and multicast; run with --ignored to verify firewall settings"]
    fn mdns_advertise_and_browse_round_trip() {
        let identity = Identity::new("mdns-probe-device", "com.example.mdnsprobe", "probe");
        let advertiser = super::register_mdns(&identity, "0.0.0.0:52345".parse().expect("addr"))
            .expect("register");
        let mut found = Vec::new();
        for _ in 0..5 {
            found = browse_mdns("com.example.mdnsprobe", Duration::from_secs(2)).expect("browse");
            if !found.is_empty() {
                break;
            }
        }
        advertiser.shutdown().expect("shutdown");
        assert!(
            found
                .iter()
                .any(|device| device.identity.device_id == "mdns-probe-device"),
            "own advertisement not visible: check that UDP 5353 is allowed (firewalld `mdns` \
             service / ufw 5353/udp) and that the interface allows multicast"
        );
    }

    #[test]
    fn parse_static_overlay_peers_parses_socket_and_ip() {
        let devices = parse_static_overlay_peers(
            "com.example.app",
            52345,
            "198.51.100.5:7000,198.51.100.6,invalid",
        );

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].address, "198.51.100.5:7000".parse().expect("addr"));
        assert_eq!(
            devices[1].address,
            "198.51.100.6:52345".parse().expect("addr")
        );
        assert_eq!(devices[0].source, DiscoverySource::StaticOverlay);
        assert_eq!(devices[1].source, DiscoverySource::StaticOverlay);
    }
}

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::{Error, Identity, Result};

const SERVICE_TYPE: &str = "_libresync._tcp.local.";

#[derive(Clone, Debug)]
pub struct DiscoveredDevice {
    pub identity: Identity,
    pub address: SocketAddr,
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
    use super::browse_mdns;
    use std::time::Duration;

    #[test]
    fn browse_mdns_returns_empty_for_short_timeout() {
        let devices = browse_mdns("com.example.app", Duration::from_millis(10)).expect("browse");
        assert!(devices.is_empty());
    }
}

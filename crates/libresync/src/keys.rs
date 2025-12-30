use rcgen::{Certificate, CertificateParams, DistinguishedName, DnType, SanType};
use sha2::{Digest, Sha256};

use crate::{Error, Identity, Result};

#[derive(Clone, Debug)]
pub struct DeviceKeys {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    fingerprint: String,
}

impl DeviceKeys {
    pub fn generate(identity: &Identity) -> Result<Self> {
        let mut params = CertificateParams::new(vec![identity.device_id.clone()]);
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, identity.device_id.clone());
        params
            .distinguished_name
            .push(DnType::OrganizationName, identity.app_id.clone());
        params
            .distinguished_name
            .push(DnType::OrganizationalUnitName, identity.user_id.clone());
        params.subject_alt_names = vec![
            SanType::DnsName(identity.device_id.clone()),
            SanType::Rfc822Name(identity.app_id.clone()),
        ];

        let cert = Certificate::from_params(params)
            .map_err(|error| Error::Crypto(error.to_string()))?;
        let cert_der = cert
            .serialize_der()
            .map_err(|error| Error::Crypto(error.to_string()))?;
        let key_der = cert.serialize_private_key_der();
        let fingerprint = fingerprint_cert(&cert_der);

        Ok(Self {
            cert_der,
            key_der,
            fingerprint,
        })
    }

    pub fn from_der(cert_der: Vec<u8>, key_der: Vec<u8>) -> Result<Self> {
        let fingerprint = fingerprint_cert(&cert_der);
        Ok(Self {
            cert_der,
            key_der,
            fingerprint,
        })
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    pub fn key_der(&self) -> &[u8] {
        &self.key_der
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

pub fn fingerprint_cert(cert_der: &[u8]) -> String {
    let hash = Sha256::digest(cert_der);
    let mut out = String::with_capacity(hash.len() * 2);
    for byte in hash {
        out.push(hex_char(byte >> 4));
        out.push(hex_char(byte & 0x0f));
    }
    out
}

fn hex_char(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'a' + (value - 10)) as char,
    }
}

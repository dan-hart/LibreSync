//! Stateless exact-checkpoint evidence, scoped to a durable peer enrollment.
use super::*;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
const DOMAIN: &[u8] = b"libresync-managed-receipt-v1";
fn mac(inner: &Inner, peer: &Peer, receipt: &ManagedReceipt) -> Result<Hmac<Sha256>> {
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(DOMAIN), inner.storage_key.as_bytes())
        .expand(b"private checkpoint proof signing", &mut key)
        .map_err(|_| Error::Crypto("receipt key derivation failed".into()))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key)
        .map_err(|_| Error::Crypto("receipt MAC key invalid".into()))?;
    for field in [
        DOMAIN,
        peer.identity.device_id.as_bytes(),
        peer.identity.app_id.as_bytes(),
        peer.identity.user_id.as_bytes(),
        peer.fingerprint.as_bytes(),
        peer.incarnation.as_bytes(),
        receipt.epoch.as_bytes(),
    ] {
        mac.update(&(field.len() as u64).to_be_bytes());
        mac.update(field);
    }
    mac.update(&receipt.sequence.to_be_bytes());
    Ok(mac)
}
pub(super) fn mint(inner: &Inner, e: &Envelope, peer: &Peer) -> Result<ManagedReceipt> {
    let mut receipt = ManagedReceipt {
        epoch: e.epoch.clone(),
        sequence: e.revision,
        proof: Vec::new(),
    };
    receipt.proof = mac(inner, peer, &receipt)?.finalize().into_bytes().to_vec();
    Ok(receipt)
}
pub(super) fn verify(
    inner: &Inner,
    e: &Envelope,
    peer: &Peer,
    receipt: &ManagedReceipt,
) -> Result<()> {
    if peer.revoked
        || peer.incarnation.is_empty()
        || receipt.epoch != e.epoch
        || receipt.sequence > e.revision
        || receipt.proof.len() != 32
    {
        return fail("receipt is outside current enrollment history");
    }
    mac(inner, peer, receipt)?
        .verify_slice(&receipt.proof)
        .map_err(|_| {
            Error::Protocol("checkpoint proof is not authentic for current enrollment".into())
        })
}

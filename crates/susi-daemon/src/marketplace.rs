//! Signed marketplace listings (Swarm OS Bullet 92)
//!
//! A listing is accepted only when the publisher's registered Ed25519 key
//! verifies a signature over the listing bytes. Ratings are the same:
//! an unsigned score is refused.

use crate::identity::IdentityManager;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub name: String,
    pub publisher: String,
    pub ratings: Vec<u8>,
}

pub fn publish(
    identities: &IdentityManager,
    publisher: &str,
    name: &str,
    signature_hex: &str,
) -> Result<Listing, String> {
    let payload = listing_bytes(publisher, name);
    if !identities.verify_signature(publisher, &payload, signature_hex) {
        return Err(format!("publisher '{publisher}' signature does not verify"));
    }
    Ok(Listing {
        name: name.to_string(),
        publisher: publisher.to_string(),
        ratings: Vec::new(),
    })
}

pub fn rate(
    identities: &IdentityManager,
    listing: &mut Listing,
    rater: &str,
    score: u8,
    signature_hex: &str,
) -> Result<(), String> {
    if !(1..=5).contains(&score) {
        return Err("rating score must be 1..=5".to_string());
    }
    let payload = format!("rate:{}:{score}", listing.name).into_bytes();
    if !identities.verify_signature(rater, &payload, signature_hex) {
        return Err(format!("rater '{rater}' signature does not verify"));
    }
    listing.ratings.push(score);
    Ok(())
}

fn listing_bytes(publisher: &str, name: &str) -> Vec<u8> {
    format!("listing:{publisher}:{name}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    #[test]
    fn an_unsigned_listing_is_refused_and_a_signed_rating_is_stored() {
        let mut identities = IdentityManager::new();
        let (publisher, publisher_key) = IdentityManager::generate_identity("pub").unwrap();
        identities.register_identity(&publisher).unwrap();
        assert!(publish(&identities, "pub", "cell-pack", "00").is_err());

        let payload = listing_bytes("pub", "cell-pack");
        let signature = hex::encode(publisher_key.sign(&payload).to_bytes());
        let mut listing = publish(&identities, "pub", "cell-pack", &signature).unwrap();

        let (rater, rater_key) = IdentityManager::generate_identity("rater").unwrap();
        identities.register_identity(&rater).unwrap();
        let rate_payload = b"rate:cell-pack:4";
        let rate_sig = hex::encode(rater_key.sign(rate_payload).to_bytes());
        rate(&identities, &mut listing, "rater", 4, &rate_sig).unwrap();
        assert_eq!(listing.ratings, vec![4]);
    }
}

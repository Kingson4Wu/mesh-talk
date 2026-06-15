//! Deterministic post-office election. Among the eligible (always-on) peers, the
//! post office is the one with the lowest identity fingerprint (`user_id`).
//! Because the rule is a pure function of the eligible set, every peer computes
//! the same answer without coordination.

use crate::identity::device::PublicIdentity;

/// Elect the post office: the eligible peer with the lowest `user_id` fingerprint,
/// or `None` if no peer is eligible.
pub fn elect(eligible: &[PublicIdentity]) -> Option<PublicIdentity> {
    eligible.iter().min_by_key(|p| p.user_id()).cloned()
}

/// Whether `me` is the elected post office for the given eligible set.
pub fn is_post_office(me: &PublicIdentity, eligible: &[PublicIdentity]) -> bool {
    elect(eligible).as_ref() == Some(me)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    fn identities(n: usize) -> Vec<PublicIdentity> {
        (0..n)
            .map(|_| DeviceIdentity::generate().public())
            .collect()
    }

    #[test]
    fn elects_lowest_fingerprint_independent_of_order() {
        let ids = identities(5);
        let expected = ids.iter().min_by_key(|p| p.user_id()).unwrap().clone();

        assert_eq!(elect(&ids).unwrap(), expected);

        // Order of the input must not change the result (every peer agrees).
        let mut reversed = ids.clone();
        reversed.reverse();
        assert_eq!(elect(&reversed).unwrap(), expected);
    }

    #[test]
    fn no_eligible_peers_means_no_post_office() {
        assert!(elect(&[]).is_none());
    }

    #[test]
    fn single_eligible_peer_is_elected() {
        let only = DeviceIdentity::generate().public();
        assert_eq!(elect(std::slice::from_ref(&only)), Some(only));
    }

    #[test]
    fn is_post_office_identifies_the_elected_node() {
        let ids = identities(4);
        let elected = elect(&ids).unwrap();
        assert!(is_post_office(&elected, &ids));

        // A different eligible peer is not the post office.
        let other = ids.iter().find(|p| **p != elected).unwrap();
        assert!(!is_post_office(other, &ids));
    }
}

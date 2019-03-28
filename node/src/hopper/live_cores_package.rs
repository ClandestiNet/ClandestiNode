// Copyright (c) 2017-2019, Substratum LLC (https://substratum.net) and/or its affiliates. All rights reserved.

use crate::sub_lib::cryptde::CryptData;
use crate::sub_lib::cryptde::PublicKey;
use crate::sub_lib::cryptde::{decodex, CryptDE};
use crate::sub_lib::hop::LiveHop;
use crate::sub_lib::hopper::IncipientCoresPackage;
use crate::sub_lib::hopper::{ExpiredCoresPackage, MessageType};
use crate::sub_lib::route::Route;
use crate::sub_lib::route::RouteError;
use serde_derive::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveCoresPackage {
    pub route: Route,
    pub payload: CryptData,
}

impl LiveCoresPackage {
    pub fn new(route: Route, payload: CryptData) -> LiveCoresPackage {
        LiveCoresPackage { route, payload }
    }

    pub fn to_next_live(
        mut self,
        cryptde: &dyn CryptDE, // must be the CryptDE of the Node to which the top hop is encrypted
    ) -> Result<(LiveHop, LiveCoresPackage), RouteError> {
        let next_hop = self.route.shift(cryptde)?;
        let next_live = LiveCoresPackage::new(self.route, self.payload);
        Ok((next_hop, next_live))
    }

    pub fn from_incipient(
        incipient: IncipientCoresPackage,
        cryptde: &dyn CryptDE, // must be the CryptDE of the Node to which the top hop is encrypted
    ) -> Result<(LiveCoresPackage, PublicKey), String> {
        let mut route = incipient.route.clone();
        let next_hop = match route.shift(cryptde) {
            Ok(h) => h,
            Err(e) => return Err(format!("Could not decrypt next hop: {:?}", e)),
        };
        Ok((
            LiveCoresPackage::new(route, incipient.payload),
            next_hop.public_key,
        ))
    }

    pub fn to_expired(
        self,
        immediate_neighbor_ip: IpAddr,
        cryptde: &dyn CryptDE, // Must be the CryptDE of the Node for which the payload is intended.
    ) -> Result<ExpiredCoresPackage<MessageType>, String> {
        let top_hop = match self.route.next_hop(cryptde) {
            Err(e) => return Err(format!("{:?}", e)),
            Ok(hop) => hop,
        };
        decodex::<MessageType>(cryptde, &self.payload).map(|decoded_payload| {
            ExpiredCoresPackage::new(
                immediate_neighbor_ip,
                top_hop.consuming_wallet,
                self.route,
                decoded_payload,
                self.payload.len(),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sub_lib::cryptde::encodex;
    use crate::sub_lib::cryptde::PlainData;
    use crate::sub_lib::cryptde_null::CryptDENull;
    use crate::sub_lib::dispatcher::Component;
    use crate::sub_lib::hopper::{IncipientCoresPackage, MessageType};
    use crate::sub_lib::proxy_client::DnsResolveFailure;
    use crate::sub_lib::route::Route;
    use crate::sub_lib::route::RouteSegment;
    use crate::sub_lib::wallet::Wallet;
    use crate::test_utils::test_utils::make_meaningless_route;
    use crate::test_utils::test_utils::{cryptde, make_meaningless_stream_key};
    use std::str::FromStr;

    #[test]
    fn live_cores_package_can_be_constructed_from_scratch() {
        let payload = CryptData::new(&[5, 6]);
        let cryptde = cryptde();
        let consuming_wallet = Wallet::new("wallet");
        let route = Route::one_way(
            RouteSegment::new(
                vec![&PublicKey::new(&[1, 2]), &PublicKey::new(&[3, 4])],
                Component::Neighborhood,
            ),
            cryptde,
            Some(consuming_wallet),
        )
        .unwrap();

        let subject = LiveCoresPackage::new(route.clone(), payload.clone());

        assert_eq!(subject.route, route);
        assert_eq!(subject.payload, payload);
    }

    #[test]
    fn live_cores_package_can_be_produced_from_older_live_cores_package() {
        let payload = MessageType::DnsResolveFailed(DnsResolveFailure {
            stream_key: make_meaningless_stream_key(),
        });
        let destination_key = PublicKey::new(&[3, 4]);
        let destination_cryptde = CryptDENull::from(&destination_key);
        let relay_key = PublicKey::new(&[1, 2]);
        let relay_cryptde = CryptDENull::from(&relay_key);
        let cryptde = cryptde();
        let serialized_payload = serde_cbor::ser::to_vec(&payload).unwrap();
        let encrypted_payload = cryptde
            .encode(&destination_key, &PlainData::new(&serialized_payload))
            .unwrap();
        let consuming_wallet = Wallet::new("wallet");
        let route = Route::one_way(
            RouteSegment::new(vec![&relay_key, &destination_key], Component::Neighborhood),
            cryptde,
            Some(consuming_wallet),
        )
        .unwrap();
        let subject = LiveCoresPackage::new(route.clone(), encrypted_payload.clone());

        let (next_hop, next_pkg) = subject.to_next_live(&relay_cryptde).unwrap();

        assert_eq!(
            next_hop,
            LiveHop::new(
                &destination_key,
                Some(Wallet::new("wallet")),
                Component::Hopper,
            )
        );
        assert_eq!(next_pkg.payload, encrypted_payload);
        let mut route = next_pkg.route.clone();
        assert_eq!(
            route.shift(&destination_cryptde).unwrap(),
            LiveHop::new(
                &PublicKey::new(&[]),
                Some(Wallet::new("wallet")),
                Component::Neighborhood,
            )
        );
        assert_eq!(
            &route.hops[0].as_slice()[..8],
            &[52, 52, 52, 52, 52, 52, 52, 52]
        ); // garbage
    }

    #[test]
    fn to_next_live_complains_about_bad_input() {
        let subject = LiveCoresPackage::new(Route { hops: vec![] }, CryptData::new(&[]));

        let result = subject.to_next_live(cryptde());

        assert_eq!(result, Err(RouteError::EmptyRoute));
    }

    #[test]
    fn live_cores_package_can_be_constructed_from_incipient_cores_package() {
        let cryptde = cryptde();
        let consuming_wallet = Wallet::new("wallet");
        let key12 = cryptde.public_key();
        let key34 = PublicKey::new(&[3, 4]);
        let key56 = PublicKey::new(&[5, 6]);
        let mut route = Route::one_way(
            RouteSegment::new(vec![&key12, &key34, &key56], Component::Neighborhood),
            cryptde,
            Some(consuming_wallet),
        )
        .unwrap();
        let payload = MessageType::DnsResolveFailed(DnsResolveFailure {
            stream_key: make_meaningless_stream_key(),
        });

        let incipient =
            IncipientCoresPackage::new(cryptde, route.clone(), payload.clone(), &key56).unwrap();
        let (subject, next_stop) = LiveCoresPackage::from_incipient(incipient, cryptde).unwrap();

        assert_eq!(next_stop, key34);
        route.shift(cryptde).unwrap();

        assert_eq!(subject.route, route);
        assert_eq!(
            subject.payload,
            cryptde
                .encode(
                    &key56,
                    &PlainData::new(&serde_cbor::ser::to_vec(&payload).unwrap()),
                )
                .unwrap()
        );
    }

    #[test]
    fn from_incipient_complains_about_problems_decrypting_next_hop() {
        let cryptde = cryptde();
        let incipient = IncipientCoresPackage::new(
            cryptde,
            Route { hops: vec![] },
            MessageType::DnsResolveFailed(DnsResolveFailure {
                stream_key: make_meaningless_stream_key(),
            }),
            &PublicKey::new(&[3, 4]),
        )
        .unwrap();
        let result = LiveCoresPackage::from_incipient(incipient, cryptde);

        assert_eq!(
            result,
            Err(String::from("Could not decrypt next hop: EmptyRoute"))
        );
    }

    #[test]
    fn expired_cores_package_can_be_constructed_from_live_cores_package() {
        let immediate_neighbor_ip = IpAddr::from_str("1.2.3.4").unwrap();
        let payload = MessageType::DnsResolveFailed(DnsResolveFailure {
            stream_key: make_meaningless_stream_key(),
        });
        let first_stop_key = PublicKey::new(&[3, 4]);
        let first_stop_cryptde = CryptDENull::from(&first_stop_key);
        let relay_key = PublicKey::new(&[1, 2]);
        let relay_cryptde = CryptDENull::from(&relay_key);
        let second_stop_key = PublicKey::new(&[5, 6]);
        let second_stop_cryptde = CryptDENull::from(&second_stop_key);
        let cryptde = cryptde();
        let encrypted_payload = encodex(cryptde, &first_stop_key, &payload).unwrap();
        let consuming_wallet = Wallet::new("wallet");
        let mut route = Route::round_trip(
            RouteSegment::new(vec![&relay_key, &first_stop_key], Component::Neighborhood),
            RouteSegment::new(
                vec![&first_stop_key, &relay_key, &second_stop_key],
                Component::ProxyServer,
            ),
            cryptde,
            Some(consuming_wallet),
            1234,
        )
        .unwrap();
        route.shift(&relay_cryptde).unwrap();
        let subject = LiveCoresPackage::new(route.clone(), encrypted_payload.clone());

        let result = subject
            .to_expired(immediate_neighbor_ip, &first_stop_cryptde)
            .unwrap();

        assert_eq!(result.immediate_neighbor_ip, immediate_neighbor_ip);
        assert_eq!(result.consuming_wallet, Some(Wallet::new("wallet")));
        assert_eq!(result.payload, payload);
        let mut route = result.remaining_route.clone();
        assert_eq!(
            route.shift(&first_stop_cryptde).unwrap(),
            LiveHop::new(
                &relay_key,
                Some(Wallet::new("wallet")),
                Component::Neighborhood,
            )
        );
        assert_eq!(
            route.shift(&relay_cryptde).unwrap(),
            LiveHop::new(
                &second_stop_key,
                Some(Wallet::new("wallet")),
                Component::Hopper,
            )
        );
        assert_eq!(
            route.shift(&second_stop_cryptde).unwrap(),
            LiveHop::new(
                &PublicKey::new(&[]),
                Some(Wallet::new("wallet")),
                Component::ProxyServer,
            )
        );
        assert_eq!(
            route.hops[0],
            crate::test_utils::test_utils::encrypt_return_route_id(1234, cryptde),
        );
        route.hops.remove(0);
        assert_eq!(
            &route.hops[0].as_slice()[..8],
            &[52, 52, 52, 52, 52, 52, 52, 52]
        ); // garbage
    }

    #[test]
    fn to_expired_complains_about_bad_route() {
        let subject = LiveCoresPackage::new(
            Route { hops: vec![] },
            CryptData::new(cryptde().private_key().as_slice()),
        );

        let result = subject.to_expired(IpAddr::from_str("1.2.3.4").unwrap(), cryptde());

        assert_eq!(result, Err(format!("{:?}", RouteError::EmptyRoute)));
    }

    #[test]
    fn live_cores_package_serialization_deserialization() {
        let original = LiveCoresPackage {
            route: make_meaningless_route(),
            payload: CryptData::new(&[1, 2, 3, 4]),
        };

        let serialized = serde_cbor::ser::to_vec(&original).unwrap();

        let deserialized = serde_cbor::de::from_slice::<LiveCoresPackage>(&serialized[..]).unwrap();

        assert_eq!(deserialized, original);
    }
}

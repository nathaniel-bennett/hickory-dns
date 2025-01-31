mod no_rrsig_dnskey;

use dns_test::{
    client::{Client, DigOutput, DigSettings, ExtendedDnsError},
    name_server::{Graph, NameServer, Sign},
    record::{Record, RecordType, DS},
    zone_file::{SignSettings, Signer},
    Network, Resolver, Result, TrustAnchor, FQDN, PEER,
};

#[test]
fn ds_unassigned_key_algo() -> Result<()> {
    let output =
        malformed_ds_fixture(&FQDN::TEST_TLD.push_label("ds-unassigned-key-algo"), |ds| {
            ds.algorithm = 100
        })?;

    dbg!(&output);

    assert!(output.status.is_noerror() && !output.flags.authenticated_data);

    if dns_test::SUBJECT.is_unbound() {
        assert!(output.ede.is_empty());
    }

    Ok(())
}

#[test]
fn ds_reserved_key_algo() -> Result<()> {
    let output = malformed_ds_fixture(&FQDN::TEST_TLD.push_label("ds-reserved-key-algo"), |ds| {
        ds.algorithm = 200
    })?;

    dbg!(&output);

    assert!(output.status.is_noerror() && !output.flags.authenticated_data);

    if dns_test::SUBJECT.is_unbound() {
        assert!(output.ede.is_empty());
    }

    Ok(())
}

// the key tag in the DS record does not match the key tag in the DNSKEY record
#[test]
fn ds_bad_tag() -> Result<()> {
    let output = malformed_ds_fixture(&FQDN::TEST_TLD.push_label("ds-bad-tag"), |ds| {
        ds.key_tag = !ds.key_tag;
    })?;

    dbg!(&output);

    assert!(output.status.is_servfail());

    if dns_test::SUBJECT.is_unbound() {
        assert!(output.ede.iter().eq([&ExtendedDnsError::DnssecBogus]));
    }

    Ok(())
}

// the algorithm field in the DS record does not match the algorithm field in the DNSKEY record
#[test]
fn ds_bad_key_algo() -> Result<()> {
    let output = malformed_ds_fixture(&FQDN::TEST_TLD.push_label("ds-bad-key-algo"), |ds| {
        assert_eq!(8, ds.algorithm, "number below may need to change");
        ds.algorithm = 7;
    })?;

    dbg!(&output);

    assert!(output.status.is_servfail());

    if dns_test::SUBJECT.is_unbound() {
        assert!(output.ede.iter().eq([&ExtendedDnsError::DnssecBogus]));
    }

    Ok(())
}

// the RRSIG covering the DNSKEYs generated using the KSK has been removed
// but there's an RRSIG covering the DNSKEYs generated using the ZSK
#[test]
fn no_rrsig_ksk() -> Result<()> {
    let network = Network::new()?;
    let leaf_zone = FQDN::TEST_TLD.push_label("no-rrsig-ksk");
    let leaf_ns = NameServer::new(&dns_test::PEER, leaf_zone.clone(), &network)?;

    let Graph {
        nameservers: _nameservers,
        root,
        trust_anchor,
    } = Graph::build(
        leaf_ns,
        Sign::AndAmend {
            settings: SignSettings::default(),
            mutate: &|zone, records| {
                if zone == &leaf_zone {
                    let mut ksk_tag = None;
                    let mut zsk_tag = None;
                    for record in records.iter() {
                        if let Record::DNSKEY(dnskey) = record {
                            if dnskey.is_key_signing_key() {
                                assert!(ksk_tag.is_none(), "more than one KSK");
                                ksk_tag = Some(dnskey.rdata.calculate_key_tag());
                            } else {
                                assert!(zsk_tag.is_none(), "more than one ZSK");
                                zsk_tag = Some(dnskey.rdata.calculate_key_tag());
                            }
                        }
                    }

                    let ksk_tag = ksk_tag.expect("did not find the KSK");
                    let mut did_remove = false;
                    for (index, record) in records.iter().enumerate() {
                        if let Record::RRSIG(rrsig) = record {
                            if rrsig.type_covered == RecordType::DNSKEY && rrsig.key_tag == ksk_tag
                            {
                                records.remove(index);
                                did_remove = true;
                                break;
                            }
                        }
                    }
                    assert!(
                        did_remove,
                        "did not find an RRSIG covering DNSKEY generated using the KSK"
                    );

                    // PRE-CONDITION there must be a RRSIG covering DNSKEY but generated using
                    // the ZSK
                    let zsk_tag = zsk_tag.expect("did not find the ZSK");
                    let mut found = false;
                    for record in records.iter() {
                        if let Record::RRSIG(rrsig) = record {
                            if rrsig.type_covered == RecordType::DNSKEY && rrsig.key_tag == zsk_tag
                            {
                                found = true;
                                break;
                            }
                        }
                    }
                    assert!(
                        found,
                        "did not find an RRSIG covering DNSKEY generated using the ZSK"
                    );
                }
            },
        },
    )?;

    let mut resolver = Resolver::new(&network, root);

    let supports_ede = dns_test::SUBJECT.is_unbound();
    if supports_ede {
        resolver.extended_dns_errors();
    }

    let resolver = resolver.trust_anchor(&trust_anchor.unwrap()).start()?;

    let client = Client::new(resolver.network())?;
    let settings = *DigSettings::default().recurse().authentic_data();
    let output = client.dig(settings, resolver.ipv4_addr(), RecordType::NS, &leaf_zone)?;

    dbg!(&output);

    assert!(output.status.is_servfail());

    if supports_ede {
        assert!(output.ede.iter().eq([&ExtendedDnsError::DnssecBogus]));
    }

    Ok(())
}

fn malformed_ds_fixture(leaf_zone: &FQDN, mutate: impl FnOnce(&mut DS)) -> Result<DigOutput> {
    let network = Network::new()?;
    let sign_settings = SignSettings::default();

    let peer = &dns_test::PEER;
    let mut root_ns = NameServer::new(peer, FQDN::ROOT, &network)?;
    let mut tld_ns = NameServer::new(peer, FQDN::TEST_TLD, &network)?;
    let mut nameservers_ns = NameServer::new(peer, FQDN::TEST_DOMAIN, &network)?;
    let leaf_ns = NameServer::new(peer, leaf_zone.clone(), &network)?;

    root_ns.referral_nameserver(&tld_ns);
    tld_ns.referral_nameserver(&nameservers_ns);
    tld_ns.referral_nameserver(&leaf_ns);

    nameservers_ns.add(root_ns.a());
    nameservers_ns.add(tld_ns.a());

    let nameservers_ns = nameservers_ns.sign(sign_settings.clone())?;
    let leaf_ns = leaf_ns.sign(sign_settings.clone())?;

    tld_ns.add(nameservers_ns.ds().ksk.clone());
    let mut ds = leaf_ns.ds().ksk.clone();
    mutate(&mut ds);
    tld_ns.add(ds);

    let tld_ns = tld_ns.sign(sign_settings.clone())?;
    root_ns.add(tld_ns.ds().ksk.clone());

    let mut trust_anchor = TrustAnchor::empty();
    let root_ns = root_ns.sign(sign_settings)?;
    trust_anchor.add(root_ns.key_signing_key().clone());
    trust_anchor.add(root_ns.zone_signing_key().clone());

    let root_hint = root_ns.root_hint();
    let _root_ns = root_ns.start()?;
    let _tld_ns = tld_ns.start()?;
    let _nameservers_ns = nameservers_ns.start()?;
    let _leaf_ns = leaf_ns.start()?;

    let mut resolver = Resolver::new(&network, root_hint);
    if dns_test::SUBJECT.is_unbound() {
        resolver.extended_dns_errors();
    }
    let resolver = resolver.trust_anchor(&trust_anchor).start()?;

    let client = Client::new(&network)?;
    let settings = *DigSettings::default().recurse().authentic_data();

    client.dig(settings, resolver.ipv4_addr(), RecordType::SOA, leaf_zone)
}

#[test]
#[ignore = "hickory does not detect that this zone is bogus"]
fn bogus_zone_plus_trust_anchor_dnskey() -> Result<()> {
    let network = Network::new()?;
    let sign_settings = SignSettings::default();

    let leaf_zone = FQDN::TEST_TLD.push_label("domain");

    let mut root_ns = NameServer::new(&PEER, FQDN::ROOT, &network)?;
    let mut tld_ns = NameServer::new(&PEER, FQDN::TEST_TLD, &network)?;
    let mut nameservers_ns = NameServer::new(&PEER, FQDN::TEST_DOMAIN, &network)?;
    let victim_leaf_ns = NameServer::new(&PEER, leaf_zone.clone(), &network)?;
    let mut attacker_leaf_ns = NameServer::new(&PEER, leaf_zone.clone(), &network)?;

    root_ns.referral_nameserver(&tld_ns);
    tld_ns.referral_nameserver(&nameservers_ns);
    // Add NS and A records pointing to the attacker's name server. The attacker
    // could alternately interfere with network traffic without tampering with
    // these records in the parent zone.
    tld_ns.referral_nameserver(&attacker_leaf_ns);

    nameservers_ns.add(root_ns.a());
    nameservers_ns.add(tld_ns.a());

    let nameservers_ns = nameservers_ns.sign(sign_settings.clone())?;

    // We need to phase root zone key generation before root zone signing, so we
    // can copy the root zone public key into a child zone before generating
    // keys and signatures for the child zone, which produces records that are
    // needed before parent zone signing can happen.
    let root_signer = Signer::new(root_ns.container(), sign_settings.clone())?;
    let root_keys = root_signer.generate_keys(&FQDN::ROOT)?;

    // The victim signs the leaf zone, and the victim's DS record goes into the parent zone.
    let victim_signer = Signer::new(victim_leaf_ns.container(), sign_settings.clone())?;
    let victim_keys = victim_signer.generate_keys(&leaf_zone)?;
    let victim_leaf_zone = victim_signer.sign_zone(victim_leaf_ns.zone_file(), &victim_keys)?;
    // The victim's private keys are not used past this point.
    drop(victim_keys);
    drop(victim_leaf_ns);

    // The attacker adds a DNSKEY record to its zone that contains the root
    // zone's public key, and signs the zone with its own keys.
    let mut modified_trust_anchor_key = root_keys.ksk.public.clone().with_ttl(86400);
    modified_trust_anchor_key.zone = leaf_zone.clone();
    attacker_leaf_ns.add(modified_trust_anchor_key);
    println!("before signing:\n{}", attacker_leaf_ns.zone_file());
    let attacker_leaf_ns = attacker_leaf_ns.sign(sign_settings.clone())?;
    println!("after signing:\n{}", attacker_leaf_ns.signed_zone_file());

    tld_ns.add(nameservers_ns.ds().ksk.clone());
    // Note that the victim's DS record is signed by the TLD zone.
    tld_ns.add(victim_leaf_zone.ds().ksk.clone());

    let tld_ns = tld_ns.sign(sign_settings.clone())?;
    root_ns.add(tld_ns.ds().ksk.clone());

    let mut trust_anchor = TrustAnchor::empty();
    let root_ns = root_ns.sign_with_keys(sign_settings, &root_keys)?;
    trust_anchor.add(root_ns.key_signing_key().clone());

    let root_hint = root_ns.root_hint();
    let _root_ns = root_ns.start()?;
    let _tld_ns = tld_ns.start()?;
    let _nameservers_ns = nameservers_ns.start()?;
    let _leaf_ns = attacker_leaf_ns.start()?;

    let mut resolver = Resolver::new(&network, root_hint);
    if dns_test::SUBJECT.is_unbound() {
        resolver.extended_dns_errors();
    }
    let resolver = resolver.trust_anchor(&trust_anchor).start()?;

    let client = Client::new(&network)?;
    let settings = *DigSettings::default().recurse().authentic_data();

    let output = client.dig(settings, resolver.ipv4_addr(), RecordType::SOA, &leaf_zone)?;

    println!("{}", _leaf_ns.logs()?);
    println!("{}", resolver.logs()?);

    dbg!(&output);

    assert!(output.status.is_servfail());

    if dns_test::SUBJECT.is_unbound() {
        assert!(output.ede.iter().eq(&[ExtendedDnsError::DnssecBogus]));
    }

    Ok(())
}

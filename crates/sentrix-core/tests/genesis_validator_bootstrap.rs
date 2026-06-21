//! Genesis-validator bootstrap regression.
//!
//! Node init must seat the `[[genesis.validators]]` in the `AuthorityManager`
//! (so a fresh chain has an authority set to produce from), and
//! `activate_voyager` must migrate that exact set into the stake registry so
//! DPoS has a non-empty `active_set` to elect a proposer from. Without the
//! seeding the chain booted with an empty active_set ("BFT activation blocked:
//! active_set is empty") and never produced a block. The bare constructor
//! stays validator-free so it remains a clean slate for other tests.

use sentrix_core::Genesis;
use sentrix_core::blockchain::Blockchain;
use std::collections::BTreeSet;

const GENESIS_TOML: &str = r#"
[chain]
chain_id = 7120
name = "bootstrap-test"
[genesis]
timestamp = 1714200000
parent_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"
[[genesis.validators]]
address = "0x1111111111111111111111111111111111111111"
stake = 1050000000000000
[[genesis.validators]]
address = "0x2222222222222222222222222222222222222222"
stake = 1050000000000000
[[genesis.validators]]
address = "0x3333333333333333333333333333333333333333"
stake = 1050000000000000
[[genesis.balances]]
address = "0x1111111111111111111111111111111111111111"
amount = 1050000000000000
"#;

#[test]
fn genesis_validators_seed_authority_and_migrate_to_stake_registry() {
    let genesis = Genesis::parse(GENESIS_TOML).expect("parse genesis");
    let expected: BTreeSet<String> = genesis
        .genesis
        .validators
        .iter()
        .map(|v| v.address.clone())
        .collect();
    assert_eq!(expected.len(), 3, "fixture should declare 3 distinct validators");

    let mut bc = Blockchain::new_with_genesis("admin".to_string(), &genesis);

    // The bare constructor is a clean slate: it must NOT seat validators (other
    // tests rely on this to add a single proposer of their own).
    assert!(
        bc.authority.active_validators().is_empty(),
        "constructor must not seat genesis validators"
    );

    // Node init seats exactly the declared genesis validators.
    bc.seat_genesis_validators(&genesis);
    let seated: BTreeSet<String> = bc
        .authority
        .active_validators()
        .iter()
        .map(|v| v.address.clone())
        .collect();
    assert_eq!(
        seated, expected,
        "authority set must be exactly the genesis validators"
    );

    // Pre-fork the stake registry stays empty (genesis state-root identity).
    assert_eq!(bc.stake_registry.active_count(), 0);

    // The Voyager DPoS migration moves that exact set into the stake registry.
    bc.activate_voyager().expect("activate_voyager");
    let active: BTreeSet<String> = bc.stake_registry.active_set.iter().cloned().collect();
    assert_eq!(
        active, expected,
        "active_set must be exactly the genesis validators after migration"
    );
}

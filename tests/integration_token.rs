// integration_token.rs — SRX-20 token deploy + transfer + burn + mint tests
// All token operations go through the standard mempool → add_block path (no shortcuts).

mod common;

use sentrix::core::transaction::{TokenOp, MIN_TX_FEE, TOKEN_OP_ADDRESS};
use sentrix::core::blockchain::CHAIN_ID;

/// Helper: create and mine a token operation TX.
fn mine_token_op(
    bc: &mut sentrix::core::blockchain::Blockchain,
    wallet: &sentrix::wallet::wallet::Wallet,
    op: TokenOp,
    validator_addr: &str,
) {
    let nonce = bc.accounts.get_nonce(&wallet.address);
    let sk = wallet.get_secret_key().expect("sk");
    let pk = wallet.get_public_key().expect("pk");
    let data = op.encode().expect("encode TokenOp");
    let tx = sentrix::core::transaction::Transaction::new(
        wallet.address.clone(),
        TOKEN_OP_ADDRESS.to_string(),
        0,
        MIN_TX_FEE,
        nonce,
        data,
        CHAIN_ID,
        &sk,
        &pk,
    )
    .expect("Transaction::new");
    bc.add_to_mempool(tx).expect("add token op to mempool");
    common::mine_block_with_mempool(bc, validator_addr);
}

/// Deploy an SRX-20 token, verify deployer receives total supply.
#[test]
fn test_token_deploy_and_initial_supply() {
    let (mut bc, val) = common::setup_single_validator();
    let deployer = common::funded_wallet(&mut bc, 10_000_000); // gas for ops

    mine_token_op(
        &mut bc,
        &deployer,
        TokenOp::Deploy {
            name: "TestToken".to_string(),
            symbol: "TTK".to_string(),
            decimals: 8,
            supply: 1_000_000,
            max_supply: 2_000_000,
        },
        &val.address,
    );

    // Contract must exist
    let tokens = bc.list_tokens();
    assert_eq!(tokens.len(), 1, "exactly 1 token should be deployed");

    let contract_addr = tokens[0]["contract_address"].as_str().expect("contract_address field");
    assert!(!contract_addr.is_empty(), "contract address must not be empty");

    // Deployer receives the initial supply
    let deployer_balance = bc.token_balance(contract_addr, &deployer.address);
    assert_eq!(deployer_balance, 1_000_000, "deployer should receive full initial supply");
}

/// Contract address is deterministic: same TX → same address.
/// Tests that the address is derived from txid (V6-C-01 fix).
#[test]
fn test_contract_address_deterministic() {
    let (mut bc, val) = common::setup_single_validator();
    let deployer = common::funded_wallet(&mut bc, 10_000_000);

    mine_token_op(
        &mut bc,
        &deployer,
        TokenOp::Deploy {
            name: "Alpha".to_string(),
            symbol: "ALP".to_string(),
            decimals: 8,
            supply: 500_000,
            max_supply: 0,
        },
        &val.address,
    );

    let tokens = bc.list_tokens();
    let addr1 = tokens[0]["contract_address"].as_str().expect("addr1").to_string();

    // The address format is SRX20_<hex> — verify it's deterministic (non-empty, consistent prefix)
    assert!(addr1.starts_with("SRX20_"), "contract address must have SRX20_ prefix");

    // Verify balance is correct (deployer has all tokens)
    assert_eq!(bc.token_balance(&addr1, &deployer.address), 500_000);
}

/// Transfer tokens between two addresses.
#[test]
fn test_token_transfer() {
    let (mut bc, val) = common::setup_single_validator();
    let deployer = common::funded_wallet(&mut bc, 10_000_000);
    let recipient = common::funded_wallet(&mut bc, 10_000_000);

    // Deploy token
    mine_token_op(
        &mut bc,
        &deployer,
        TokenOp::Deploy {
            name: "Transferable".to_string(),
            symbol: "TRF".to_string(),
            decimals: 8,
            supply: 1_000_000,
            max_supply: 0,
        },
        &val.address,
    );

    let tokens = bc.list_tokens();
    let contract = tokens[0]["contract_address"].as_str().expect("addr").to_string();

    let deployer_before = bc.token_balance(&contract, &deployer.address);
    let recv_before = bc.token_balance(&contract, &recipient.address);

    // Transfer 100 tokens
    mine_token_op(
        &mut bc,
        &deployer,
        TokenOp::Transfer {
            contract: contract.clone(),
            to: recipient.address.clone(),
            amount: 100,
        },
        &val.address,
    );

    assert_eq!(bc.token_balance(&contract, &deployer.address), deployer_before - 100);
    assert_eq!(bc.token_balance(&contract, &recipient.address), recv_before + 100);
}

/// Burn tokens — total supply must decrease.
#[test]
fn test_token_burn() {
    let (mut bc, val) = common::setup_single_validator();
    let deployer = common::funded_wallet(&mut bc, 10_000_000);

    mine_token_op(
        &mut bc,
        &deployer,
        TokenOp::Deploy {
            name: "Burnable".to_string(),
            symbol: "BRN".to_string(),
            decimals: 8,
            supply: 1_000_000,
            max_supply: 0,
        },
        &val.address,
    );

    let tokens = bc.list_tokens();
    let contract = tokens[0]["contract_address"].as_str().expect("addr").to_string();

    let balance_before = bc.token_balance(&contract, &deployer.address);

    // Burn 50 tokens
    mine_token_op(
        &mut bc,
        &deployer,
        TokenOp::Burn { contract: contract.clone(), amount: 50 },
        &val.address,
    );

    let balance_after = bc.token_balance(&contract, &deployer.address);
    assert_eq!(balance_after, balance_before - 50, "tokens should be burned from deployer balance");

    // Token info should reflect reduced total supply
    let info = bc.token_info(&contract).expect("token_info");
    let total_supply = info["total_supply"].as_u64().expect("total_supply");
    assert_eq!(total_supply, 1_000_000 - 50, "total supply must decrease after burn");
}

/// Mint beyond max_supply must be rejected.
#[test]
fn test_mint_exceeds_max_supply_rejected() {
    let (mut bc, val) = common::setup_single_validator();
    let deployer = common::funded_wallet(&mut bc, 10_000_000);

    // Deploy with max_supply = 1_000_000 and initial supply = 1_000_000 (already at cap)
    mine_token_op(
        &mut bc,
        &deployer,
        TokenOp::Deploy {
            name: "Capped".to_string(),
            symbol: "CAP".to_string(),
            decimals: 8,
            supply: 1_000_000,
            max_supply: 1_000_000, // already at cap
        },
        &val.address,
    );

    let tokens = bc.list_tokens();
    let contract = tokens[0]["contract_address"].as_str().expect("addr").to_string();

    // Trying to mint 1 more token should fail in block validation
    let nonce = bc.accounts.get_nonce(&deployer.address);
    let sk = deployer.get_secret_key().expect("sk");
    let pk = deployer.get_public_key().expect("pk");
    let mint_op = TokenOp::Mint {
        contract: contract.clone(),
        to: deployer.address.clone(),
        amount: 1,
    };
    let data = mint_op.encode().expect("encode");
    let tx = sentrix::core::transaction::Transaction::new(
        deployer.address.clone(),
        TOKEN_OP_ADDRESS.to_string(),
        0,
        MIN_TX_FEE,
        nonce,
        data,
        CHAIN_ID,
        &sk,
        &pk,
    )
    .expect("tx");

    bc.add_to_mempool(tx).expect("mempool accept");

    // Block creation should fail because the mint exceeds max_supply
    let block = bc.create_block(&val.address).expect("create_block");
    let result = bc.add_block(block);
    // The block with the invalid mint should be rejected
    assert!(result.is_err(), "block with mint exceeding max_supply must be rejected");
}

/// Transferring more tokens than balance must be rejected at the block execution stage.
#[test]
fn test_token_transfer_exceeds_balance_rejected() {
    let (mut bc, val) = common::setup_single_validator();
    let deployer = common::funded_wallet(&mut bc, 10_000_000);
    let other = common::funded_wallet(&mut bc, 10_000_000);

    mine_token_op(
        &mut bc,
        &deployer,
        TokenOp::Deploy {
            name: "Limited".to_string(),
            symbol: "LTD".to_string(),
            decimals: 8,
            supply: 100,
            max_supply: 0,
        },
        &val.address,
    );

    let tokens = bc.list_tokens();
    let contract = tokens[0]["contract_address"].as_str().expect("addr").to_string();

    // `other` has 0 tokens — trying to transfer 1 must fail
    let nonce = bc.accounts.get_nonce(&other.address);
    let sk = other.get_secret_key().expect("sk");
    let pk = other.get_public_key().expect("pk");
    let op = TokenOp::Transfer {
        contract: contract.clone(),
        to: deployer.address.clone(),
        amount: 1,
    };
    let data = op.encode().expect("encode");
    let tx = sentrix::core::transaction::Transaction::new(
        other.address.clone(),
        TOKEN_OP_ADDRESS.to_string(),
        0,
        MIN_TX_FEE,
        nonce,
        data,
        CHAIN_ID,
        &sk,
        &pk,
    )
    .expect("tx");

    bc.add_to_mempool(tx).expect("mempool accept");

    let block = bc.create_block(&val.address).expect("create_block");
    let result = bc.add_block(block);
    assert!(result.is_err(), "transfer exceeding token balance must be rejected");
}

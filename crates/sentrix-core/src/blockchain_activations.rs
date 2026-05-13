//! `Blockchain` fork-activation mutations — `activate_evm` and
//! `activate_voyager`. One-shot migrations called from the validator
//! loop at the corresponding fork height; idempotent via persistent
//! `evm_activated` / `voyager_activated` flags on the `Blockchain`
//! struct so post-fork restarts don't re-run the migration.
//!
//! Rust permits splitting `impl T { … }` across modules within the
//! same crate; the call sites in the validator loop (sentrix bin)
//! keep `bc.activate_evm()` / `bc.activate_voyager()` unchanged.

use sentrix_primitives::error::{SentrixError, SentrixResult};

use crate::blockchain::Blockchain;

impl Blockchain {
    /// Initialize EVM state at fork activation.
    /// Called once when chain reaches VOYAGER_EVM_HEIGHT.
    /// Migrates all account code_hash fields and initializes gas tracking.
    /// Idempotent — guarded by the persistent `evm_activated` flag.
    pub fn activate_evm(&mut self) {
        if self.evm_activated {
            tracing::debug!("activate_evm: already activated, skipping");
            return;
        }
        tracing::info!("Activating EVM at height {}", self.height());
        let migrated = self.accounts.migrate_to_evm();
        self.evm_activated = true;
        tracing::info!(
            "EVM activated: {} accounts migrated, gas metering enabled",
            migrated
        );
    }

    /// Initialize Voyager state at fork activation.
    /// Called once when chain reaches VOYAGER_DPOS_HEIGHT.
    /// Migrates existing Pioneer validators to DPoS with equal stake.
    /// Idempotent — guarded by the persistent `voyager_activated` flag so
    /// validator restarts post-fork don't re-register validators or
    /// re-snapshot the epoch.
    ///
    /// **Fail-fast on partial migration.** Pre-fix this loop logged
    /// `register_validator` failures as warnings and continued, then
    /// flipped `voyager_activated = true` regardless. A single failed
    /// migration would silently strand a Pioneer validator outside the
    /// DPoS stake registry while the persistent flag claimed Voyager was
    /// fully activated — restart-safe in the wrong direction. Now any
    /// per-validator failure aborts immediately: epoch state is not
    /// initialized, the flag is not flipped, and the operator's retry
    /// path sees an unactivated chain instead of a half-migrated one.
    pub fn activate_voyager(&mut self) -> SentrixResult<()> {
        use sentrix_staking::MIN_SELF_STAKE;

        if self.voyager_activated {
            tracing::debug!("activate_voyager: already activated, skipping");
            return Ok(());
        }

        // Migrate Pioneer validators → DPoS validators. Bail on the first
        // failure so a partial registry write doesn't get cemented by the
        // flag flip below.
        let validators: Vec<String> = self
            .authority
            .active_validators()
            .iter()
            .map(|v| v.address.clone())
            .collect();
        let height = self.height();
        for address in &validators {
            self.stake_registry
                .register_validator(address, MIN_SELF_STAKE, 1000, height)
                .map_err(|e| {
                    tracing::error!(
                        "activate_voyager: register_validator failed for {} at h={}: {} — aborting activation",
                        address, height, e
                    );
                    SentrixError::Internal(format!(
                        "activate_voyager: register_validator({}) at height {} failed: {}",
                        address, height, e
                    ))
                })?;
        }

        // All migrations succeeded — finalise epoch state + flip the
        // persistent flag in that order. Any failure in the loop above
        // returned early, so we only reach here after a clean migration.
        self.stake_registry.update_active_set();
        self.epoch_manager.initialize(&self.stake_registry, height);
        self.voyager_activated = true;

        tracing::info!(
            "Voyager DPoS activated at height {}. {} validators migrated.",
            height,
            self.stake_registry.active_count()
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_chain_with_two_validators() -> Blockchain {
        let mut bc = Blockchain::new("admin".to_string());
        bc.authority
            .add_validator_unchecked("v1".to_string(), "V1".to_string(), "pk1".to_string());
        bc.authority
            .add_validator_unchecked("v2".to_string(), "V2".to_string(), "pk2".to_string());
        bc
    }

    #[test]
    fn activate_voyager_happy_path_flips_flag_and_migrates_all() {
        let mut bc = setup_chain_with_two_validators();
        assert!(!bc.voyager_activated);

        bc.activate_voyager().expect("activation must succeed");

        assert!(bc.voyager_activated);
        assert!(
            bc.stake_registry.get_validator("v1").is_some(),
            "v1 must end up in stake_registry"
        );
        assert!(
            bc.stake_registry.get_validator("v2").is_some(),
            "v2 must end up in stake_registry"
        );
    }

    #[test]
    fn activate_voyager_is_idempotent_on_repeat() {
        let mut bc = setup_chain_with_two_validators();
        bc.activate_voyager().unwrap();
        assert!(bc.voyager_activated);
        // Second call must return Ok without re-registering (which would
        // itself fail with "already registered").
        bc.activate_voyager()
            .expect("second activation must be a no-op, not Err");
    }

    #[test]
    fn activate_voyager_aborts_on_register_failure_without_flipping_flag() {
        // Pre-stage v1 in the stake_registry so the migration loop hits
        // an "already registered" error on the first validator. This is
        // the realistic shape of partial-migration: register fails mid-
        // loop, and the pre-fix code would mask it as a warn and still
        // flip voyager_activated.
        let mut bc = setup_chain_with_two_validators();
        bc.stake_registry
            .register_validator("v1", sentrix_staking::MIN_SELF_STAKE, 1000, 0)
            .expect("pre-stage v1 must succeed");

        let err = bc
            .activate_voyager()
            .expect_err("activation must fail when register_validator fails");
        let msg = err.to_string();
        assert!(
            msg.contains("v1"),
            "error must name the failing validator: {msg}"
        );
        assert!(
            !bc.voyager_activated,
            "voyager_activated must stay false after a partial-migration abort"
        );
    }
}

//! `Blockchain` fork-activation mutations — `activate_evm` and
//! `activate_voyager`. One-shot migrations called from the validator
//! loop at the corresponding fork height; idempotent via persistent
//! `evm_activated` / `voyager_activated` flags on the `Blockchain`
//! struct so post-fork restarts don't re-run the migration.
//!
//! Rust permits splitting `impl T { … }` across modules within the
//! same crate; the call sites in the validator loop (sentrix bin)
//! keep `bc.activate_evm()` / `bc.activate_voyager()` unchanged.

use sentrix_primitives::error::SentrixResult;

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
    pub fn activate_voyager(&mut self) -> SentrixResult<()> {
        use sentrix_staking::MIN_SELF_STAKE;

        if self.voyager_activated {
            tracing::debug!("activate_voyager: already activated, skipping");
            return Ok(());
        }

        // Migrate Pioneer validators → DPoS validators
        let validators: Vec<String> = self
            .authority
            .active_validators()
            .iter()
            .map(|v| v.address.clone())
            .collect();
        for address in &validators {
            if let Err(e) = self.stake_registry.register_validator(
                address,
                MIN_SELF_STAKE,
                1000, // 10% default commission
                self.height(),
            ) {
                tracing::warn!("Failed to migrate validator {}: {}", address, e);
            }
        }

        // Initialize epoch manager with the new stake registry
        self.stake_registry.update_active_set();
        self.epoch_manager
            .initialize(&self.stake_registry, self.height());

        self.voyager_activated = true;

        tracing::info!(
            "Voyager DPoS activated at height {}. {} validators migrated.",
            self.height(),
            self.stake_registry.active_count()
        );

        Ok(())
    }
}

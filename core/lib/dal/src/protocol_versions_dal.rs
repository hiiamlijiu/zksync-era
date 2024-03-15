use std::convert::TryInto;

use anyhow::Context as _;
use zksync_contracts::{BaseSystemContracts, BaseSystemContractsHashes};
use zksync_types::{
    protocol_version::{L1VerifierConfig, ProtocolUpgradeTx, ProtocolVersion, VerifierParams},
    Address, ProtocolVersionId, H256,
};

use crate::{
    models::storage_protocol_version::{protocol_version_from_storage, StorageProtocolVersion},
    StorageProcessor,
};

#[derive(Debug)]
pub struct ProtocolVersionsDal<'a, 'c> {
    pub storage: &'a mut StorageProcessor<'c>,
}

impl ProtocolVersionsDal<'_, '_> {
    pub async fn save_protocol_version(
        &mut self,
        id: ProtocolVersionId,
        timestamp: u64,
        l1_verifier_config: L1VerifierConfig,
        base_system_contracts_hashes: BaseSystemContractsHashes,
        verifier_address: Address,
        tx_hash: Option<H256>,
    ) {
        sqlx::query!(
            r#"
            INSERT INTO
                protocol_versions (
                    id,
                    timestamp,
                    recursion_scheduler_level_vk_hash,
                    recursion_node_level_vk_hash,
                    recursion_leaf_level_vk_hash,
                    recursion_circuits_set_vks_hash,
                    bootloader_code_hash,
                    default_account_code_hash,
                    verifier_address,
                    upgrade_tx_hash,
                    created_at
                )
            VALUES
                ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW())
            "#,
            id as i32,
            timestamp as i64,
            l1_verifier_config
                .recursion_scheduler_level_vk_hash
                .as_bytes(),
            l1_verifier_config
                .params
                .recursion_node_level_vk_hash
                .as_bytes(),
            l1_verifier_config
                .params
                .recursion_leaf_level_vk_hash
                .as_bytes(),
            l1_verifier_config
                .params
                .recursion_circuits_set_vks_hash
                .as_bytes(),
            base_system_contracts_hashes.bootloader.as_bytes(),
            base_system_contracts_hashes.default_aa.as_bytes(),
            verifier_address.as_bytes(),
            tx_hash.as_ref().map(H256::as_bytes),
        )
        .execute(self.storage.conn())
        .await
        .unwrap();
    }

    pub async fn save_protocol_version_with_tx(&mut self, version: ProtocolVersion) {
        let tx_hash = version.tx.as_ref().map(|tx| tx.common_data.hash());

        let mut db_transaction = self.storage.start_transaction().await.unwrap();
        if let Some(tx) = version.tx {
            db_transaction
                .transactions_dal()
                .insert_system_transaction(tx)
                .await;
        }

        db_transaction
            .protocol_versions_dal()
            .save_protocol_version(
                version.id,
                version.timestamp,
                version.l1_verifier_config,
                version.base_system_contracts_hashes,
                version.verifier_address,
                tx_hash,
            )
            .await;

        db_transaction.commit().await.unwrap();
    }

    async fn save_genesis_upgrade_tx_hash(&mut self, id: ProtocolVersionId, tx_hash: Option<H256>) {
        sqlx::query!(
            r#"
            UPDATE protocol_versions
            SET
                upgrade_tx_hash = $1
            WHERE
                id = $2
            "#,
            tx_hash.as_ref().map(H256::as_bytes),
            id as i32,
        )
        .execute(self.storage.conn())
        .await
        .unwrap();
    }

    /// Attaches a transaction used to set ChainId to the genesis protocol version.
    /// Also inserts that transaction into the database.
    pub async fn save_genesis_upgrade_with_tx(
        &mut self,
        id: ProtocolVersionId,
        tx: ProtocolUpgradeTx,
    ) {
        let tx_hash = Some(tx.common_data.hash());

        let mut db_transaction = self.storage.start_transaction().await.unwrap();

        db_transaction
            .transactions_dal()
            .insert_system_transaction(tx)
            .await;

        db_transaction
            .protocol_versions_dal()
            .save_genesis_upgrade_tx_hash(id, tx_hash)
            .await;

        db_transaction.commit().await.unwrap();
    }

    pub async fn protocol_version_id_by_timestamp(
        &mut self,
        current_timestamp: u64,
    ) -> sqlx::Result<ProtocolVersionId> {
        let row = sqlx::query!(
            r#"
            SELECT
                id
            FROM
                protocol_versions
            WHERE
                timestamp <= $1
            ORDER BY
                id DESC
            LIMIT
                1
            "#,
            current_timestamp as i64
        )
        .fetch_one(self.storage.conn())
        .await?;

        ProtocolVersionId::try_from(row.id as u16).map_err(|err| sqlx::Error::Decode(err.into()))
    }

    pub async fn load_base_system_contracts_by_version_id(
        &mut self,
        version_id: u16,
    ) -> anyhow::Result<Option<BaseSystemContracts>> {
        let row = sqlx::query!(
            r#"
            SELECT
                bootloader_code_hash,
                default_account_code_hash
            FROM
                protocol_versions
            WHERE
                id = $1
            "#,
            i32::from(version_id)
        )
        .fetch_optional(self.storage.conn())
        .await
        .context("cannot fetch system contract hashes")?;

        Ok(if let Some(row) = row {
            let contracts = self
                .storage
                .factory_deps_dal()
                .get_base_system_contracts(
                    H256::from_slice(&row.bootloader_code_hash),
                    H256::from_slice(&row.default_account_code_hash),
                )
                .await?;
            Some(contracts)
        } else {
            None
        })
    }

    pub async fn load_previous_version(
        &mut self,
        version_id: ProtocolVersionId,
    ) -> Option<ProtocolVersion> {
        let storage_protocol_version: StorageProtocolVersion = sqlx::query_as!(
            StorageProtocolVersion,
            r#"
            SELECT
                *
            FROM
                protocol_versions
            WHERE
                id < $1
            ORDER BY
                id DESC
            LIMIT
                1
            "#,
            version_id as i32
        )
        .fetch_optional(self.storage.conn())
        .await
        .unwrap()?;
        let tx = self
            .get_protocol_upgrade_tx((storage_protocol_version.id as u16).try_into().unwrap())
            .await;

        Some(protocol_version_from_storage(storage_protocol_version, tx))
    }

    pub async fn get_protocol_version(
        &mut self,
        version_id: ProtocolVersionId,
    ) -> Option<ProtocolVersion> {
        let storage_protocol_version: StorageProtocolVersion = sqlx::query_as!(
            StorageProtocolVersion,
            r#"
            SELECT
                *
            FROM
                protocol_versions
            WHERE
                id = $1
            "#,
            version_id as i32
        )
        .fetch_optional(self.storage.conn())
        .await
        .unwrap()?;
        let tx = self.get_protocol_upgrade_tx(version_id).await;

        Some(protocol_version_from_storage(storage_protocol_version, tx))
    }

    pub async fn l1_verifier_config_for_version(
        &mut self,
        version_id: ProtocolVersionId,
    ) -> Option<L1VerifierConfig> {
        let row = sqlx::query!(
            r#"
            SELECT
                recursion_scheduler_level_vk_hash,
                recursion_node_level_vk_hash,
                recursion_leaf_level_vk_hash,
                recursion_circuits_set_vks_hash
            FROM
                protocol_versions
            WHERE
                id = $1
            "#,
            version_id as i32
        )
        .fetch_optional(self.storage.conn())
        .await
        .unwrap()?;
        Some(L1VerifierConfig {
            params: VerifierParams {
                recursion_node_level_vk_hash: H256::from_slice(&row.recursion_node_level_vk_hash),
                recursion_leaf_level_vk_hash: H256::from_slice(&row.recursion_leaf_level_vk_hash),
                recursion_circuits_set_vks_hash: H256::from_slice(
                    &row.recursion_circuits_set_vks_hash,
                ),
            },
            recursion_scheduler_level_vk_hash: H256::from_slice(
                &row.recursion_scheduler_level_vk_hash,
            ),
        })
    }

    pub async fn last_version_id(&mut self) -> Option<ProtocolVersionId> {
        let id = sqlx::query!(
            r#"
            SELECT
                MAX(id) AS "max?"
            FROM
                protocol_versions
            "#
        )
        .fetch_optional(self.storage.conn())
        .await
        .unwrap()?
        .max?;
        Some((id as u16).try_into().unwrap())
    }

    pub async fn last_used_version_id(&mut self) -> Option<ProtocolVersionId> {
        let id = sqlx::query!(
            r#"
            SELECT
                protocol_version
            FROM
                l1_batches
            ORDER BY
                number DESC
            LIMIT
                1
            "#
        )
        .fetch_optional(self.storage.conn())
        .await
        .unwrap()?
        .protocol_version?;

        Some((id as u16).try_into().unwrap())
    }

    pub async fn all_version_ids(&mut self) -> Vec<ProtocolVersionId> {
        let rows = sqlx::query!(
            r#"
            SELECT
                id
            FROM
                protocol_versions
            "#
        )
        .fetch_all(self.storage.conn())
        .await
        .unwrap();
        rows.into_iter()
            .map(|row| (row.id as u16).try_into().unwrap())
            .collect()
    }

    pub async fn get_protocol_upgrade_tx(
        &mut self,
        protocol_version_id: ProtocolVersionId,
    ) -> Option<ProtocolUpgradeTx> {
        let row = sqlx::query!(
            r#"
            SELECT
                upgrade_tx_hash
            FROM
                protocol_versions
            WHERE
                id = $1
            "#,
            protocol_version_id as i32
        )
        .fetch_optional(self.storage.conn())
        .await
        .unwrap()?;
        if let Some(hash) = row.upgrade_tx_hash {
            Some(
                self.storage
                    .transactions_dal()
                    .get_tx_by_hash(H256::from_slice(&hash))
                    .await
                    .unwrap_or_else(|| {
                        panic!(
                            "Missing upgrade tx for protocol version {}",
                            protocol_version_id as u16
                        );
                    })
                    .try_into()
                    .unwrap(),
            )
        } else {
            None
        }
    }
}

use multivm::{
    interface::{L1BatchEnv, SystemEnv, VmExecutionResultAndLogs},
    utils::get_batch_base_fee,
};
use zksync_contracts::BaseSystemContractsHashes;
use zksync_types::{
    block::BlockGasCount, fee_model::BatchFeeInput,
    storage_writes_deduplicator::StorageWritesDeduplicator,
    tx::tx_execution_info::ExecutionMetrics, vm_trace::Call, Address, L1BatchNumber,
    MiniblockNumber, ProtocolVersionId, Transaction,
};
use zksync_utils::bytecode::CompressedBytecodeInfo;

pub(crate) use self::{l1_batch_updates::L1BatchUpdates, miniblock_updates::MiniblockUpdates};
use super::io::{IoCursor, MiniblockParams};

pub mod l1_batch_updates;
pub mod miniblock_updates;

/// Most of the information needed to seal the l1 batch/mini-block is contained within the VM,
/// things that are not captured there are accumulated externally.
/// `MiniblockUpdates` keeps updates for the pending mini-block.
/// `L1BatchUpdates` keeps updates for the already sealed mini-blocks of the pending L1 batch.
/// `UpdatesManager` manages the state of both of these accumulators to be consistent
/// and provides information about the pending state of the current L1 batch.
#[derive(Debug, PartialEq)]
pub struct UpdatesManager {
    batch_timestamp: u64,
    fee_account_address: Address,
    batch_fee_input: BatchFeeInput,
    base_fee_per_gas: u64,
    base_system_contract_hashes: BaseSystemContractsHashes,
    protocol_version: ProtocolVersionId,
    pub l1_batch: L1BatchUpdates,
    pub miniblock: MiniblockUpdates,
    pub storage_writes_deduplicator: StorageWritesDeduplicator,
}

impl UpdatesManager {
    pub(crate) fn new(l1_batch_env: &L1BatchEnv, system_env: &SystemEnv) -> Self {
        let protocol_version = system_env.version;
        Self {
            batch_timestamp: l1_batch_env.timestamp,
            fee_account_address: l1_batch_env.fee_account,
            batch_fee_input: l1_batch_env.fee_input,
            base_fee_per_gas: get_batch_base_fee(l1_batch_env, protocol_version.into()),
            protocol_version,
            base_system_contract_hashes: system_env.base_system_smart_contracts.hashes(),
            l1_batch: L1BatchUpdates::new(l1_batch_env.number),
            miniblock: MiniblockUpdates::new(
                l1_batch_env.first_l2_block.timestamp,
                MiniblockNumber(l1_batch_env.first_l2_block.number),
                l1_batch_env.first_l2_block.prev_block_hash,
                l1_batch_env.first_l2_block.max_virtual_blocks_to_create,
                protocol_version,
            ),
            storage_writes_deduplicator: StorageWritesDeduplicator::new(),
        }
    }

    pub(crate) fn batch_timestamp(&self) -> u64 {
        self.batch_timestamp
    }

    pub(crate) fn base_system_contract_hashes(&self) -> BaseSystemContractsHashes {
        self.base_system_contract_hashes
    }

    pub(crate) fn io_cursor(&self) -> IoCursor {
        IoCursor {
            next_miniblock: self.miniblock.number + 1,
            prev_miniblock_hash: self.miniblock.get_miniblock_hash(),
            prev_miniblock_timestamp: self.miniblock.timestamp,
            l1_batch: self.l1_batch.number,
        }
    }

    pub(crate) fn seal_miniblock_command(
        &self,
        l2_erc20_bridge_addr: Address,
        pre_insert_txs: bool,
    ) -> MiniblockSealCommand {
        MiniblockSealCommand {
            l1_batch_number: self.l1_batch.number,
            miniblock: self.miniblock.clone(),
            first_tx_index: self.l1_batch.executed_transactions.len(),
            fee_account_address: self.fee_account_address,
            fee_input: self.batch_fee_input,
            base_fee_per_gas: self.base_fee_per_gas,
            base_system_contracts_hashes: self.base_system_contract_hashes,
            protocol_version: Some(self.protocol_version),
            l2_erc20_bridge_addr,
            pre_insert_txs,
        }
    }

    pub(crate) fn protocol_version(&self) -> ProtocolVersionId {
        self.protocol_version
    }

    pub(crate) fn extend_from_executed_transaction(
        &mut self,
        tx: Transaction,
        tx_execution_result: VmExecutionResultAndLogs,
        compressed_bytecodes: Vec<CompressedBytecodeInfo>,
        tx_l1_gas_this_tx: BlockGasCount,
        execution_metrics: ExecutionMetrics,
        call_traces: Vec<Call>,
    ) {
        self.storage_writes_deduplicator
            .apply(&tx_execution_result.logs.storage_logs);
        self.miniblock.extend_from_executed_transaction(
            tx,
            tx_execution_result,
            tx_l1_gas_this_tx,
            execution_metrics,
            compressed_bytecodes,
            call_traces,
        );
    }

    pub(crate) fn extend_from_fictive_transaction(
        &mut self,
        result: VmExecutionResultAndLogs,
        l1_gas_count: BlockGasCount,
        execution_metrics: ExecutionMetrics,
    ) {
        self.storage_writes_deduplicator
            .apply(&result.logs.storage_logs);
        self.miniblock
            .extend_from_fictive_transaction(result, l1_gas_count, execution_metrics);
    }

    /// Pushes a new miniblock with the specified timestamp into this manager. The previously
    /// held miniblock is considered sealed and is used to extend the L1 batch data.
    pub(crate) fn push_miniblock(&mut self, miniblock_params: MiniblockParams) {
        let new_miniblock_updates = MiniblockUpdates::new(
            miniblock_params.timestamp,
            self.miniblock.number + 1,
            self.miniblock.get_miniblock_hash(),
            miniblock_params.virtual_blocks,
            self.protocol_version,
        );
        let old_miniblock_updates = std::mem::replace(&mut self.miniblock, new_miniblock_updates);
        self.l1_batch
            .extend_from_sealed_miniblock(old_miniblock_updates);
    }

    pub(crate) fn pending_executed_transactions_len(&self) -> usize {
        self.l1_batch.executed_transactions.len() + self.miniblock.executed_transactions.len()
    }

    pub(crate) fn pending_l1_gas_count(&self) -> BlockGasCount {
        self.l1_batch.l1_gas_count + self.miniblock.l1_gas_count
    }

    pub(crate) fn pending_execution_metrics(&self) -> ExecutionMetrics {
        self.l1_batch.block_execution_metrics + self.miniblock.block_execution_metrics
    }

    pub(crate) fn pending_txs_encoding_size(&self) -> usize {
        self.l1_batch.txs_encoding_size + self.miniblock.txs_encoding_size
    }
}

/// Command to seal a miniblock containing all necessary data for it.
#[derive(Debug)]
pub(crate) struct MiniblockSealCommand {
    pub l1_batch_number: L1BatchNumber,
    pub miniblock: MiniblockUpdates,
    pub first_tx_index: usize,
    pub fee_account_address: Address,
    pub fee_input: BatchFeeInput,
    pub base_fee_per_gas: u64,
    pub base_system_contracts_hashes: BaseSystemContractsHashes,
    pub protocol_version: Option<ProtocolVersionId>,
    pub l2_erc20_bridge_addr: Address,
    /// Whether transactions should be pre-inserted to DB.
    /// Should be set to `true` for EN's IO as EN doesn't store transactions in DB
    /// before they are included into miniblocks.
    pub pre_insert_txs: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gas_tracker::new_block_gas_count,
        state_keeper::tests::{
            create_execution_result, create_transaction, create_updates_manager,
        },
    };

    #[test]
    fn apply_miniblock() {
        // Init accumulators.
        let mut updates_manager = create_updates_manager();
        assert_eq!(updates_manager.pending_executed_transactions_len(), 0);

        // Apply tx.
        let tx = create_transaction(10, 100);
        updates_manager.extend_from_executed_transaction(
            tx,
            create_execution_result(0, []),
            vec![],
            new_block_gas_count(),
            ExecutionMetrics::default(),
            vec![],
        );

        // Check that only pending state is updated.
        assert_eq!(updates_manager.pending_executed_transactions_len(), 1);
        assert_eq!(updates_manager.miniblock.executed_transactions.len(), 1);
        assert_eq!(updates_manager.l1_batch.executed_transactions.len(), 0);

        // Seal miniblock.
        updates_manager.push_miniblock(MiniblockParams {
            timestamp: 2,
            virtual_blocks: 1,
        });

        // Check that L1 batch updates are the same with the pending state
        // and miniblock updates are empty.
        assert_eq!(updates_manager.pending_executed_transactions_len(), 1);
        assert_eq!(updates_manager.miniblock.executed_transactions.len(), 0);
        assert_eq!(updates_manager.l1_batch.executed_transactions.len(), 1);
    }
}

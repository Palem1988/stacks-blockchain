use std::collections::VecDeque;
use std::time::Instant;

use super::super::{Config};
use super::{BurnchainController, BurnchainTip};
use super::super::operations::BurnchainOpSigner;

use stacks::burnchains::{Burnchain, BurnchainBlockHeader, BurnchainHeaderHash, BurnchainBlock, Txid, BurnchainStateTransition};
use stacks::burnchains::bitcoin::BitcoinBlock;
use stacks::chainstate::burn::db::sortdb::{
    SortitionDB, SortitionHandleTx
};
use stacks::chainstate::burn::{BlockSnapshot};
use stacks::chainstate::burn::operations::{
    LeaderBlockCommitOp,
    LeaderKeyRegisterOp,
    UserBurnSupportOp,
    BlockstackOperationType,
};
use stacks::util::hash::Sha256Sum;
use stacks::util::get_epoch_time_secs;

/// MocknetController is simulating a simplistic burnchain.
pub struct MocknetController {
    config: Config,
    burnchain: Burnchain,
    db: Option<SortitionDB>,
    chain_tip: Option<BurnchainTip>,
    queued_operations: VecDeque<BlockstackOperationType>,
}

impl MocknetController {

    pub fn generic(config: Config) -> Box<dyn BurnchainController> {
        Box::new(Self::new(config))
    }

    fn new(config: Config) -> Self {
        debug!("Opening Burnchain at {}", &config.get_burn_db_path());
        let burnchain = Burnchain::new(&config.get_burn_db_path(), &config.burnchain.chain, &"regtest".to_string())
            .expect("Error while instantiating burnchain");

        Self {
            config: config,
            burnchain: burnchain,
            db: None,
            queued_operations: VecDeque::new(),
            chain_tip: None,
        }
    }

    fn build_next_block_header(current_block: &BlockSnapshot) -> BurnchainBlockHeader {
        let curr_hash = &current_block.burn_header_hash.to_bytes()[..];
        let next_hash = Sha256Sum::from_data(&curr_hash);

        let block = BurnchainBlock::Bitcoin(BitcoinBlock::new(
            current_block.block_height + 1,
            &BurnchainHeaderHash::from_bytes(next_hash.as_bytes()).unwrap(), 
            &current_block.burn_header_hash, 
            &vec![],
            get_epoch_time_secs()));
        block.header()
    }
}

impl BurnchainController for MocknetController {
    
    fn sortdb_ref(&self) -> &SortitionDB {
        self.db.as_ref().expect("BUG: did not instantiate burn DB")
    }

    fn sortdb_mut(&mut self) -> &mut SortitionDB {
        match self.db {
            Some(ref mut sortdb) => sortdb,
            None => {
                unreachable!();
            }
        }
    }
    
    fn get_chain_tip(&mut self) -> BurnchainTip {
        match &self.chain_tip {
            Some(chain_tip) => {
                chain_tip.clone()
            },
            None => {
                unreachable!();
            }
        }
    }
   
    fn start(&mut self) -> BurnchainTip {
        let db = match SortitionDB::connect(&self.config.get_burn_db_file_path(), 0, &BurnchainHeaderHash([0u8; 32]), get_epoch_time_secs(), true) {
            Ok(db) => db,
            Err(_) => panic!("Error while connecting to burnchain db")
        };
        let block_snapshot = SortitionDB::get_canonical_burn_chain_tip_stubbed(db.conn())
            .expect("FATAL: failed to get canonical chain tip");

        self.db = Some(db);

        let genesis_state = BurnchainTip {
            block_snapshot,
            state_transition: BurnchainStateTransition {
                burn_dist: vec![],
                accepted_ops: vec![],
                consumed_leader_keys: vec![]
            },
            received_at: Instant::now(),
        };
        self.chain_tip = Some(genesis_state.clone());

        genesis_state
    }

    fn submit_operation(&mut self, operation: BlockstackOperationType, _op_signer: &mut BurnchainOpSigner) -> bool {
        self.queued_operations.push_back(operation);
        true
    }

    fn sync(&mut self) -> BurnchainTip {
        let chain_tip = self.get_chain_tip();

        // Simulating mining
        let next_block_header = Self::build_next_block_header(&chain_tip.block_snapshot);
        let mut vtxindex = 1;
        let mut ops = vec![];

        while let Some(payload) = self.queued_operations.pop_front() {
            let txid = Txid(Sha256Sum::from_data(format!("{}::{}", next_block_header.block_height, vtxindex).as_bytes()).0);
            let op = match payload {
                BlockstackOperationType::LeaderKeyRegister(payload) => {
                    BlockstackOperationType::LeaderKeyRegister(LeaderKeyRegisterOp {
                        consensus_hash: payload.consensus_hash,
                        public_key: payload.public_key,
                        memo: payload.memo,
                        address: payload.address,
                        txid,
                        vtxindex: vtxindex,
                        block_height: next_block_header.block_height,
                        burn_header_hash: next_block_header.block_hash,
                    })
                },
                BlockstackOperationType::LeaderBlockCommit(payload) => {
                    BlockstackOperationType::LeaderBlockCommit(LeaderBlockCommitOp {
                        block_header_hash: payload.block_header_hash,
                        new_seed: payload.new_seed,
                        parent_block_ptr: payload.parent_block_ptr,
                        parent_vtxindex: payload.parent_vtxindex,
                        key_block_ptr: payload.key_block_ptr,
                        key_vtxindex: payload.key_vtxindex,
                        memo: payload.memo,
                        burn_fee: payload.burn_fee,
                        input: payload.input,
                        txid,
                        vtxindex: vtxindex,
                        block_height: next_block_header.block_height,
                        burn_header_hash: next_block_header.block_hash,
                    })
                },
                BlockstackOperationType::UserBurnSupport(payload) => {
                    BlockstackOperationType::UserBurnSupport(UserBurnSupportOp {
                        address: payload.address,
                        consensus_hash: payload.consensus_hash,
                        public_key: payload.public_key,
                        key_block_ptr: payload.key_block_ptr,
                        key_vtxindex: payload.key_vtxindex,
                        block_header_hash_160: payload.block_header_hash_160,
                        burn_fee: payload.burn_fee,
                        txid,
                        vtxindex: vtxindex,
                        block_height: next_block_header.block_height,
                        burn_header_hash: next_block_header.block_hash,
                    })
                }
            };
            ops.push(op);
            vtxindex += 1;
        }

        // Include txs in a new block   
        let (block_snapshot, state_transition) = {
            match self.db {
                None => {
                    unreachable!();
                },
                Some(ref mut burn_db) => {
                    let mut burn_tx = SortitionHandleTx::begin(
                        burn_db, &chain_tip.block_snapshot.sortition_id).unwrap();
                    let new_chain_tip = burn_tx.process_block_ops(
                        &self.burnchain, &chain_tip.block_snapshot, &next_block_header, ops).unwrap();
                    burn_tx.commit().unwrap();
                    new_chain_tip
                }
            }
        };

        // Transmit the new state
        let new_state = BurnchainTip {
            block_snapshot,
            state_transition,
            received_at: Instant::now()
        };
        self.chain_tip = Some(new_state.clone());

        new_state
    }

    #[cfg(test)]
    fn bootstrap_chain(&mut self, _num_blocks: u64) {}
}


//! Contains [Chain], a chain of blocks and their final state.

use crate::ExecutionOutcome;
use alloc::{borrow::Cow, collections::BTreeMap, vec::Vec};
use alloy_consensus::{transaction::Recovered, BlockHeader};
use alloy_eips::{eip1898::ForkBlock, eip2718::Encodable2718, BlockNumHash};
use alloy_primitives::{Address, BlockHash, BlockNumber, TxHash};
use core::{fmt, ops::RangeInclusive};
use reth_primitives_traits::{
    transaction::signed::SignedTransaction, Block, BlockBody, NodePrimitives, RecoveredBlock,
    SealedHeader,
};
use reth_trie_common::updates::TrieUpdates;
use revm::database::BundleState;

/// A chain of blocks and their final state.
///
/// The chain contains the state of accounts after execution of its blocks,
/// changesets for those blocks (and their transactions), as well as the blocks themselves.
///
/// Used inside the `BlockchainTree`.
///
/// # Warning
///
/// A chain of blocks should not be empty.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Chain<N: NodePrimitives = reth_ethereum_primitives::EthPrimitives> {
    /// All blocks in this chain.
    blocks: BTreeMap<BlockNumber, RecoveredBlock<N::Block>>,
    /// The outcome of block execution for this chain.
    ///
    /// This field contains the state of all accounts after the execution of all blocks in this
    /// chain, ranging from the [`Chain::first`] block to the [`Chain::tip`] block, inclusive.
    ///
    /// Additionally, it includes the individual state changes that led to the current state.
    execution_outcome: ExecutionOutcome<N::Receipt>,
    /// State trie updates after block is added to the chain.
    /// NOTE: Currently, trie updates are present only for
    /// single-block chains that extend the canonical chain.
    trie_updates: Option<TrieUpdates>,
}

impl<N: NodePrimitives> Default for Chain<N> {
    fn default() -> Self {
        Self {
            blocks: Default::default(),
            execution_outcome: Default::default(),
            trie_updates: Default::default(),
        }
    }
}

impl<N: NodePrimitives> Chain<N> {
    /// Create new Chain from blocks and state.
    ///
    /// # Warning
    ///
    /// A chain of blocks should not be empty.
    pub fn new(
        blocks: impl IntoIterator<Item = RecoveredBlock<N::Block>>,
        execution_outcome: ExecutionOutcome<N::Receipt>,
        trie_updates: Option<TrieUpdates>,
    ) -> Self {
        let blocks =
            blocks.into_iter().map(|b| (b.header().number(), b)).collect::<BTreeMap<_, _>>();
        debug_assert!(!blocks.is_empty(), "Chain should have at least one block");

        Self { blocks, execution_outcome, trie_updates }
    }

    /// Create new Chain from a single block and its state.
    pub fn from_block(
        block: RecoveredBlock<N::Block>,
        execution_outcome: ExecutionOutcome<N::Receipt>,
        trie_updates: Option<TrieUpdates>,
    ) -> Self {
        Self::new([block], execution_outcome, trie_updates)
    }

    /// Get the blocks in this chain.
    pub const fn blocks(&self) -> &BTreeMap<BlockNumber, RecoveredBlock<N::Block>> {
        &self.blocks
    }

    /// Consumes the type and only returns the blocks in this chain.
    pub fn into_blocks(self) -> BTreeMap<BlockNumber, RecoveredBlock<N::Block>> {
        self.blocks
    }

    /// Returns an iterator over all headers in the block with increasing block numbers.
    pub fn headers(&self) -> impl Iterator<Item = SealedHeader<N::BlockHeader>> + '_ {
        self.blocks.values().map(|block| block.clone_sealed_header())
    }

    /// Get cached trie updates for this chain.
    pub const fn trie_updates(&self) -> Option<&TrieUpdates> {
        self.trie_updates.as_ref()
    }

    /// Remove cached trie updates for this chain.
    pub fn clear_trie_updates(&mut self) {
        self.trie_updates.take();
    }

    /// Get execution outcome of this chain
    pub const fn execution_outcome(&self) -> &ExecutionOutcome<N::Receipt> {
        &self.execution_outcome
    }

    /// Get mutable execution outcome of this chain
    pub const fn execution_outcome_mut(&mut self) -> &mut ExecutionOutcome<N::Receipt> {
        &mut self.execution_outcome
    }

    /// Prepends the given state to the current state.
    pub fn prepend_state(&mut self, state: BundleState) {
        self.execution_outcome.prepend_state(state);
        self.trie_updates.take(); // invalidate cached trie updates
    }

    /// Return true if chain is empty and has no blocks.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Return block number of the block hash.
    pub fn block_number(&self, block_hash: BlockHash) -> Option<BlockNumber> {
        self.blocks.iter().find_map(|(num, block)| (block.hash() == block_hash).then_some(*num))
    }

    /// Returns the block with matching hash.
    pub fn recovered_block(&self, block_hash: BlockHash) -> Option<&RecoveredBlock<N::Block>> {
        self.blocks.iter().find_map(|(_num, block)| (block.hash() == block_hash).then_some(block))
    }

    /// Return execution outcome at the `block_number` or None if block is not known
    pub fn execution_outcome_at_block(
        &self,
        block_number: BlockNumber,
    ) -> Option<ExecutionOutcome<N::Receipt>> {
        if self.tip().number() == block_number {
            return Some(self.execution_outcome.clone())
        }

        if self.blocks.contains_key(&block_number) {
            let mut execution_outcome = self.execution_outcome.clone();
            execution_outcome.revert_to(block_number);
            return Some(execution_outcome)
        }
        None
    }

    /// Destructure the chain into its inner components:
    /// 1. The blocks contained in the chain.
    /// 2. The execution outcome representing the final state.
    /// 3. The optional trie updates.
    pub fn into_inner(
        self,
    ) -> (ChainBlocks<'static, N::Block>, ExecutionOutcome<N::Receipt>, Option<TrieUpdates>) {
        (ChainBlocks { blocks: Cow::Owned(self.blocks) }, self.execution_outcome, self.trie_updates)
    }

    /// Destructure the chain into its inner components:
    /// 1. A reference to the blocks contained in the chain.
    /// 2. A reference to the execution outcome representing the final state.
    pub const fn inner(&self) -> (ChainBlocks<'_, N::Block>, &ExecutionOutcome<N::Receipt>) {
        (ChainBlocks { blocks: Cow::Borrowed(&self.blocks) }, &self.execution_outcome)
    }

    /// Returns an iterator over all the receipts of the blocks in the chain.
    pub fn block_receipts_iter(&self) -> impl Iterator<Item = &Vec<N::Receipt>> + '_ {
        self.execution_outcome.receipts().iter()
    }

    /// Returns an iterator over all blocks in the chain with increasing block number.
    pub fn blocks_iter(&self) -> impl Iterator<Item = &RecoveredBlock<N::Block>> + '_ {
        self.blocks().iter().map(|block| block.1)
    }

    /// Returns an iterator over all blocks and their receipts in the chain.
    pub fn blocks_and_receipts(
        &self,
    ) -> impl Iterator<Item = (&RecoveredBlock<N::Block>, &Vec<N::Receipt>)> + '_ {
        self.blocks_iter().zip(self.block_receipts_iter())
    }

    /// Get the block at which this chain forked.
    pub fn fork_block(&self) -> ForkBlock {
        let first = self.first();
        ForkBlock {
            number: first.header().number().saturating_sub(1),
            hash: first.header().parent_hash(),
        }
    }

    /// Get the first block in this chain.
    ///
    /// # Panics
    ///
    /// If chain doesn't have any blocks.
    #[track_caller]
    pub fn first(&self) -> &RecoveredBlock<N::Block> {
        self.blocks.first_key_value().expect("Chain should have at least one block").1
    }

    /// Get the tip of the chain.
    ///
    /// # Panics
    ///
    /// If chain doesn't have any blocks.
    #[track_caller]
    pub fn tip(&self) -> &RecoveredBlock<N::Block> {
        self.blocks.last_key_value().expect("Chain should have at least one block").1
    }

    /// Returns length of the chain.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Returns the range of block numbers in the chain.
    ///
    /// # Panics
    ///
    /// If chain doesn't have any blocks.
    pub fn range(&self) -> RangeInclusive<BlockNumber> {
        self.first().header().number()..=self.tip().header().number()
    }

    /// Get all receipts for the given block.
    pub fn receipts_by_block_hash(&self, block_hash: BlockHash) -> Option<Vec<&N::Receipt>> {
        let num = self.block_number(block_hash)?;
        Some(self.execution_outcome.receipts_by_block(num).iter().collect())
    }

    /// Get all receipts with attachment.
    ///
    /// Attachment includes block number, block hash, transaction hash and transaction index.
    pub fn receipts_with_attachment(&self) -> Vec<BlockReceipts<N::Receipt>>
    where
        N::SignedTx: Encodable2718,
    {
        let mut receipt_attach = Vec::with_capacity(self.blocks().len());

        self.blocks_and_receipts().for_each(|(block, receipts)| {
            let block_num_hash = BlockNumHash::new(block.number(), block.hash());

            let tx_receipts = block
                .body()
                .transactions()
                .iter()
                .zip(receipts)
                .map(|(tx, receipt)| (tx.trie_hash(), receipt.clone()))
                .collect();

            receipt_attach.push(BlockReceipts {
                block: block_num_hash,
                tx_receipts,
                timestamp: block.timestamp(),
            });
        });

        receipt_attach
    }

    /// Append a single block with state to the chain.
    /// This method assumes that blocks attachment to the chain has already been validated.
    pub fn append_block(
        &mut self,
        block: RecoveredBlock<N::Block>,
        execution_outcome: ExecutionOutcome<N::Receipt>,
    ) {
        self.blocks.insert(block.header().number(), block);
        self.execution_outcome.extend(execution_outcome);
        self.trie_updates.take(); // reset
    }

    /// Merge two chains by appending the given chain into the current one.
    ///
    /// The state of accounts for this chain is set to the state of the newest chain.
    ///
    /// Returns the passed `other` chain in [`Result::Err`] variant if the chains could not be
    /// connected.
    pub fn append_chain(&mut self, other: Self) -> Result<(), Self> {
        let chain_tip = self.tip();
        let other_fork_block = other.fork_block();
        if chain_tip.hash() != other_fork_block.hash {
            return Err(other)
        }

        // Insert blocks from other chain
        self.blocks.extend(other.blocks);
        self.execution_outcome.extend(other.execution_outcome);
        self.trie_updates.take(); // reset

        Ok(())
    }
}

/// Wrapper type for `blocks` display in `Chain`
#[derive(Debug)]
pub struct DisplayBlocksChain<'a, B: reth_primitives_traits::Block>(
    pub &'a BTreeMap<BlockNumber, RecoveredBlock<B>>,
);

impl<B: reth_primitives_traits::Block> fmt::Display for DisplayBlocksChain<'_, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut list = f.debug_list();
        let mut values = self.0.values().map(|block| block.num_hash());
        if values.len() <= 3 {
            list.entries(values);
        } else {
            list.entry(&values.next().unwrap());
            list.entry(&format_args!("..."));
            list.entry(&values.next_back().unwrap());
        }
        list.finish()
    }
}

/// All blocks in the chain
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainBlocks<'a, B: Block> {
    blocks: Cow<'a, BTreeMap<BlockNumber, RecoveredBlock<B>>>,
}

impl<B: Block<Body: BlockBody<Transaction: SignedTransaction>>> ChainBlocks<'_, B> {
    /// Creates a consuming iterator over all blocks in the chain with increasing block number.
    ///
    /// Note: this always yields at least one block.
    #[inline]
    pub fn into_blocks(self) -> impl Iterator<Item = RecoveredBlock<B>> {
        self.blocks.into_owned().into_values()
    }

    /// Creates an iterator over all blocks in the chain with increasing block number.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (&BlockNumber, &RecoveredBlock<B>)> {
        self.blocks.iter()
    }

    /// Get the tip of the chain.
    ///
    /// # Note
    ///
    /// Chains always have at least one block.
    #[inline]
    pub fn tip(&self) -> &RecoveredBlock<B> {
        self.blocks.last_key_value().expect("Chain should have at least one block").1
    }

    /// Get the _first_ block of the chain.
    ///
    /// # Note
    ///
    /// Chains always have at least one block.
    #[inline]
    pub fn first(&self) -> &RecoveredBlock<B> {
        self.blocks.first_key_value().expect("Chain should have at least one block").1
    }

    /// Returns an iterator over all transactions in the chain.
    #[inline]
    pub fn transactions(&self) -> impl Iterator<Item = &<B::Body as BlockBody>::Transaction> + '_ {
        self.blocks.values().flat_map(|block| block.body().transactions_iter())
    }

    /// Returns an iterator over all transactions and their senders.
    #[inline]
    pub fn transactions_with_sender(
        &self,
    ) -> impl Iterator<Item = (&Address, &<B::Body as BlockBody>::Transaction)> + '_ {
        self.blocks.values().flat_map(|block| block.transactions_with_sender())
    }

    /// Returns an iterator over all [`Recovered`] in the blocks
    ///
    /// Note: This clones the transactions since it is assumed this is part of a shared [Chain].
    #[inline]
    pub fn transactions_ecrecovered(
        &self,
    ) -> impl Iterator<Item = Recovered<<B::Body as BlockBody>::Transaction>> + '_ {
        self.transactions_with_sender().map(|(signer, tx)| tx.clone().with_signer(*signer))
    }

    /// Returns an iterator over all transaction hashes in the block
    #[inline]
    pub fn transaction_hashes(&self) -> impl Iterator<Item = TxHash> + '_ {
        self.blocks
            .values()
            .flat_map(|block| block.body().transactions_iter().map(|tx| tx.trie_hash()))
    }
}

impl<B: Block> IntoIterator for ChainBlocks<'_, B> {
    type Item = (BlockNumber, RecoveredBlock<B>);
    type IntoIter = alloc::collections::btree_map::IntoIter<BlockNumber, RecoveredBlock<B>>;

    fn into_iter(self) -> Self::IntoIter {
        self.blocks.into_owned().into_iter()
    }
}

/// Used to hold receipts and their attachment.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct BlockReceipts<T = reth_ethereum_primitives::Receipt> {
    /// Block identifier
    pub block: BlockNumHash,
    /// Transaction identifier and receipt.
    pub tx_receipts: Vec<(TxHash, T)>,
    /// Block timestamp
    pub timestamp: u64,
}

/// Bincode-compatible [`Chain`] serde implementation.
#[cfg(feature = "serde-bincode-compat")]
pub(super) mod serde_bincode_compat {
    use crate::{serde_bincode_compat, ExecutionOutcome};
    use alloc::{borrow::Cow, collections::BTreeMap};
    use alloy_primitives::BlockNumber;
    use reth_ethereum_primitives::EthPrimitives;
    use reth_primitives_traits::{
        serde_bincode_compat::{RecoveredBlock, SerdeBincodeCompat},
        Block, NodePrimitives,
    };
    use reth_trie_common::serde_bincode_compat::updates::TrieUpdates;
    use serde::{ser::SerializeMap, Deserialize, Deserializer, Serialize, Serializer};
    use serde_with::{DeserializeAs, SerializeAs};

    /// Bincode-compatible [`super::Chain`] serde implementation.
    ///
    /// Intended to use with the [`serde_with::serde_as`] macro in the following way:
    /// ```rust
    /// use reth_execution_types::{serde_bincode_compat, Chain};
    /// use serde::{Deserialize, Serialize};
    /// use serde_with::serde_as;
    ///
    /// #[serde_as]
    /// #[derive(Serialize, Deserialize)]
    /// struct Data {
    ///     #[serde_as(as = "serde_bincode_compat::Chain")]
    ///     chain: Chain,
    /// }
    /// ```
    #[derive(Debug, Serialize, Deserialize)]
    pub struct Chain<'a, N = EthPrimitives>
    where
        N: NodePrimitives<
            Block: Block<Header: SerdeBincodeCompat, Body: SerdeBincodeCompat> + 'static,
        >,
    {
        blocks: RecoveredBlocks<'a, N::Block>,
        execution_outcome: serde_bincode_compat::ExecutionOutcome<'a, N::Receipt>,
        trie_updates: Option<TrieUpdates<'a>>,
    }

    #[derive(Debug)]
    struct RecoveredBlocks<
        'a,
        B: reth_primitives_traits::Block<Header: SerdeBincodeCompat, Body: SerdeBincodeCompat>
            + 'static,
    >(Cow<'a, BTreeMap<BlockNumber, reth_primitives_traits::RecoveredBlock<B>>>);

    impl<B> Serialize for RecoveredBlocks<'_, B>
    where
        B: Block<Header: SerdeBincodeCompat, Body: SerdeBincodeCompat> + 'static,
    {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut state = serializer.serialize_map(Some(self.0.len()))?;

            for (block_number, block) in self.0.iter() {
                state.serialize_entry(block_number, &RecoveredBlock::<'_, B>::from(block))?;
            }

            state.end()
        }
    }

    impl<'de, B> Deserialize<'de> for RecoveredBlocks<'_, B>
    where
        B: Block<Header: SerdeBincodeCompat, Body: SerdeBincodeCompat> + 'static,
    {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            Ok(Self(Cow::Owned(
                BTreeMap::<BlockNumber, RecoveredBlock<'_, B>>::deserialize(deserializer)
                    .map(|blocks| blocks.into_iter().map(|(n, b)| (n, b.into())).collect())?,
            )))
        }
    }

    impl<'a, N> From<&'a super::Chain<N>> for Chain<'a, N>
    where
        N: NodePrimitives<
            Block: Block<Header: SerdeBincodeCompat, Body: SerdeBincodeCompat> + 'static,
        >,
    {
        fn from(value: &'a super::Chain<N>) -> Self {
            Self {
                blocks: RecoveredBlocks(Cow::Borrowed(&value.blocks)),
                execution_outcome: value.execution_outcome.as_repr(),
                trie_updates: value.trie_updates.as_ref().map(Into::into),
            }
        }
    }

    impl<'a, N> From<Chain<'a, N>> for super::Chain<N>
    where
        N: NodePrimitives<
            Block: Block<Header: SerdeBincodeCompat, Body: SerdeBincodeCompat> + 'static,
        >,
    {
        fn from(value: Chain<'a, N>) -> Self {
            Self {
                blocks: value.blocks.0.into_owned(),
                execution_outcome: ExecutionOutcome::from_repr(value.execution_outcome),
                trie_updates: value.trie_updates.map(Into::into),
            }
        }
    }

    impl<N> SerializeAs<super::Chain<N>> for Chain<'_, N>
    where
        N: NodePrimitives<
            Block: Block<Header: SerdeBincodeCompat, Body: SerdeBincodeCompat> + 'static,
        >,
    {
        fn serialize_as<S>(source: &super::Chain<N>, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Chain::from(source).serialize(serializer)
        }
    }

    impl<'de, N> DeserializeAs<'de, super::Chain<N>> for Chain<'de, N>
    where
        N: NodePrimitives<
            Block: Block<Header: SerdeBincodeCompat, Body: SerdeBincodeCompat> + 'static,
        >,
    {
        fn deserialize_as<D>(deserializer: D) -> Result<super::Chain<N>, D::Error>
        where
            D: Deserializer<'de>,
        {
            Chain::deserialize(deserializer).map(Into::into)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::{serde_bincode_compat, Chain};
        use arbitrary::Arbitrary;
        use rand::Rng;
        use reth_primitives_traits::RecoveredBlock;
        use serde::{Deserialize, Serialize};
        use serde_with::serde_as;

        #[test]
        fn test_chain_bincode_roundtrip() {
            #[serde_as]
            #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
            struct Data {
                #[serde_as(as = "serde_bincode_compat::Chain")]
                chain: Chain,
            }

            let mut bytes = [0u8; 1024];
            rand::rng().fill(bytes.as_mut_slice());
            let data = Data {
                chain: Chain::new(
                    vec![RecoveredBlock::arbitrary(&mut arbitrary::Unstructured::new(&bytes))
                        .unwrap()],
                    Default::default(),
                    None,
                ),
            };

            let encoded = bincode::serialize(&data).unwrap();
            let decoded: Data = bincode::deserialize(&encoded).unwrap();
            assert_eq!(decoded, data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::TxType;
    use alloy_primitives::{Address, B256};
    use reth_ethereum_primitives::Receipt;
    use revm::{primitives::HashMap, state::AccountInfo};

    #[test]
    fn chain_append() {
        let block: RecoveredBlock<reth_ethereum_primitives::Block> = Default::default();
        let block1_hash = B256::new([0x01; 32]);
        let block2_hash = B256::new([0x02; 32]);
        let block3_hash = B256::new([0x03; 32]);
        let block4_hash = B256::new([0x04; 32]);

        let mut block1 = block.clone();
        let mut block2 = block.clone();
        let mut block3 = block.clone();
        let mut block4 = block;

        block1.set_hash(block1_hash);
        block2.set_hash(block2_hash);
        block3.set_hash(block3_hash);
        block4.set_hash(block4_hash);

        block3.set_parent_hash(block2_hash);

        let mut chain1: Chain =
            Chain { blocks: BTreeMap::from([(1, block1), (2, block2)]), ..Default::default() };

        let chain2 =
            Chain { blocks: BTreeMap::from([(3, block3), (4, block4)]), ..Default::default() };

        assert!(chain1.append_chain(chain2.clone()).is_ok());

        // chain1 got changed so this will fail
        assert!(chain1.append_chain(chain2).is_err());
    }

    #[test]
    fn test_number_split() {
        let execution_outcome1: ExecutionOutcome = ExecutionOutcome::new(
            BundleState::new(
                vec![(
                    Address::new([2; 20]),
                    None,
                    Some(AccountInfo::default()),
                    HashMap::default(),
                )],
                vec![vec![(Address::new([2; 20]), None, vec![])]],
                vec![],
            ),
            vec![vec![]],
            1,
            vec![],
        );

        let execution_outcome2 = ExecutionOutcome::new(
            BundleState::new(
                vec![(
                    Address::new([3; 20]),
                    None,
                    Some(AccountInfo::default()),
                    HashMap::default(),
                )],
                vec![vec![(Address::new([3; 20]), None, vec![])]],
                vec![],
            ),
            vec![vec![]],
            2,
            vec![],
        );

        let mut block1: RecoveredBlock<reth_ethereum_primitives::Block> = Default::default();
        let block1_hash = B256::new([15; 32]);
        block1.set_block_number(1);
        block1.set_hash(block1_hash);
        block1.push_sender(Address::new([4; 20]));

        let mut block2: RecoveredBlock<reth_ethereum_primitives::Block> = Default::default();
        let block2_hash = B256::new([16; 32]);
        block2.set_block_number(2);
        block2.set_hash(block2_hash);
        block2.push_sender(Address::new([4; 20]));

        let mut block_state_extended = execution_outcome1;
        block_state_extended.extend(execution_outcome2);

        let chain: Chain =
            Chain::new(vec![block1.clone(), block2.clone()], block_state_extended, None);

        // return tip state
        assert_eq!(
            chain.execution_outcome_at_block(block2.number),
            Some(chain.execution_outcome.clone())
        );
        // state at unknown block
        assert_eq!(chain.execution_outcome_at_block(100), None);
    }

    #[test]
    fn receipts_by_block_hash() {
        // Create a default RecoveredBlock object
        let block: RecoveredBlock<reth_ethereum_primitives::Block> = Default::default();

        // Define block hashes for block1 and block2
        let block1_hash = B256::new([0x01; 32]);
        let block2_hash = B256::new([0x02; 32]);

        // Clone the default block into block1 and block2
        let mut block1 = block.clone();
        let mut block2 = block;

        // Set the hashes of block1 and block2
        block1.set_hash(block1_hash);
        block2.set_hash(block2_hash);

        // Create a random receipt object, receipt1
        let receipt1 = Receipt {
            tx_type: TxType::Legacy,
            cumulative_gas_used: 46913,
            logs: vec![],
            success: true,
        };

        // Create another random receipt object, receipt2
        let receipt2 = Receipt {
            tx_type: TxType::Legacy,
            cumulative_gas_used: 1325345,
            logs: vec![],
            success: true,
        };

        // Create a Receipts object with a vector of receipt vectors
        let receipts = vec![vec![receipt1.clone()], vec![receipt2]];

        // Create an ExecutionOutcome object with the created bundle, receipts, an empty requests
        // vector, and first_block set to 10
        let execution_outcome = ExecutionOutcome {
            bundle: Default::default(),
            receipts,
            requests: vec![],
            first_block: 10,
        };

        // Create a Chain object with a BTreeMap of blocks mapped to their block numbers,
        // including block1_hash and block2_hash, and the execution_outcome
        let chain: Chain = Chain {
            blocks: BTreeMap::from([(10, block1), (11, block2)]),
            execution_outcome: execution_outcome.clone(),
            ..Default::default()
        };

        // Assert that the proper receipt vector is returned for block1_hash
        assert_eq!(chain.receipts_by_block_hash(block1_hash), Some(vec![&receipt1]));

        // Create an ExecutionOutcome object with a single receipt vector containing receipt1
        let execution_outcome1 = ExecutionOutcome {
            bundle: Default::default(),
            receipts: vec![vec![receipt1]],
            requests: vec![],
            first_block: 10,
        };

        // Assert that the execution outcome at the first block contains only the first receipt
        assert_eq!(chain.execution_outcome_at_block(10), Some(execution_outcome1));

        // Assert that the execution outcome at the tip block contains the whole execution outcome
        assert_eq!(chain.execution_outcome_at_block(11), Some(execution_outcome));
    }
}

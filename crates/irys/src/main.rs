use std::{
    marker::PhantomData,
    sync::Arc,
    time::{Duration, SystemTime},
};

use alloy_consensus::{SignableTransaction, TxEip4844, TxLegacy};
use alloy_eips::{eip7840::BlobParams, merge::EPOCH_SLOTS, Encodable2718};
use alloy_genesis::Genesis;
use alloy_network::{EthereumWallet, NetworkWallet};
use alloy_primitives::{keccak256, Address, Bytes, Log, LogData, Signature, TxKind, B256, U256};
use alloy_rpc_types::{engine::PayloadAttributes, TransactionInput, TransactionRequest};
use evm::{CustomBlockAssembler, MyEthEvmFactory};
use futures::StreamExt;
use reth::{
    api::{
        BuiltPayload, FullNodeComponents, FullNodeTypes, NodePrimitives, NodeTypes, PayloadTypes,
    },
    builder::{
        components::{BasicPayloadServiceBuilder, ComponentsBuilder, ExecutorBuilder, PoolBuilder},
        BuilderContext, DebugNode, Node, NodeAdapter, NodeComponentsBuilder, PayloadBuilderConfig,
    },
    payload::{EthBuiltPayload, EthPayloadBuilderAttributes},
    primitives::{EthPrimitives, Recovered},
    providers::{
        providers::ProviderFactoryBuilder, AccountReader, CanonStateSubscriptions, EthStorage,
    },
    transaction_pool::{
        blobstore::InMemoryBlobStore, EthTransactionPool, PoolConfig,
        TransactionValidationTaskExecutor,
    },
};
use reth_chainspec::{ChainSpec, EthChainSpec, EthereumHardforks};
use reth_e2e_test_utils::{setup, transaction::TransactionTestContext, wallet::Wallet};
use reth_ethereum_engine_primitives::EthPayloadAttributes;
use reth_ethereum_primitives::TransactionSigned;
use reth_evm::EthEvmFactory;
use reth_evm_ethereum::RethReceiptBuilder;
use reth_network::NetworkEventListenerProvider;
use reth_node_ethereum::{
    node::{
        EthereumAddOns, EthereumConsensusBuilder, EthereumExecutorBuilder, EthereumNetworkBuilder,
        EthereumPayloadBuilder,
    },
    EthEngineTypes, EthEvmConfig, EthereumNode,
};
use reth_tracing::{
    tracing::{self, level_filters::LevelFilter},
    LayerInfo, LogFormat, RethTracer, Tracer,
};
use reth_transaction_pool::{
    blobstore::{DiskFileBlobStore, DiskFileBlobStoreConfig},
    CoinbaseTipOrdering, EthPooledTransaction, EthTransactionValidator, Pool, PoolTransaction,
    Priority, TransactionOrdering, TransactionPool,
};
use reth_trie_db::MerklePatriciaTrie;
use tracing::{debug, info};

pub(crate) fn eth_payload_attributes(timestamp: u64) -> EthPayloadBuilderAttributes {
    let attributes = PayloadAttributes {
        timestamp,
        prev_randao: B256::ZERO,
        suggested_fee_recipient: Address::ZERO,
        withdrawals: Some(vec![]),
        parent_beacon_block_root: Some(B256::ZERO),
    };
    EthPayloadBuilderAttributes::new(B256::ZERO, attributes)
}

// todo make this into EIP-712 structured data, domain separators, etc
#[derive(serde::Deserialize, serde::Serialize, Debug)]
struct BalanceDecrement {
    amount_to_decrement_by: U256,
    target: Address,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let _ = RethTracer::new()
        .with_stdout(LayerInfo::new(
            LogFormat::Terminal,
            LevelFilter::INFO.to_string(),
            "reth_transaction_pool=debug".to_string(),
            Some("always".to_string()),
        ))
        .init();

    let (mut nodes, _tasks, wallet) =
        setup::<IrysEthereumNode>(1, custom_chain(), false, eth_payload_attributes).await?;
    let wallets = Wallet::new(3).wallet_gen();

    let mut node = nodes.pop().unwrap();
    let mut node_engine_api_events = node.inner.provider.canonical_state_stream();
    let mut node_reth_events = node.inner.network.event_listener();

    let tx_loop = async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        let mut nonce = 0;
        interval.reset();

        if false {
            return eyre::Result::<_, eyre::Report>::Ok(());
        }
        let signer_a = EthereumWallet::from(wallets[1].clone());
        let signer_a = signer_a.default_signer();

        let signer_b = EthereumWallet::from(wallet.inner.clone());
        let signer_b = signer_b.default_signer();

        loop {
            interval.tick().await;
            tracing::info!("start tx sending");

            // todo current issues:
            // - our custom tx does not get processed on subsequent calls (does it get stuck in mempol or not accepted by the mempool? )
            // - if we *only* provide our custom tx, then the whole node crashes on the second call attempt
            // {
            //     // submit a valid tx
            //     let mut tx_raw = TxLegacy {
            //         gas_limit: 99000,
            //         value: U256::ZERO,
            //         nonce,
            //         gas_price: 1_000_000_000u128, // 1 Gwei
            //         chain_id: Some(1),
            //         input: vec![123].into(),
            //         to: TxKind::Call(Address::random()),
            //     };
            //     let pooled_tx = sign_tx(tx_raw, &signer_a).await;
            //     let tx_hash = node
            //         .inner
            //         .pool
            //         .add_transaction(reth_transaction_pool::TransactionOrigin::Local, pooled_tx)
            //         .await
            //         .unwrap();
            // }
            // Get the balance of signer_b
            let signer_a_address = signer_a.address();
            let signer_a_balance = node
                .inner
                .provider
                .basic_account(&signer_a_address)
                .map(|account_info| account_info.map_or(U256::ZERO, |acc| acc.balance))
                .unwrap_or_else(|err| {
                    tracing::warn!("Failed to get signer_a balance: {}", err);
                    U256::ZERO
                });
            let signer_b_address = signer_b.address();
            let signer_b_balance = node
                .inner
                .provider
                .basic_account(&signer_b_address)
                .map(|account_info| account_info.map_or(U256::ZERO, |acc| acc.balance))
                .unwrap_or_else(|err| {
                    tracing::warn!("Failed to get signer_b balance: {}", err);
                    U256::ZERO
                });
            tracing::info!(?signer_b_address, ?signer_b_balance, "Signer B balance");
            tracing::info!(?signer_a_address, ?signer_a_balance, "Signer A balance");
            {
                // submit a valid tx
                let tx_raw = TxLegacy {
                    gas_limit: 99000,
                    value: U256::ZERO,
                    nonce: 0,
                    gas_price: 1_000_000_000u128, // 1 Gwei
                    chain_id: Some(1),
                    to: TxKind::Call(Address::ZERO),
                    input: serde_json::to_vec(&BalanceDecrement {
                        amount_to_decrement_by: {
                            // every second "decrement tx" will fail
                            // if nonce % 2 == 1 {
                            //     tracing::warn!("max");
                            //     U256::MAX
                            // } else {
                            //     tracing::warn!("one");
                            //     U256::ONE
                            // }
                            U256::ONE
                        },
                        target: signer_a.address(),
                    })
                    .unwrap()
                    .into(),
                    ..Default::default()
                };
                let pooled_tx = sign_tx(tx_raw, &signer_b).await;
                let tx_hash = node
                    .inner
                    .pool
                    .add_transaction(reth_transaction_pool::TransactionOrigin::Private, pooled_tx)
                    .await
                    .unwrap();
                tracing::error!("expected hash {:}", tx_hash);
            }
            let block_payload = node.new_payload().await?;
            let block_payload_hash = node.submit_payload(block_payload.clone()).await?;
            // // trigger forkchoice update via engine api to commit the block to the blockchain
            node.update_forkchoice(block_payload_hash, block_payload_hash).await?;
            let outcome = node.inner.provider.get_state(0..=1).unwrap().unwrap();
            tracing::info!(?outcome.receipts);

            // // assert that the tx is included in the block
            // let header = block_payload.block().header();
            // node.assert_new_block(tx_hash, block_payload_hash, header.number).await?;

            // print out the transactions that were included in the block
            // let body = block_payload.block().body();
            // let txs = &body.transactions;
            // tracing::info!(?txs);

            nonce += 1;
        }
    };

    // Reth event stream
    let reth_events = tokio::spawn(async move {
        use futures::StreamExt;
        while let Some(update) = node_reth_events.next().await {
            tracing::warn!(?update, "Received network event");
        }
    });

    // Process canonical state updates concurrently
    let state_processing = tokio::spawn(async move {
        use futures::StreamExt;

        while let Some(update) = node_engine_api_events.next().await {
            tracing::debug!(?update, "Received canonical state update");
        }
    });

    tokio::select! {
        err = tx_loop => {
            tracing::error!(?err, "transaction loop crashed");
        }
        _ = state_processing => {
            tracing::error!("state processing task crashed");
        }
        _ = reth_events => {
            tracing::error!("state processing task crashed");
        }
    }
    Ok(())
}

async fn sign_tx(
    mut tx_raw: TxLegacy,
    new_signer: &Arc<dyn alloy_network::TxSigner<Signature> + Send + Sync>,
) -> EthPooledTransaction<alloy_consensus::EthereumTxEnvelope<TxEip4844>> {
    let signed_tx = new_signer.sign_transaction(&mut tx_raw).await.unwrap();
    let tx = alloy_consensus::EthereumTxEnvelope::Legacy(tx_raw.into_signed(signed_tx))
        .try_into_recovered()
        .unwrap();

    let pooled_tx = EthPooledTransaction::new(tx.clone(), 300);

    return pooled_tx;
}

fn custom_chain() -> Arc<ChainSpec> {
    let custom_genesis = r#"
{
  "config": {
    "chainId": 1,
    "homesteadBlock": 0,
    "daoForkSupport": true,
    "eip150Block": 0,
    "eip155Block": 0,
    "eip158Block": 0,
    "byzantiumBlock": 0,
    "constantinopleBlock": 0,
    "petersburgBlock": 0,
    "istanbulBlock": 0,
    "muirGlacierBlock": 0,
    "berlinBlock": 0,
    "londonBlock": 0,
    "arrowGlacierBlock": 0,
    "grayGlacierBlock": 0,
    "shanghaiTime": 0,
    "cancunTime": 0,
    "terminalTotalDifficulty": "0x0",
    "terminalTotalDifficultyPassed": true
  },
  "nonce": "0x0",
  "timestamp": "0x0",
  "extraData": "0x00",
  "gasLimit": "0x1c9c380",
  "difficulty": "0x0",
  "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
  "coinbase": "0x0000000000000000000000000000000000000000",
  "alloc": {
    "0x14dc79964da2c08b23698b3d3cc7ca32193d9955": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x15d34aaf54267db7d7c367839aaf71a00a2c6a65": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x1cbd3b2770909d4e10f157cabc84c7264073c9ec": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x23618e81e3f5cdf7f54c3d65f7fbc0abf5b21e8f": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x2546bcd3c84621e976d8185a91a922ae77ecec30": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x70997970c51812dc3a010c7d01b50e0d17dc79c8": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x71be63f3384f5fb98995898a86b02fb2426c5788": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x8626f6940e2eb28930efb4cef49b2d1f2c9c1199": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x90f79bf6eb2c4f870365e785982e1f101e93b906": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x976ea74026e726554db657fa54763abd0c3a0aa9": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x9965507d1a55bcc2695c58ba16fb37d819b0a4dc": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0x9c41de96b2088cdc640c6182dfcf5491dc574a57": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0xa0ee7a142d267c1f36714e4a8f75612f20a79720": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0xbcd4042de499d14e55001ccbb24a551f3b954096": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0xbda5747bfd65f08deb54cb465eb87d40e51b197e": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0xcd3b766ccdd6ae721141f452c550ca635964ce71": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0xdd2fd4581271e230360230f9337d5c0430bf44c0": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0xdf3e18d64bc6a983f673ab319ccae4f1a57c7097": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266": {
      "balance": "0xd3c21bcecceda1000000"
    },
    "0xfabb0ac9d68b0b445fb7357272ff202c5651694a": {
      "balance": "0xd3c21bcecceda1000000"
    }
  },
  "number": "0x0"
}
"#;
    let genesis: Genesis = serde_json::from_str(custom_genesis).unwrap();
    Arc::new(genesis.into())
}

/// -- eth node custom logic
/// Type configuration for a regular Ethereum node.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct IrysEthereumNode;

impl NodeTypes for IrysEthereumNode {
    type Primitives = EthPrimitives;
    type ChainSpec = ChainSpec;
    type StateCommitment = MerklePatriciaTrie;
    type Storage = EthStorage;
    type Payload = EthEngineTypes;
}

impl IrysEthereumNode {
    /// Returns a [`ComponentsBuilder`] configured for a regular Ethereum node.
    pub fn components<Node>() -> ComponentsBuilder<
        Node,
        CustomPoolBuilder,
        BasicPayloadServiceBuilder<EthereumPayloadBuilder>,
        EthereumNetworkBuilder,
        CustomEthereumExecutorBuilder,
        EthereumConsensusBuilder,
    >
    where
        Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec, Primitives = EthPrimitives>>,
        <Node::Types as NodeTypes>::Payload: PayloadTypes<
            BuiltPayload = EthBuiltPayload,
            PayloadAttributes = EthPayloadAttributes,
            PayloadBuilderAttributes = EthPayloadBuilderAttributes,
        >,
    {
        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(CustomPoolBuilder::default())
            .executor(CustomEthereumExecutorBuilder::default())
            .payload(BasicPayloadServiceBuilder::default())
            .network(EthereumNetworkBuilder::default())
            .consensus(EthereumConsensusBuilder::default())
    }

    pub fn provider_factory_builder() -> ProviderFactoryBuilder<Self> {
        ProviderFactoryBuilder::default()
    }
}

impl<N> Node<N> for IrysEthereumNode
where
    N: FullNodeTypes<Types = Self>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        CustomPoolBuilder,
        BasicPayloadServiceBuilder<EthereumPayloadBuilder>,
        EthereumNetworkBuilder,
        CustomEthereumExecutorBuilder,
        EthereumConsensusBuilder,
    >;

    type AddOns = EthereumAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        Self::components()
    }

    fn add_ons(&self) -> Self::AddOns {
        EthereumAddOns::default()
    }
}

impl<N: FullNodeComponents<Types = Self>> DebugNode<N> for IrysEthereumNode {
    type RpcBlock = alloy_rpc_types_eth::Block;

    fn rpc_to_primitive_block(rpc_block: Self::RpcBlock) -> reth_ethereum_primitives::Block {
        let alloy_rpc_types_eth::Block { header, transactions, withdrawals, .. } = rpc_block;
        reth_ethereum_primitives::Block {
            header: header.inner,
            body: reth_ethereum_primitives::BlockBody {
                transactions: transactions
                    .into_transactions()
                    .map(|tx| tx.inner.into_inner().into())
                    .collect(),
                ommers: Default::default(),
                withdrawals,
            },
        }
    }
}

/// A custom pool builder
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CustomPoolBuilder;

/// Implement the [`PoolBuilder`] trait for the custom pool builder
///
/// This will be used to build the transaction pool and its maintenance tasks during launch.
impl<Types, Node> PoolBuilder<Node> for CustomPoolBuilder
where
    Types: NodeTypes<
        ChainSpec: EthereumHardforks,
        Primitives: NodePrimitives<SignedTx = TransactionSigned>,
    >,
    Node: FullNodeTypes<Types = Types>,
{
    type Pool = Pool<
        TransactionValidationTaskExecutor<
            EthTransactionValidator<Node::Provider, EthPooledTransaction>,
        >,
        SystemTxsCoinbaseTipOrdering<EthPooledTransaction>,
        DiskFileBlobStore,
    >;

    async fn build_pool(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Pool> {
        let data_dir = ctx.config().datadir();
        let pool_config = ctx.pool_config();

        let blob_cache_size = if let Some(blob_cache_size) = pool_config.blob_cache_size {
            blob_cache_size
        } else {
            // get the current blob params for the current timestamp, fallback to default Cancun
            // params
            let current_timestamp =
                SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
            let blob_params = ctx
                .chain_spec()
                .blob_params_at_timestamp(current_timestamp)
                .unwrap_or_else(BlobParams::cancun);

            // Derive the blob cache size from the target blob count, to auto scale it by
            // multiplying it with the slot count for 2 epochs: 384 for pectra
            (blob_params.target_blob_count * EPOCH_SLOTS * 2) as u32
        };

        let custom_config =
            DiskFileBlobStoreConfig::default().with_max_cached_entries(blob_cache_size);

        let blob_store = DiskFileBlobStore::open(data_dir.blobstore(), custom_config)?;
        let validator = TransactionValidationTaskExecutor::eth_builder(ctx.provider().clone())
            .with_head_timestamp(ctx.head().timestamp)
            .kzg_settings(ctx.kzg_settings()?)
            .with_local_transactions_config(pool_config.local_transactions_config.clone())
            .set_tx_fee_cap(ctx.config().rpc.rpc_tx_fee_cap)
            .with_additional_tasks(ctx.config().txpool.additional_validation_tasks)
            .build_with_tasks(ctx.task_executor().clone(), blob_store.clone());

        let ordering = SystemTxsCoinbaseTipOrdering::default();
        let transaction_pool =
            reth_transaction_pool::Pool::new(validator, ordering, blob_store, pool_config);
        info!(target: "reth::cli", "Transaction pool initialized");

        // spawn txpool maintenance task
        {
            let pool = transaction_pool.clone();
            let chain_events = ctx.provider().canonical_state_stream();
            let client = ctx.provider().clone();
            // Only spawn backup task if not disabled
            if !ctx.config().txpool.disable_transactions_backup {
                // Use configured backup path or default to data dir
                let transactions_path = ctx
                    .config()
                    .txpool
                    .transactions_backup_path
                    .clone()
                    .unwrap_or_else(|| data_dir.txpool_transactions());

                let transactions_backup_config =
                    reth_transaction_pool::maintain::LocalTransactionBackupConfig::with_local_txs_backup(transactions_path);

                ctx.task_executor().spawn_critical_with_graceful_shutdown_signal(
                    "local transactions backup task",
                    |shutdown| {
                        reth_transaction_pool::maintain::backup_local_transactions_task(
                            shutdown,
                            pool.clone(),
                            transactions_backup_config,
                        )
                    },
                );
            }

            // spawn the maintenance task
            ctx.task_executor().spawn_critical(
                "txpool maintenance task",
                reth_transaction_pool::maintain::maintain_transaction_pool_future(
                    client,
                    pool,
                    chain_events,
                    ctx.task_executor().clone(),
                    reth_transaction_pool::maintain::MaintainPoolConfig {
                        max_tx_lifetime: transaction_pool.config().max_queued_lifetime,
                        no_local_exemptions: transaction_pool
                            .config()
                            .local_transactions_config
                            .no_exemptions,
                        ..Default::default()
                    },
                ),
            );
            debug!(target: "reth::cli", "Spawned txpool maintenance task");
        }

        Ok(transaction_pool)
    }
}

/// System txs go to the top
/// The transactions are ordered by their coinbase tip.
/// The higher the coinbase tip is, the higher the priority of the transaction.
#[derive(Debug)]
#[non_exhaustive]
pub struct SystemTxsCoinbaseTipOrdering<T>(PhantomData<T>);

impl<T> TransactionOrdering for SystemTxsCoinbaseTipOrdering<T>
where
    T: PoolTransaction + 'static,
{
    type PriorityValue = U256;
    type Transaction = T;

    /// Source: <https://github.com/ethereum/go-ethereum/blob/7f756dc1185d7f1eeeacb1d12341606b7135f9ea/core/txpool/legacypool/list.go#L469-L482>.
    ///
    /// NOTE: The implementation is incomplete for missing base fee.
    fn priority(
        &self,
        transaction: &Self::Transaction,
        base_fee: u64,
    ) -> Priority<Self::PriorityValue> {
        let bytes = transaction.input();
        if let Ok(_decrement_tx) = serde_json::from_slice::<BalanceDecrement>(bytes.as_ref()) {
            return Priority::Value(U256::MAX);
        }
        transaction.effective_tip_per_gas(base_fee).map(U256::from).into()
    }
}

impl<T> Default for SystemTxsCoinbaseTipOrdering<T> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<T> Clone for SystemTxsCoinbaseTipOrdering<T> {
    fn clone(&self) -> Self {
        Self::default()
    }
}

/// A regular ethereum evm and executor builder.
#[derive(Debug, Default, Clone, Copy)]
pub struct CustomEthereumExecutorBuilder;

impl<Types, Node> ExecutorBuilder<Node> for CustomEthereumExecutorBuilder
where
    Types: NodeTypes<ChainSpec = ChainSpec, Primitives = EthPrimitives>,
    Node: FullNodeTypes<Types = Types>,
{
    type EVM = evm::CustomEvmConfig;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        let evm_config = EthEvmConfig::new(ctx.chain_spec())
            .with_extra_data(ctx.payload_builder_config().extra_data_bytes());
        let spec = ctx.chain_spec();
        let evm_factory = MyEthEvmFactory::default();
        let evm_config = evm::CustomEvmConfig {
            inner: evm_config,
            assembler: CustomBlockAssembler::new(ctx.chain_spec()),
            executor_factory: evm::CustomBlockExecutorFactory::new(
                RethReceiptBuilder::default(),
                spec,
                evm_factory,
            ),
        };
        Ok(evm_config)
    }
}

mod evm {
    use std::convert::Infallible;

    use alloy_consensus::{Block, Header, Transaction, TxReceipt};
    use alloy_dyn_abi::{DynSolType, DynSolValue};
    use alloy_evm::block::{BlockExecutionError, BlockExecutor, ExecutableTx, OnStateHook};
    use alloy_evm::eth::receipt_builder::ReceiptBuilder;
    use alloy_evm::eth::spec::EthExecutorSpec;
    use alloy_evm::eth::EthBlockExecutor;
    use alloy_evm::{Database, Evm, FromRecoveredTx, FromTxWithEncoded};
    use alloy_primitives::{keccak256, Bytes, Log, LogData, Uint};
    use reth::primitives::{SealedBlock, SealedHeader};
    use reth::providers::BlockExecutionResult;
    use reth::revm::context::result::ExecutionResult;
    use reth::revm::context::TxEnv;
    use reth::revm::primitives::hardfork::SpecId;
    use reth::revm::{Inspector, State};
    use reth_ethereum_primitives::Receipt;
    use reth_evm::block::{BlockExecutorFactory, BlockExecutorFor, BlockValidationError};
    use reth_evm::eth::receipt_builder::ReceiptBuilderCtx;
    use reth_evm::eth::spec::EthSpec;
    use reth_evm::eth::{EthBlockExecutionCtx, EthBlockExecutorFactory, EthEvmContext};
    use reth_evm::execute::{BlockAssembler, BlockAssemblerInput};
    use reth_evm::precompiles::PrecompilesMap;
    use reth_evm::{
        ConfigureEvm, EthEvm, EthEvmFactory, EvmEnv, EvmFactory, InspectorFor, IntoTxEnv,
        NextBlockEnvAttributes, TransactionEnv,
    };
    use reth_evm_ethereum::{EthBlockAssembler, RethReceiptBuilder};
    use revm::context::result::{EVMError, HaltReason, InvalidTransaction, Output};
    use revm::context::{BlockEnv, CfgEnv};
    use revm::database::PlainAccount;
    use revm::inspector::NoOpInspector;
    use revm::precompile::{PrecompileSpecId, Precompiles};
    use revm::state::{Account, EvmStorageSlot};
    use revm::{DatabaseCommit, MainBuilder, MainContext};

    use super::*;

    pub struct CustomBlockExecutor<'a, Evm> {
        receipt_builder: &'a RethReceiptBuilder,
        system_call_receipts: Vec<Receipt>,
        pub inner: EthBlockExecutor<'a, Evm, &'a Arc<ChainSpec>, &'a RethReceiptBuilder>,
    }

    impl<'db, DB, E> BlockExecutor for CustomBlockExecutor<'_, E>
    where
        DB: Database + 'db,
        E: Evm<
            DB = &'db mut State<DB>,
            Tx: FromRecoveredTx<TransactionSigned> + FromTxWithEncoded<TransactionSigned>,
        >,
    {
        type Transaction = TransactionSigned;
        type Receipt = Receipt;
        type Evm = E;

        fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
            self.inner.apply_pre_execution_changes()
        }

        fn execute_transaction_with_result_closure(
            &mut self,
            tx: impl ExecutableTx<Self>,
            f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>),
        ) -> Result<u64, BlockExecutionError> {
            let aa = tx.tx();
            if let Ok(decrement_tx) =
                serde_json::from_slice::<BalanceDecrement>(aa.input().as_ref())
            {
                let evm = self.inner.evm_mut();
                let db = evm.db_mut();
                let state = db.load_cache_account(decrement_tx.target).unwrap();
                let hash = *aa.hash();
                tracing::error!("special tx {:}", hash);

                // handle a case when an account has never existed (0 balance, no data stored on it)
                // We don't even create a receipt in this case (eth does the same with native txs)
                let Some(plain_account) = state.account.as_ref() else {
                    tracing::warn!("account does not exist");
                    return Err(BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                        hash: *aa.hash(),
                        // todo is there a more appropriate error to use?
                        error: Box::new(InvalidTransaction::OverflowPaymentInTransaction),
                    }));
                };

                // handle a case where the balance of the user is too low
                let mut new_state = alloy_primitives::map::foldhash::HashMap::default();
                let execution_result;
                if plain_account.info.balance < decrement_tx.amount_to_decrement_by {
                    tracing::warn!(?plain_account.info.balance, ?decrement_tx.amount_to_decrement_by);
                    execution_result =
                        ExecutionResult::Revert { gas_used: 0, output: Bytes::new() };
                } else {
                    // Create new account info with updated balance
                    let new_account_balance =
                        plain_account.info.balance - decrement_tx.amount_to_decrement_by;
                    let storage = plain_account
                        .storage
                        .iter()
                        .map(|(k, v)| (*k, EvmStorageSlot::new(*v)))
                        .collect();
                    let mut new_account_info = plain_account.info.clone();
                    new_account_info.nonce = new_account_info.nonce.saturating_add(1);

                    new_account_info.balance = new_account_balance;
                    new_state.insert(
                        decrement_tx.target,
                        Account {
                            info: new_account_info,
                            storage,
                            status: revm::state::AccountStatus::Touched,
                        },
                    );

                    // generate logs to attach to the receipt
                    let param_values = DynSolValue::Tuple(vec![
                        DynSolValue::Uint(decrement_tx.amount_to_decrement_by, 256),
                        DynSolValue::Address(decrement_tx.target),
                    ]);
                    let encoded_data = param_values.abi_encode();
                    let log = Log {
                        address: decrement_tx.target,
                        data: LogData::new(
                            vec![keccak256("SYSTEM_TX_DECREMENT_BALANCE")],
                            encoded_data.into(),
                        )
                        .unwrap(),
                    };
                    execution_result = ExecutionResult::Success {
                        reason: revm::context::result::SuccessReason::Return,
                        gas_used: 0,
                        gas_refunded: 0,
                        logs: vec![log],
                        output: Output::Call(Bytes::new()),
                    }
                }
                f(&execution_result);

                self.system_call_receipts.push(self.receipt_builder.build_receipt(
                    ReceiptBuilderCtx {
                        tx: tx.tx(),
                        evm,
                        result: execution_result,
                        state: &new_state,
                        // cumulative gas used is 0 because system txs are always executed at the beginning of the block, and use 0 gas
                        cumulative_gas_used: 0,
                    },
                ));

                // Commit the changes to the database
                let evm = self.inner.evm_mut();
                let db = evm.db_mut();
                db.commit(new_state);

                Ok(0)
            } else {
                let res = self.inner.execute_transaction_with_result_closure(tx, f);
                dbg!(&res);
                res
            }
        }

        fn finish(self) -> Result<(Self::Evm, BlockExecutionResult<Receipt>), BlockExecutionError> {
            let (evm, mut block_res) = self.inner.finish()?;
            let total_receipts = [self.system_call_receipts, block_res.receipts].concat();
            block_res.receipts = total_receipts;

            Ok((evm, block_res))
        }

        fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
            self.inner.set_state_hook(hook)
        }

        fn evm_mut(&mut self) -> &mut Self::Evm {
            self.inner.evm_mut()
        }

        fn evm(&self) -> &Self::Evm {
            self.inner.evm()
        }
    }

    /// Block builder for Ethereum.
    #[derive(Debug, Clone)]
    pub struct CustomBlockAssembler<ChainSpec = reth_chainspec::ChainSpec> {
        inner: EthBlockAssembler<ChainSpec>,
    }

    impl<ChainSpec> CustomBlockAssembler<ChainSpec> {
        /// Creates a new [`CustomBlockAssembler`].
        pub fn new(chain_spec: Arc<ChainSpec>) -> Self {
            Self { inner: EthBlockAssembler::new(chain_spec) }
        }
    }

    impl<F, ChainSpec> BlockAssembler<F> for CustomBlockAssembler<ChainSpec>
    where
        F: for<'a> BlockExecutorFactory<
            ExecutionCtx<'a> = EthBlockExecutionCtx<'a>,
            Transaction = TransactionSigned,
            Receipt = Receipt,
        >,
        ChainSpec: EthChainSpec + EthereumHardforks,
    {
        type Block = Block<TransactionSigned>;

        fn assemble_block(
            &self,
            input: BlockAssemblerInput<'_, '_, F>,
        ) -> Result<Block<TransactionSigned>, BlockExecutionError> {
            self.inner.assemble_block(input)
        }
    }

    /// Ethereum block executor factory.
    #[derive(Debug, Clone, Default)]
    pub struct CustomBlockExecutorFactory {
        inner: EthBlockExecutorFactory<RethReceiptBuilder, Arc<ChainSpec>, MyEthEvmFactory>,
    }

    impl CustomBlockExecutorFactory {
        /// Creates a new [`EthBlockExecutorFactory`] with the given spec, [`EvmFactory`], and
        /// [`ReceiptBuilder`].
        pub const fn new(
            receipt_builder: RethReceiptBuilder,
            spec: Arc<ChainSpec>,
            evm_factory: MyEthEvmFactory,
        ) -> Self {
            Self { inner: EthBlockExecutorFactory::new(receipt_builder, spec, evm_factory) }
        }

        /// Exposes the receipt builder.
        pub const fn receipt_builder(&self) -> &RethReceiptBuilder {
            self.inner.receipt_builder()
        }

        /// Exposes the chain specification.
        pub const fn spec(&self) -> &Arc<ChainSpec> {
            self.inner.spec()
        }

        /// Exposes the EVM factory.
        pub const fn evm_factory(&self) -> &MyEthEvmFactory {
            self.inner.evm_factory()
        }
    }

    impl BlockExecutorFactory for CustomBlockExecutorFactory
    where
        Self: 'static,
    {
        type EvmFactory = MyEthEvmFactory;
        type ExecutionCtx<'a> = EthBlockExecutionCtx<'a>;
        type Transaction = TransactionSigned;
        type Receipt = Receipt;

        fn evm_factory(&self) -> &Self::EvmFactory {
            &self.inner.evm_factory()
        }

        fn create_executor<'a, DB, I>(
            &'a self,
            evm: <MyEthEvmFactory as EvmFactory>::Evm<&'a mut State<DB>, I>,
            ctx: Self::ExecutionCtx<'a>,
        ) -> impl BlockExecutorFor<'a, Self, DB, I>
        where
            DB: Database + 'a,
            I: Inspector<<EthEvmFactory as EvmFactory>::Context<&'a mut State<DB>>> + 'a,
        {
            let receipt_builder = self.inner.receipt_builder();
            CustomBlockExecutor {
                inner: EthBlockExecutor::new(evm, ctx, self.inner.spec(), receipt_builder),
                receipt_builder,
                system_call_receipts: vec![],
            }
        }
    }

    #[derive(Debug, Clone)]
    pub struct CustomEvmConfig {
        pub inner: EthEvmConfig<EthEvmFactory>,
        pub executor_factory: CustomBlockExecutorFactory,
        pub assembler: CustomBlockAssembler,
    }

    impl ConfigureEvm for CustomEvmConfig {
        type Primitives = EthPrimitives;
        type Error = Infallible;
        type NextBlockEnvCtx = NextBlockEnvAttributes;
        type BlockExecutorFactory = CustomBlockExecutorFactory;
        type BlockAssembler = CustomBlockAssembler<ChainSpec>;

        fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
            &self.executor_factory
        }

        fn block_assembler(&self) -> &Self::BlockAssembler {
            &self.assembler
        }

        fn evm_env(&self, header: &Header) -> EvmEnv {
            self.inner.evm_env(header)
        }

        fn next_evm_env(
            &self,
            parent: &Header,
            attributes: &NextBlockEnvAttributes,
        ) -> Result<EvmEnv, Self::Error> {
            self.inner.next_evm_env(parent, attributes)
        }

        fn context_for_block<'a>(
            &self,
            block: &'a SealedBlock<alloy_consensus::Block<TransactionSigned>>,
        ) -> EthBlockExecutionCtx<'a> {
            self.inner.context_for_block(block)
        }

        fn context_for_next_block(
            &self,
            parent: &SealedHeader,
            attributes: Self::NextBlockEnvCtx,
        ) -> EthBlockExecutionCtx<'_> {
            self.inner.context_for_next_block(parent, attributes)
        }
    }

    /// Factory producing [`EthEvm`].
    #[derive(Debug, Default, Clone, Copy)]
    #[non_exhaustive]
    pub struct MyEthEvmFactory;

    impl EvmFactory for MyEthEvmFactory {
        type Evm<DB: Database, I: Inspector<EthEvmContext<DB>>> = EthEvm<DB, I, Self::Precompiles>;
        type Context<DB: Database> = revm::Context<BlockEnv, TxEnv, CfgEnv, DB>;
        type Tx = TxEnv;
        type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
        type HaltReason = HaltReason;
        type Spec = SpecId;
        type Precompiles = PrecompilesMap;

        fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
            let spec_id = input.cfg_env.spec;
            EthEvm::new(
                revm::Context::mainnet()
                    .with_block(input.block_env)
                    .with_cfg(input.cfg_env)
                    .with_db(db)
                    .build_mainnet_with_inspector(NoOpInspector {})
                    .with_precompiles(PrecompilesMap::from_static(Precompiles::new(
                        PrecompileSpecId::from_spec_id(spec_id),
                    ))),
                false,
            )
        }

        fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>>>(
            &self,
            db: DB,
            input: EvmEnv,
            inspector: I,
        ) -> Self::Evm<DB, I> {
            let spec_id = input.cfg_env.spec;
            EthEvm::new(
                revm::Context::mainnet()
                    .with_block(input.block_env)
                    .with_cfg(input.cfg_env)
                    .with_db(db)
                    .build_mainnet_with_inspector(inspector)
                    .with_precompiles(PrecompilesMap::from_static(Precompiles::new(
                        PrecompileSpecId::from_spec_id(spec_id),
                    ))),
                true,
            )
        }
    }
}

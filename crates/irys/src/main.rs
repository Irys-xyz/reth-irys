use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use alloy_eips::{eip7840::BlobParams, merge::EPOCH_SLOTS, Encodable2718};
use alloy_genesis::Genesis;
use alloy_primitives::{Address, TxKind, B256, U256};
use alloy_rpc_types::{engine::PayloadAttributes, TransactionInput, TransactionRequest};
use reth::{
    api::{FullNodeComponents, FullNodeTypes, NodePrimitives, NodeTypes, PayloadTypes},
    builder::{
        components::{BasicPayloadServiceBuilder, ComponentsBuilder, PoolBuilder},
        BuilderContext, DebugNode, Node, NodeAdapter, NodeComponentsBuilder,
    },
    payload::{EthBuiltPayload, EthPayloadBuilderAttributes},
    primitives::EthPrimitives,
    providers::{providers::ProviderFactoryBuilder, CanonStateSubscriptions, EthStorage},
    transaction_pool::{
        blobstore::InMemoryBlobStore, EthTransactionPool, PoolConfig,
        TransactionValidationTaskExecutor,
    },
};
use reth_chainspec::{ChainSpec, EthChainSpec, EthereumHardforks};
use reth_e2e_test_utils::{setup, transaction::TransactionTestContext};
use reth_ethereum_engine_primitives::EthPayloadAttributes;
use reth_ethereum_primitives::TransactionSigned;
use reth_network::NetworkEventListenerProvider;
use reth_node_ethereum::{
    node::{
        EthereumAddOns, EthereumConsensusBuilder, EthereumExecutorBuilder, EthereumNetworkBuilder,
        EthereumPayloadBuilder,
    },
    EthEngineTypes, EthereumNode,
};
use reth_tracing::{
    tracing::{self, level_filters::LevelFilter},
    LayerInfo, LogFormat, RethTracer, Tracer,
};
use reth_transaction_pool::blobstore::{DiskFileBlobStore, DiskFileBlobStoreConfig};
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

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let _ = RethTracer::new()
        .with_stdout(LayerInfo::new(
            LogFormat::Terminal,
            LevelFilter::INFO.to_string(),
            "".to_string(),
            Some("always".to_string()),
        ))
        .init();

    let (mut nodes, _tasks, wallet) =
        setup::<IrysEthereumNode>(1, custom_chain(), false, eth_payload_attributes).await?;

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

        loop {
            interval.tick().await;
            tracing::info!("start tx sending");

            // Make the first node advance
            let evm_tx = TransactionRequest {
                nonce: Some(nonce),
                value: Some(U256::from(100)),
                to: Some(TxKind::Call(Address::random())),
                gas: Some(21000),
                max_fee_per_gas: Some(20e9 as u128),
                max_priority_fee_per_gas: Some(20e9 as u128),
                chain_id: Some(1),
                input: TransactionInput { input: None, data: None },
                authorization_list: None,
                ..Default::default()
            };
            let raw_tx = TransactionTestContext::sign_tx(wallet.inner.clone(), evm_tx)
                .await
                .encoded_2718()
                .into();
            let tx_hash = node.rpc.inject_tx(raw_tx).await?;
            let block_payload = node.new_payload().await?;
            let block_payload_hash = node.submit_payload(block_payload.clone()).await?;
            // trigger forkchoice update via engine api to commit the block to the blockchain
            node.update_forkchoice(block_payload_hash, block_payload_hash).await?;

            // assert that the tx is included in the block
            node.assert_new_block(
                tx_hash,
                block_payload_hash,
                block_payload.block().header().number,
            )
            .await?;

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
        EthereumExecutorBuilder,
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
            .executor(EthereumExecutorBuilder::default())
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
        EthereumExecutorBuilder,
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
    type Pool = EthTransactionPool<Node::Provider, DiskFileBlobStore>;

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

        let transaction_pool =
            reth_transaction_pool::Pool::eth_pool(validator, blob_store, pool_config);
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

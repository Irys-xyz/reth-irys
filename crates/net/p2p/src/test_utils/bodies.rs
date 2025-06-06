use crate::{
    bodies::client::{BodiesClient, BodiesFut},
    download::DownloadClient,
    error::PeerRequestResult,
    priority::Priority,
};
use alloy_primitives::B256;
use futures::FutureExt;
use reth_ethereum_primitives::BlockBody;
use reth_network_peers::PeerId;
use std::{
    fmt::{Debug, Formatter},
    ops::RangeInclusive,
};
use tokio::sync::oneshot;

/// A test client for fetching bodies
pub struct TestBodiesClient<F> {
    /// The function that is called on each body request.
    pub responder: F,
}

impl<F> Debug for TestBodiesClient<F> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestBodiesClient").finish_non_exhaustive()
    }
}

impl<F: Sync + Send> DownloadClient for TestBodiesClient<F> {
    fn report_bad_message(&self, _peer_id: PeerId) {
        // noop
    }

    fn num_connected_peers(&self) -> usize {
        0
    }
}

impl<F> BodiesClient for TestBodiesClient<F>
where
    F: Fn(Vec<B256>) -> PeerRequestResult<Vec<BlockBody>> + Send + Sync,
{
    type Body = BlockBody;
    type Output = BodiesFut;

    fn get_block_bodies_with_priority_and_range_hint(
        &self,
        hashes: Vec<B256>,
        _priority: Priority,
        _range_hint: Option<RangeInclusive<u64>>,
    ) -> Self::Output {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send((self.responder)(hashes));
        Box::pin(rx.map(|x| match x {
            Ok(value) => value,
            Err(err) => Err(err.into()),
        }))
    }
}

use anyhow::Context;
use async_trait::async_trait;
use committable::Commitment;
use espresso_types::{
    traits::StateCatchup, v0_99::ChainConfig, BackoffParams, BlockMerkleTree, EpochCommittees,
    FeeAccount, FeeAccountProof, FeeMerkleCommitment, FeeMerkleTree, Leaf2, NodeState, SeqTypes,
};
use hotshot::traits::NodeImplementation;
use hotshot_types::{data::{EpochNumber, ViewNumber}, traits::node_implementation::Versions};

use super::{request::Request, RequestResponseProtocol};

#[async_trait]
impl<I: NodeImplementation<SeqTypes>, V: Versions> StateCatchup for RequestResponseProtocol<I, V> {
    async fn try_fetch_leaves(&self, retry: usize, height: u64) -> anyhow::Result<Vec<Leaf2>> {
        unreachable!()
    }

    async fn fetch_leaf(
        &self,
        height: u64,
        membership: &EpochCommittees,
        epoch: EpochNumber,
        epoch_height: u64,
    ) -> anyhow::Result<Leaf2> {
        // Create a request for a leaf at the given height
        let request = Request::Leaf(height);

        // Request the leaf from the other nodes
        let response = self
            .request_indefinitely(
                &self.public_key,
                &self.private_key,
                self.config.incoming_request_ttl,
                request,
            )
            .await
            .with_context(|| format!("failed to fetch leaf at height {}", height));

        todo!()
    }

    async fn try_fetch_accounts(
        &self,
        retry: usize,
        instance: &NodeState,
        height: u64,
        view: ViewNumber,
        fee_merkle_tree_root: FeeMerkleCommitment,
        account: &[FeeAccount],
    ) -> anyhow::Result<FeeMerkleTree> {
        todo!()
    }

    /// Fetch the given list of accounts, retrying on transient errors.
    async fn fetch_accounts(
        &self,
        instance: &NodeState,
        height: u64,
        view: ViewNumber,
        fee_merkle_tree_root: FeeMerkleCommitment,
        accounts: Vec<FeeAccount>,
    ) -> anyhow::Result<Vec<FeeAccountProof>> {
        todo!()
    }

    /// Try to fetch and remember the blocks frontier, failing without retrying if unable.
    async fn try_remember_blocks_merkle_tree(
        &self,
        retry: usize,
        instance: &NodeState,
        height: u64,
        view: ViewNumber,
        mt: &mut BlockMerkleTree,
    ) -> anyhow::Result<()> {
        todo!()
    }

    /// Fetch and remember the blocks frontier, retrying on transient errors.
    async fn remember_blocks_merkle_tree(
        &self,
        instance: &NodeState,
        height: u64,
        view: ViewNumber,
        mt: &mut BlockMerkleTree,
    ) -> anyhow::Result<()> {
        todo!()
    }

    async fn try_fetch_chain_config(
        &self,
        retry: usize,
        commitment: Commitment<ChainConfig>,
    ) -> anyhow::Result<ChainConfig> {
        todo!()
    }

    async fn fetch_chain_config(
        &self,
        commitment: Commitment<ChainConfig>,
    ) -> anyhow::Result<ChainConfig> {
        todo!()
    }

    fn backoff(&self) -> &BackoffParams {
        todo!()
    }

    fn name(&self) -> String {
        todo!()
    }
}

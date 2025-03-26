use std::{num::NonZeroU64, sync::Arc};

use crate::request_response::request::{Request, Response};
use anyhow::Context;
use async_trait::async_trait;
use committable::{Commitment, Committable};
use espresso_types::{
    traits::StateCatchup,
    v0_1::{RewardAccount, RewardAccountProof, RewardMerkleCommitment, RewardMerkleTree},
    v0_99::ChainConfig,
    BackoffParams, BlockMerkleTree, EpochVersion, FeeAccount, FeeAccountProof, FeeMerkleCommitment,
    FeeMerkleTree, Leaf2, NodeState, PubKey, SeqTypes, SequencerVersions,
};
use hotshot::traits::NodeImplementation;
use hotshot_types::{
    data::ViewNumber, message::UpgradeLock, traits::node_implementation::Versions,
    utils::verify_epoch_root_chain, PeerConfig,
};
use jf_merkle_tree::{ForgetableMerkleTreeScheme, MerkleTreeScheme};
use parking_lot::Mutex;
use tracing::warn;

use crate::request_response::RequestResponseProtocol;

#[async_trait]
impl<I: NodeImplementation<SeqTypes>, V: Versions> StateCatchup for RequestResponseProtocol<I, V> {
    async fn try_fetch_leaves(&self, _retry: usize, _height: u64) -> anyhow::Result<Vec<Leaf2>> {
        unreachable!()
    }

    async fn try_fetch_accounts(
        &self,
        _retry: usize,
        _instance: &NodeState,
        _height: u64,
        _view: ViewNumber,
        _fee_merkle_tree_root: FeeMerkleCommitment,
        _accounts: &[FeeAccount],
    ) -> anyhow::Result<FeeMerkleTree> {
        unreachable!()
    }

    async fn try_remember_blocks_merkle_tree(
        &self,
        _retry: usize,
        _instance: &NodeState,
        _height: u64,
        _view: ViewNumber,
        _mt: &mut BlockMerkleTree,
    ) -> anyhow::Result<()> {
        unreachable!()
    }

    async fn try_fetch_chain_config(
        &self,
        _retry: usize,
        _commitment: Commitment<ChainConfig>,
    ) -> anyhow::Result<ChainConfig> {
        unreachable!()
    }

    #[tracing::instrument(skip(self, _instance))]
    async fn try_fetch_reward_accounts(
        &self,
        _retry: usize,
        _instance: &NodeState,
        _height: u64,
        _view: ViewNumber,
        _reward_merkle_tree_root: RewardMerkleCommitment,
        _accounts: &[RewardAccount],
    ) -> anyhow::Result<RewardMerkleTree> {
        unreachable!()
    }

    async fn fetch_accounts(
        &self,
        _instance: &NodeState,
        height: u64,
        view: ViewNumber,
        fee_merkle_tree_root: FeeMerkleCommitment,
        accounts: Vec<FeeAccount>,
    ) -> anyhow::Result<Vec<FeeAccountProof>> {
        warn!("Fetching accounts");
        // Clone things we need in the closure
        let accounts_clone = accounts.clone();
        let proofs_pointer = Arc::new(Mutex::new(Vec::new()));
        let proofs_pointer_clone = proofs_pointer.clone();

        // The response validation function should verify the merkle proofs
        let response_validation_fn = move |_request: &Request, response: &Response| {
            // Clone things we need in the closure
            let accounts_clone = accounts_clone.clone();
            let fee_merkle_tree_root = fee_merkle_tree_root.clone();
            let response_clone = response.clone();
            let proofs_pointer = proofs_pointer_clone.clone();
            let mut proofs = Vec::new();
            async move {
                // Make sure the response is the correct type
                let Response::Accounts(fee_merkle_tree) = response_clone else {
                    return Err(anyhow::anyhow!("expected accounts response"));
                };

                // Verify the merkle proofs
                for account in accounts_clone {
                    let (proof, _) = FeeAccountProof::prove(&fee_merkle_tree, account.into())
                        .with_context(|| format!("response was missing account {account}"))?;
                    proof
                        .verify(&fee_merkle_tree_root)
                        .with_context(|| format!("invalid proof for account {account}"))?;
                    proofs.push(proof);
                }

                proofs_pointer.lock().extend(proofs);

                Ok(())
            }
        };

        // Wait for the protocol to send us the accounts
        self.request_indefinitely(
            &self.public_key,
            &self.private_key,
            self.config.incoming_request_ttl,
            Request::Accounts(height, *view, accounts.to_vec()),
            response_validation_fn,
        )
        .await
        .with_context(|| "failed to request accounts")?;

        // Return the proofs
        let mut proofs = proofs_pointer.lock();
        let proofs = std::mem::take(&mut *proofs);
        Ok(proofs)
    }

    async fn fetch_leaf(
        &self,
        height: u64,
        stake_table: Vec<PeerConfig<PubKey>>,
        success_threshold: NonZeroU64,
        epoch_height: u64,
    ) -> anyhow::Result<Leaf2> {
        warn!("Fetching leaf");
        // Clone things we need in the closure
        let leaf_pointer = Arc::new(Mutex::new(None));
        let leaf_pointer_clone = leaf_pointer.clone();

        // When we receive a leaf chain, we should verify the epoch root
        let response_validation_fn = move |_request: &Request, response: &Response| {
            // Clone things we need in the closure
            let success_threshold = success_threshold.clone();
            let epoch_height = epoch_height;
            let stake_table = stake_table.clone();
            let response_clone = response.clone();
            let leaf_pointer_clone = leaf_pointer_clone.clone();

            async move {
                // Make sure the response is the correct type
                let Response::LeafChain(leaf_chain) = response_clone else {
                    return Err(anyhow::anyhow!("expected leaf chain response"));
                };

                // Sort the received chain
                let mut leaf_chain = leaf_chain.clone();
                leaf_chain.sort_by_key(|l| l.view_number());
                leaf_chain.reverse();

                let leaf = verify_epoch_root_chain(
                    leaf_chain.clone(),
                    stake_table,
                    success_threshold,
                    epoch_height,
                    &UpgradeLock::<SeqTypes, SequencerVersions<EpochVersion, EpochVersion>>::new(),
                )
                .await
                .with_context(|| "failed to verify epoch root chain")?;

                // Replace the leaf so we can get it later
                leaf_pointer_clone.lock().replace(Some(leaf));

                Ok(())
            }
        };

        self.request_indefinitely(
            &self.public_key,
            &self.private_key,
            self.config.incoming_request_ttl,
            Request::LeafChain(height),
            response_validation_fn,
        )
        .await
        .with_context(|| "failed to get leaf chain")?;

        // Return the leaf
        let mut leaf = leaf_pointer.lock();
        let leaf = leaf
            .take()
            .expect("leaf not found")
            .expect("leaf not found");
        Ok(leaf)
    }

    async fn fetch_chain_config(
        &self,
        commitment: Commitment<ChainConfig>,
    ) -> anyhow::Result<ChainConfig> {
        warn!("Fetching chain config");
        // The response validation function just checks that the commitments match
        let response_validation_fn = move |_request: &Request, response: &Response| {
            let response_clone = response.clone();
            async move {
                // Make sure the response is the correct type
                let Response::ChainConfig(chain_config) = response_clone else {
                    return Err(anyhow::anyhow!("expected chain config response"));
                };

                if chain_config.commit() != commitment {
                    return Err(anyhow::anyhow!(
                        "received chain config with mismatched commitment"
                    ));
                }

                Ok(())
            }
        };

        let response = self
            .request_indefinitely(
                &self.public_key,
                &self.private_key,
                self.config.incoming_request_ttl,
                Request::ChainConfig(commitment),
                response_validation_fn,
            )
            .await
            .with_context(|| "failed to fetch chain config")?;

        // This is irrefutable, enforced by the protocol
        let Response::ChainConfig(chain_config) = response else {
            return Err(anyhow::anyhow!("expected chain config response"));
        };

        Ok(chain_config)
    }

    async fn remember_blocks_merkle_tree(
        &self,
        _instance: &NodeState,
        height: u64,
        view: ViewNumber,
        mt: &mut BlockMerkleTree,
    ) -> anyhow::Result<()> {
        warn!("Remembering blocks frontier");
        let mt_pointer = Arc::new(Mutex::new(mt.clone()));
        let mt_pointer_clone = mt_pointer.clone();

        // The response validation function should verify the proof
        let response_validation_fn = move |_request: &Request, response: &Response| {
            // Clone things we need in the closure
            let response_clone = response.clone();
            let mt_pointer_clone = mt_pointer_clone.clone();
            async move {
                // Make sure the response is the correct type
                let Response::BlocksFrontier(frontier) = response_clone else {
                    return Err(anyhow::anyhow!("expected blocks frontier response"));
                };

                // Make sure it is not missing the leaf element
                let elem = frontier
                    .elem()
                    .with_context(|| "received frontier is missing leaf element")?;

                // Prove the block proof and remember it
                let mut mt = mt_pointer_clone.lock();
                let num_leaves = mt.num_leaves();
                mt.remember(num_leaves - 1, *elem, &frontier)
                    .with_context(|| "received block proof is invalid")?;

                Ok(())
            }
        };

        self.request_indefinitely(
            &self.public_key,
            &self.private_key,
            self.config.incoming_request_ttl,
            Request::BlocksFrontier(height, *view),
            response_validation_fn,
        )
        .await
        .with_context(|| "failed to remember blocks frontier")?;

        // Update the merkle tree to the new, validated one
        *mt = mt_pointer.lock().clone();

        Ok(())
    }

    async fn fetch_reward_accounts(
        &self,
        _instance: &NodeState,
        height: u64,
        view: ViewNumber,
        reward_merkle_tree_root: RewardMerkleCommitment,
        accounts: Vec<RewardAccount>,
    ) -> anyhow::Result<Vec<RewardAccountProof>> {
        warn!("Fetching reward accounts");
        // Clone things we need in the closure
        let accounts_clone = accounts.clone();
        let proofs_pointer = Arc::new(Mutex::new(Vec::new()));
        let proofs_pointer_clone = proofs_pointer.clone();

        // The response validation function should verify the merkle proofs
        let response_validation_fn = move |_request: &Request, response: &Response| {
            // Clone things we need in the closure
            let accounts_clone = accounts_clone.clone();
            let reward_merkle_tree_root = reward_merkle_tree_root.clone();
            let response_clone = response.clone();
            let proofs_pointer = proofs_pointer_clone.clone();
            let mut proofs = Vec::new();
            async move {
                // Make sure the response is the correct type
                let Response::RewardAccounts(reward_merkle_tree) = response_clone else {
                    return Err(anyhow::anyhow!("expected reward accounts response"));
                };

                // Verify the merkle proofs
                for account in accounts_clone {
                    let (proof, _) = RewardAccountProof::prove(&reward_merkle_tree, account.into())
                        .with_context(|| format!("response was missing account {account}"))?;
                    proof
                        .verify(&reward_merkle_tree_root)
                        .with_context(|| format!("invalid proof for account {account}"))?;
                    proofs.push(proof);
                }

                proofs_pointer.lock().extend(proofs);

                Ok(())
            }
        };

        // Wait for the protocol to send us the accounts
        self.request_indefinitely(
            &self.public_key,
            &self.private_key,
            self.config.incoming_request_ttl,
            Request::RewardAccounts(height, *view, accounts),
            response_validation_fn,
        )
        .await
        .with_context(|| "failed to request reward accounts")?;

        // Return the proofs
        let mut proofs = proofs_pointer.lock();
        let proofs = std::mem::take(&mut *proofs);
        Ok(proofs)
    }

    fn backoff(&self) -> &BackoffParams {
        unreachable!()
    }

    fn name(&self) -> String {
        "request-response".to_string()
    }
}

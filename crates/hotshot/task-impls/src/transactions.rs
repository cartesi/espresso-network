// Copyright (c) 2021-2024 Espresso Systems (espressosys.com)
// This file is part of the HotShot repository.

// You should have received a copy of the MIT License
// along with the HotShot repository. If not, see <https://mit-license.org/>.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_broadcast::{Receiver, Sender};
use async_trait::async_trait;
use futures::{stream::FuturesUnordered, StreamExt};
use hotshot_builder_api::v0_1::block_info::AvailableBlockInfo;
use hotshot_task::task::TaskState;
use hotshot_types::{
    consensus::OuterConsensus,
    data::{null_block, PackedBundle, VidCommitment},
    epoch_membership::EpochMembershipCoordinator,
    event::{Event, EventType},
    message::UpgradeLock,
    traits::{
        block_contents::{BuilderFee, EncodeBytes},
        node_implementation::{ConsensusTime, NodeType, Versions},
        signature_key::{BuilderSignatureKey, SignatureKey},
        BlockPayload,
    },
    utils::{is_epoch_transition, is_last_block, ViewInner},
};
use hotshot_utils::anytrace::*;
use tokio::time::{sleep, timeout};
use tracing::instrument;
use vbs::version::{StaticVersionType, Version};

use crate::{
    builder::v0_1::BuilderClient as BuilderClientBase,
    events::{HotShotEvent, HotShotTaskCompleted},
    helpers::broadcast_event,
};

// Parameters for builder querying algorithm

/// Proportion of builders queried in first batch, dividend
const BUILDER_MAIN_BATCH_THRESHOLD_DIVIDEND: usize = 2;
/// Proportion of builders queried in the first batch, divisor
const BUILDER_MAIN_BATCH_THRESHOLD_DIVISOR: usize = 3;
/// Time the first batch of builders has to respond
const BUILDER_MAIN_BATCH_CUTOFF: Duration = Duration::from_millis(700);
/// Multiplier for extra time to give to the second batch of builders
const BUILDER_ADDITIONAL_TIME_MULTIPLIER: f32 = 0.2;
/// Minimum amount of time allotted to both batches, cannot be cut shorter if the first batch
/// responds extremely fast.
const BUILDER_MINIMUM_QUERY_TIME: Duration = Duration::from_millis(300);
/// Delay between re-tries on unsuccessful calls
const RETRY_DELAY: Duration = Duration::from_millis(100);

/// Builder Provided Responses
pub struct BuilderResponse<TYPES: NodeType> {
    /// Fee information
    pub fee: BuilderFee<TYPES>,

    /// Block payload
    pub block_payload: TYPES::BlockPayload,

    /// Block metadata
    pub metadata: <TYPES::BlockPayload as BlockPayload<TYPES>>::Metadata,
}

/// Tracks state of a Transaction task
pub struct TransactionTaskState<TYPES: NodeType, V: Versions> {
    /// The state's api
    pub builder_timeout: Duration,

    /// Output events to application
    pub output_event_stream: async_broadcast::Sender<Event<TYPES>>,

    /// View number this view is executing in.
    pub cur_view: TYPES::View,

    /// Epoch number this node is executing in.
    pub cur_epoch: Option<TYPES::Epoch>,

    /// Reference to consensus. Leader will require a read lock on this.
    pub consensus: OuterConsensus<TYPES>,

    /// Membership for the quorum
    pub membership_coordinator: EpochMembershipCoordinator<TYPES>,

    /// Builder 0.1 API clients
    pub builder_clients: Vec<BuilderClientBase<TYPES>>,

    /// This Nodes Public Key
    pub public_key: TYPES::SignatureKey,

    /// Our Private Key
    pub private_key: <TYPES::SignatureKey as SignatureKey>::PrivateKey,

    /// InstanceState
    pub instance_state: Arc<TYPES::InstanceState>,

    /// This state's ID
    pub id: u64,

    /// Lock for a decided upgrade
    pub upgrade_lock: UpgradeLock<TYPES, V>,

    /// Number of blocks in an epoch, zero means there are no epochs
    pub epoch_height: u64,
}

impl<TYPES: NodeType, V: Versions> TransactionTaskState<TYPES, V> {
    /// handle view change decide legacy or not
    pub async fn handle_view_change(
        &mut self,
        event_stream: &Sender<Arc<HotShotEvent<TYPES>>>,
        block_view: TYPES::View,
        block_epoch: Option<TYPES::Epoch>,
    ) -> Option<HotShotTaskCompleted> {
        let _version = match self.upgrade_lock.version(block_view).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to calculate version: {:?}", e);
                return None;
            },
        };

        self.handle_view_change_legacy(event_stream, block_view, block_epoch)
            .await
    }

    /// legacy view change handler
    #[instrument(skip_all, fields(id = self.id, view = *self.cur_view), name = "Transaction task", level = "error", target = "TransactionTaskState")]
    pub async fn handle_view_change_legacy(
        &mut self,
        event_stream: &Sender<Arc<HotShotEvent<TYPES>>>,
        block_view: TYPES::View,
        block_epoch: Option<TYPES::Epoch>,
    ) -> Option<HotShotTaskCompleted> {
        let version = match self.upgrade_lock.version(block_view).await {
            Ok(v) => v,
            Err(err) => {
                tracing::error!("Upgrade certificate requires unsupported version, refusing to request blocks: {err}");
                return None;
            },
        };

        // Short circuit if we are in epochs and we are likely proposing a transition block
        // If it's the first view of the upgrade, we don't need to check for transition blocks
        if version >= V::Epochs::VERSION {
            let Some(epoch) = block_epoch else {
                tracing::error!("Epoch is required for epoch-based view change");
                return None;
            };
            let high_qc = self.consensus.read().await.high_qc().clone();
            let mut high_qc_block_number = if let Some(bn) = high_qc.data.block_number {
                bn
            } else {
                // If it's the first view after the upgrade the high QC won't have a block number
                // So just use the highest_block number we've stored
                if block_view
                    > self
                        .upgrade_lock
                        .upgrade_view()
                        .await
                        .unwrap_or(TYPES::View::new(0))
                        + 1
                {
                    tracing::warn!("High QC in epoch version and not the first QC after upgrade");
                    self.send_empty_block(event_stream, block_view, block_epoch, version)
                        .await;
                    return None;
                }
                // 0 here so we use the highest block number in the calculation below
                0
            };
            high_qc_block_number = std::cmp::max(
                high_qc_block_number,
                self.consensus.read().await.highest_block,
            );
            if self
                .consensus
                .read()
                .await
                .transition_qc()
                .is_some_and(|qc| {
                    let Some(e) = qc.0.data.epoch else {
                        return false;
                    };
                    e == epoch
                })
                || is_epoch_transition(high_qc_block_number, self.epoch_height)
            {
                // We are proposing a transition block it should be empty
                if !is_last_block(high_qc_block_number, self.epoch_height) {
                    tracing::info!("Sending empty block event. View number: {block_view}. Parent Block number: {high_qc_block_number}");
                    self.send_empty_block(event_stream, block_view, block_epoch, version)
                        .await;
                    return None;
                }
            }
        }

        // Request a block from the builder unless we are between versions.
        let block = {
            if self
                .upgrade_lock
                .decided_upgrade_certificate
                .read()
                .await
                .as_ref()
                .is_some_and(|cert| cert.upgrading_in(block_view))
            {
                None
            } else {
                self.wait_for_block(block_view).await
            }
        };

        if let Some(BuilderResponse {
            block_payload,
            metadata,
            fee,
        }) = block
        {
            broadcast_event(
                Arc::new(HotShotEvent::BlockRecv(PackedBundle::new(
                    block_payload.encode(),
                    metadata,
                    block_view,
                    block_epoch,
                    vec1::vec1![fee],
                ))),
                event_stream,
            )
            .await;
        } else {
            self.send_empty_block(event_stream, block_view, block_epoch, version)
                .await;
        };

        return None;
    }

    /// Send the event to the event stream that we are proposing an empty block
    async fn send_empty_block(
        &self,
        event_stream: &Sender<Arc<HotShotEvent<TYPES>>>,
        block_view: TYPES::View,
        block_epoch: Option<TYPES::Epoch>,
        version: Version,
    ) {
        // If we couldn't get a block, send an empty block
        tracing::info!("Failed to get a block for view {block_view:?}, proposing empty block");

        // Increment the metric for number of empty blocks proposed
        self.consensus
            .write()
            .await
            .metrics
            .number_of_empty_blocks_proposed
            .add(1);

        let num_storage_nodes = match self
            .membership_coordinator
            .stake_table_for_epoch(block_epoch)
            .await
        {
            Ok(epoch_stake_table) => epoch_stake_table.total_nodes().await,
            Err(e) => {
                tracing::warn!(
                    "Failed to get num_storage_nodes for epoch {:?}: {}",
                    block_epoch,
                    e
                );
                return;
            },
        };

        let Some(null_fee) = null_block::builder_fee::<TYPES, V>(num_storage_nodes, version) else {
            tracing::error!("Failed to get null fee");
            return;
        };

        // Create an empty block payload and metadata
        let (_, metadata) = <TYPES as NodeType>::BlockPayload::empty();

        // Broadcast the empty block
        broadcast_event(
            Arc::new(HotShotEvent::BlockRecv(PackedBundle::new(
                vec![].into(),
                metadata,
                block_view,
                block_epoch,
                vec1::vec1![null_fee],
            ))),
            event_stream,
        )
        .await;
    }

    /// Produce a null block
    pub async fn null_block(
        &self,
        block_view: TYPES::View,
        block_epoch: Option<TYPES::Epoch>,
        version: Version,
        num_storage_nodes: usize,
    ) -> Option<PackedBundle<TYPES>> {
        let Some(null_fee) = null_block::builder_fee::<TYPES, V>(num_storage_nodes, version) else {
            tracing::error!("Failed to calculate null block fee.");
            return None;
        };

        // Create an empty block payload and metadata
        let (_, metadata) = <TYPES as NodeType>::BlockPayload::empty();

        Some(PackedBundle::new(
            vec![].into(),
            metadata,
            block_view,
            block_epoch,
            vec1::vec1![null_fee],
        ))
    }

    /// main task event handler
    #[instrument(skip_all, fields(id = self.id, view = *self.cur_view, epoch = self.cur_epoch.map(|x| *x)), name = "Transaction task", level = "error", target = "TransactionTaskState")]
    pub async fn handle(
        &mut self,
        event: Arc<HotShotEvent<TYPES>>,
        event_stream: Sender<Arc<HotShotEvent<TYPES>>>,
    ) -> Result<()> {
        match event.as_ref() {
            HotShotEvent::TransactionsRecv(transactions) => {
                broadcast_event(
                    Event {
                        view_number: self.cur_view,
                        event: EventType::Transactions {
                            transactions: transactions.clone(),
                        },
                    },
                    &self.output_event_stream,
                )
                .await;
            },
            HotShotEvent::ViewChange(view, epoch) => {
                let view = TYPES::View::new(std::cmp::max(1, **view));
                ensure!(
                    *view > *self.cur_view && *epoch >= self.cur_epoch,
                    debug!(
                      "Received a view change to an older view and epoch: tried to change view to {:?}\
                      and epoch {:?} though we are at view {:?} and epoch {:?}",
                        view, epoch, self.cur_view, self.cur_epoch
                    )
                );
                self.cur_view = view;
                self.cur_epoch = *epoch;

                let leader = self
                    .membership_coordinator
                    .membership_for_epoch(*epoch)
                    .await?
                    .leader(view)
                    .await?;
                if leader == self.public_key {
                    self.handle_view_change(&event_stream, view, *epoch).await;
                    return Ok(());
                }
            },
            _ => {},
        }
        Ok(())
    }

    /// Get VID commitment for the last successful view before `block_view`.
    /// Returns None if we don't have said commitment recorded.
    #[instrument(skip_all, target = "TransactionTaskState", fields(id = self.id, cur_view = *self.cur_view, block_view = *block_view))]
    async fn last_vid_commitment_retry(
        &self,
        block_view: TYPES::View,
        task_start_time: Instant,
    ) -> Result<(TYPES::View, VidCommitment)> {
        loop {
            match self.last_vid_commitment(block_view).await {
                Ok((view, comm)) => break Ok((view, comm)),
                Err(e) if task_start_time.elapsed() >= self.builder_timeout => break Err(e),
                _ => {
                    // We still have time, will re-try in a bit
                    sleep(RETRY_DELAY).await;
                    continue;
                },
            }
        }
    }

    /// Get VID commitment for the last successful view before `block_view`.
    /// Returns None if we don't have said commitment recorded.
    #[instrument(skip_all, target = "TransactionTaskState", fields(id = self.id, cur_view = *self.cur_view, block_view = *block_view))]
    async fn last_vid_commitment(
        &self,
        block_view: TYPES::View,
    ) -> Result<(TYPES::View, VidCommitment)> {
        let consensus_reader = self.consensus.read().await;
        let mut target_view = TYPES::View::new(block_view.saturating_sub(1));

        loop {
            let view_data = consensus_reader
                .validated_state_map()
                .get(&target_view)
                .context(info!(
                    "Missing record for view {target_view} in validated state",
                ))?;

            match &view_data.view_inner {
                ViewInner::Da {
                    payload_commitment, ..
                } => return Ok((target_view, *payload_commitment)),
                ViewInner::Leaf {
                    leaf: leaf_commitment,
                    ..
                } => {
                    let leaf = consensus_reader
                        .saved_leaves()
                        .get(leaf_commitment)
                        .context(info!(
                            "Missing leaf with commitment {leaf_commitment} for view {target_view} in saved_leaves",
                        ))?;
                    return Ok((target_view, leaf.payload_commitment()));
                },
                ViewInner::Failed => {
                    // For failed views, backtrack
                    target_view =
                        TYPES::View::new(target_view.checked_sub(1).context(warn!("Reached genesis. Something is wrong -- have we not decided any blocks since genesis?"))?);
                    continue;
                },
            }
        }
    }

    #[instrument(skip_all, fields(id = self.id, cur_view = *self.cur_view, block_view = *block_view), name = "wait_for_block", level = "error")]
    async fn wait_for_block(&self, block_view: TYPES::View) -> Option<BuilderResponse<TYPES>> {
        let task_start_time = Instant::now();

        // Find commitment to the block we want to build upon
        let (parent_view, parent_comm) = match self
            .last_vid_commitment_retry(block_view, task_start_time)
            .await
        {
            Ok((v, c)) => (v, c),
            Err(e) => {
                tracing::warn!("Failed to find last vid commitment in time: {e}");
                return None;
            },
        };

        let parent_comm_sig = match <<TYPES as NodeType>::SignatureKey as SignatureKey>::sign(
            &self.private_key,
            parent_comm.as_ref(),
        ) {
            Ok(sig) => sig,
            Err(err) => {
                tracing::error!(%err, "Failed to sign block hash");
                return None;
            },
        };

        while task_start_time.elapsed() < self.builder_timeout {
            match timeout(
                self.builder_timeout
                    .saturating_sub(task_start_time.elapsed()),
                self.block_from_builder(parent_comm, parent_view, &parent_comm_sig),
            )
            .await
            {
                // We got a block
                Ok(Ok(block)) => {
                    return Some(block);
                },

                // We failed to get a block
                Ok(Err(err)) => {
                    tracing::info!("Couldn't get a block: {err:#}");
                    // pause a bit
                    sleep(RETRY_DELAY).await;
                    continue;
                },

                // We timed out while getting available blocks
                Err(err) => {
                    tracing::info!(%err, "Timeout while getting available blocks");
                    return None;
                },
            }
        }

        tracing::warn!("could not get a block from the builder in time");
        None
    }

    /// Query the builders for available blocks. Queries only fraction of the builders
    /// based on the response time.
    async fn get_available_blocks(
        &self,
        parent_comm: VidCommitment,
        view_number: TYPES::View,
        parent_comm_sig: &<<TYPES as NodeType>::SignatureKey as SignatureKey>::PureAssembledSignatureType,
    ) -> Vec<(AvailableBlockInfo<TYPES>, usize)> {
        let tasks = self
            .builder_clients
            .iter()
            .enumerate()
            .map(|(builder_idx, client)| async move {
                client
                    .available_blocks(
                        parent_comm,
                        view_number.u64(),
                        self.public_key.clone(),
                        parent_comm_sig,
                    )
                    .await
                    .map(move |blocks| {
                        blocks
                            .into_iter()
                            .map(move |block_info| (block_info, builder_idx))
                    })
            })
            .collect::<FuturesUnordered<_>>();
        let mut results = Vec::with_capacity(self.builder_clients.len());
        let query_start = Instant::now();
        let threshold = (self.builder_clients.len() * BUILDER_MAIN_BATCH_THRESHOLD_DIVIDEND)
            .div_ceil(BUILDER_MAIN_BATCH_THRESHOLD_DIVISOR);
        let mut tasks = tasks.take(threshold);
        while let Some(result) = tasks.next().await {
            results.push(result);
            if query_start.elapsed() > BUILDER_MAIN_BATCH_CUTOFF {
                break;
            }
        }
        let timeout = sleep(std::cmp::max(
            query_start
                .elapsed()
                .mul_f32(BUILDER_ADDITIONAL_TIME_MULTIPLIER),
            BUILDER_MINIMUM_QUERY_TIME.saturating_sub(query_start.elapsed()),
        ));
        futures::pin_mut!(timeout);
        let mut tasks = tasks.into_inner().take_until(timeout);
        while let Some(result) = tasks.next().await {
            results.push(result);
        }
        results
            .into_iter()
            .filter_map(|result| result.ok())
            .flatten()
            .collect::<Vec<_>>()
    }

    /// Get a block from builder.
    /// Queries the sufficiently fast builders for available blocks and chooses the one with the
    /// best fee/byte ratio, re-trying with the next best one in case of failure.
    ///
    /// # Errors
    /// If none of the builder reports any available blocks or claiming block fails for all of the
    /// builders.
    #[instrument(skip_all, fields(id = self.id, view = *self.cur_view), name = "block_from_builder", level = "error")]
    async fn block_from_builder(
        &self,
        parent_comm: VidCommitment,
        view_number: TYPES::View,
        parent_comm_sig: &<<TYPES as NodeType>::SignatureKey as SignatureKey>::PureAssembledSignatureType,
    ) -> Result<BuilderResponse<TYPES>> {
        let mut available_blocks = self
            .get_available_blocks(parent_comm, view_number, parent_comm_sig)
            .await;

        available_blocks.sort_by(|(l, _), (r, _)| {
            // We want the block with the highest fee per byte of data we're going to have to
            // process, thus our comparison function is:
            //      (l.offered_fee / l.block_size) < (r.offered_fee / r.block_size)
            // To avoid floating point math (which doesn't even have an `Ord` impl) we multiply
            // through by the denominators to get
            //      l.offered_fee * r.block_size < r.offered_fee * l.block_size
            // We cast up to u128 to avoid overflow.
            (u128::from(l.offered_fee) * u128::from(r.block_size))
                .cmp(&(u128::from(r.offered_fee) * u128::from(l.block_size)))
        });

        if available_blocks.is_empty() {
            tracing::info!("No available blocks");
            bail!("No available blocks");
        }

        for (block_info, builder_idx) in available_blocks {
            // Verify signature over chosen block.
            if !block_info.sender.validate_block_info_signature(
                &block_info.signature,
                block_info.block_size,
                block_info.offered_fee,
                &block_info.block_hash,
            ) {
                tracing::warn!("Failed to verify available block info response message signature");
                continue;
            }

            let request_signature = match <<TYPES as NodeType>::SignatureKey as SignatureKey>::sign(
                &self.private_key,
                block_info.block_hash.as_ref(),
            ) {
                Ok(request_signature) => request_signature,
                Err(err) => {
                    tracing::error!(%err, "Failed to sign block hash");
                    continue;
                },
            };

            let response = {
                let client = &self.builder_clients[builder_idx];

                let (block, either_header_input) = futures::join! {
                    client.claim_block(block_info.block_hash.clone(), view_number.u64(), self.public_key.clone(), &request_signature),
                    client.claim_either_block_header_input(block_info.block_hash.clone(), view_number.u64(), self.public_key.clone(), &request_signature)
                };

                let block_data = match block {
                    Ok(block_data) => block_data,
                    Err(err) => {
                        tracing::warn!(%err, "Error claiming block data");
                        continue;
                    },
                };

                let Ok(either_header_input) = either_header_input
                    .inspect_err(|err| tracing::warn!(%err, "Error claiming header input"))
                else {
                    continue;
                };

                let Some(header_input) = either_header_input
                    .validate_signature_and_get_input(block_info.offered_fee, &block_data.metadata)
                else {
                    tracing::warn!(
                        "Failed to verify available new or legacy block header input data response message signature"
                      );
                    continue;
                };

                // verify the signature over the message
                if !block_data.validate_signature() {
                    tracing::warn!(
                        "Failed to verify available block data response message signature"
                    );
                    continue;
                }

                // verify the message signature and the fee_signature
                if !header_input.validate_signature(block_info.offered_fee, &block_data.metadata) {
                    tracing::warn!(
                        "Failed to verify available block header input data response message signature"
                    );
                    continue;
                }

                let fee = BuilderFee {
                    fee_amount: block_info.offered_fee,
                    fee_account: header_input.sender,
                    fee_signature: header_input.fee_signature,
                };

                BuilderResponse {
                    fee,
                    block_payload: block_data.block_payload,
                    metadata: block_data.metadata,
                }
            };

            return Ok(response);
        }

        bail!("Couldn't claim a block from any of the builders");
    }
}

#[async_trait]
/// task state implementation for Transactions Task
impl<TYPES: NodeType, V: Versions> TaskState for TransactionTaskState<TYPES, V> {
    type Event = HotShotEvent<TYPES>;

    async fn handle_event(
        &mut self,
        event: Arc<Self::Event>,
        sender: &Sender<Arc<Self::Event>>,
        _receiver: &Receiver<Arc<Self::Event>>,
    ) -> Result<()> {
        self.handle(event, sender.clone()).await
    }

    fn cancel_subtasks(&mut self) {}
}

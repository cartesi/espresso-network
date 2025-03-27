use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use espresso_types::{PubKey, SeqTypes};
use hotshot::{traits::NodeImplementation, SystemContext};
use hotshot_types::{
    data::EpochNumber,
    epoch_membership::EpochMembershipCoordinator,
    traits::node_implementation::{ConsensusTime, Versions},
};
use request_response::recipient_source::RecipientSource as RecipientSourceTrait;

use super::request::Request;

#[derive(Clone)]
pub struct RecipientSource<I: NodeImplementation<SeqTypes>, V: Versions> {
    pub memberships: EpochMembershipCoordinator<SeqTypes>,
    pub consensus: Arc<SystemContext<SeqTypes, I, V>>,
}

/// Implement the RecipientSourceTrait, which allows the request-response protocol to derive the
/// intended recipients for a given request
#[async_trait]
impl<I: NodeImplementation<SeqTypes>, V: Versions> RecipientSourceTrait<Request, PubKey>
    for RecipientSource<I, V>
{
    async fn get_expected_responders(&self, _request: &Request) -> Result<Vec<PubKey>> {
        // Get the current epoch number
        let epoch_number = self
            .consensus
            .consensus()
            .read()
            .await
            .cur_epoch()
            .unwrap_or(EpochNumber::genesis());

        // Get the stake table for the epoch
        let stake_table = self.memberships.wait_for_catchup(epoch_number).await;

        let stake_table = match stake_table {
            Ok(stake_table) => stake_table
                .stake_table()
                .await
                .iter()
                .map(|entry| entry.stake_table_entry.stake_key)
                .collect(),
            Err(e) => {
                self.memberships
                    .membership_for_epoch(Some(EpochNumber::genesis()))
                    .await
                    .with_context(|| "failed to get stake table for epoch")?
                    .stake_table()
                    .await
                    .iter()
                    .map(|entry| entry.stake_table_entry.stake_key)
                    .collect()
            },
        };

        // Get everyone in the stake table
        Ok(stake_table)
    }
}

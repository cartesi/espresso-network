// Copyright (c) 2021-2024 Espresso Systems (espressosys.com)
// This file is part of the HotShot repository.

// You should have received a copy of the MIT License
// along with the HotShot repository. If not, see <https://mit-license.org/>.

//! The whitelist is an adaptor that is able to update the allowed public keys for
//! all brokers. Right now, we do this by asking the orchestrator for the list of
//! allowed public keys. In the future, we will pull the stake table from the L1.

use std::sync::Arc;

use anyhow::{Context, Result};
use cdn_broker::reexports::discovery::{DiscoveryClient, Embedded, Redis};
use clap::Parser;
use espresso_types::SeqTypes;
use hotshot_types::{traits::signature_key::SignatureKey, PeerConfig};
use sequencer::api::data_source::StakeTableWithEpochNumber;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The query node endpoint that we will fetch the stake table from.
    #[arg(short, long)]
    query_node_url: String,

    /// The CDN database endpoint (including scheme) to connect to.
    /// With the local discovery feature, this is a file path.
    /// With the remote (redis) discovery feature, this is a redis URL (e.g. `redis://127.0.0.1:6789`).
    #[arg(short, long)]
    database_endpoint: String,

    /// Whether or not to use the local database client
    #[arg(short, long)]
    local_database: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse the command line arguments
    let args = Args::parse();

    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Get the current stake table from the supplied node
    let stake_table_and_epoch_number = get_current_stake_table(&args.query_node_url)
        .await
        .with_context(|| "failed to get stake table from query node")?;

    // Extract the epoch number and stake table
    let (epoch_number, mut stake_table) = (
        stake_table_and_epoch_number.epoch,
        stake_table_and_epoch_number.stake_table,
    );

    // If the stake table has an epoch number, get the stake table for the next epoch and merge them
    if let Some(epoch_number) = epoch_number {
        // Get the stake table for the next epoch
        let next_epoch_stake_table = get_stake_table(&args.query_node_url, *epoch_number + 1)
            .await
            .with_context(|| "failed to get stake table from query node")?;

        // Merge the tables and deduplicate the keys
        stake_table.extend(next_epoch_stake_table);
        stake_table.sort_by_key(|peer| peer.stake_table_entry.stake_key);
        stake_table.dedup_by_key(|peer| peer.stake_table_entry.stake_key);
    }

    // Extrapolate the state_ver_keys from the config and convert them to a compatible format
    let whitelist = stake_table
        .iter()
        .map(|k| Arc::from(k.stake_table_entry.stake_key.to_bytes()))
        .collect();

    // Update the whitelist in the DB depending on whether we are using a local or remote DB
    if args.local_database {
        <Embedded as DiscoveryClient>::new(args.database_endpoint, None)
            .await?
            .set_whitelist(whitelist)
            .await?;
    } else {
        <Redis as DiscoveryClient>::new(args.database_endpoint, None)
            .await?
            .set_whitelist(whitelist)
            .await?;
    }

    Ok(())
}

/// Get the current stake table
async fn get_current_stake_table(
    query_node_url: &str,
) -> Result<StakeTableWithEpochNumber<SeqTypes>> {
    // Get the current stake table
    let response = reqwest::get(format!("{}/v0/node/stake-table/current", query_node_url))
        .await
        .with_context(|| "failed to fetch stake table")?;

    // Parse the response
    response
        .json()
        .await
        .with_context(|| "failed to parse stake table")
}

/// Get the stake table for a specific epoch
async fn get_stake_table(
    query_node_url: &str,
    epoch_number: u64,
) -> Result<Vec<PeerConfig<SeqTypes>>> {
    // Get the stake table for the given epoch number
    let response = reqwest::get(format!(
        "{}/v0/node/stake-table/{}",
        query_node_url, epoch_number
    ))
    .await
    .with_context(|| "failed to fetch stake table")?;

    // Parse the response
    response
        .json()
        .await
        .with_context(|| "failed to parse stake table")
}

//! This module contains the namespace proof implementation for the new AvidM scheme.

use hotshot_types::{data::VidCommitment, vid::avidm::AvidMCommon};
use vid::avid_m::namespaced::NsAvidMScheme;

use crate::{v0_3::AvidMNsProof, NamespaceId, NsIndex, NsPayload, NsTable, Payload, Transaction};

impl AvidMNsProof {
    pub fn new(payload: &Payload, index: &NsIndex, common: &AvidMCommon) -> Option<AvidMNsProof> {
        let payload_byte_len = payload.byte_len();
        let index = index.0;
        let ns_table = payload.ns_table();
        let ns_table = ns_table
            .iter()
            .map(|index| ns_table.ns_range(&index, &payload_byte_len).0)
            .collect::<Vec<_>>();

        if index >= ns_table.len() {
            tracing::warn!("ns_index {:?} out of bounds", index);
            return None; // error: index out of bounds
        }

        if ns_table[index].is_empty() {
            None
        } else {
            match NsAvidMScheme::namespace_proof(common, &payload.raw_payload, index, ns_table) {
                Ok(proof) => Some(AvidMNsProof(proof)),
                Err(e) => {
                    tracing::error!("error generating namespace proof: {:?}", e);
                    None
                },
            }
        }
    }

    /// Unlike the ADVZ scheme, this function won't fail with a wrong `ns_table`.
    /// It only uses `ns_table` to get the namespace id.
    pub fn verify(
        &self,
        ns_table: &NsTable,
        commit: &VidCommitment,
        common: &AvidMCommon,
    ) -> Option<(Vec<Transaction>, NamespaceId)> {
        match commit {
            VidCommitment::V1(commit) => {
                match NsAvidMScheme::verify_namespace_proof(common, commit, &self.0) {
                    Ok(Ok(_)) => {
                        let ns_id = ns_table.read_ns_id(&NsIndex(self.0.ns_index))?;
                        let ns_payload = NsPayload::from_bytes_slice(&self.0.ns_payload);
                        Some((ns_payload.export_all_txs(&ns_id), ns_id))
                    },
                    Ok(Err(_)) => None,
                    Err(e) => {
                        tracing::warn!("error verifying namespace proof: {:?}", e);
                        None
                    },
                }
            },
            _ => None,
        }
    }
}

/// Copied from ADVZNsProof tests.
#[cfg(test)]
mod tests {
    use futures::future;
    use hotshot::{helpers::initialize_logging, traits::BlockPayload};
    use hotshot_query_service::VidCommon;
    use hotshot_types::{
        data::VidCommitment,
        traits::EncodeBytes,
        vid::avidm::{AvidMParam, AvidMScheme},
    };

    use crate::{
        v0::impls::block::test::ValidTest, v0_3::AvidMNsProof, NsIndex, NsProof, Payload,
        Transaction,
    };

    #[tokio::test(flavor = "multi_thread")]
    async fn ns_proof() {
        let test_cases = vec![
            vec![
                vec![5, 8, 8],
                vec![7, 9, 11],
                vec![10, 5, 8],
                vec![7, 8, 9],
                vec![],
            ],
            vec![vec![1, 2, 3], vec![4, 5, 6]],
            vec![],
        ];

        initialize_logging();

        let mut rng = jf_utils::test_rng();
        let mut tests = ValidTest::many_from_tx_lengths(test_cases, &mut rng);

        let param = AvidMParam::new(5usize, 10usize).unwrap();

        struct BlockInfo {
            block: Payload,
            vid_commit: VidCommitment,
            ns_proofs: Vec<AvidMNsProof>,
        }

        let blocks: Vec<BlockInfo> = future::join_all(tests.iter().map(|t| async {
            let block =
                Payload::from_transactions(t.all_txs(), &Default::default(), &Default::default())
                    .await
                    .unwrap()
                    .0;
            let payload_byte_len = block.byte_len();
            let ns_table = block.ns_table();
            let ns_table = ns_table
                .iter()
                .map(|index| ns_table.ns_range(&index, &payload_byte_len).0)
                .collect::<Vec<_>>();
            let vid_commit = AvidMScheme::commit(&param, &block.encode(), ns_table).unwrap();
            let ns_proofs: Vec<AvidMNsProof> = block
                .ns_table()
                .iter()
                .map(|ns_index| AvidMNsProof::new(&block, &ns_index, &param).unwrap())
                .collect();
            BlockInfo {
                block,
                vid_commit: VidCommitment::V1(vid_commit),
                ns_proofs,
            }
        }))
        .await;

        // sanity: verify all valid namespace proofs
        for (
            BlockInfo {
                block,
                vid_commit,
                ns_proofs,
            },
            test,
        ) in blocks.iter().zip(tests.iter_mut())
        {
            for ns_proof in ns_proofs.iter() {
                let ns_id = block
                    .ns_table()
                    .read_ns_id(&NsIndex(ns_proof.0.ns_index))
                    .unwrap();
                let txs = test
                    .nss
                    .remove(&ns_id)
                    .unwrap_or_else(|| panic!("namespace {} missing from test", ns_id));

                // verify ns_proof
                let (ns_proof_txs, ns_proof_ns_id) = ns_proof
                    .verify(block.ns_table(), vid_commit, &param)
                    .unwrap_or_else(|| panic!("namespace {} proof verification failure", ns_id));

                assert_eq!(ns_proof_ns_id, ns_id);
                assert_eq!(ns_proof_txs, txs);

                println!("ns_id: {:?}", ns_id);
                let p = NsProof::V1(ns_proof.clone());
                println!(
                    "ns_proof: {:?}",
                    serde_json::to_string(&p).unwrap().as_bytes()
                );
                println!("vid_commit: {:?}", vid_commit.to_string().as_bytes(),);
                let vid_common = VidCommon::V1(param.clone());
                println!(
                    "vid_common: {:?}",
                    serde_json::to_string(&vid_common).unwrap().as_bytes()
                );
                // println!("namespace: {:?}", ns_id);
                println!("ns_table: {:?}", block.ns_table().bytes);
                println!("tx_commit: {:?}", hash_txns(ns_id.into(), &txs).as_bytes());
                println!("====================================");
            }
        }

        assert!(blocks.len() >= 2, "need at least 2 test_cases");

        let ns_proof_0_0 = &blocks[0].ns_proofs[0];
        let ns_table_0 = blocks[0].block.ns_table();
        let ns_table_1 = blocks[1].block.ns_table();
        let vid_commit_1 = &blocks[1].vid_commit;

        // mix and match ns_table, vid_commit, vid_common
        {
            // wrong vid commitment
            assert!(ns_proof_0_0
                .verify(ns_table_0, vid_commit_1, &param)
                .is_none());

            // wrong ns_proof
            assert!(ns_proof_0_0
                .verify(ns_table_1, vid_commit_1, &param)
                .is_none());
        }
    }

    fn hash_txns(namespace: u32, txns: &[Transaction]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(namespace.to_le_bytes());
        for txn in txns {
            hasher.update(&txn.payload());
        }
        let hash_result = hasher.finalize();
        format!("{:x}", hash_result)
    }

    use serde::{Deserialize, Serialize};
    #[derive(Serialize, Deserialize)]
    struct TestData {
        ns_proof: Vec<u8>,
        vid_commit: Vec<u8>,
        vid_common: Vec<u8>,
        namespace: u64,
        tx_commit: Vec<u8>,
        ns_table: Vec<u8>,
    }

    #[test]
    fn serde_ns_proof() {
        let bytes = [
            123, 34, 86, 49, 34, 58, 123, 34, 110, 115, 95, 105, 110, 100, 101, 120, 34, 58, 49,
            44, 34, 110, 115, 95, 112, 97, 121, 108, 111, 97, 100, 34, 58, 91, 51, 44, 48, 44, 48,
            44, 48, 44, 49, 44, 48, 44, 48, 44, 48, 44, 51, 44, 48, 44, 48, 44, 48, 44, 54, 44, 48,
            44, 48, 44, 48, 44, 50, 48, 44, 50, 54, 44, 50, 53, 48, 44, 49, 49, 49, 44, 55, 50, 44,
            52, 48, 93, 44, 34, 110, 115, 95, 112, 114, 111, 111, 102, 34, 58, 34, 77, 69, 82, 75,
            76, 69, 95, 80, 82, 79, 79, 70, 126, 65, 81, 65, 65, 65, 65, 65, 65, 65, 65, 65, 67,
            65, 65, 65, 65, 65, 65, 65, 65, 65, 68, 78, 65, 101, 56, 116, 67, 112, 98, 119, 85, 55,
            116, 99, 108, 119, 87, 72, 74, 65, 121, 86, 68, 72, 45, 112, 55, 109, 109, 56, 51, 107,
            116, 50, 110, 53, 66, 103, 103, 108, 69, 51, 49, 65, 65, 65, 65, 65, 65, 65, 65, 65,
            65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65,
            65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 52, 34, 125, 125,
        ];
        let proof: NsProof = serde_json::from_slice(&bytes).unwrap();
        println!("{:?}", proof);
    }
}

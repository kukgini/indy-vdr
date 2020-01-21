use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use serde_json::{self, Value as SJsonValue};

use super::state_proof;

use crate::utils::base58::FromBase58;
use crate::utils::error::prelude::*;
use crate::utils::merkletree::MerkleTree;

use super::requests::{PoolRequest, RequestEvent, TimingResult};
use super::types;

mod catchup;
mod consensus;
mod full;
mod single;
mod status;

pub use catchup::{build_catchup_req, handle_catchup_request};
pub use consensus::handle_consensus_request;
pub use full::handle_full_request;
pub use single::handle_single_request;
pub use status::{build_ledger_status_req, handle_status_request};

#[derive(Debug)]
pub enum SingleReply<T> {
    Reply(T),
    Failed(String),
    Timeout(),
}

impl<T: ToString> ToString for SingleReply<T> {
    fn to_string(&self) -> String {
        match self {
            Self::Reply(msg) => msg.to_string(),
            Self::Failed(msg) => msg.clone(),
            Self::Timeout() => "timeout".to_owned(),
        }
    }
}

pub type NodeReplies<T> = HashMap<String, SingleReply<T>>;

#[derive(Debug)]
pub struct ReplyState<T> {
    pub inner: HashMap<String, SingleReply<T>>,
}

impl<T> ReplyState<T> {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    pub fn add_reply(&mut self, node_alias: String, reply: T) {
        self.inner.insert(node_alias, SingleReply::Reply(reply));
    }

    pub fn add_failed(&mut self, node_alias: String, raw_msg: String) {
        self.inner.insert(node_alias, SingleReply::Failed(raw_msg));
    }

    pub fn add_timeout(&mut self, node_alias: String) {
        if !self.inner.contains_key(&node_alias) {
            self.inner.insert(node_alias, SingleReply::Timeout());
        }
    }

    pub fn result(self) -> NodeReplies<T> {
        self.inner
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }
}

#[derive(Debug)]
pub enum RequestResult<T> {
    Reply(T),
    Failed(LedgerError),
}

impl<T> RequestResult<T> {
    pub fn map<F, R>(self, f: F) -> RequestResult<R>
    where
        F: FnOnce(T) -> R,
    {
        match self {
            Self::Reply(reply) => RequestResult::Reply(f(reply)),
            Self::Failed(err) => RequestResult::Failed(err),
        }
    }
}

#[derive(Debug)]
struct ConsensusState<K: Eq + Hash, T: Eq + Hash> {
    inner: HashMap<K, HashSet<T>>,
}

impl<K: Eq + Hash, T: Eq + Hash> ConsensusState<K, T> {
    fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    fn max_entry(&self) -> Option<(&K, usize)> {
        self.inner
            .iter()
            .map(|(key, set)| (key, set.len()))
            .max_by_key(|entry| entry.1)
    }

    fn max_len(&self) -> usize {
        self.max_entry().map(|entry| entry.1).unwrap_or(0)
    }

    pub fn insert(&mut self, key: K, reply: T) -> &mut HashSet<T> {
        let set = self.inner.entry(key).or_insert_with(HashSet::new);
        set.insert(reply);
        set
    }
}

#[derive(Debug)]
pub struct HashableValue {
    pub inner: SJsonValue,
}

impl Hash for HashableValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // FIXME does to_string produce canonical results??
        serde_json::to_string(&self.inner).unwrap().hash(state); //TODO
    }
}

impl PartialEq for HashableValue {
    fn eq(&self, other: &HashableValue) -> bool {
        self.inner.eq(&other.inner)
    }
}

impl Eq for HashableValue {}

fn min_consensus(cnt: usize) -> usize {
    if cnt < 4 {
        return 0;
    }
    (cnt - 1) / 3
}

fn check_cons_proofs(
    mt: &MerkleTree,
    cons_proofs: &Vec<String>,
    target_mt_root: &Vec<u8>,
    target_mt_size: usize,
) -> LedgerResult<()> {
    let mut bytes_proofs: Vec<Vec<u8>> = Vec::new();

    for cons_proof in cons_proofs {
        let cons_proof: &String = cons_proof;

        bytes_proofs.push(
            cons_proof.from_base58().to_result(
                LedgerErrorKind::InvalidStructure,
                "Can't decode node consistency proof",
            )?, // FIXME: review kind
        );
    }

    if !mt.consistency_proof(target_mt_root, target_mt_size, &bytes_proofs)? {
        return Err(err_msg(
            LedgerErrorKind::InvalidState,
            "Consistency proof verification failed",
        )); // FIXME: review kind
    }

    Ok(())
}

fn get_msg_result_without_state_proof(msg: &str) -> LedgerResult<(SJsonValue, SJsonValue)> {
    let msg = serde_json::from_str::<SJsonValue>(msg).to_result(
        LedgerErrorKind::InvalidStructure,
        "Response is malformed json",
    )?;

    let msg_result = msg["result"].clone();

    let mut msg_result_without_proof: SJsonValue = msg_result.clone();
    msg_result_without_proof
        .as_object_mut()
        .map(|obj| obj.remove("state_proof"));

    if msg_result_without_proof["data"].is_object() {
        msg_result_without_proof["data"]
            .as_object_mut()
            .map(|obj| obj.remove("stateProofFrom"));
    }

    Ok((msg_result, msg_result_without_proof))
}

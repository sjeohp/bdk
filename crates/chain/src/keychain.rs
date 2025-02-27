//! Module for keychain related structures.
//!
//! A keychain here is a set of application-defined indexes for a miniscript descriptor where we can
//! derive script pubkeys at a particular derivation index. The application's index is simply
//! anything that implements `Ord`.
//!
//! [`KeychainTxOutIndex`] indexes script pubkeys of keychains and scans in relevant outpoints (that
//! has a `txout` containing an indexed script pubkey). Internally, this uses [`SpkTxOutIndex`], but
//! also maintains "revealed" and "lookahead" index counts per keychain.
//!
//! [`SpkTxOutIndex`]: crate::SpkTxOutIndex

use crate::{
    collections::BTreeMap, indexed_tx_graph, local_chain, tx_graph::TxGraph, Anchor, Append,
};

#[cfg(feature = "miniscript")]
mod txout_index;
#[cfg(feature = "miniscript")]
pub use txout_index::*;

/// Represents updates to the derivation index of a [`KeychainTxOutIndex`].
/// It maps each keychain `K` to its last revealed index.
///
/// It can be applied to [`KeychainTxOutIndex`] with [`apply_changeset`]. [`ChangeSet] are
/// monotone in that they will never decrease the revealed derivation index.
///
/// [`KeychainTxOutIndex`]: crate::keychain::KeychainTxOutIndex
/// [`apply_changeset`]: crate::keychain::KeychainTxOutIndex::apply_changeset
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize, serde::Serialize),
    serde(
        crate = "serde_crate",
        bound(
            deserialize = "K: Ord + serde::Deserialize<'de>",
            serialize = "K: Ord + serde::Serialize"
        )
    )
)]
#[must_use]
pub struct ChangeSet<K>(pub BTreeMap<K, u32>);

impl<K> ChangeSet<K> {
    /// Get the inner map of the keychain to its new derivation index.
    pub fn as_inner(&self) -> &BTreeMap<K, u32> {
        &self.0
    }
}

impl<K: Ord> Append for ChangeSet<K> {
    /// Append another [`ChangeSet`] into self.
    ///
    /// If the keychain already exists, increase the index when the other's index > self's index.
    /// If the keychain did not exist, append the new keychain.
    fn append(&mut self, mut other: Self) {
        self.0.iter_mut().for_each(|(key, index)| {
            if let Some(other_index) = other.0.remove(key) {
                *index = other_index.max(*index);
            }
        });

        self.0.append(&mut other.0);
    }

    /// Returns whether the changeset are empty.
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<K> Default for ChangeSet<K> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<K> AsRef<BTreeMap<K, u32>> for ChangeSet<K> {
    fn as_ref(&self) -> &BTreeMap<K, u32> {
        &self.0
    }
}

/// A structure to update [`KeychainTxOutIndex`], [`TxGraph`] and [`LocalChain`] atomically.
///
/// [`LocalChain`]: local_chain::LocalChain
#[derive(Debug, Clone)]
pub struct WalletUpdate<K, A> {
    /// Contains the last active derivation indices per keychain (`K`), which is used to update the
    /// [`KeychainTxOutIndex`].
    pub last_active_indices: BTreeMap<K, u32>,

    /// Update for the [`TxGraph`].
    pub graph: TxGraph<A>,

    /// Update for the [`LocalChain`].
    ///
    /// [`LocalChain`]: local_chain::LocalChain
    pub chain: local_chain::Update,
}

impl<K, A> WalletUpdate<K, A> {
    /// Construct a [`WalletUpdate`] with a given [`local_chain::Update`].
    pub fn new(chain_update: local_chain::Update) -> Self {
        Self {
            last_active_indices: BTreeMap::new(),
            graph: TxGraph::default(),
            chain: chain_update,
        }
    }
}

/// A structure that records the corresponding changes as result of applying an [`WalletUpdate`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize, serde::Serialize),
    serde(
        crate = "serde_crate",
        bound(
            deserialize = "K: Ord + serde::Deserialize<'de>, A: Ord + serde::Deserialize<'de>",
            serialize = "K: Ord + serde::Serialize, A: Ord + serde::Serialize",
        )
    )
)]
pub struct WalletChangeSet<K, A> {
    /// Changes to the [`LocalChain`].
    ///
    /// [`LocalChain`]: local_chain::LocalChain
    pub chain: local_chain::ChangeSet,

    /// ChangeSet to [`IndexedTxGraph`].
    ///
    /// [`IndexedTxGraph`]: crate::indexed_tx_graph::IndexedTxGraph
    pub indexed_tx_graph: indexed_tx_graph::ChangeSet<A, ChangeSet<K>>,
}

impl<K, A> Default for WalletChangeSet<K, A> {
    fn default() -> Self {
        Self {
            chain: Default::default(),
            indexed_tx_graph: Default::default(),
        }
    }
}

impl<K: Ord, A: Anchor> Append for WalletChangeSet<K, A> {
    fn append(&mut self, other: Self) {
        Append::append(&mut self.chain, other.chain);
        Append::append(&mut self.indexed_tx_graph, other.indexed_tx_graph);
    }

    fn is_empty(&self) -> bool {
        self.chain.is_empty() && self.indexed_tx_graph.is_empty()
    }
}

impl<K, A> From<local_chain::ChangeSet> for WalletChangeSet<K, A> {
    fn from(chain: local_chain::ChangeSet) -> Self {
        Self {
            chain,
            ..Default::default()
        }
    }
}

impl<K, A> From<indexed_tx_graph::ChangeSet<A, ChangeSet<K>>> for WalletChangeSet<K, A> {
    fn from(indexed_tx_graph: indexed_tx_graph::ChangeSet<A, ChangeSet<K>>) -> Self {
        Self {
            indexed_tx_graph,
            ..Default::default()
        }
    }
}

/// Balance, differentiated into various categories.
#[derive(Debug, PartialEq, Eq, Clone, Default)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize, serde::Serialize),
    serde(crate = "serde_crate",)
)]
pub struct Balance {
    /// All coinbase outputs not yet matured
    pub immature: u64,
    /// Unconfirmed UTXOs generated by a wallet tx
    pub trusted_pending: u64,
    /// Unconfirmed UTXOs received from an external wallet
    pub untrusted_pending: u64,
    /// Confirmed and immediately spendable balance
    pub confirmed: u64,
}

impl Balance {
    /// Get sum of trusted_pending and confirmed coins.
    ///
    /// This is the balance you can spend right now that shouldn't get cancelled via another party
    /// double spending it.
    pub fn trusted_spendable(&self) -> u64 {
        self.confirmed + self.trusted_pending
    }

    /// Get the whole balance visible to the wallet.
    pub fn total(&self) -> u64 {
        self.confirmed + self.trusted_pending + self.untrusted_pending + self.immature
    }
}

impl core::fmt::Display for Balance {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{{ immature: {}, trusted_pending: {}, untrusted_pending: {}, confirmed: {} }}",
            self.immature, self.trusted_pending, self.untrusted_pending, self.confirmed
        )
    }
}

impl core::ops::Add for Balance {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            immature: self.immature + other.immature,
            trusted_pending: self.trusted_pending + other.trusted_pending,
            untrusted_pending: self.untrusted_pending + other.untrusted_pending,
            confirmed: self.confirmed + other.confirmed,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn append_keychain_derivation_indices() {
        #[derive(Ord, PartialOrd, Eq, PartialEq, Clone, Debug)]
        enum Keychain {
            One,
            Two,
            Three,
            Four,
        }
        let mut lhs_di = BTreeMap::<Keychain, u32>::default();
        let mut rhs_di = BTreeMap::<Keychain, u32>::default();
        lhs_di.insert(Keychain::One, 7);
        lhs_di.insert(Keychain::Two, 0);
        rhs_di.insert(Keychain::One, 3);
        rhs_di.insert(Keychain::Two, 5);
        lhs_di.insert(Keychain::Three, 3);
        rhs_di.insert(Keychain::Four, 4);

        let mut lhs = ChangeSet(lhs_di);
        let rhs = ChangeSet(rhs_di);
        lhs.append(rhs);

        // Exiting index doesn't update if the new index in `other` is lower than `self`.
        assert_eq!(lhs.0.get(&Keychain::One), Some(&7));
        // Existing index updates if the new index in `other` is higher than `self`.
        assert_eq!(lhs.0.get(&Keychain::Two), Some(&5));
        // Existing index is unchanged if keychain doesn't exist in `other`.
        assert_eq!(lhs.0.get(&Keychain::Three), Some(&3));
        // New keychain gets added if the keychain is in `other` but not in `self`.
        assert_eq!(lhs.0.get(&Keychain::Four), Some(&4));
    }
}

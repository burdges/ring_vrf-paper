// -*- mode: rust; -*-
//
// Copyright (c) 2019 Web 3 Foundation
//
// Authors:
// - Sergey Vasilyev <swasilyev@gmail.com>
// - Jeffrey Burdges <jeff@web3.foundation>

//! ### Ring representation as Merkle tree 


use std::io;

use core::ops::{Deref, DerefMut};
use core::iter::IntoIterator;
use core::borrow::Borrow;

use ff::{PrimeField, PrimeFieldRepr, BitIterator, Field}; // ScalarEngine
use pairing::bls12_381::Fr;
use zcash_primitives::jubjub::JubjubEngine;
use zcash_primitives::pedersen_hash;
use crate::{JubjubEngineWithParams, Params, PublicKey};


/// Direction of the binary merkle path, either going left or right.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MerkleSelection {
    /// Move left to the sub-node.
    Left,
    /// Move right to the sub-node.
    Right,
}

impl MerkleSelection {
    /// Create a random path direction from a random source.
    pub fn random<R: rand_core::RngCore>(rng: &mut R) -> Self {
        if rng.next_u32() % 2 == 0 {
            MerkleSelection::Left
        } else {
            MerkleSelection::Right
        }
    }
}


/// A point in the authentication path.
#[derive(Clone, Debug)]
pub struct AuthPathPoint<E: JubjubEngine> {
    /// The current selection. That is, the opposite of sibling.
    pub current_selection: MerkleSelection,
    /// Sibling value, if it exists.
    pub sibling: Option<E::Fr>,
}

impl<E: JubjubEngine> AuthPathPoint<E> {
    pub fn read<R: io::Read>(reader: R) -> io::Result<Self> {
        let mut repr = <E::Fr as PrimeField>::Repr::default();
        repr.read_le(reader) ?;

        use MerkleSelection::*;
        let current_selection = if (repr.as_ref()[3] >> 63) == 1 { Left } else { Right };
        repr.as_mut()[3] &= 0x7fffffffffffffff;

        let err = |_| io::Error::new(io::ErrorKind::InvalidInput, "auth path point is not in field" );

        // zcash_primitives::jubjub::fs::MODULUS_BITS = 252
        let sibling = if (repr.as_ref()[3] >> 62) == 1 {
            repr.as_mut()[3] &= 0x3fffffffffffffff;
            Some(E::Fr::from_repr(repr).map_err(err) ?)
        } else { None };

        Ok(AuthPathPoint { current_selection, sibling })
    }

    pub fn write<W: io::Write>(&self, writer: W) -> io::Result<()> {
        let mut repr = self.sibling.map( |x| x.into_repr() ).unwrap_or_default();
        assert!((repr.as_mut()[3] & 0x7fffffffffffffff) == 0);

        if self.sibling.is_none() {
            repr.as_mut()[3] |= 0x4000000000000000u64;
        }

        if self.current_selection == MerkleSelection::Left {
            repr.as_mut()[3] |= 0x8000000000000000u64;
        }

        repr.write_le(writer)
    }
}


/// Compute Merkle root and path
///
/// Leaves list argument unusable
fn merkleize<E>(
    depth: usize,
    mut list: &mut [E::Fr],
    mut index: usize,
    mut f: impl FnMut(AuthPathPoint<E>) -> (),
) -> E::Fr
where E: JubjubEngineWithParams,
{
    assert!( list.len() > 0 );
    // let mut tail = 0usize;
    // if list.len().count_ones() != 1 {
    //    let s = 0usize.leadng_zeros() - list.len().leading_zeros() - 1;
    //     tail = (1usize << s) - list.len();
    // }

    for depth_to_bottom in 0..depth {
        let (current_selection, sibling) = if index % 2 == 0 {
            (MerkleSelection::Left, list.get(index).cloned())
        } else {
            (MerkleSelection::Right, list.get(index).cloned())
        };
        f(AuthPathPoint { current_selection, sibling, });

        for i in (0..list.len()).filter(|x| x % 2 == 0) { 
            let left = list.get(i);
            let right = list.get(i+1);
            list[i/2] = auth_hash::<E>(left, right, depth_to_bottom);
        }

        index /= 2;

        let len = list.len() + (list.len() % 2);
        list = &mut list[0..len/2]
    }    

    list[0].clone()
}

/// The authentication path of the merkle tree.
#[derive(Clone, Debug)]
pub struct AuthPath<E: JubjubEngine>(pub Vec<AuthPathPoint<E>>);

impl<E: JubjubEngineWithParams> AuthPath<E> {
    /// Create a random path.
    pub fn random<R: rand_core::RngCore>(depth: usize, rng: &mut R) -> AuthPath<E> {
        Self(vec![AuthPathPoint {
            current_selection: MerkleSelection::random(rng),
            sibling: Some(<E::Fr>::random(rng))
        }; depth])
    }

    /// Create a path from a given plain list, of target specified as `list_index`.
    /// Panic if `list_index` is out of bound.
    pub fn from_publickeys<B,I>(iter: I, index: usize, depth: usize) -> (AuthPath<E>,AuthRoot<E>) 
    where B: Borrow<PublicKey<E>>, I: IntoIterator<Item=B>
    {
        let mut list = iter.into_iter().map( |pk| pk.borrow().0.to_xy().0 ).collect::<Vec<_>>();
        let path_len = 0usize.leading_zeros() - list.len().leading_zeros();
        let mut path = Vec::with_capacity(path_len as usize);
        assert!(list.len() > 1);
        let root = merkleize( depth, list.as_mut_slice(), index, |x| path.push(x) );
        (AuthPath(path), AuthRoot(root))
    }

    /*
    TODO:
    pub fn read<R: io::Read>(reader: R) -> io::Result<Self> {
    }

    pub fn write<W: io::Write>(&self, writer: W) -> io::Result<()> {
    }
    */
}

impl<E: JubjubEngine> Default for AuthPath<E> {
    fn default() -> AuthPath<E> {
        AuthPath(Default::default())
    }
}

impl<E: JubjubEngine> Deref for AuthPath<E> {
    type Target = Vec<AuthPathPoint<E>>;

    fn deref(&self) -> &Vec<AuthPathPoint<E>> {
        &self.0
    }
}

impl<E: JubjubEngine> DerefMut for AuthPath<E> {
    fn deref_mut(&mut self) -> &mut Vec<AuthPathPoint<E>> {
        &mut self.0
    }
}

/// The authentication root / merkle root of a given tree.
pub struct AuthRoot<E: JubjubEngine>(pub E::Fr);

impl<E: JubjubEngine> Deref for AuthRoot<E> {
    type Target = E::Fr;
    fn deref(&self) -> &E::Fr { &self.0 }
}

impl<E: JubjubEngine> DerefMut for AuthRoot<E> {
    fn deref_mut(&mut self) -> &mut E::Fr { &mut self.0 }
}

impl<E: JubjubEngineWithParams> AuthRoot<E> {
    /// Get the merkle root from proof.
    pub fn from_proof(path: &AuthPath<E>, target: &PublicKey<E>) -> Self {
        let mut cur = target.0.to_xy().0;

        for (depth_to_bottom, point) in path.iter().enumerate() {
            let (left, right) = match point.current_selection {
                MerkleSelection::Right => (point.sibling.as_ref(), Some(&cur)),
                MerkleSelection::Left => (Some(&cur), point.sibling.as_ref()),
            };

            cur = auth_hash::<E>(left, right, depth_to_bottom);
        }

        Self(cur)
    }

    /// Get the merkle root from a list of public keys. Panic if length of the list is zero.
    ///
    /// TODO: We do no initial hashing here for the leaves, but maybe that's fine.
    pub fn from_publickeys<B,I>(iter: I, depth: usize) -> Self
    where B: Borrow<PublicKey<E>>, I: IntoIterator<Item=B>
    {
        let mut list = iter.into_iter().map( |pk| pk.borrow().0.to_xy().0 ).collect::<Vec<_>>();
        assert!(list.len() > 1);
        AuthRoot(merkleize( depth, list.as_mut_slice(), 0 , |_: AuthPathPoint<E>| () ))
    }

    pub fn read<R: io::Read>(reader: R) -> io::Result<Self> {
        let mut repr = <E::Fr as PrimeField>::Repr::default();
        repr.read_le(reader) ?;
        let err = |_| io::Error::new(io::ErrorKind::InvalidInput, "auth path point is not in field" );
        Ok(AuthRoot( E::Fr::from_repr(repr).map_err(err) ? ))
    }

    pub fn write<W: io::Write>(&self, writer: W) -> io::Result<()> {
        self.0.into_repr().write_le(writer)
    }
}

/// Hash function used to create the authentication merkle tree.
pub fn auth_hash<E: JubjubEngineWithParams>(
    left: Option<&E::Fr>,
    right: Option<&E::Fr>,
    depth_to_bottom: usize,
) -> E::Fr {
    let zero = <E::Fr>::zero();

    let mut lhs = BitIterator::new(left.unwrap_or(&zero).into_repr()).collect::<Vec<bool>>();
    let mut rhs = BitIterator::new(right.unwrap_or(&zero).into_repr()).collect::<Vec<bool>>();

    lhs.reverse();
    rhs.reverse();

    pedersen_hash::pedersen_hash::<E, _>(
        pedersen_hash::Personalization::MerkleTree(depth_to_bottom),
        lhs.into_iter()
            .take(Fr::NUM_BITS as usize)
            .chain(rhs.into_iter().take(Fr::NUM_BITS as usize)),
        E::params(),
    ).to_xy().0
}


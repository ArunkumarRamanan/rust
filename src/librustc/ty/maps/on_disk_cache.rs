// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use dep_graph::{DepNodeIndex, SerializedDepNodeIndex};
use rustc_data_structures::fx::FxHashMap;
use rustc_data_structures::indexed_vec::Idx;
use errors::Diagnostic;
use rustc_serialize::{Decodable, Decoder, Encodable, Encoder, opaque,
                      SpecializedDecoder};
use session::Session;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::mem;
use syntax::codemap::{CodeMap, StableFilemapId};
use syntax_pos::{BytePos, Span, NO_EXPANSION, DUMMY_SP};

pub struct OnDiskCache<'sess> {
    prev_diagnostics: FxHashMap<SerializedDepNodeIndex, Vec<Diagnostic>>,

    _prev_filemap_starts: BTreeMap<BytePos, StableFilemapId>,
    codemap: &'sess CodeMap,

    current_diagnostics: RefCell<FxHashMap<DepNodeIndex, Vec<Diagnostic>>>,
}

#[derive(RustcEncodable, RustcDecodable)]
struct Header {
    prev_filemap_starts: BTreeMap<BytePos, StableFilemapId>,
}

#[derive(RustcEncodable, RustcDecodable)]
struct Body {
    diagnostics: Vec<(SerializedDepNodeIndex, Vec<Diagnostic>)>,
}

impl<'sess> OnDiskCache<'sess> {
    pub fn new_empty(codemap: &'sess CodeMap) -> OnDiskCache<'sess> {
        OnDiskCache {
            prev_diagnostics: FxHashMap(),
            _prev_filemap_starts: BTreeMap::new(),
            codemap,
            current_diagnostics: RefCell::new(FxHashMap()),
        }
    }

    pub fn new(sess: &'sess Session, data: &[u8]) -> OnDiskCache<'sess> {
        debug_assert!(sess.opts.incremental.is_some());

        let mut decoder = opaque::Decoder::new(&data[..], 0);
        let header = Header::decode(&mut decoder).unwrap();

        let prev_diagnostics: FxHashMap<_, _> = {
            let mut decoder = CacheDecoder {
                opaque: decoder,
                codemap: sess.codemap(),
                prev_filemap_starts: &header.prev_filemap_starts,
            };
            let body = Body::decode(&mut decoder).unwrap();
            body.diagnostics.into_iter().collect()
        };

        OnDiskCache {
            prev_diagnostics,
            _prev_filemap_starts: header.prev_filemap_starts,
            codemap: sess.codemap(),
            current_diagnostics: RefCell::new(FxHashMap()),
        }
    }

    pub fn serialize<'a, 'tcx, E>(&self,
                                  encoder: &mut E)
                                  -> Result<(), E::Error>
        where E: Encoder
    {
        let prev_filemap_starts: BTreeMap<_, _> = self
            .codemap
            .files()
            .iter()
            .map(|fm| (fm.start_pos, StableFilemapId::new(fm)))
            .collect();

        Header { prev_filemap_starts }.encode(encoder)?;

        let diagnostics: Vec<(SerializedDepNodeIndex, Vec<Diagnostic>)> =
            self.current_diagnostics
                .borrow()
                .iter()
                .map(|(k, v)| (SerializedDepNodeIndex::new(k.index()), v.clone()))
                .collect();

        Body { diagnostics }.encode(encoder)?;

        Ok(())
    }

    pub fn load_diagnostics(&self,
                            dep_node_index: SerializedDepNodeIndex)
                            -> Vec<Diagnostic> {
        self.prev_diagnostics.get(&dep_node_index).cloned().unwrap_or(vec![])
    }

    pub fn store_diagnostics(&self,
                             dep_node_index: DepNodeIndex,
                             diagnostics: Vec<Diagnostic>) {
        let mut current_diagnostics = self.current_diagnostics.borrow_mut();
        let prev = current_diagnostics.insert(dep_node_index, diagnostics);
        debug_assert!(prev.is_none());
    }

    pub fn store_diagnostics_for_anon_node(&self,
                                           dep_node_index: DepNodeIndex,
                                           mut diagnostics: Vec<Diagnostic>) {
        let mut current_diagnostics = self.current_diagnostics.borrow_mut();

        let x = current_diagnostics.entry(dep_node_index).or_insert_with(|| {
            mem::replace(&mut diagnostics, Vec::new())
        });

        x.extend(diagnostics.into_iter());
    }
}

impl<'a> SpecializedDecoder<Span> for CacheDecoder<'a> {
    fn specialized_decode(&mut self) -> Result<Span, Self::Error> {
        let lo = BytePos::decode(self)?;
        let hi = BytePos::decode(self)?;

        if let Some((prev_filemap_start, filemap_id)) = self.find_filemap_prev_bytepos(lo) {
            if let Some(current_filemap) = self.codemap.filemap_by_stable_id(filemap_id) {
                let lo = (lo + current_filemap.start_pos) - prev_filemap_start;
                let hi = (hi + current_filemap.start_pos) - prev_filemap_start;
                return Ok(Span::new(lo, hi, NO_EXPANSION));
            }
        }

        Ok(DUMMY_SP)
    }
}

struct CacheDecoder<'a> {
    opaque: opaque::Decoder<'a>,
    codemap: &'a CodeMap,
    prev_filemap_starts: &'a BTreeMap<BytePos, StableFilemapId>,
}

impl<'a> CacheDecoder<'a> {
    fn find_filemap_prev_bytepos(&self,
                                 prev_bytepos: BytePos)
                                 -> Option<(BytePos, StableFilemapId)> {
        for (start, id) in self.prev_filemap_starts.range(BytePos(0) ... prev_bytepos).rev() {
            return Some((*start, *id))
        }

        None
    }
}

macro_rules! decoder_methods {
    ($($name:ident -> $ty:ty;)*) => {
        $(fn $name(&mut self) -> Result<$ty, Self::Error> {
            self.opaque.$name()
        })*
    }
}

impl<'sess> Decoder for CacheDecoder<'sess> {
    type Error = String;

    decoder_methods! {
        read_nil -> ();

        read_u128 -> u128;
        read_u64 -> u64;
        read_u32 -> u32;
        read_u16 -> u16;
        read_u8 -> u8;
        read_usize -> usize;

        read_i128 -> i128;
        read_i64 -> i64;
        read_i32 -> i32;
        read_i16 -> i16;
        read_i8 -> i8;
        read_isize -> isize;

        read_bool -> bool;
        read_f64 -> f64;
        read_f32 -> f32;
        read_char -> char;
        read_str -> Cow<str>;
    }

    fn error(&mut self, err: &str) -> Self::Error {
        self.opaque.error(err)
    }
}

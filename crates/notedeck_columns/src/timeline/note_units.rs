use std::collections::{HashMap, HashSet};

use nostrdb::NoteKey;
use notedeck::NoteRef;

use crate::timeline::{
    unit::{CompositeUnit, NoteUnit, NoteUnitFragment},
    MergeKind,
};

type StorageIndex = usize;

/// Provides efficient access to `NoteUnit`s
/// Useful for threads and timelines
/// when reversed=false, sorts from newest to oldest
#[derive(Debug, Default)]
pub struct NoteUnits {
    reversed: bool,
    storage: Vec<NoteUnit>,
    lookup: HashMap<NoteKey, StorageIndex>, // `NoteKey` to index in `NoteUnits::storage`
    order: Vec<StorageIndex>, // the sorted order of the `NoteUnit`s in `NoteUnits::storage`
}

impl NoteUnits {
    pub fn values(&self) -> Values<'_> {
        Values {
            set: self,
            front: 0,
            back: self.order.len(),
        }
    }

    pub fn contains_key(&self, k: &NoteKey) -> bool {
        self.lookup.contains_key(k)
    }

    pub fn new_with_cap(cap: usize, reversed: bool) -> Self {
        Self {
            reversed,
            storage: Vec::with_capacity(cap),
            lookup: HashMap::with_capacity(cap),
            order: Vec::with_capacity(cap),
        }
    }

    pub fn len(&self) -> usize {
        self.storage.len()
    }

    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    /// Get the kth index from 0..Self::len
    pub fn kth(&self, k: usize) -> Option<&NoteUnit> {
        if k >= self.order.len() {
            return None;
        }
        let idx = if self.reversed {
            self.order[self.order.len() - 1 - k]
        } else {
            self.order[k]
        };
        Some(&self.storage[idx])
    }

    /// Core bulk insert for already-built `NoteUnit`s
    /// Merges new `NoteUnit`s into `Self::storage`
    /// Updates `Self::order`
    fn merge_many_internal(
        &mut self,
        mut units: Vec<NoteUnit>,
        touched_indices: &[usize],
    ) -> InsertManyResponse {
        units.retain(|e| !self.lookup.contains_key(&e.key()));
        if units.is_empty() && touched_indices.is_empty() {
            return InsertManyResponse::Zero;
        }

        if !touched_indices.is_empty() {
            self.order.retain(|i| !touched_indices.contains(i));
        }

        units.sort_unstable();
        units.dedup_by_key(|u| u.key());

        let base = self.storage.len();
        let mut new_order = Vec::with_capacity(units.len());
        self.storage.reserve(units.len());
        for (i, unit) in units.into_iter().enumerate() {
            let idx = base + i;
            let key = unit.key();
            self.storage.push(unit);
            self.lookup.insert(key, idx);
            new_order.push(idx);
        }

        let front_insertion = if self.order.is_empty() || new_order.is_empty() {
            true
        } else if !self.reversed {
            let first_new = *new_order.first().unwrap();
            let last_old = *self.order.last().unwrap();
            self.storage[first_new] >= self.storage[last_old]
        } else {
            let last_new = *new_order.last().unwrap();
            let first_old = *self.order.first().unwrap();
            self.storage[last_new] <= self.storage[first_old]
        };

        let mut merged = Vec::with_capacity(self.order.len() + new_order.len());
        let (mut i, mut j) = (0, 0);
        while i < self.order.len() && j < new_order.len() {
            let index_left = self.order[i];
            let index_right = new_order[j];
            let left_item = &self.storage[index_left];
            let right_item = &self.storage[index_right];
            if left_item <= right_item {
                // left_item is newer than right_item
                merged.push(index_left);
                i += 1;
            } else {
                merged.push(index_right);
                j += 1;
            }
        }
        merged.extend_from_slice(&self.order[i..]);
        merged.extend_from_slice(&new_order[j..]);

        for &touched_index in touched_indices {
            let pos = merged
                .binary_search_by(|&i2| self.storage[i2].cmp(&self.storage[touched_index]))
                .unwrap_or_else(|p| p);
            merged.insert(pos, touched_index);
        }

        let inserted = merged.len() - self.order.len();
        self.order = merged;

        if inserted == 0 {
            InsertManyResponse::Zero
        } else if front_insertion {
            InsertManyResponse::Some {
                entries_merged: inserted,
                merge_kind: MergeKind::FrontInsert,
            }
        } else {
            InsertManyResponse::Some {
                entries_merged: inserted,
                merge_kind: MergeKind::Spliced,
            }
        }
    }

    /// Merges `NoteUnitFragment`s
    /// `NoteUnitFragment::Single` is added normally
    /// if `NoteUnitFragment::Composite` exists already, it will fold the fragment into the `CompositeUnit`
    /// otherwise, it will generate the `NoteUnit::CompositeUnit` from the `NoteUnitFragment::Composite`
    pub fn merge_fragments(&mut self, frags: Vec<NoteUnitFragment>) -> InsertManyResponse {
        let mut to_build: HashMap<NoteKey, CompositeUnit> = HashMap::new(); // new composites by key
        let mut singles_to_build: Vec<NoteRef> = Vec::new();
        let mut singles_seen: HashSet<NoteKey> = HashSet::new();

        let mut touched = Vec::new();
        for frag in frags {
            match frag {
                NoteUnitFragment::Single(note_ref) => {
                    let key = note_ref.key;
                    if self.lookup.contains_key(&key) {
                        continue;
                    }
                    if singles_seen.insert(key) {
                        singles_to_build.push(note_ref);
                    }
                }
                NoteUnitFragment::Composite(c_frag) => {
                    let key = c_frag.get_underlying_noteref().key;

                    if let Some(&storage_idx) = self.lookup.get(&key) {
                        if let Some(NoteUnit::Composite(c_unit)) = self.storage.get_mut(storage_idx)
                        {
                            if c_frag.get_latest_ref() < c_unit.get_latest_ref() {
                                touched.push(storage_idx);
                            }
                            c_frag.fold_into(c_unit);
                            continue;
                        }
                    }
                    // aggregate for new composite
                    use std::collections::hash_map::Entry;
                    match to_build.entry(key) {
                        Entry::Occupied(mut o) => {
                            c_frag.fold_into(o.get_mut());
                        }
                        Entry::Vacant(v) => {
                            v.insert(c_frag.into());
                        }
                    }
                }
            }
        }

        let mut items: Vec<NoteUnit> = Vec::with_capacity(singles_to_build.len() + to_build.len());
        items.extend(singles_to_build.into_iter().map(NoteUnit::Single));
        items.extend(to_build.into_values().map(NoteUnit::Composite));

        self.merge_many_internal(items, &touched)
    }

    /// Convienience method to merge a single note
    pub fn merge_single_unit(&mut self, note_ref: NoteRef) -> InsertionResponse {
        match self.merge_many_internal(vec![NoteUnit::Single(note_ref)], &[]) {
            InsertManyResponse::Zero => InsertionResponse::AlreadyExists,
            InsertManyResponse::Some {
                entries_merged: _,
                merge_kind,
            } => InsertionResponse::Merged(merge_kind),
        }
    }

    pub fn latest_ref(&self) -> Option<&NoteRef> {
        if self.reversed {
            self.order.last().map(|&i| &self.storage[i])
        } else {
            self.order.first().map(|&i| &self.storage[i])
        }
        .map(NoteUnit::get_latest_ref)
    }
}

pub enum InsertManyResponse {
    Zero,
    Some {
        entries_merged: usize,
        merge_kind: MergeKind,
    },
}

pub struct Values<'a> {
    set: &'a NoteUnits,
    front: usize,
    back: usize,
}

impl<'a> Iterator for Values<'a> {
    type Item = &'a NoteUnit;
    fn next(&mut self) -> Option<Self::Item> {
        if self.front >= self.back {
            return None;
        }
        let idx = if !self.set.reversed {
            let i = self.front;
            self.front += 1;
            self.set.order[i]
        } else {
            self.back -= 1;
            self.set.order[self.back]
        };
        Some(&self.set.storage[idx])
    }
}

impl<'a> DoubleEndedIterator for Values<'a> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front >= self.back {
            return None;
        }
        let idx = if !self.set.reversed {
            self.back -= 1;
            self.set.order[self.back]
        } else {
            let i = self.front;
            self.front += 1;
            self.set.order[i]
        };
        Some(&self.set.storage[idx])
    }
}

pub enum InsertionResponse {
    AlreadyExists,
    Merged(MergeKind),
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use egui::ahash::HashMap;
    use enostr::Pubkey;
    use nostrdb::NoteKey;
    use notedeck::NoteRef;
    use pretty_assertions::assert_eq;

    use uuid::Uuid;

    use crate::timeline::{
        unit::{
            CompositeFragment, CompositeUnit, NoteUnit, NoteUnitFragment, Reaction,
            ReactionFragment, ReactionUnit,
        },
        NoteUnits,
    };

    #[derive(Default)]
    struct UnitBuilder {
        counter: u64,
        frags: HashMap<String, NoteUnitFragment>,
        units: NoteUnits,
    }

    impl UnitBuilder {
        fn counter(&mut self) -> u64 {
            let res = self.counter;
            self.counter += 1;
            res
        }

        fn random_sender(&mut self) -> Pubkey {
            let mut out = [0u8; 32];
            out[..8].copy_from_slice(&self.counter().to_le_bytes());

            Pubkey::new(out)
        }

        fn build_fragment(&mut self, reacted_to: NoteRef) -> NoteUnitFragment {
            NoteUnitFragment::Composite(CompositeFragment::Reaction(ReactionFragment {
                noteref_reacted_to: reacted_to,
                reaction_note_ref: NoteRef {
                    key: NoteKey::new(self.counter()),
                    created_at: self.counter(),
                },
                reaction: Reaction {
                    reaction: "+".to_owned(),
                    sender: self.random_sender(),
                },
            }))
        }

        fn fragment(&mut self, reacted_to: NoteRef) -> String {
            let frag = self.build_fragment(reacted_to);
            let id = Uuid::new_v4().to_string();
            self.frags.insert(id.clone(), frag.clone());

            self.units.merge_fragments(vec![frag]);

            id
        }

        fn fragments_pair(&mut self, reacted_to: NoteRef) -> (String, String) {
            let frag1 = self.build_fragment(reacted_to);
            let frag2 = self.build_fragment(reacted_to);

            self.units
                .merge_fragments(vec![frag1.clone(), frag2.clone()]);

            let id1 = Uuid::new_v4().to_string();
            self.frags.insert(id1.clone(), frag1);
            let id2 = Uuid::new_v4().to_string();
            self.frags.insert(id2.clone(), frag2);

            (id1, id2)
        }

        fn generate_reaction_note(&mut self) -> NoteRef {
            NoteRef {
                key: NoteKey::new(self.counter()),
                created_at: self.counter(),
            }
        }

        fn insert_note(&mut self) -> String {
            let note_ref = NoteRef {
                key: NoteKey::new(self.counter()),
                created_at: self.counter(),
            };

            let id = Uuid::new_v4().to_string();
            self.frags
                .insert(id.clone(), NoteUnitFragment::Single(note_ref.clone()));

            self.units.merge_single_unit(note_ref);

            id
        }

        fn expected_reactions(&mut self, ids: Vec<&String>) -> NoteUnit {
            let mut reactions = BTreeMap::new();
            let mut reaction_id = None;
            let mut senders = HashSet::new();
            for id in ids {
                let NoteUnitFragment::Composite(CompositeFragment::Reaction(reac)) =
                    self.frags.get(id).unwrap()
                else {
                    panic!("got something other than reaction");
                };

                if let Some(prev_reac_id) = reaction_id {
                    if prev_reac_id != reac.noteref_reacted_to {
                        panic!("internal error");
                    }
                }

                reaction_id = Some(reac.noteref_reacted_to);

                reactions.insert(reac.reaction_note_ref, reac.reaction.clone());
                senders.insert(reac.reaction.sender);
            }

            NoteUnit::Composite(CompositeUnit::Reaction(ReactionUnit {
                note_reacted_to: reaction_id.unwrap(),
                reactions,
                senders: senders,
            }))
        }

        fn expected_single(&mut self, id: &String) -> NoteUnit {
            let Some(NoteUnitFragment::Single(note_ref)) = self.frags.get(id) else {
                panic!("fail");
            };

            NoteUnit::Single(*note_ref)
        }

        fn asserted_at(&self, index: usize) -> NoteUnit {
            self.units.kth(index).unwrap().clone()
        }

        fn aeq(&mut self, units_kth: usize, expect: Expect) {
            assert_eq!(
                self.asserted_at(units_kth),
                match expect {
                    Expect::Single(id) => self.expected_single(id),
                    Expect::Reaction(items) => self.expected_reactions(items),
                }
            );
        }
    }

    enum Expect<'a> {
        Single(&'a String),
        Reaction(Vec<&'a String>),
    }

    #[test]
    fn test() {
        let mut builder = UnitBuilder::default();
        let reaction_note = builder.generate_reaction_note();

        let single0 = builder.insert_note();
        builder.aeq(0, Expect::Single(&single0));

        let reac1 = builder.fragment(reaction_note);
        builder.aeq(0, Expect::Reaction(vec![&reac1]));
        builder.aeq(1, Expect::Single(&single0));

        let single1 = builder.insert_note();
        builder.aeq(0, Expect::Single(&single1));
        builder.aeq(1, Expect::Reaction(vec![&reac1]));
        builder.aeq(2, Expect::Single(&single0));

        let reac2 = builder.fragment(reaction_note);
        builder.aeq(0, Expect::Reaction(vec![&reac2, &reac1]));
        builder.aeq(1, Expect::Single(&single1));
        builder.aeq(2, Expect::Single(&single0));

        let single2 = builder.insert_note();
        builder.aeq(0, Expect::Single(&single2));
        builder.aeq(1, Expect::Reaction(vec![&reac2, &reac1]));
        builder.aeq(2, Expect::Single(&single1));
        builder.aeq(3, Expect::Single(&single0));

        let reac3 = builder.fragment(reaction_note);
        builder.aeq(0, Expect::Reaction(vec![&reac1, &reac2, &reac3]));
        builder.aeq(1, Expect::Single(&single2));
        builder.aeq(2, Expect::Single(&single1));
        builder.aeq(3, Expect::Single(&single0));
    }

    #[test]
    fn test2() {
        let mut builder = UnitBuilder::default();
        let reaction_note1 = builder.generate_reaction_note();
        let reaction_note2 = builder.generate_reaction_note();

        let single0 = builder.insert_note();
        builder.aeq(0, Expect::Single(&single0));

        let reac1_1 = builder.fragment(reaction_note1);
        builder.aeq(0, Expect::Reaction(vec![&reac1_1]));
        builder.aeq(1, Expect::Single(&single0));

        let reac2_1 = builder.fragment(reaction_note2);
        builder.aeq(0, Expect::Reaction(vec![&reac2_1]));
        builder.aeq(1, Expect::Reaction(vec![&reac1_1]));
        builder.aeq(2, Expect::Single(&single0));

        let single1 = builder.insert_note();
        builder.aeq(0, Expect::Single(&single1));
        builder.aeq(1, Expect::Reaction(vec![&reac2_1]));
        builder.aeq(2, Expect::Reaction(vec![&reac1_1]));
        builder.aeq(3, Expect::Single(&single0));

        let reac1_2 = builder.fragment(reaction_note1);
        builder.aeq(0, Expect::Reaction(vec![&reac1_2, &reac1_1]));
        builder.aeq(1, Expect::Single(&single1));
        builder.aeq(2, Expect::Reaction(vec![&reac2_1]));
        builder.aeq(3, Expect::Single(&single0));

        let single2 = builder.insert_note();
        builder.aeq(0, Expect::Single(&single2));
        builder.aeq(1, Expect::Reaction(vec![&reac1_2, &reac1_1]));
        builder.aeq(2, Expect::Single(&single1));
        builder.aeq(3, Expect::Reaction(vec![&reac2_1]));
        builder.aeq(4, Expect::Single(&single0));

        let reac1_3 = builder.fragment(reaction_note1);
        builder.aeq(0, Expect::Reaction(vec![&reac1_2, &reac1_1, &reac1_3]));
        builder.aeq(1, Expect::Single(&single2));
        builder.aeq(2, Expect::Single(&single1));
        builder.aeq(3, Expect::Reaction(vec![&reac2_1]));
        builder.aeq(4, Expect::Single(&single0));

        let reac2_2 = builder.fragment(reaction_note2);
        builder.aeq(0, Expect::Reaction(vec![&reac2_1, &reac2_2]));
        builder.aeq(1, Expect::Reaction(vec![&reac1_2, &reac1_1, &reac1_3]));
        builder.aeq(2, Expect::Single(&single2));
        builder.aeq(3, Expect::Single(&single1));
        builder.aeq(4, Expect::Single(&single0));
    }

    #[test]
    fn test3() {
        let mut builder = UnitBuilder::default();
        let reaction_note1 = builder.generate_reaction_note();

        let single1 = builder.insert_note();
        builder.aeq(0, Expect::Single(&single1));

        let reac0 = builder.fragment(reaction_note1);
        builder.aeq(0, Expect::Reaction(vec![&reac0]));
        builder.aeq(1, Expect::Single(&single1));

        let (reac1, reac2) = builder.fragments_pair(reaction_note1);
        builder.aeq(0, Expect::Reaction(vec![&reac0, &reac1, &reac2]));
        builder.aeq(1, Expect::Single(&single1));

        let single2 = builder.insert_note();
        builder.aeq(0, Expect::Single(&single2));
        builder.aeq(1, Expect::Reaction(vec![&reac0, &reac1, &reac2]));
        builder.aeq(2, Expect::Single(&single1));
    }
}

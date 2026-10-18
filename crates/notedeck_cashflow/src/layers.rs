//! Which journal layers the books are loaded with.
//!
//! Every segment of the journal belongs to a layer (`bank`, `cln`,
//! `cln-chain`, …), and a reader can leave whole layers out: on-chain BTC
//! that overlaps another importer's view, say. [`Layers`] is the app's
//! switch for that: the Overview's layer chips and the Settings toggles flip
//! it, and whoever loads the books reloads them with [`Layers::filter`] when
//! [`Layers::generation`] moves. It holds only what is switched off, so a
//! layer that arrives later is on, unless it is one of
//! [`cashflow::settings::DEFAULT_OFF_LAYERS`].
//!
//! The switches follow the account across machines: each flip is queued as
//! a [`LayerSwitch`] for the journal source to write to the vault
//! ([`Layers::take_writes`]), and the vault's switches come back in through
//! [`Layers::apply`].

use cashflow::Books;
use cashflow::settings::{DEFAULT_OFF_LAYERS, LayerSwitch};
use ledgerdb::LayerFilter;

/// One flip of a layer, to write to the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerWrite {
    /// The layer.
    pub layer: String,
    /// Whether it loads now.
    pub on: bool,
}

/// The layers switched off, and a count that moves when they change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layers {
    /// The names of the layers left out.
    off: Vec<String>,
    /// Bumped by every change to [`Self::off`].
    generation: u64,
    /// Flips not yet written to the vault, oldest first, one per layer.
    writes: Vec<LayerWrite>,
}

impl Default for Layers {
    /// Every layer on but [`DEFAULT_OFF_LAYERS`].
    fn default() -> Self {
        Self {
            off: DEFAULT_OFF_LAYERS.iter().map(|&l| l.to_owned()).collect(),
            generation: 0,
            writes: Vec::new(),
        }
    }
}

impl Layers {
    /// Whether `layer` is loaded.
    pub fn is_on(&self, layer: &str) -> bool {
        !self.off.iter().any(|off| off == layer)
    }

    /// Switch `layer` on if it is off, off if it is on, and queue the flip
    /// for the vault.
    pub fn toggle(&mut self, layer: &str) {
        let on = match self.off.iter().position(|off| off == layer) {
            Some(at) => {
                self.off.remove(at);
                true
            }
            None => {
                self.off.push(layer.to_owned());
                false
            }
        };
        self.generation += 1;
        self.writes.retain(|w| w.layer != layer);
        self.writes.push(LayerWrite {
            layer: layer.to_owned(),
            on,
        });
    }

    /// Take the flips not yet written to the vault.
    pub fn take_writes(&mut self) -> Vec<LayerWrite> {
        std::mem::take(&mut self.writes)
    }

    /// Switch the layers as the vault says: [`DEFAULT_OFF_LAYERS`] off, then
    /// each of `switches` on or off. The generation moves only if that
    /// changes what loads.
    pub fn apply<'a>(&mut self, switches: impl Iterator<Item = LayerSwitch<'a>>) {
        let mut off: Vec<String> = DEFAULT_OFF_LAYERS.iter().map(|&l| l.to_owned()).collect();
        for switch in switches {
            off.retain(|l| l != switch.layer);
            if !switch.on {
                off.push(switch.layer.to_owned());
            }
        }
        off.sort_unstable();
        let mut current = self.off.clone();
        current.sort_unstable();
        if off != current {
            self.off = off;
            self.generation += 1;
        }
    }

    /// The filter to load the books with: every layer but those off.
    pub fn filter(&self) -> LayerFilter {
        if self.off.is_empty() {
            return LayerFilter::All;
        }
        LayerFilter::Except(self.off.clone())
    }

    /// Moves whenever a layer is switched, so a loader knows to reload.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Every layer `books`' journal has a segment in, loaded or filtered out,
/// sorted: the layers there are to switch.
pub fn known(books: &Books) -> Vec<String> {
    let ledger = books.ledger();
    let mut layers: Vec<String> = ledger
        .segments()
        .iter()
        .chain(ledger.excluded())
        .map(|segment| segment.header.layer.clone())
        .collect();
    layers.sort_unstable();
    layers.dedup();
    layers
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Switching a layer off leaves it out of the filter, moves the
    /// generation and queues the flip; switching it back on restores every
    /// layer and leaves one write for it.
    #[test]
    fn toggling_a_layer_changes_the_filter() {
        let mut layers = Layers {
            off: Vec::new(),
            ..Layers::default()
        };
        assert!(layers.is_on("bank"));
        assert_eq!(layers.filter(), LayerFilter::All);

        layers.toggle("bank");
        assert!(!layers.is_on("bank"));
        assert_eq!(
            layers.filter(),
            LayerFilter::Except(vec!["bank".to_owned()])
        );
        assert_eq!(layers.generation(), 1);

        layers.toggle("bank");
        assert_eq!(layers.filter(), LayerFilter::All);
        assert_eq!(layers.generation(), 2);
        assert_eq!(
            layers.take_writes(),
            [LayerWrite {
                layer: "bank".to_owned(),
                on: true
            }]
        );
        assert!(layers.take_writes().is_empty());
    }

    /// `cln-chain` starts off; the vault's switches override the default,
    /// and applying what is already in effect leaves the generation alone.
    #[test]
    fn the_vault_switches_layers() {
        let mut layers = Layers::default();
        assert!(!layers.is_on("cln-chain"));
        assert!(layers.is_on("amex"));

        let switches = [
            LayerSwitch {
                layer: "cln-chain",
                on: true,
            },
            LayerSwitch {
                layer: "amex",
                on: false,
            },
        ];
        layers.apply(switches.into_iter());
        assert!(layers.is_on("cln-chain"));
        assert!(!layers.is_on("amex"));
        assert_eq!(layers.generation(), 1);

        layers.apply(switches.into_iter());
        assert_eq!(layers.generation(), 1, "nothing changed");
        assert!(layers.take_writes().is_empty(), "the vault's own, not ours");
    }
}

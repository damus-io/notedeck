//! Which journal layers the books are loaded with.
//!
//! Every segment of the journal belongs to a layer (`bank`, `cln`,
//! `cln-chain`, …), and a reader can leave whole layers out: on-chain BTC
//! that overlaps another importer's view, say. [`Layers`] is the app's
//! switch for that: the Overview's layer chips flip it, and whoever loads
//! the books reloads them with [`Layers::filter`] when
//! [`Layers::generation`] moves. It holds only what is switched off, so a
//! layer that arrives later is on.

use ledgerdb::LayerFilter;

/// The layers switched off, and a count that moves when they change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layers {
    /// The names of the layers left out.
    off: Vec<String>,
    /// Bumped by every change to [`Self::off`].
    generation: u64,
}

impl Layers {
    /// Whether `layer` is loaded.
    pub fn is_on(&self, layer: &str) -> bool {
        !self.off.iter().any(|off| off == layer)
    }

    /// Switch `layer` on if it is off, off if it is on.
    pub fn toggle(&mut self, layer: &str) {
        match self.off.iter().position(|off| off == layer) {
            Some(at) => {
                self.off.remove(at);
            }
            None => self.off.push(layer.to_owned()),
        }
        self.generation += 1;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Switching a layer off leaves it out of the filter and moves the
    /// generation; switching it back on restores every layer.
    #[test]
    fn toggling_a_layer_changes_the_filter() {
        let mut layers = Layers::default();
        assert!(layers.is_on("cln-chain"));
        assert_eq!(layers.filter(), LayerFilter::All);

        layers.toggle("cln-chain");
        assert!(!layers.is_on("cln-chain"));
        assert!(layers.is_on("bank"));
        assert_eq!(
            layers.filter(),
            LayerFilter::Except(vec!["cln-chain".to_owned()])
        );
        assert_eq!(layers.generation(), 1);

        layers.toggle("cln-chain");
        assert_eq!(layers.filter(), LayerFilter::All);
        assert_eq!(layers.generation(), 2);
    }
}

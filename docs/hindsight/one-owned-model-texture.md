# One owned model texture on Renderer3d

Drawing a model in egui is insert, make a view, render, register the texture,
then free both under the scene lock on drop; each app writes that by hand.

- headway:cashflow/firm-tuition-better — ~6 min: copied `review_models.rs`'s insert → view → render → register → `Drop` into `CoinView` in `notedeck_cashflow/src/ui/hud/coin.rs`; a `notedeck::ModelTexture { view, texture }` with `turn`/`Drop` would have been a one-liner

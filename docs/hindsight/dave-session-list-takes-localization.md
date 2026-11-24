# Dave's session list and scene toolbar take a Localization handle

`SessionListUi` and `scene_toolbar_ui` get no `Localization`, so their strings stay
English and a card asked to `tr!` one has to plumb i18n through or skip it.

- headway:ux-feedback/search-slam-another — the "+ New" label and "New session (Ctrl+T)" tooltip shipped as plain &str against the card's `tr!` ask, since neither widget can reach `app_ctx.i18n`

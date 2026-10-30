# Register rows say their text to accesskit

The register paints rows as bare galleys, so a kittest test sees only the count
label; which rows a filter keeps has to be asserted on the model instead.

- headway:cashflow/scorpion-business-prefer — the Projects "No project" and the Categories payee tests could assert only the count label; which rows matched had to move to `model::matching` tests, and a check that routing stays out was deleted from the UI test

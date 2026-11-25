# cashflow import takes a clock

A segment carries its `imported` second (`import::now()`) and no test can pin it,
so a re-import expecting the same hash passes only inside one wall-clock second.

- headway:cashflow/service-cousin-primary — `manifests::two_hosts_see_whats_missing_where_and_retirements_propagate` failed about half its runs at manifests.rs:172 (re-import gave a new hash), costing three gate reruns and a hunt through my secret-store change before the clock turned out to be the cause
- headway:cashflow/roof-table-labor — the same manifests.rs:172 assert failed 1 of 5 runs of the async-importer gate; this file named the clock at once, so it cost one rerun instead of a hunt through the importer change
- headway:notedeck/basket-say-force — the same manifests.rs:172 assert failed Linux CI and 2 of 15 local runs; with no clock to pin, the test now expects whatever hash the re-import returns instead of a1's

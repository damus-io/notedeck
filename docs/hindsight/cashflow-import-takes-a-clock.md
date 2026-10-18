# cashflow import takes a clock

A segment carries its `imported` second (`import::now()`) and no test can pin it,
so a re-import expecting the same hash passes only inside one wall-clock second.

- headway:cashflow/service-cousin-primary — `manifests::two_hosts_see_whats_missing_where_and_retirements_propagate` failed about half its runs at manifests.rs:172 (re-import gave a new hash), costing three gate reruns and a hunt through my secret-store change before the clock turned out to be the cause

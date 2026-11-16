# Bulk ingest into nostrdb without silent drops

`Ndb::process_client_event` queues without backpressure, so a re-ingest loop
loses notes with every call `Ok` and has to throttle on a subscription.

- headway:notedeck/cement-grass-nice — re-ingesting 705k kind-1988 notes stored 459k with every call `Ok`; the rerun throttled on a subscription's arrivals, a second ingest pass and ~10 min

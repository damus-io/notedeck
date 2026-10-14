# A per-board channel sync report that prints no secrets

selfshare_doctor prints team roots, so finding why a board is missing elsewhere
means scripting flush markers and envelope counts around it to hide the secrets.

- headway:headway/primary-knife-eyebrow — ~6 min: awk over selfshare_doctor to drop roots and match them against the two flush markers, then a per-pubkey `ndb` count loop to find the one channel past 5000 envelopes

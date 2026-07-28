# NIP-SNS

## Shared Note Storage

`draft` `optional`

> **Working name.** "Shared Note Storage" (SNS) is provisional — it's the
> shared, sealed sibling of [NIP-PNS](./nip-pns.md). Rename freely.

## Abstract

Shared Note Storage (SNS) is a private, multi-writer channel on nostr. It sits
**between [NIP-PNS][pns] (Private Note Storage) and [NIP-59][nip59] (Gift Wrap)**:

- like **PNS**, events are published under a single key *derived from one shared
  secret* — no per-event ephemeral keys, no `p` tag, and the channel is queried
  by that derived pubkey;
- like a **gift wrap**, the encrypted payload is a **seal** (kind `13`,
  author-signed), so even though many people hold the channel key, every note
  carries *verifiable authorship* by the real member who wrote it.

The shared secret (`team_root`) is handed to new members by gift-wrapping a small
**key-share** event to them. Removing a member is a key rotation: mint a new
`team_root` and re-share it with everyone except the removed member.

The motivating use case is a **shared team board** (Headway): a private kanban any
team member can edit, where each edit is attributable to the member who made it.

## Comparison

|                      | PNS (1080)                       | Gift Wrap (1059)                  | **SNS (this spec)**                       |
| -------------------- | -------------------------------- | --------------------------------- | ----------------------------------------- |
| Outer key            | derived from owner's device key  | ephemeral, per event              | derived from a shared `team_root`         |
| Addressed to         | nobody (owner queries own pubkey)| one recipient via `p` tag         | nobody — members query the team pubkey    |
| Who can publish      | only the owner                   | anyone, to the recipient          | anyone holding `team_root`                |
| Inner authenticity   | implied by the outer key         | **seal (kind 13), author-signed** | **seal (kind 13), author-signed**         |
| Inner broadcastable? | no (rumor, unsigned)             | no (rumor, unsigned)              | no (rumor, unsigned)                      |

The seal is the load-bearing addition over PNS: once a key is *shared*, knowledge
of the outer key no longer implies who authored a note, so authorship must be
proven inside. The shared-derived outer key is the addition over a gift wrap:
there's no ephemeral key or `p` tag, and the channel has a stable (but
secret-derived, hence unguessable) pubkey to subscribe to.

## Terminology

- `team_root` – 32-byte shared secret. The channel's root key. Distributed to
  members and rotated on removal.
- `team_keypair` – deterministic secp256k1 keypair derived from `team_root`. Its
  pubkey **is** the channel: members subscribe to events authored by it, and it
  signs the envelope.
- `team_nip44_key` – symmetric NIP-44 key derived from `team_root`, encrypting the
  envelope payload.
- `envelope` – the kind `1081` event published to relays (the outer wrapper).
- `seal` – a NIP-59 kind `13` event, signed by the authoring member, whose content
  is the encrypted rumor.
- `rumor` – the unsigned inner nostr event: the actual content (e.g. a board
  action). Its `pubkey` is the authoring member's real pubkey.
- `member_key` – a member's own long-term nostr secret key (their real identity).
- `nip44_encrypt/decrypt` – NIP-44 v2 authenticated encryption.

## Key derivation

Identical to PNS, but seeded from the shared `team_root` rather than a device key:

```
team_keypair   = derive_secp256k1_keypair(team_root)
team_nip44_key = hkdf_extract(ikm=team_root, salt="nip44-v2")
```

`team_root` itself is a random 32-byte secret (not derived from any member's key),
so that membership can rotate without any member having to change identity.

## Event kinds

| Kind     | Role                          | Notes                            |
| -------- | ----------------------------- | -------------------------------- |
| **1081** | SNS envelope (outer wrapper)  | provisional                      |
| **13**   | Seal                          | reused verbatim from [NIP-59][nip59] |
| **1082** | Key-share (rumor)             | provisional; delivered gift-wrapped |

> Kinds `1081` / `1082` are **provisional** and will be coordinated before any
> non-draft status.

## Event structure

### Envelope — kind `1081`

```jsonc
{
  "kind": 1081,
  "pubkey": "<team_keypair pubkey>",
  "content": "<base64( nip44(team_nip44_key, seal_json) )>",
  "tags": [],                       // no `p` tag, no addressing
  // signed by team_keypair
}
```

The envelope carries **no** routing metadata. Outsiders cannot compute the team
pubkey (it's derived from the secret `team_root`), so the channel pubkey is itself
unguessable; only keyholders know which author to subscribe to.

### Seal — kind `13` (NIP-59)

```jsonc
{
  "kind": 13,
  "pubkey": "<authoring member pubkey>",
  "content": "<nip44(member_key ⇄ team_keypair.pubkey, rumor_json)>",
  "tags": [],
  // signed by member_key
}
```

The seal is an ordinary NIP-59 seal whose **recipient is the team keypair**. That
makes it decryptable by anyone holding `team_root` (via ECDH against the seal's
`pubkey`) and verifiable as authored by that member.

### Rumor (inner note)

Any unsigned nostr event whose `pubkey` equals the seal's `pubkey`. For a Headway
board this is the action event itself (card `1621`, placement `30620`, title/label
`1985`, description `1624`, board `30619`).

### Key-share — kind `1082` (delivered as a NIP-59 gift wrap)

To add a member, gift-wrap (kind `1059`, `p`-tagged to the new member's real
pubkey) a key-share rumor:

```jsonc
{
  "kind": 1082,
  "pubkey": "<sharer's pubkey>",
  "content": "",
  "tags": [
    ["team_root", "<bech32 or hex of the 32-byte secret>"],
    ["a", "30619:<board-author-hex>:<board-id>"],  // which board/channel this unlocks
    ["epoch", "2"]                                  // optional: rotation generation
  ]
}
```

Distribution and rotation reuse NIP-59 wholesale — no new transport. The recipient
unwraps the gift wrap (already supported by NIP-59 clients), reads `team_root`, and
registers it locally.

## Publishing workflow

```
rumor   = { ...action..., pubkey: member_pubkey /* unsigned */ }
seal    = kind 13 signed by member_key,
          content = nip44(member_key ⇄ team_keypair.pubkey, json(rumor))
envelope = kind 1081, pubkey = team_keypair.pubkey,
          content = base64(nip44(team_nip44_key, json(seal))),
          signed by team_keypair
relay.publish(envelope)
```

## Reading workflow

1. Subscribe to kind `1081` events authored by `team_keypair.pubkey` (for each
   `team_root` you hold — see *Rotation*).
2. For each envelope:
   - symmetric-decrypt `content` with `team_nip44_key` → `seal_json`;
   - parse the seal (kind `13`); **verify its signature** against `seal.pubkey`;
   - ECDH-decrypt the seal `content` (your `team_keypair` secret ⇄ `seal.pubkey`)
     → `rumor_json`;
   - require `rumor.pubkey == seal.pubkey`; discard otherwise.
3. The rumor is now an authenticated note from `seal.pubkey`. Store it **unsigned**
   (it has no signature, by construction) but attributed to its real author.

A consumer (e.g. the Headway reducer) sees only the inner rumors, with correct,
verified authors. It never sees the envelope or team pubkey — those are stripped
during unwrap.

## Membership & authority

**v1 — implicit membership.** Possession of `team_root` *is* membership: anyone who
can decrypt can also publish, and any keyholder may add another member by
gift-wrapping a key-share. The seal still records *who* did each action, which is
enough for an audit trail and for a consumer to attribute and display authorship.

**Later — explicit roster (TODO).** An admin-signed roster event (member pubkeys +
roles) lets a consumer enforce *whose* actions count (e.g. ignore a former
member's actions, or gate destructive ops to admins) independently of who can
decrypt. This is the natural upgrade for controlled add/remove and per-key
permissions ("approve/deny certain keys"). It is deliberately out of scope for v1.

Note that even with implicit membership, **no roster event is required to rotate**:
the set of current members is recoverable from the sharer's own sent key-share
gift wraps (their `p` tags name each recipient).

## Rotation

Rotation is how a member is *removed* (added members just receive a key-share):

1. Mint a fresh random `team_root'` (a new generation / `epoch`).
2. Gift-wrap a key-share of `team_root'` to every current member **except** the
   removed one.
3. From now on, publish envelopes under `team_keypair'` (derived from
   `team_root'`); its pubkey differs, so the channel effectively moves.

Properties:

- Remaining members trial-decrypt across **all** `team_root`s they hold, so they
  read the full history (old envelopes under the old pubkey, new under the new) and
  publish under the latest.
- The removed member keeps the old root: it can still read history up to the
  rotation, but cannot decrypt or publish the new channel. This is a clean forward
  cut (no post-compromise security beyond the rotation boundary — the expected bar
  for a shared symmetric channel).
- There is **no** `team_root`-in-board-state coupling: board identity and all `a`
  tags are anchored to members' *real* pubkeys (inside rumors), never to the
  rotating team pubkey. Rotation is invisible to the reduced board.

## nostrdb integration

nostrdb already auto-unwraps PNS (`1080`) and gift wraps (`1059`), ingesting the
inner note unsigned. SNS adds one capability — **seal support in the shared
envelope path**:

- register a `team_root`; ndb derives `team_keypair` + `team_nip44_key` and matches
  kind `1081` events from that pubkey;
- symmetric-decrypt the envelope, then **peel the kind-13 seal** (verify sig,
  ECDH-decrypt) exactly as in the gift-wrap path, with `team_keypair` as recipient;
- ingest the rumor unsigned, attributed to the verified `seal.pubkey`.

Multiple registered `team_root`s (post-rotation) are matched independently — this
is the trial-decrypt behaviour, realised as multiple matched pubkeys rather than a
linear scan.

## Security considerations

- **`team_root` compromise exposes the whole channel** (read and write) until the
  next rotation. Rotate on any suspected leak, not only on member removal.
- **No post-compromise security.** A removed/leaked member retains read access to
  all pre-rotation history. SNS protects *future* events only.
- **Authorship is cryptographic, confidentiality is shared.** The seal's signature
  proves which member authored a rumor even though every member can decrypt it.
  Without the seal (i.e. plain shared PNS) any keyholder could forge any author.
- **Unguessable channel pubkey.** `team_keypair.pubkey` is derived from the secret
  `team_root`, so the channel identifier is not enumerable by outsiders; only
  ciphertext and coarse timestamps are visible on relays.
- **Same key signs every envelope.** All members sign envelopes with
  `team_keypair`, so the envelope signature attests "a keyholder," not which one —
  by design. Per-member attribution lives in the seal.

## References

- [NIP-PNS][pns]: Private Note Storage — the single-owner shared-key scheme SNS
  generalises to multiple holders.
- [NIP-59][nip59]: Gift Wrap — the seal/rumor layering SNS reuses for authorship,
  and the transport for key-shares.
- [NIP-44][nip44]: Encrypted payloads v2.
- [NIP-17][nip17]: Private DMs, the canonical gift-wrap application (key-shares are
  a sibling application of the same wrap).

[pns]: ./nip-pns.md
[nip44]: https://github.com/nostr-protocol/nips/blob/master/44.md
[nip59]: https://github.com/nostr-protocol/nips/blob/master/59.md
[nip17]: https://github.com/nostr-protocol/nips/blob/master/17.md

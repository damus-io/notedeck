# headway_cli threads one resolved identity (me, owner) instead of loose pubkeys

Card: headway:headway/transfer-force-speed
signer and owner travel as bare `&Pubkey`s named `author`, so they are easy to
swap and every arm re-derives the owner check its own way.

- headway:headway/vacuum-priority-ordinary — the owner/signer conflation went unnoticed because every function took "a pubkey called author"
- headway:headway/place-wheel-web — author, roster and me are three loose locals that must agree; Roster bakes in the owner, so two had to be rebound together
- headway:headway/color-urban-kick — each arm redoes load_board + roster.channel/team_root + secret by hand, and the owner check now has two shapes

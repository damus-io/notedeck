# One GUI card lookup that says where the card sits

The GUI finds cards with live-only `find_card` and `view.card`, or with
`card_with_column_in_board`; each caller picks whether archived cards count.

- headway:headway/album-trouble-treat — making archived cards openable meant auditing ~30 `find_card` call sites (detail, keys, review, graph, queue, nav prune) one by one, and adding `find_detail_card` beside it; a `find_card` returning a placement enum would have made each site's choice show in its type

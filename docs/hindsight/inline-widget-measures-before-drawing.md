# An inline reference widget that says how wide it is before it draws

`inline_chip` breaks the row on its own text's width, so a chip whose body is
another renderer's widget breaks early or truncates instead of breaking.

- headway:dave/fringe-cup-pigeon — ~30 min: prototyped a headway card chip nested in the agentium chip, snapshotted both width failures, and backed it out for want of a measure

# kittest hover fails loudly on a widget outside the window

`node.hover()` aims at a node centered outside the harness window, so the hover
never lands and the test sees only "no tooltip", not that the widget was off-screen.

- headway:cashflow/describe-wheel-culture — ~6 min: the XP bar's tooltip test saw nothing; debug prints of the node rect and `contains_pointer` showed the bar at y=563 in a 560px window, pushed down by a too-wide tooltip row

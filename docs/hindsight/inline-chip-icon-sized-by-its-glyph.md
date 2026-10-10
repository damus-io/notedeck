# An inline chip whose icon slot is as wide as the glyph it draws

inline_chip measures its icon as a square one text row high, so a narrower glyph
(a 3px status bar) makes the chip break the row early by the difference.

- headway:dave/lady-comfort-surround — the agentium chip's status bar allocates 3px of a ~12px slot; documented the early break in session_glyph instead of threading an icon width through inline_chip/inline_chip_with and their six callers

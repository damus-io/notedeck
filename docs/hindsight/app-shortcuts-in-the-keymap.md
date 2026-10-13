# Every app shortcut goes through the chrome keymap

Dave reads its Ctrl keys off egui input, outside the keymap (ui/keybindings.rs).
A chrome chord can't see what it shadows; finding a clash means grepping Key::.

- headway:notedeck/fury-excuse-accident — ~4 min grepping for Ctrl-K users; Cmd-K for the command bar shadows Dave's Ctrl-K (previous agent) off macOS, and nothing in the keymap could have said so

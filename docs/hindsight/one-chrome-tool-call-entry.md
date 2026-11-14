# One chrome entry point that calls a tool, data or UI

The socket, a key, a macro step and the command bar each call a data tool apart,
so a rule for calling one (start its app, sign) is added at every call site.

- headway:notedeck/few-domain-coach — starting a stopped app's owner had to go into ipc call_tool, dispatch_keys and call_macro_step separately; the command bar's tool_action got no change only because the bar lists running apps' tools alone

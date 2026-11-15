# A secret argument kind for nd

nd can read an argument from stdin only through a `--<x>-file -` flag, with
echo on, so a tool taking a token loses the hidden prompt a CLI could give it.

- headway:notedeck/equal-question-sail — `agentium profile secret set` read its token from stdin by default with terminal echo off (`EchoOff` in agentium_cli); `agentium_profile_secret_set` takes `--value-file -` instead, typed visibly at a tty, because no `x-nd-kind: secret` exists to prompt hidden and refuse a value on the command line

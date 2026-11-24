# notedeck_headway's lib tests are red on the tip

`headway_tools_are_the_notedeck_toolset` fails in `Catalog::build` before any
change, so each card has to show the red test isn't theirs.

- headway:headway/friend-raven-pond — traced UnknownApp into agentium-core's catalog to show the failure was outside the diff, instead of a green -p notedeck_headway run

# Headway's UI tools are one enum, as Dave's are

a Headway UI tool is a name const, an inline spec and a `parse` arm, so a new
per-tool fact is one edit per tool; Dave's `DaveUiTool` takes it as one match.

- headway:notedeck/indoor-vital-glad — titles for 33 Headway UI tools went in as 33 scattered `.titled()` calls, inserted by a throwaway script, where Dave's 17 took one `title()` match

# Mac Clamshell MCP

- Operate normally when no active `[mac-clamshell]` throttled notice is present (or after `profile=normal` is reported).
- When a `[mac-clamshell]` notice reports an active throttled profile (`utility`, `eco`, or `strict`), load the `mac-clamshell` skill and follow its `mac-clamshell-mcp run` wrapping and concurrency-cap rules until `profile=normal` resumes.

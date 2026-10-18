# Crash reports

When Postvak crashes, it writes a report to its crash folder: `~/Library/Application Support/postvak/crashes/` on macOS, `~/.local/state/postvak/crashes/` on Linux, or `crashes/` under `state/` with `POSTVAK_HOME` set. Each report is named after the time and the process that crashed: the mail window (`gui`), the background sync (`daemon`), the MCP server (`mcp`) or another command (`cli`). The newest 20 are kept.

Reports are never sent anywhere. To report a bug, attach one to an [issue](https://github.com/postvak-app/postvak/issues).

```sh
postvak crashes                  # the reports, newest first
postvak crashes show <id>        # print one
```

When the mail window cannot start and the crash folder holds reports, the window says where it is.

## What a report holds

The version, the operating system, the process, the thread, where in the code it crashed and how it got there (the backtrace), and the kind of the last thing the window or the daemon was asked to do, such as `SearchServer` or `MoveTo`, without what it was asked to do it with.

A report never holds your mail: no bodies, subjects, addresses, folder names or passwords. The crash's own message can quote any of those, so it is left out unless it is fixed text in Postvak's code.

# Command line (`opengent`)

`opengent` runs OpenGent without a window: list your models and MCP connections, run a prompt
or an agent and print the reply, and manage agent schedules. It is a separate binary built on
the same runtime as the desktop app, and it uses the same data folder (`~/.opengent`), provider
connections, MCP connections, tool permissions and keychain entries — with the app open or
closed.

Providers, connections, agents and permissions are set up in the desktop app. The command
line uses them and never changes them.

- [Install](#install)
- [Quick start](#quick-start)
- [Commands](#commands)
- [Running prompts](#running-prompts)
- [Tools and permissions](#tools-and-permissions)
- [Scripting](#scripting)
- [Schedules](#schedules)
- [Environment variables](#environment-variables)
- [Using it alongside the app](#using-it-alongside-the-app)
- [Troubleshooting](#troubleshooting)

For how it works under the hood and its limits, see the
[feature guide](features.md#command-line-opengent); for its security model, see
[SECURITY.md](../SECURITY.md).

## Install

The command line isn't in the release downloads yet
([#65](https://github.com/samkerrcode/opengent/issues/65)), so it is built from source. That
needs only Rust — install [`rustup`](https://rustup.rs), which picks up the version pinned in
`rust-toolchain.toml` — plus the Xcode Command Line Tools on macOS or the MSVC Build Tools on
Windows. Node and the Tauri dependencies are not needed.

```sh
git clone https://github.com/samkerrcode/opengent.git
cd opengent
cargo install --path crates/cli --locked
```

This builds an optimized binary and copies it to Cargo's bin directory (`~/.cargo/bin`, or
`%USERPROFILE%\.cargo\bin` on Windows), which `rustup` already put on your `PATH`. Check it:

```sh
opengent version
```

To build without installing, use `cargo build -p opengent-cli --release`; the binary is
`target/release/opengent` (`opengent.exe` on Windows) and can be copied anywhere on your
`PATH`.

**Update** by pulling and running the install command again. Keep the command line and the
desktop app on the same version: after an update changes the database format, an older
binary refuses to open the newer data.

**Uninstall** with `cargo uninstall opengent-cli`. Your data folder is left alone.

**macOS keychain prompt.** The first time a newly built binary reads a stored API key, macOS
asks whether it may use the keychain item. The binary is unsigned, so the prompt returns
after each rebuild; *Always Allow* lasts until then.

## Quick start

```sh
opengent models                                   # what can I run?
opengent run --model simulated "hello"            # no account needed
opengent run --model "Google Gemini/gemini-2.5-flash" "Explain WAL mode in two sentences"
opengent run --chat <id> "And in one?"            # continue; the id was printed on stderr
opengent run --agent "Release notes" "What changed this week?"
```

Without `--model`, a new chat uses the model last picked in the app (marked *active* in
`opengent models`).

## Commands

| Command | Does |
|---|---|
| `opengent models [--json]` | Lists providers and their models. *default* is the provider's default model; *active* is the one last picked in the app |
| `opengent mcp list [--tools] [--json]` | Lists MCP connections: name, id, transport, on/off, default permission. `--tools` adds each server's last known tools and their permission |
| `opengent run [options] <prompt>` | Runs one prompt and prints the reply |
| `opengent schedule add --agent <name> (--at <time> \| --every <interval>) [--skip-missed] <prompt>` | Adds an agent schedule and prints its id |
| `opengent schedule list [--agent <name>] [--json]` | Lists schedules |
| `opengent schedule remove <id>` | Deletes a schedule |
| `opengent help`, `--help`, `-h` | Usage. Also works after any command (`opengent run --help`) |
| `opengent version`, `--version`, `-V` | The version |

Options are written `--name value` or `--name=value`. Everything after a bare `--` is taken
literally, which is how to pass a prompt that starts with a dash:
`opengent run -- "--verbose means what?"`.

`models` and `mcp list` only read the configuration files. They never open the database,
connect to anything or touch the keychain, and they never print secret values.

**Exit codes**

| Code | Meaning |
|---|---|
| `0` | The command succeeded; for `run`, the reply was stored complete |
| `1` | The run or command failed (the reason is on stderr) |
| `2` | Usage error: bad arguments, or something named that doesn't exist |
| `3` | Cancelled: Ctrl-C, a termination signal, or the output was closed |
| `130` | A second Ctrl-C quit without waiting for the cancellation to be saved |

## Running prompts

`run` sends one prompt, streams the reply and exits. **The reply goes to stdout; everything
else goes to stderr** — the new chat's id, tool activity, warnings and errors — so
`opengent run … > reply.md` captures only the reply.

| Option | Does |
|---|---|
| `--model <provider>/<model>` | The provider's name (case-insensitive) or id, then the model. `--model <provider>` alone uses its default model. Model ids may contain slashes |
| `--agent <name>` | Runs with an agent — its models, instructions, servers, tools and permissions. Name (case-insensitive) or id |
| `--chat <id>` | Continues a chat instead of starting one |
| `--mcp <server>` | Only these connections are on for the chat (repeatable; name or id) |
| `--tool <server>/<tool>` | Only these tools of that server (repeatable) |
| `--no-mcp` | No connections for this chat |
| `--json` | Prints the run's events as JSON lines instead of the reply text |

**The prompt** is the rest of the command line; several words are joined with spaces, so
quoting is only needed to protect characters from your shell. If there is no prompt argument,
the prompt is read from piped stdin:

```sh
git diff --staged | opengent run --model "Mac Local"
```

A prompt argument and piped input are not combined; when both are present stdin is ignored.
To put a file under an instruction, build one prompt:

```sh
opengent run "Write a commit message for this diff:

$(git diff --staged)"
```

A prompt can be up to 65,536 characters. Files can't be attached from the command line yet.

**Chats.** Every run belongs to a chat. A new one is created unless `--chat` is given, and
its id is printed on stderr (`chat <id> — continue it with …`). Chats made here are ordinary
chats: they appear in the app's sidebar, and a chat started in the app can be continued here.
A continued chat keeps its model unless `--model` says otherwise, and keeps its agent — an
agent's chat can only be continued with that agent.

**Stopping.** Ctrl-C cancels the run, waits up to ten seconds for the cancellation to be
saved, and exits with `3`; what was generated so far stays in the chat, marked cancelled.
A second Ctrl-C quits at once. SIGTERM and SIGHUP (what `timeout`, a service manager or a
closing terminal send) are handled like the first Ctrl-C, as is Ctrl-Break on Windows.
Closing the output (`opengent run … | head -3`) cancels the run too.

## Tools and permissions

The command line has no permission settings of its own. It applies what the app stored:

| The tool is set to | On a terminal | Piped, scripted or `--json` |
|---|---|---|
| **Allow** | Runs | Runs |
| **Ask** | You are asked: `y` allows this one call, anything else denies it | Denied at once, and named on stderr |
| **Disable** | Never offered to the model | Never offered to the model |

"On a terminal" means stdin and stderr are both terminals and `--json` isn't used. A prompt
shows the server, the tool and its arguments with control characters removed; if the
arguments are too long to show in full, it says how much is hidden. A prompt left unanswered
is denied after 15 minutes.

Two rules keep an unattended run from doing more than you approved:

- **There is no "always allow" here.** To let a tool run without asking, set it to *Allow*
  in the app (Connections, a chat's Tools menu, or the agent).
- **Allow covers the tool you reviewed.** If a server has changed a tool's definition since
  you allowed it in the app, or the app has never listed that tool, it asks instead — which
  in a script means it is denied — and a note tells you to review it under Connections.

**Choosing servers and tools.** A new chat uses every connection that is on, as in the app.
`--mcp`, `--tool` and `--no-mcp` narrow that for the chat, like its ⚒ Tools menu:

```sh
opengent run --mcp yfinance "What did NVDA close at?"
opengent run --tool yfinance/get_stock_info --tool yfinance/get_recommendations "Is AAPL a buy?"
opengent run --no-mcp "Just answer from what you know"
```

The selection is saved on the chat, so the app shows what the run used and a later
`--chat` run keeps it. These options only narrow: they never set Allow or Ask, and they
can't use a connection that is off or a tool that is disabled in Connections. Agent runs use
the agent's own servers and tools, so they don't take these options.

Only the servers a run can use are started, and they get 30 seconds to connect. One that
isn't ready is left out with a warning on stderr and the run goes on without its tools.

## Scripting

**Plain text.** Stdout is exactly the reply. Check the exit code rather than parsing stderr:

```sh
if summary=$(opengent run --no-mcp --model "Mac Local" "Summarize: $(cat notes.txt)"); then
  printf '%s\n' "$summary" > summary.md
fi
```

When stdout isn't a terminal the reply is written as the model produced it. On a terminal,
control characters and escape sequences are removed from it first.

**JSON events.** `--json` prints one event per line, in order, as they happen — the same
events the desktop app receives:

```json
{"schema":2,"chat_id":"dda3f0f9-…","run_id":"664867e4-…","seq":0,"payload":{"type":"phase","phase":"queued"}}
{"schema":2,"chat_id":"dda3f0f9-…","run_id":"664867e4-…","seq":47,"payload":{"type":"text_delta","text":"Hello"}}
{"schema":2,"chat_id":"dda3f0f9-…","run_id":"664867e4-…","seq":53,"payload":{"type":"completed","stop_reason":"end_turn"}}
```

`seq` counts up from 0 without gaps. `schema` changes if the shape of an event changes
incompatibly. `payload.type` is one of:

| Type | Fields | Meaning |
|---|---|---|
| `phase` | `phase` | `queued`, `preparing`, `generating`, `awaiting_approval`, `calling_tools`, `completed`, `cancelled`, `failed` |
| `text_delta` | `text` | A piece of the reply |
| `thinking_delta` | `text` | A piece of the model's reasoning, for models that expose it (not shown in plain-text mode) |
| `usage` | `usage` | Token counts reported by the provider |
| `notice` | `message` | Something worth knowing about how the run was prepared, e.g. trimmed history |
| `model_switched` | `provider_id`, `model`, `reason` | An agent fell back to its second model |
| `tool_call` | `call` | The model asked for a tool: id, server, tool, arguments |
| `tool_status` | `call_id`, `status` | The call is `pending_approval` or running |
| `tool_result` | `call_id`, `content`, `is_error`, `status` | How the call ended |
| `completed` | `stop_reason` | Last event of a finished run |
| `cancelled` | — | Last event of a cancelled run |
| `failed` | `message` | Last event of a failed run |

Exactly one of the last three ends every stream. With [`jq`](https://jqlang.org):

```sh
# The reply text
opengent run --json "hi" | jq -rj 'select(.payload.type == "text_delta") | .payload.text'

# The chat id, to continue the conversation
chat=$(opengent run --json "Pick a number" | jq -rs '.[0].chat_id')
opengent run --chat "$chat" "Double it"
```

Read the stream to its end: a consumer that stops early (`head -1`) closes the pipe, which
cancels the run.

`models --json`, `mcp list --json` and `schedule list --json` print a single JSON array.

**Unattended runs.** In a script nobody can answer, so *Ask* tools are denied and the run
continues without them. Set the tools a job needs to *Allow* in the app beforehand, or give
the job an agent with exactly those permissions. An agent run without a terminal is told it
is unattended and finishes on its own instead of asking you questions.

## Schedules

Schedules belong to agents. They are stored for the desktop app, which fires them **while it
is open** — the command line adds, lists and removes them but does not run them.

```sh
opengent schedule add --agent "Market brief" --every weekdays@08:30 "Summarize overnight moves"
opengent schedule add --agent "Market brief" --at "2026-12-01 09:00" "Month-start review"
opengent schedule list
opengent schedule remove <id>
```

| Option | Accepts |
|---|---|
| `--at` | `"YYYY-MM-DD HH:MM"` in local time, or `HH:MM` for the next time the clock shows it. Runs once |
| `--every` | An interval — `30m`, `2h`, `1d` — or days and a local time: `daily@07:15`, `weekdays@09:00`, `weekends@10:00`, `mon,thu@18:30` |
| `--skip-missed` | If the app was closed when the schedule came due, skip that run. By default it runs once when the app next opens |

`schedule add` prints the new schedule's id on stdout and a summary with the next run time
on stderr.

## Environment variables

| Variable | Does |
|---|---|
| `OPENGENT_DATA_DIR` | Uses this folder instead of `~/.opengent`. The desktop app reads it too. Useful for trying things without touching your data: `OPENGENT_DATA_DIR=/tmp/og opengent run --model simulated hi` |
| `OPENGENT_LOG` | Log filter for diagnostics on stderr, e.g. `debug` or `agent_core=debug`. Without it only warnings are shown. API keys and tokens are never logged |

API keys are never read from the environment or the command line; they come from the OS
keychain, where the app stored them.

## Using it alongside the app

The command line and the app can run at the same time, and so can several command-line runs.

- **One reply per chat at a time.** Starting a run in a chat that is still generating — here
  or in the app — fails with a message instead of interleaving two replies.
- **The app's configuration is read once, at start.** A provider, connection or permission
  changed in the app applies to the next command, not to a run already in progress.
- **Nothing in the app's configuration is written.** `providers.json`, `mcp.json` and
  `settings.json` are never modified. App settings such as *Share environment with models*
  apply to command-line runs.
- **Chats are shared.** A chat made here is an ordinary chat in the app's sidebar, and one
  started in the app can be continued here with `--chat`.

## Troubleshooting

| Message or symptom | What to do |
|---|---|
| `choose a model with --model …` | No model was picked in the app yet. Pass `--model`; `opengent models` lists them |
| `a reply is still being generated in this chat` | Wait for it, or stop it in the app. If a process was killed mid-reply while another kept running, the chat stays busy until every `opengent` process and the app have exited and one starts again |
| `the database was created by a newer version` | Update the binary (see [Install](#install)) |
| `the database is in use by an older version` | The running app (or another `opengent`) is older than this binary. Close it, or update it |
| `denied <server>/<tool>: it needs approval and nobody can answer here` | The tool is set to *Ask* and the run isn't on a terminal. Set it to *Allow* in the app, or run it in a terminal without `--json` |
| `<tool> is set to Allow, but its definition changed or was never reviewed in the app` | Open Connections in the app and review the tool; the command line can't approve a changed definition |
| `warning: <server> isn't connected (…)` | The reason is in the parentheses. For `sign in to it in the app`, sign in there — the command line can't open a browser sign-in |
| `<server> is turned off in Connections` | `--mcp` and `--tool` can't turn a connection on; do that in the app |
| `warning: the OS credential store is unavailable` | The keychain can't be reached (for example in a session without a login keychain), so stored API keys can't be read |
| A keychain prompt on macOS | Expected for a newly built binary; see [Install](#install) |
| Nothing useful on stderr | Run again with `OPENGENT_LOG=debug` |

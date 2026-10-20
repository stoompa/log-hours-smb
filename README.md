# automate-smb

Terminal app for logging working hours in SMB HR from a plain-English phrase.

## Install

Needs [Rust](https://rustup.rs). Installs the `automate-smb` binary into `~/.cargo/bin`
(make sure it's on your `PATH`):

```
cargo install --git https://github.com/stoompa/log-hours-smb
```

Run the same command with `--force` to update. Google Chrome and the 1Password CLI
(`op`) must be installed separately.

```
automate-smb setup             # company tag, 1Password item, API key reference
automate-smb                   # week-calendar TUI (press ? for keys)
automate-smb parse <phrase>    # show how the AI reads a phrase (no SMB access)
automate-smb week [YYYY-MM-DD] # print entries already logged that week (read-only)
```

In the TUI, move with ←/→ (days) and [ ] (weeks); weeks load from SMB as you go.
`a` opens a time picker, `n` stages your normal day, `w` fills empty weekdays,
`/` lets Claude turn a phrase into blocks, and `s` reviews and submits new blocks.

Add `--headed` to watch the browser. Needs Google Chrome and the 1Password CLI (`op`)
with desktop-app integration.

## AI provider

Set in `~/.config/automate-smb/config.toml` (or via `setup`):

| `provider`   | API                      | key from                                   |
|--------------|--------------------------|--------------------------------------------|
| `claude`     | Anthropic Messages API   | `ANTHROPIC_API_KEY` or `api_key_ref`       |
| `openrouter` | openrouter.ai            | `OPENROUTER_API_KEY` or `api_key_ref`      |
| `openai`     | OpenAI or any compatible server via `base_url` | `OPENAI_API_KEY`, `api_key_ref`, or none for a local `base_url` |

```toml
provider = "openai"
model = "qwen3:8b"
base_url = "http://localhost:11434/v1"   # Ollama
```

`api_key_ref` is an `op://` reference read with 1Password. Replies are constrained
to a JSON schema; servers that reject `response_format` are retried with the schema
in the prompt.

Login and timesheet details: [docs/smb-notes.md](docs/smb-notes.md).

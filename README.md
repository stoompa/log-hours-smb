# log-hours-smb

Terminal app for logging working hours in SMB HR from a plain-English phrase.

## Install

Needs [Rust](https://rustup.rs). Installs the `log-hours-smb` binary into `~/.cargo/bin`:

```
cargo install --git https://github.com/stoompa/log-hours-smb
```

Run the same command with `--force` to update. Google Chrome and the 1Password CLI
(`op`) must be installed separately (see below).

If you then get `command not found: log-hours-smb`, `~/.cargo/bin` isn't on your `PATH`:

```
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.zshrc && source ~/.zshrc
```

## Prerequisites

### Google Chrome

Install [Google Chrome](https://www.google.com/chrome/) (or `brew install --cask google-chrome`).
It's found automatically in its standard location; otherwise point `CHROME` at the binary:

```
export CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
```

Each run uses a throwaway profile, so your normal Chrome profile and logins are untouched.

### 1Password CLI

1. Install the 1Password desktop app and the CLI:
   ```
   brew install --cask 1password 1password-cli
   ```
2. In the desktop app, open **Settings → Developer** and enable
   **Integrate with 1Password CLI**. (Turning on Touch ID / Apple Watch unlock under
   **Settings → Security** lets you approve CLI access with a fingerprint.)
3. Check it works — this should prompt for approval, then list your vaults:
   ```
   op vault list
   ```
4. Make sure your SMB login item has a **username**, **password** and a
   **one-time password** field (add the 2FA TOTP via *Edit → + → One-Time Password*,
   scanning the QR code SMB shows when setting up Google Authenticator). Verify with:
   ```
   op item get "<item name>" --otp
   ```

Then run `log-hours-smb setup` and enter that item's name (and vault, if the name
isn't unique).

```
log-hours-smb setup             # company tag, 1Password item, API key reference
log-hours-smb                   # week-calendar TUI (press ? for keys)
log-hours-smb parse <phrase>    # show how the AI reads a phrase (no SMB access)
log-hours-smb week [YYYY-MM-DD] # print entries already logged that week (read-only)
```

In the TUI, move with ←/→ (days) and [ ] (weeks); weeks load from SMB as you go.
`a` opens a time picker, `n` stages your normal day, `w` fills empty weekdays,
`/` lets Claude turn a phrase into blocks, and `s` reviews and submits new blocks.

Add `--headed` to watch the browser.

## AI provider

Set in `~/.config/log-hours-smb/config.toml` (or via `setup`):

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

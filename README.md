# log-hours-smb

Terminal app for logging working hours in SMB HR from a plain-English phrase.

## Install

The instructions below assume macOS (Homebrew, zsh); on Linux, install Chrome and the
1Password CLI with your package manager and use your own shell profile.

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

### AI API key

Save your Anthropic, OpenRouter or OpenAI key in 1Password, right-click the field and
choose **Copy Secret Reference**. Setup asks for that `op://…` reference, not the key
itself, and rejects anything else. Alternatively leave it blank and export the
provider's env var (see [AI provider](#ai-provider)). A local server such as Ollama
needs no key.

## Setup

Run `log-hours-smb setup` (it also runs automatically the first time you start the
app). It walks through four steps:

1. **SMB login**: your company tag and the 1Password item name (and vault, if the
   name isn't unique).
2. **Normal working day**: blocks like `8-1230 1300-1700`, used by `n`, `w` and the
   phrase "normal day".
3. **AI provider**: provider, model and API key reference.
4. **Check**: reads your 1Password item and sends a test phrase to the AI.

Run it again any time to change a setting; Enter keeps the current value.

## Usage

```
log-hours-smb                   # week-calendar TUI (press ? for keys)
log-hours-smb setup             # change settings (see above)
log-hours-smb parse <phrase>    # show how the AI reads a phrase (no SMB access)
log-hours-smb week [YYYY-MM-DD] # print entries already logged that week (read-only)
```

In the TUI, move with ←/→ (days) and [ ] (weeks); weeks load from SMB as you go.
`a` opens a time picker, `n` stages your normal day, `w` fills empty weekdays,
`/` lets the AI turn a phrase into blocks, and `s` reviews and submits new blocks.
Press `?` for the rest (edit, delete, copy/paste a day, reload…).

Submitting only adds blocks that are new and don't overlap anything already in SMB;
it never edits or deletes existing entries.

Add `--headed` to watch the browser.

## Configuration

Settings live in `~/.config/log-hours-smb/config.toml` (override the path with
`LOG_HOURS_SMB_CONFIG`). No passwords or keys are stored there.

```toml
company_tag = "acme"
op_item = "SMB HR"
op_vault = "Private"                  # optional
provider = "claude"
model = "claude-haiku-5-5"
api_key_ref = "op://Private/Anthropic/credential"
default_day = ["08:00-12:30", "13:00-17:00"]
```

### AI provider

| `provider`   | API                      | key from                                   |
|--------------|--------------------------|--------------------------------------------|
| `claude`     | Anthropic Messages API   | `ANTHROPIC_API_KEY` or `api_key_ref`       |
| `openrouter` | openrouter.ai            | `OPENROUTER_API_KEY` or `api_key_ref`      |
| `openai`     | OpenAI or any compatible server via `base_url` | `OPENAI_API_KEY`, `api_key_ref`, or none for a local `base_url` |

If the env var is set it wins over `api_key_ref`. For `claude` the model defaults to
`claude-haiku-5-5`; setup also accepts `haiku`, `sonnet`, `opus` or `fable`.

For a local server, point `base_url` at it:

```toml
provider = "openai"
model = "qwen3:8b"
base_url = "http://localhost:11434/v1"   # Ollama; LM Studio is http://localhost:1234/v1
```

Replies are constrained to a JSON schema; servers that reject `response_format` are
retried with the schema in the prompt.

## Disclaimer

This is an unofficial, personal tool. It is not affiliated with, endorsed by, or
supported by SMB HR or any employer. It is provided "as is", without warranty of any
kind, and you use it entirely at your own risk. The author accepts no responsibility
or liability for any loss, damage or consequence arising from its use, including
incorrect, missing or duplicate timesheet entries, account lockouts, exposed
credentials, or breaches of your employer's policies. You are solely responsible for
checking that every hour it logs is accurate and that using it is allowed where you work.

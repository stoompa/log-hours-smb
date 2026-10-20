# automate-smb

Terminal app for logging working hours in SMB HR from a plain-English phrase.

```
automate-smb setup             # company tag, 1Password item, API key reference
automate-smb                   # TUI: type "normal day today, left at 16:00", review, Enter to submit
automate-smb parse <phrase>    # show how Claude reads a phrase (no SMB access)
automate-smb week [YYYY-MM-DD] # print entries already logged that week (read-only)
```

Add `--headed` to watch the browser. Needs Google Chrome, the 1Password CLI (`op`)
with desktop-app integration, and an Anthropic API key (`ANTHROPIC_API_KEY` or an
`op://` reference in `~/.config/automate-smb/config.toml`).

Login and timesheet details: [docs/smb-notes.md](docs/smb-notes.md).

# easy-ssh

`easy-ssh` is a terminal project manager for SSH connections. Save a project
name, its connection details, and a remote working directory, then connect with:

```bash
ssh demo
```

Only the exact interactive form `ssh <project>` is intercepted. Commands such
as `ssh -v server`, `ssh server uptime`, `scp`, and `rsync` continue to use
OpenSSH normally.

## Install

Build and install the binary:

```bash
cargo install --path .
easy-ssh install
```

`easy-ssh install` detects Bash or Zsh from `$SHELL`. Reload the startup file it
reports after installation:

```bash
# Bash
source ~/.bashrc

# Zsh
source ~/.zshrc
```

Select a shell explicitly when detection is unavailable, or configure both
shells by repeating the option:

```bash
easy-ssh install --shell zsh
easy-ssh install --shell bash --shell zsh
```

Open the manager:

```bash
easy-ssh
```

The installer adds a marked shell function to the selected startup file, adds
`Include ~/.ssh/easy-ssh.conf` to `~/.ssh/config`, and creates backups before
changing existing files.

## CLI

Create a new SSH connection:

```bash
easy-ssh add demo \
  --host server.example.com \
  --user deploy \
  --port 2222 \
  --identity ~/.ssh/id_ed25519 \
  --path ~/projects/demo
```

Use a connection already defined in `~/.ssh/config`:

```bash
easy-ssh add demo --ssh-alias production --path ~/projects/demo
```

Other commands:

```bash
easy-ssh list
easy-ssh edit demo --path /srv/demo
easy-ssh edit demo --agent claude
easy-ssh connect demo
easy-ssh connect demo --terminal ghostty
easy-ssh connect demo --agent codex
easy-ssh remove demo
easy-ssh doctor
easy-ssh doctor --shell bash --shell zsh
```

Projects are stored in
`${XDG_CONFIG_HOME:-~/.config}/easy-ssh/projects.toml`. Passwords are never
stored; OpenSSH continues to handle keys, agents, and password prompts.

## Ghostty and agent resume

Press `s` in the TUI to choose Ghostty as the default terminal, or use the
`--terminal ghostty` override above. Ghostty mode uses the same plain SSH and
remote login-shell flow as a manual Ghostty connection.

Set a project's **Agent** in the TUI, or run `easy-ssh edit demo --agent
claude` or `--agent codex`. On the next connection, Easy SSH asks that agent to
resume its most recent conversation for the project folder. Closing Ghostty ends
the terminal process; the next connection starts a fresh agent process with its
saved conversation. Select **Shell** to always open only the normal remote
shell.

When adding or editing a project, Remote folder offers fuzzy directory matches
at the typed path's current level. Up/Down selects a match, Tab inserts it, and
Esc dismisses the list. Every text input supports Left/Right, Home/End,
Backspace, and Delete.

## Connection lifetime

For connections created with `--host`, easy-ssh configures OpenSSH to send a
server-alive probe every 30 seconds and disconnect after three unanswered
probes. This keeps healthy idle connections active and detects broken network
links in about 90 seconds. Projects that use `--ssh-alias` keep the settings
from the user's SSH config unchanged.

Keepalive does not reconnect a dropped session or preserve arbitrary programs
running in the remote shell. Claude and Codex projects resume their own saved
conversations when Easy SSH reconnects; other programs end with the terminal.

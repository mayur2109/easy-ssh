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
easy-ssh connect demo
easy-ssh remove demo
easy-ssh doctor
easy-ssh doctor --shell bash --shell zsh
```

Projects are stored in
`${XDG_CONFIG_HOME:-~/.config}/easy-ssh/projects.toml`. Passwords are never
stored; OpenSSH continues to handle keys, agents, and password prompts.

## Connection lifetime

For connections created with `--host`, easy-ssh configures OpenSSH to send a
server-alive probe every 30 seconds and disconnect after three unanswered
probes. This keeps healthy idle connections active and detects broken network
links in about 90 seconds. Projects that use `--ssh-alias` keep the settings
from the user's SSH config unchanged.

Keepalive does not reconnect a dropped session or preserve programs running in
the remote shell. Use a remote session manager such as `tmux` when work must
survive a disconnect.

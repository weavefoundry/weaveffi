# Shell Completions

`weaveffi completions <shell>` prints a completion script for bash, zsh, fish, powershell, or elvish. Generate one with:

```bash
weaveffi completions bash
```

Replace `bash` with your shell name. Install the script using the matching section below.

## bash

```bash
mkdir -p ~/.local/share/bash-completion/completions
weaveffi completions bash > ~/.local/share/bash-completion/completions/weaveffi
```

On systems that load `/etc/bash_completion.d`:

```bash
sudo weaveffi completions bash > /etc/bash_completion.d/weaveffi
```

## zsh

Write the script as `_weaveffi` in a directory on your `$fpath`:

```bash
mkdir -p ~/.zfunc
weaveffi completions zsh > ~/.zfunc/_weaveffi
```

Then in `~/.zshrc` (run `compinit` after updating `fpath`):

```zsh
fpath=(~/.zfunc $fpath)
autoload -Uz compinit && compinit
```

## fish

```bash
mkdir -p ~/.config/fish/completions
weaveffi completions fish > ~/.config/fish/completions/weaveffi.fish
```

## PowerShell

Append the script to your profile:

```powershell
weaveffi completions powershell >> $PROFILE
```

Reload the profile or open a new session.

## elvish

Evaluate the script from `rc.elv` (for example):

```elvish
eval (weaveffi completions elvish | slurp)
```

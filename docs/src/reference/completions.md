# Shell Completions

`weaveffi completions <shell>` prints a completion script to standard
output, where `<shell>` is `bash`, `zsh`, `fish`, `powershell`, or `elvish`:

```bash
weaveffi completions bash
```

Install the script with the section for your shell below. The Bash, Zsh, and
fish setups save a copy of the script, so regenerate it after upgrading the
CLI to pick up new commands and flags; the PowerShell and Elvish setups run
the CLI each time the shell starts and stay current on their own.

## Bash

With the [bash-completion](https://github.com/scop/bash-completion) package
installed, write the script to your user completions directory, where it's
loaded the first time you complete `weaveffi`:

```bash
mkdir -p ~/.local/share/bash-completion/completions
weaveffi completions bash > ~/.local/share/bash-completion/completions/weaveffi
```

To install it for every user on a system that loads
`/etc/bash_completion.d`, write it with `sudo tee` (a plain `sudo weaveffi
... >` fails, because the redirection runs without root):

```bash
weaveffi completions bash | sudo tee /etc/bash_completion.d/weaveffi > /dev/null
```

## Zsh

Write the script as `_weaveffi` in a directory on your `$fpath`:

```zsh
mkdir -p ~/.zfunc
weaveffi completions zsh > ~/.zfunc/_weaveffi
```

Then add the directory to `fpath` in `~/.zshrc`, before `compinit` runs. If
your configuration already calls `compinit` (as Oh My Zsh does), add only the
first line, above that call:

```zsh
fpath=(~/.zfunc $fpath)
autoload -Uz compinit && compinit
```

## fish

```fish
mkdir -p ~/.config/fish/completions
weaveffi completions fish > ~/.config/fish/completions/weaveffi.fish
```

## PowerShell

Add a line to your profile that loads the script in each session, creating
the profile first if it doesn't exist:

```powershell
if (!(Test-Path $PROFILE)) { New-Item -ItemType File -Path $PROFILE -Force | Out-Null }
Add-Content $PROFILE 'weaveffi completions powershell | Out-String | Invoke-Expression'
```

Open a new session to load it.

## Elvish

Add this line to `~/.config/elvish/rc.elv`:

```elvish
eval (weaveffi completions elvish | slurp)
```

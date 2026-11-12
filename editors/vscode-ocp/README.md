# OpenCompose DSL for VS Code

Syntax highlighting for `.ocp` (OpenCompose) music description files.

## Features

- Full syntax highlighting for all DSL constructs
- Comment toggling (`Cmd+/`)
- Bracket matching and auto-closing
- Folding on `@` blocks
- Smart indentation

## Highlighted elements

| Element | Example | Color |
|---------|---------|-------|
| Keywords | `@project` `@track` `@phrase` `@preset` | Pink/Magenta |
| Properties | `tempo:` `instrument:` `mix:` `source:` | Cyan |
| Pitches | `C4` `Bb3` `F#5` | Green |
| Durations | `q` `h.` `8th` `16th~q` | Purple |
| Dynamics | `pp` `mf` `ff` | Orange |
| Articulations | `legato` `staccato` `pizzicato` | Red |
| Drums | `kick` `snare` `hihat` | Cyan bold |
| Strings | `"My Song"` | Yellow |
| Numbers | `-3dB` `25%` `440Hz` `100ms` | Purple |
| Comments | `// comment` | Gray italic |
| Bar lines | `\|` | Dim gray |

## Install

### From source (development)

```bash
# From the repo root
cd editors/vscode-ocp
code --install-extension .
```

### Or symlink for development

```bash
ln -s $(pwd)/editors/vscode-ocp ~/.vscode/extensions/opencompose-0.1.0
```

Then reload VS Code.

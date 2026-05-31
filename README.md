<div align="center">

<h1>
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="logo_dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="logo_light.svg">
  <img alt="treelix" height="128" src="logo_light.svg">
</picture>
</h1>

**treelix** — A post-modern modal text editor (Helix fork) with a built-in nvim-tree style file explorer sidebar.

[![GitHub Repo](https://img.shields.io/badge/repo-treelix-blue)](https://github.com/nathaniel-fargo/treelix)
[![Build status](https://github.com/nathaniel-fargo/treelix/actions/workflows/build.yml/badge.svg)](https://github.com/nathaniel-fargo/treelix/actions)
[![Documentation](https://shields.io/badge/-documentation-452859)](https://docs.helix-editor.com/)

</div>

> **Note:** This is a personal fork of [Helix](https://github.com/helix-editor/helix) focused on adding a persistent file tree sidebar (right side, nvim-tree inspired). Most features and keybindings remain identical to upstream Helix.

![Screenshot](./screenshot.png)

A [Kakoune](https://github.com/mawww/kakoune) / [Neovim](https://github.com/neovim/neovim) inspired editor, written in Rust.

The editing model is very heavily based on Kakoune; during development I found
myself agreeing with most of Kakoune's design decisions.

For more information, see the [website](https://helix-editor.com) or
[documentation](https://docs.helix-editor.com/).

All shortcuts/keymaps can be found [in the documentation on the website](https://docs.helix-editor.com/keymap.html).

[Troubleshooting](https://github.com/helix-editor/helix/wiki/Troubleshooting)

# Features

- Vim-like modal editing
- Multiple selections
- Built-in language server support
- Smart, incremental syntax highlighting and code editing via tree-sitter
- **Built-in file tree sidebar** (toggle with `Space`+`E`, nvim-tree inspired — right side, respects .gitignore, expand/collapse, open files)

> The file tree is a core feature of this treelix fork (not present in upstream Helix).

Although it's primarily a terminal-based editor, I am interested in exploring
a custom renderer (similar to Emacs) using wgpu.

Note: Only certain languages have indentation definitions at the moment. Check
`runtime/queries/<lang>/` for `indents.scm`.

# Installation

[Installation documentation](https://docs.helix-editor.com/install.html).

[![Packaging status](https://repology.org/badge/vertical-allrepos/helix-editor.svg?exclude_unsupported=1)](https://repology.org/project/helix-editor/versions)

# Contributing

Contributing guidelines can be found [here](./docs/CONTRIBUTING.md).

# Getting help

Your question might already be answered on the [FAQ](https://github.com/helix-editor/helix/wiki/FAQ).

Discuss upstream Helix on the community [Matrix Space](https://matrix.to/#/#helix-community:matrix.org). This treelix fork is a personal experiment — feel free to open issues/PRs here for the file tree enhancements.

# Credits

Thanks to [@jakenvac](https://github.com/jakenvac) for designing the logo!
